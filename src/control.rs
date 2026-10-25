// SPDX-License-Identifier: GPL-3.0-only

//! Small UI-independent decisions of the applet, kept here to be unit tested.

use std::io;
use std::time::Duration;

use crate::usage::{Period, Tracker, Usage};

pub const SAVE_EVERY: Duration = Duration::from_secs(30);

/// Whether unsaved usage must be written now: at most every [`SAVE_EVERY`],
/// immediately after a rollover, an alert, a cap change or a reset (`urgent`).
pub fn save_due(dirty: bool, urgent: bool, since_save: Duration) -> bool {
    dirty && (urgent || since_save >= SAVE_EVERY)
}

/// Two-step "reset this month" button.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResetConfirm {
    armed: bool,
}

impl ResetConfirm {
    /// Returns true when the reset must happen (second press).
    pub fn press(&mut self) -> bool {
        self.armed = !self.armed;
        !self.armed
    }

    /// Cancels a pending confirmation (popup closed, other action).
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    pub fn is_armed(self) -> bool {
        self.armed
    }
}

/// The month to roll to: the clock's, or when it is absurd the usage's own
/// period, so a broken clock never relabels nor resets anything.
pub fn period_for(clock: Option<Period>, usage: &Usage) -> Period {
    clock.unwrap_or(usage.period)
}

/// Tracks the shared `reset_requests` counter. Only an increase is a request:
/// a stale instance writing back an older value must never reset the month.
#[derive(Debug, Clone, Copy)]
pub struct ResetWatch {
    seen: u64,
}

impl ResetWatch {
    pub fn new(seen: u64) -> Self {
        Self { seen }
    }

    pub fn seen(self) -> u64 {
        self.seen
    }

    /// Returns true if `incoming` is a new reset request.
    pub fn observe(&mut self, incoming: u64) -> bool {
        let new = incoming > self.seen;
        self.note(incoming);
        new
    }

    /// Records a value this instance wrote itself.
    pub fn note(&mut self, value: u64) {
        self.seen = self.seen.max(value);
    }
}

/// Outcome of becoming the counting instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Takeover {
    /// The state file can neither be loaded nor moved aside: never save over it.
    pub save_blocked: bool,
    /// The usage was rolled over or relabelled: save right away.
    pub save_now: bool,
}

/// Applies the state file loaded when taking the lock. If it is exactly what we
/// last followed, our view (that plus what we observed since) is kept; otherwise
/// the file wins and the baseline restarts, since the previous owner already
/// counted up to its last save (at most one tick is lost, never counted twice).
pub fn takeover(
    loaded: io::Result<Option<Usage>>,
    followed: Option<&Usage>,
    tracker: &mut Tracker,
    clock: Option<Period>,
) -> Takeover {
    let save_now = match loaded {
        Err(_) => {
            return Takeover {
                save_blocked: true,
                save_now: false,
            };
        }
        Ok(Some(usage)) if Some(&usage) != followed => {
            let now = period_for(clock, &usage);
            let changed = tracker.adopt(usage, now);
            tracker.rebase();
            changed
        }
        Ok(_) => clock.is_some_and(|now| tracker.roll(now)),
    };
    Takeover {
        save_blocked: false,
        save_now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_due_rules() {
        let s = Duration::from_secs;
        assert!(!save_due(false, true, s(999)), "nothing to save");
        assert!(!save_due(true, false, s(29)), "throttled under 30 s");
        assert!(save_due(true, false, s(30)), "30 s elapsed");
        assert!(save_due(true, true, s(0)), "urgent saves now");
    }

    #[test]
    fn reset_needs_two_presses_and_disarms() {
        let mut r = ResetConfirm::default();
        assert!(!r.press(), "first press only arms");
        assert!(r.is_armed());
        assert!(r.press(), "second press resets");
        assert!(!r.is_armed());
        assert!(!r.press(), "re-armed from scratch after a reset");
        r.disarm();
        assert!(!r.press(), "disarm cancels a pending confirmation");
    }

    use crate::usage::{Counters, Period, Tracker, Usage};

    fn p(y: i32, m: u32) -> Period {
        Period::new(y, m).unwrap()
    }

    #[test]
    fn absurd_clock_keeps_saved_usage_on_takeover() {
        let saved = Usage {
            rx: 42,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(None, p(2000, 1)); // app placeholder
        assert!(!t.adopt(saved.clone(), period_for(None, &saved)));
        assert_eq!(t.usage, saved);
        assert!(!t.roll(period_for(Some(p(2026, 10)), &t.usage)));
        assert_eq!(t.usage.rx, 42);
    }

    #[test]
    fn reset_watch_rules() {
        let mut w = ResetWatch::new(5);
        assert!(!w.observe(5), "echo / repeated notification");
        assert!(!w.observe(4), "stale value going back never resets");
        assert_eq!(w.seen(), 5);
        assert!(w.observe(6), "another instance bumped");
        assert!(w.observe(9), "several bumps coalesced: one reset");
        assert!(!w.observe(9));
        w.note(12);
        assert!(!w.observe(12), "own request echoed back");
        w.note(3);
        assert_eq!(w.seen(), 12, "never goes back");
    }

    #[test]
    fn takeover_unloadable_file_blocks_saving_only() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.usage.rx = 3;
        let r = takeover(
            Err(std::io::Error::other("x")),
            None,
            &mut t,
            Some(p(2026, 10)),
        );
        assert_eq!(
            r,
            Takeover {
                save_blocked: true,
                save_now: false
            }
        );
        assert_eq!(t.usage.rx, 3);
    }

    #[test]
    fn takeover_keeps_own_view_when_file_is_what_we_followed() {
        let file = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(None, p(2026, 10));
        t.adopt(file.clone(), p(2026, 10));
        t.usage.rx = 15;
        let r = takeover(
            Ok(Some(file.clone())),
            Some(&file),
            &mut t,
            Some(p(2026, 10)),
        );
        assert!(!r.save_blocked && !r.save_now);
        assert_eq!(t.usage.rx, 15);
    }

    #[test]
    fn takeover_adopts_newer_file_without_double_counting() {
        let file = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(Counters { rx: 100, tx: 0 }), p(2026, 10));
        let newer = Usage {
            rx: 20,
            ..file.clone()
        };
        let r = takeover(Ok(Some(newer)), Some(&file), &mut t, Some(p(2026, 10)));
        assert!(!r.save_now);
        assert_eq!(t.usage.rx, 20);
        // the previous owner already counted up to its last save: start afresh
        t.sample(Some(Counters { rx: 150, tx: 0 }), p(2026, 10));
        assert_eq!(t.usage.rx, 20);
    }

    #[test]
    fn takeover_rolls_stale_file_and_saves_now() {
        let mut t = Tracker::new(None, p(2026, 10));
        let r = takeover(
            Ok(Some(Usage {
                rx: 5,
                ..Usage::empty(p(2026, 9))
            })),
            None,
            &mut t,
            Some(p(2026, 10)),
        );
        assert!(r.save_now);
        assert_eq!(t.usage, Usage::empty(p(2026, 10)));
    }

    #[test]
    fn takeover_with_absurd_clock_keeps_file() {
        let saved = Usage {
            rx: 42,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(None, p(2000, 1));
        let r = takeover(Ok(Some(saved.clone())), None, &mut t, None);
        assert!(!r.save_now);
        assert_eq!(t.usage, saved);
    }

    #[test]
    fn takeover_without_file_rolls_own_view() {
        let mut t = Tracker::new(None, p(2026, 9));
        let r = takeover(Ok(None), None, &mut t, Some(p(2026, 10)));
        assert!(r.save_now);
        assert_eq!(t.usage.period, p(2026, 10));
        let r = takeover(Ok(None), None, &mut t, None);
        assert!(!r.save_now);
    }

    #[test]
    fn takeover_followed_file_in_new_month_rolls_and_saves_now() {
        let file = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 9))
        };
        let mut t = Tracker::new(None, p(2026, 9));
        t.adopt(file.clone(), p(2026, 9));
        t.usage.rx = 15;
        let r = takeover(
            Ok(Some(file.clone())),
            Some(&file),
            &mut t,
            Some(p(2026, 10)),
        );
        assert_eq!(
            r,
            Takeover {
                save_blocked: false,
                save_now: true
            }
        );
        assert_eq!(t.usage, Usage::empty(p(2026, 10)));
    }

    #[test]
    fn takeover_keeping_own_view_keeps_the_baseline() {
        let file = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(Counters { rx: 100, tx: 0 }), p(2026, 10));
        t.adopt(file.clone(), p(2026, 10));
        takeover(
            Ok(Some(file.clone())),
            Some(&file),
            &mut t,
            Some(p(2026, 10)),
        );
        t.sample(Some(Counters { rx: 130, tx: 0 }), p(2026, 10));
        assert_eq!(t.usage.rx, 40, "readings continue across the takeover");
    }

    #[test]
    fn takeover_with_followed_but_file_gone_keeps_own_view() {
        let followed = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(Some(followed.clone()), p(2026, 10));
        t.usage.rx = 12;
        let r = takeover(Ok(None), Some(&followed), &mut t, Some(p(2026, 10)));
        assert!(!r.save_blocked && !r.save_now);
        assert_eq!(t.usage.rx, 12);
    }

    #[test]
    fn takeover_error_never_rolls() {
        let followed = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 9))
        };
        let mut t = Tracker::new(Some(followed.clone()), p(2026, 9));
        let before = t.usage.clone();
        let r = takeover(
            Err(std::io::Error::other("EIO")),
            Some(&followed),
            &mut t,
            Some(p(2026, 10)),
        );
        assert!(r.save_blocked && !r.save_now);
        assert_eq!(t.usage, before);
    }

    #[test]
    fn retaking_own_lock_keeps_unsaved_usage_when_followed_is_last_save() {
        let last_save = Usage {
            rx: 10,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(Some(last_save.clone()), p(2026, 10));
        t.usage.rx = 25; // counted since the last save
        takeover(
            Ok(Some(last_save.clone())),
            Some(&last_save),
            &mut t,
            Some(p(2026, 10)),
        );
        assert_eq!(t.usage.rx, 25);
    }
}
