// SPDX-License-Identifier: GPL-3.0-only

//! The COSMIC panel applet: live rate and monthly usage, popup with details and cap.

use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Datelike;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::futures::{SinkExt, channel::mpsc};
use cosmic::iced::{self, Alignment, Length, Subscription, stream, window};
use cosmic::widget::{button, column, divider, progress_bar, row, text, text_input};
use cosmic::{Element, Task, app, surface, theme};

use crate::config::{self, BandwidthConfig};
use crate::control::{ResetConfirm, ResetWatch, period_for, save_due, takeover};
use crate::sysfs::SYS_CLASS_NET;
use crate::usage::{Alert, Counters, Period, Tracker, Usage, check_alerts, fraction, parse_cap_gb};
use crate::{format, store, sysfs};

pub const APP_ID: &str = "io.github.andyraitra.CosmicBandwidth";

const TICK: Duration = Duration::from_secs(2);

pub fn run() -> iced::Result {
    cosmic::applet::run::<Bandwidth>(())
}

/// `None` when the system clock is absurd (e.g. 1970 without RTC).
fn current_period() -> Option<Period> {
    let now = chrono::Local::now();
    Period::new(now.year(), now.month())
}

/// One process runs per panel/dock and per output. The one holding the state
/// lock counts, alerts and saves; the others follow the saved file and show it
/// plus what they observe in between. Any instance can request a reset or change
/// the cap through cosmic-config, which all of them watch.
struct Bandwidth {
    core: app::Core,
    popup: Option<window::Id>,
    config: BandwidthConfig,
    config_handler: Option<cosmic_config::Config>,
    tracker: Tracker,
    state_path: Option<PathBuf>,
    /// Held while this instance is the one counting and saving.
    lock: Option<File>,
    /// Usage last adopted from the file while following.
    followed: Option<Usage>,
    /// The state file exists but could neither be loaded nor moved aside.
    save_blocked: bool,
    lock_error_logged: bool,
    resets: ResetWatch,
    last_tick: Option<Instant>,
    /// Bytes per second (rx, tx); `None` while the interface is missing.
    rate: Option<(f64, f64)>,
    dirty: bool,
    last_save: Instant,
    cap_input: String,
    cap_invalid: bool,
    reset: ResetConfirm,
    /// The last reset request could not be passed on to the counting instance.
    reset_failed: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    TogglePopup,
    Closed(window::Id),
    CapInput(String),
    CapSubmit,
    ResetMonth,
    ConfigChanged(BandwidthConfig),
    Shutdown,
}

impl Bandwidth {
    /// Current month for sampling and resets. With an absurd clock, the usage's
    /// own period: rolling to it is a no-op, so nothing is relabelled or reset.
    fn period(&self) -> Period {
        period_for(current_period(), &self.tracker.usage)
    }

    fn owner(&self) -> bool {
        self.lock.is_some()
    }

    /// Becomes the counting instance if no other one is, and gives the role up
    /// if the lock file was deleted or replaced under us.
    fn try_own(&mut self) {
        let Some(path) = self.state_path.clone() else {
            return;
        };
        if let Some(lock) = &self.lock {
            if store::lock_is_current(lock, &path) {
                return;
            }
            tracing::warn!(?path, "state lock file replaced, giving up ownership");
            self.lock = None;
        }
        let lock = match store::try_lock(&path) {
            Ok(Some(lock)) => lock,
            Ok(None) => return,
            Err(err) => {
                if !std::mem::replace(&mut self.lock_error_logged, true) {
                    tracing::error!(?err, ?path, "cannot take the state lock");
                }
                return;
            }
        };
        let loaded = store::load(&path);
        if let Err(err) = &loaded {
            tracing::error!(?err, ?path, "state file unusable, saving disabled");
        }
        // keep the lock even when saving is blocked: this instance still counts
        // and alerts, it just never writes over the file
        self.lock = Some(lock);
        let outcome = takeover(
            loaded,
            self.followed.as_ref(),
            &mut self.tracker,
            current_period(),
        );
        self.save_blocked = outcome.save_blocked;
        self.dirty = true;
        if outcome.save_now {
            self.save();
        }
    }

    fn follow(&mut self) {
        let Some(saved) = self.state_path.as_deref().and_then(store::peek) else {
            return;
        };
        if Some(&saved) != self.followed.as_ref() {
            let now = period_for(current_period(), &saved);
            self.tracker.adopt(saved.clone(), now);
            self.followed = Some(saved);
        }
    }

    fn save(&mut self) {
        if !self.owner() || self.save_blocked {
            return;
        }
        let Some(path) = &self.state_path else {
            return;
        };
        match store::save(path, &self.tracker.usage) {
            Ok(()) => {
                self.dirty = false;
                // retaking our own lock later must find "what we followed" = the file
                self.followed = Some(self.tracker.usage.clone());
            }
            Err(err) => tracing::error!(?err, ?path, "cannot save usage"),
        }
        self.last_save = Instant::now();
    }

    /// Sends due alerts (counting instance only) and returns whether any was sent.
    fn alert(&mut self) -> bool {
        if !self.owner() {
            return false;
        }
        let alerts = check_alerts(&mut self.tracker.usage, self.config.cap());
        for alert in &alerts {
            notify(*alert, self.tracker.usage.total(), self.config.cap_bytes);
        }
        !alerts.is_empty()
    }

    fn tick(&mut self) {
        self.try_own();
        let now = Instant::now();
        let reading = sysfs::read_counters(SYS_CLASS_NET.as_ref(), &self.config.interface);
        let period = self.tracker.usage.period;
        let delta = self.tracker.sample(reading, self.period());
        let rolled = self.tracker.usage.period != period;

        self.rate = reading.map(|_| match self.last_tick {
            Some(prev) => {
                let secs = now.duration_since(prev).as_secs_f64().max(f64::EPSILON);
                (delta.rx as f64 / secs, delta.tx as f64 / secs)
            }
            None => (0.0, 0.0),
        });
        self.last_tick = Some(now);

        if !self.owner() {
            self.follow();
            return;
        }
        let alerted = self.alert();
        if delta != Counters::default() || rolled || alerted {
            self.dirty = true;
        }
        if save_due(self.dirty, rolled || alerted, self.last_save.elapsed()) {
            self.save();
        }
    }

    fn set_cap(&mut self) {
        self.reset.disarm();
        let Ok(cap) = parse_cap_gb(&self.cap_input) else {
            self.cap_invalid = true;
            return;
        };
        self.cap_invalid = false;
        self.config.cap_bytes = cap.unwrap_or(0);
        self.cap_input = format::cap_input(self.config.cap_bytes);
        if let Some(handler) = &self.config_handler
            && let Err(err) = config::write_cap(handler, self.config.cap_bytes)
        {
            tracing::error!(?err, "cannot write config");
        }
        self.alert();
        self.dirty = true;
        self.save();
    }

    fn reset_month(&mut self) {
        self.tracker.reset(self.period());
        self.followed = None;
        self.alert();
        self.dirty = true;
        self.save();
    }

    /// Asks every instance, the counting one included, to reset the month.
    fn request_reset(&mut self) {
        self.reset_failed = false;
        if let Some(handler) = &self.config_handler {
            match config::request_reset(handler, self.resets.seen()) {
                Ok(value) => {
                    self.resets.note(value);
                    self.config.reset_requests = value;
                }
                Err(err) => {
                    tracing::error!(?err, "cannot write reset request");
                    // a follower cannot reset alone: its view would just be
                    // replaced by the file again
                    if !self.owner() {
                        self.reset_failed = true;
                        return;
                    }
                }
            }
        } else if !self.owner() {
            self.reset_failed = true;
            return;
        }
        self.reset_month();
    }

    fn apply_config(&mut self, config: BandwidthConfig) {
        if config.interface != self.config.interface {
            self.tracker.rebase();
            self.last_tick = None;
        }
        let cap_changed = config.cap_bytes != self.config.cap_bytes;
        if cap_changed {
            self.cap_input = format::cap_input(config.cap_bytes);
            self.cap_invalid = false;
        }
        let reset = self.resets.observe(config.reset_requests);
        self.config = config;
        if reset {
            self.reset_month();
        } else if self.alert() || cap_changed {
            self.dirty = true;
            self.save();
        }
    }

    /// Drops unsubmitted edits and pending confirmations when the popup opens or closes.
    fn reset_popup_state(&mut self) {
        self.cap_input = format::cap_input(self.config.cap_bytes);
        self.cap_invalid = false;
        self.reset.disarm();
        self.reset_failed = false;
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = theme::active().cosmic().spacing;
        let usage = &self.tracker.usage;
        let cap = self.config.cap();

        let status = match self.rate {
            Some(_) => self.config.interface.clone(),
            None => format!("{} (indisponible)", self.config.interface),
        };
        let line = |label: &'static str, value: String| {
            row![
                text::body(label),
                cosmic::widget::space::horizontal(),
                text::body(value)
            ]
            .align_y(Alignment::Center)
        };

        let mut content = column![
            text::heading("Données ce mois"),
            text::caption(status),
            line("Reçu", format::bytes(usage.rx)),
            line("Envoyé", format::bytes(usage.tx)),
            line("Total", format::usage(usage.total(), cap)),
        ]
        .spacing(spacing.space_xxs);

        if let (Some(f), Some(pct)) = (
            fraction(usage.total(), cap),
            format::percent(usage.total(), cap),
        ) {
            content = content
                .push(progress_bar::determinate_linear(f).markers([0.8]))
                .push(text::caption(pct));
        }
        content = content.push(text::caption(format!(
            "Remise à zéro le {}",
            format::reset_date(usage.period.next())
        )));

        let mut cap_box = column![
            text::body("Plafond mensuel (Go, vide ou 0 = aucun)"),
            text_input("Aucun", &self.cap_input)
                .on_input(Message::CapInput)
                .on_submit(|_| Message::CapSubmit),
        ]
        .spacing(spacing.space_xxs);
        if self.cap_invalid {
            cap_box = cap_box.push(text::caption(
                "Valeur invalide : un nombre de Go, par ex. 50 ou 2,5",
            ));
        }
        if self.save_blocked {
            content = content.push(text::caption(
                "Sauvegarde désactivée : fichier d'état illisible (voir le journal)",
            ));
        }

        let reset_label = if self.reset.is_armed() {
            "Confirmer la remise à zéro"
        } else {
            "Remettre à zéro le mois"
        };
        let mut reset = column![button::destructive(reset_label).on_press(Message::ResetMonth)]
            .spacing(spacing.space_xxs);
        if self.reset_failed {
            reset = reset.push(text::caption(
                "Remise à zéro impossible : configuration non enregistrable",
            ));
        }

        let body = column![
            content,
            divider::horizontal::default(),
            cap_box,
            divider::horizontal::default(),
            reset,
        ]
        .spacing(spacing.space_s)
        .padding(spacing.space_s)
        .width(Length::Fixed(300.0));

        self.core.applet.popup_container(body).into()
    }
}

fn notify(alert: Alert, total: u64, cap: u64) {
    let (summary, icon) = match alert {
        Alert::Warn => ("80 % du forfait data atteint", "dialog-warning-symbolic"),
        Alert::Full => ("Forfait data épuisé", "dialog-error-symbolic"),
    };
    let body = format!("{} consommés ce mois", format::usage(total, Some(cap)));
    // notify-rust blocks on D-Bus; keep it off the UI thread
    std::thread::spawn(move || {
        if let Err(err) = notify_rust::Notification::new()
            .appname("Bandwidth")
            .summary(summary)
            .body(&body)
            .icon(icon)
            .show()
        {
            tracing::error!(?err, "cannot send notification");
        }
    });
}

fn shutdown_signals() -> Subscription<Message> {
    Subscription::run(|| {
        stream::channel(1, |mut output: mpsc::Sender<Message>| async move {
            use tokio::signal::unix::{SignalKind, signal};
            let (Ok(mut term), Ok(mut int)) = (
                signal(SignalKind::terminate()),
                signal(SignalKind::interrupt()),
            ) else {
                tracing::error!("cannot listen for termination signals");
                return;
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
            let _ = output.send(Message::Shutdown).await;
        })
    })
}

impl cosmic::Application for Bandwidth {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut app::Core {
        &mut self.core
    }

    fn init(core: app::Core, _flags: ()) -> (Self, app::Task<Message>) {
        let config_handler = cosmic_config::Config::new(APP_ID, BandwidthConfig::VERSION)
            .inspect_err(|err| tracing::error!(?err, "cannot open config"))
            .ok();
        let config = config_handler
            .as_ref()
            .map(|handler| match BandwidthConfig::get_entry(handler) {
                Ok(config) => config,
                Err((errs, config)) => {
                    for err in errs.into_iter().filter(cosmic_config::Error::is_err) {
                        tracing::error!(?err, "invalid config entry");
                    }
                    config
                }
            })
            .unwrap_or_default();

        let state_path =
            store::state_path(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"));
        if state_path.is_none() {
            tracing::error!("no HOME: usage will not be saved");
        }
        // a clock before 2000 is broken: start in a placeholder month, the first
        // sane reading of the clock rolls over
        let start = current_period().unwrap_or(Period {
            year: 2000,
            month: 1,
        });

        let mut app = Self {
            core,
            popup: None,
            cap_input: format::cap_input(config.cap_bytes),
            resets: ResetWatch::new(config.reset_requests),
            config,
            config_handler,
            tracker: Tracker::new(None, start),
            state_path,
            lock: None,
            followed: None,
            save_blocked: false,
            lock_error_logged: false,
            last_tick: None,
            rate: None,
            dirty: false,
            last_save: Instant::now(),
            cap_invalid: false,
            reset: ResetConfirm::default(),
            reset_failed: false,
        };
        app.tick();
        (app, Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn update(&mut self, message: Message) -> app::Task<Message> {
        match message {
            Message::Tick => self.tick(),
            Message::TogglePopup => {
                if let Some(popup) = self.popup.take() {
                    return surface::surface_task(surface::action::destroy_popup(popup));
                }
                self.reset_popup_state();
                return surface::surface_task(surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Bandwidth| {
                        let id = window::Id::unique();
                        app.popup = Some(id);
                        app.core.applet.get_popup_settings(
                            app.core.main_window_id().expect("applet has a main window"),
                            id,
                            None,
                            None,
                            None,
                        )
                    },
                    None,
                ));
            }
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.reset_popup_state();
                }
            }
            Message::CapInput(value) => {
                self.cap_input = value;
                self.cap_invalid = false;
                self.reset.disarm();
            }
            Message::CapSubmit => self.set_cap(),
            Message::ResetMonth => {
                if self.reset.press() {
                    self.request_reset();
                }
            }
            Message::ConfigChanged(config) => self.apply_config(config),
            Message::Shutdown => {
                if self.dirty {
                    self.save();
                }
                std::process::exit(0);
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        self.core
            .applet
            .text_button(
                self.core.applet.text(format::panel(
                    self.rate,
                    self.tracker.usage.total(),
                    self.config.cap(),
                    self.core.applet.is_horizontal(),
                )),
                Message::TogglePopup,
            )
            .into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if self.popup == Some(id) {
            self.popup_view()
        } else {
            text("").into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(TICK).map(|_| Message::Tick),
            shutdown_signals(),
            self.core
                .watch_config::<BandwidthConfig>(APP_ID)
                .map(|update| {
                    for err in update.errors.iter().filter(|e| e.is_err()) {
                        tracing::error!(?err, "error watching config");
                    }
                    Message::ConfigChanged(update.config)
                }),
        ])
    }

    fn style(&self) -> Option<iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
