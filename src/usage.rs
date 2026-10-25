// SPDX-License-Identifier: GPL-3.0-only

//! Pure accounting logic: counter deltas, monthly rollover, cap and alerts.
//! No I/O here so every edge case can be unit tested.

use serde::{Deserialize, Serialize};

/// A calendar month. The usage counter resets on the 1st of each month.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Period {
    pub year: i32,
    pub month: u32,
}

impl Period {
    /// `None` for an invalid month or an absurd year (a broken clock or file).
    pub fn new(year: i32, month: u32) -> Option<Self> {
        ((2000..=9999).contains(&year) && (1..=12).contains(&month)).then_some(Self { year, month })
    }

    /// The month after this one (the next reset date is its 1st day).
    pub fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year.saturating_add(1),
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }
}

/// Raw kernel byte counters of one interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub rx: u64,
    pub tx: u64,
}

/// Bytes accumulated during one period. This is what gets persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub period: Period,
    pub rx: u64,
    pub tx: u64,
    #[serde(default)]
    pub alerted_warn: bool,
    #[serde(default)]
    pub alerted_full: bool,
}

impl Usage {
    pub fn empty(period: Period) -> Self {
        Self {
            period,
            rx: 0,
            tx: 0,
            alerted_warn: false,
            alerted_full: false,
        }
    }

    pub fn total(&self) -> u64 {
        self.rx.saturating_add(self.tx)
    }
}

/// Turns successive counter readings into usage, surviving counter resets.
#[derive(Debug, Clone)]
pub struct Tracker {
    pub usage: Usage,
    last: Option<Counters>,
}

impl Tracker {
    /// Like [`Tracker::new`], also telling whether the saved usage had to be
    /// rolled over or relabelled, i.e. whether it should be saved right away.
    pub fn restore(saved: Option<Usage>, now: Period) -> (Self, bool) {
        let usage = saved.unwrap_or_else(|| Usage::empty(now));
        let mut tracker = Self { usage, last: None };
        let changed = tracker.roll(now);
        (tracker, changed)
    }

    /// Replaces the usage with one saved by another instance, keeping the
    /// baseline so this instance's own readings continue seamlessly.
    /// Returns true if the adopted usage had to be rolled over or relabelled.
    pub fn adopt(&mut self, usage: Usage, now: Period) -> bool {
        self.usage = usage;
        self.roll(now)
    }

    /// Starts from persisted usage, discarding it if it belongs to an earlier month.
    pub fn new(saved: Option<Usage>, now: Period) -> Self {
        Self::restore(saved, now).0
    }

    /// Rolls over to `now` if a new month started. A clock that went back never
    /// resets the counter: one month back is tolerated as is (clock skew around a
    /// month boundary), further back the totals are relabelled to `now` so a
    /// period saved by a wildly wrong clock cannot freeze the monthly reset.
    /// Returns true if the usage changed.
    pub fn roll(&mut self, now: Period) -> bool {
        if now > self.usage.period {
            self.usage = Usage::empty(now);
            true
        } else if now.next() < self.usage.period {
            self.usage.period = now;
            true
        } else {
            false
        }
    }

    /// Feeds one reading (`None` = interface missing or unreadable) and returns
    /// the bytes counted since the previous reading.
    pub fn sample(&mut self, reading: Option<Counters>, now: Period) -> Counters {
        self.roll(now);
        let delta = match (self.last, reading) {
            (Some(last), Some(cur)) => Counters {
                // a counter that went down was reset: count nothing, rebase on it
                rx: cur.rx.saturating_sub(last.rx),
                tx: cur.tx.saturating_sub(last.tx),
            },
            _ => Counters::default(),
        };
        self.last = reading;
        self.usage.rx = self.usage.rx.saturating_add(delta.rx);
        self.usage.tx = self.usage.tx.saturating_add(delta.tx);
        delta
    }

    /// Forgets the baseline, e.g. when the watched interface changes, so the
    /// next reading is not diffed against another interface's counters.
    pub fn rebase(&mut self) {
        self.last = None;
    }

    /// Manual "reset this month" from the popup.
    pub fn reset(&mut self, now: Period) {
        self.usage = Usage::empty(now);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// 80 % of the cap reached.
    Warn,
    /// 100 % of the cap reached.
    Full,
}

/// Returns the alerts to send now, at most once per period each. Flags re-arm
/// when usage drops back under a threshold (cap raised, manual reset).
/// `cap` of `None` or `Some(0)` means no cap: no alerts.
pub fn check_alerts(usage: &mut Usage, cap: Option<u64>) -> Vec<Alert> {
    let Some(cap) = cap.filter(|&c| c > 0) else {
        return Vec::new();
    };
    let total = u128::from(usage.total());
    let cap = u128::from(cap);
    let warn = total * 5 >= cap * 4;
    let full = total >= cap;
    usage.alerted_warn &= warn;
    usage.alerted_full &= full;

    let mut alerts = Vec::new();
    if full && !usage.alerted_full {
        alerts.push(Alert::Full);
        usage.alerted_full = true;
        usage.alerted_warn = true;
    } else if warn && !usage.alerted_warn {
        alerts.push(Alert::Warn);
        usage.alerted_warn = true;
    }
    alerts
}

/// Share of the cap used, clamped to `0.0..=1.0`. `None` without a cap.
pub fn fraction(total: u64, cap: Option<u64>) -> Option<f32> {
    let cap = cap.filter(|&c| c > 0)?;
    Some((total as f64 / cap as f64).min(1.0) as f32)
}

/// The cap typed in the popup is not a number of gigabytes in `0..=1_000_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCap;

/// Parses a cap typed in the popup, in gigabytes (1 Go = 10^9 bytes).
/// Empty or 0 means no cap. Accepts `,` or `.` as decimal separator.
pub fn parse_cap_gb(input: &str) -> Result<Option<u64>, InvalidCap> {
    const MAX_GB: f64 = 1_000_000.0;
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    // `1 000` (French thousands separator, any kind of space) and `2,5`
    let input: String = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == ',' { '.' } else { c })
        .collect();
    // plain decimal only: f64 parsing would also take `1e3`, `inf` or `NaN`
    if !input
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
    {
        return Err(InvalidCap);
    }
    let gb: f64 = input.parse().map_err(|_| InvalidCap)?;
    if !gb.is_finite() || !(0.0..=MAX_GB).contains(&gb) {
        return Err(InvalidCap);
    }
    if gb == 0.0 {
        return Ok(None);
    }
    let bytes = gb * 1e9;
    // positive but below one byte: reject rather than silently disabling the cap
    if bytes < 1.0 {
        return Err(InvalidCap);
    }
    Ok(Some(bytes.round() as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(year: i32, month: u32) -> Period {
        Period::new(year, month).unwrap()
    }

    fn c(rx: u64, tx: u64) -> Counters {
        Counters { rx, tx }
    }

    #[test]
    fn period_validates_month() {
        assert!(Period::new(2026, 0).is_none());
        assert!(Period::new(2026, 13).is_none());
        assert_eq!(
            Period::new(2026, 12),
            Some(Period {
                year: 2026,
                month: 12
            })
        );
    }

    #[test]
    fn period_next_wraps_year() {
        assert_eq!(p(2026, 10).next(), p(2026, 11));
        assert_eq!(p(2026, 12).next(), p(2027, 1));
    }

    #[test]
    fn first_reading_counts_nothing() {
        let mut t = Tracker::new(None, p(2026, 10));
        assert_eq!(t.sample(Some(c(5_000_000, 1_000)), p(2026, 10)), c(0, 0));
        assert_eq!(t.usage.total(), 0);
    }

    #[test]
    fn accumulates_deltas() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(100, 10)), p(2026, 10));
        assert_eq!(t.sample(Some(c(150, 30)), p(2026, 10)), c(50, 20));
        t.sample(Some(c(400, 30)), p(2026, 10));
        assert_eq!((t.usage.rx, t.usage.tx), (300, 20));
        assert_eq!(t.usage.total(), 320);
    }

    #[test]
    fn counter_reset_keeps_total_and_rebases() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(1_000, 1_000)), p(2026, 10));
        t.sample(Some(c(2_000, 1_500)), p(2026, 10));
        // interface down/up or driver reload: counters restart from ~0
        assert_eq!(t.sample(Some(c(10, 5)), p(2026, 10)), c(0, 0));
        assert_eq!(t.usage.total(), 1_500);
        assert_eq!(t.sample(Some(c(110, 25)), p(2026, 10)), c(100, 20));
        assert_eq!(t.usage.total(), 1_620);
    }

    #[test]
    fn reset_of_one_counter_only() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(1_000, 1_000)), p(2026, 10));
        assert_eq!(t.sample(Some(c(10, 1_100)), p(2026, 10)), c(0, 100));
    }

    #[test]
    fn missing_interface_forgets_baseline() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(1_000, 0)), p(2026, 10));
        assert_eq!(t.sample(None, p(2026, 10)), c(0, 0));
        // back with a higher counter: must not count the gap as traffic
        assert_eq!(t.sample(Some(c(9_000, 0)), p(2026, 10)), c(0, 0));
        assert_eq!(t.sample(Some(c(9_500, 0)), p(2026, 10)), c(500, 0));
        assert_eq!(t.usage.total(), 500);
    }

    #[test]
    fn saturates_instead_of_overflowing() {
        let saved = Usage {
            rx: u64::MAX - 5,
            ..Usage::empty(p(2026, 10))
        };
        let mut t = Tracker::new(Some(saved), p(2026, 10));
        t.sample(Some(c(0, 0)), p(2026, 10));
        t.sample(Some(c(100, 0)), p(2026, 10));
        assert_eq!(t.usage.rx, u64::MAX);
        assert_eq!(t.usage.total(), u64::MAX);
    }

    #[test]
    fn resumes_saved_usage_of_same_month() {
        let saved = Usage {
            rx: 7,
            tx: 3,
            ..Usage::empty(p(2026, 10))
        };
        let t = Tracker::new(Some(saved.clone()), p(2026, 10));
        assert_eq!(t.usage, saved);
    }

    #[test]
    fn discards_saved_usage_of_previous_month() {
        let saved = Usage {
            rx: 7,
            tx: 3,
            alerted_warn: true,
            ..Usage::empty(p(2026, 9))
        };
        let t = Tracker::new(Some(saved), p(2026, 10));
        assert_eq!(t.usage, Usage::empty(p(2026, 10)));
    }

    #[test]
    fn discards_saved_usage_of_previous_year() {
        let saved = Usage {
            rx: 7,
            ..Usage::empty(p(2026, 12))
        };
        let t = Tracker::new(Some(saved), p(2027, 1));
        assert_eq!(t.usage, Usage::empty(p(2027, 1)));
    }

    #[test]
    fn keeps_saved_usage_when_clock_is_behind() {
        let saved = Usage {
            rx: 7,
            ..Usage::empty(p(2026, 11))
        };
        let t = Tracker::new(Some(saved.clone()), p(2026, 10));
        assert_eq!(t.usage, saved);
    }

    #[test]
    fn month_change_between_samples_assigns_delta_to_new_month() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(100, 0)), p(2026, 10));
        t.sample(Some(c(200, 0)), p(2026, 10));
        assert_eq!(t.sample(Some(c(250, 0)), p(2026, 11)), c(50, 0));
        assert_eq!(t.usage.period, p(2026, 11));
        assert_eq!(t.usage.total(), 50);
    }

    #[test]
    fn year_change_rolls_over() {
        let mut t = Tracker::new(None, p(2026, 12));
        t.sample(Some(c(100, 0)), p(2026, 12));
        t.sample(Some(c(200, 0)), p(2026, 12));
        assert!(t.roll(p(2027, 1)));
        assert_eq!(t.usage, Usage::empty(p(2027, 1)));
        assert_eq!(t.sample(Some(c(300, 0)), p(2027, 1)), c(100, 0));
    }

    #[test]
    fn roll_is_noop_within_month_and_backwards() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(0, 0)), p(2026, 10));
        t.sample(Some(c(10, 0)), p(2026, 10));
        assert!(!t.roll(p(2026, 10)));
        assert!(!t.roll(p(2026, 9)));
        assert_eq!(t.usage.period, p(2026, 10));
        assert_eq!(t.usage.total(), 10);
    }

    #[test]
    fn manual_reset_zeroes_but_keeps_baseline() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(100, 0)), p(2026, 10));
        t.sample(Some(c(200, 0)), p(2026, 10));
        t.usage.alerted_warn = true;
        t.reset(p(2026, 10));
        assert_eq!(t.usage, Usage::empty(p(2026, 10)));
        assert_eq!(t.sample(Some(c(230, 0)), p(2026, 10)), c(30, 0));
    }

    #[test]
    fn rebase_drops_baseline_but_keeps_usage() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(100, 0)), p(2026, 10));
        t.sample(Some(c(200, 0)), p(2026, 10));
        t.rebase();
        // other interface with lower counters: neither a delta nor a fake reset
        assert_eq!(t.sample(Some(c(50, 0)), p(2026, 10)), c(0, 0));
        assert_eq!(t.sample(Some(c(80, 0)), p(2026, 10)), c(30, 0));
        assert_eq!(t.usage.total(), 130);
    }

    #[test]
    fn no_cap_means_no_alert_and_no_fraction() {
        let mut u = Usage {
            rx: u64::MAX,
            ..Usage::empty(p(2026, 10))
        };
        assert!(check_alerts(&mut u, None).is_empty());
        assert!(check_alerts(&mut u, Some(0)).is_empty());
        assert_eq!(fraction(u.total(), None), None);
        assert_eq!(fraction(u.total(), Some(0)), None);
    }

    #[test]
    fn warn_then_full_once_each() {
        let mut u = Usage::empty(p(2026, 10));
        u.rx = 79;
        assert!(check_alerts(&mut u, Some(100)).is_empty());
        u.rx = 80;
        assert_eq!(check_alerts(&mut u, Some(100)), vec![Alert::Warn]);
        assert!(check_alerts(&mut u, Some(100)).is_empty());
        u.rx = 99;
        assert!(check_alerts(&mut u, Some(100)).is_empty());
        u.rx = 100;
        assert_eq!(check_alerts(&mut u, Some(100)), vec![Alert::Full]);
        u.rx = 500;
        assert!(check_alerts(&mut u, Some(100)).is_empty());
    }

    #[test]
    fn jump_past_full_sends_only_full() {
        let mut u = Usage {
            rx: 150,
            ..Usage::empty(p(2026, 10))
        };
        assert_eq!(check_alerts(&mut u, Some(100)), vec![Alert::Full]);
        assert!(u.alerted_warn && u.alerted_full);
    }

    #[test]
    fn raising_cap_rearms_alerts() {
        let mut u = Usage {
            rx: 100,
            ..Usage::empty(p(2026, 10))
        };
        check_alerts(&mut u, Some(100));
        // cap raised to 200: 50 %, both re-armed
        assert!(check_alerts(&mut u, Some(200)).is_empty());
        assert!(!u.alerted_warn && !u.alerted_full);
        u.rx = 160;
        assert_eq!(check_alerts(&mut u, Some(200)), vec![Alert::Warn]);
    }

    #[test]
    fn raising_cap_between_80_and_100_rearms_full_only() {
        let mut u = Usage {
            rx: 100,
            ..Usage::empty(p(2026, 10))
        };
        check_alerts(&mut u, Some(100));
        assert!(check_alerts(&mut u, Some(110)).is_empty());
        assert!(u.alerted_warn && !u.alerted_full);
        u.rx = 110;
        assert_eq!(check_alerts(&mut u, Some(110)), vec![Alert::Full]);
    }

    #[test]
    fn alert_thresholds_do_not_overflow() {
        let mut u = Usage {
            rx: u64::MAX,
            ..Usage::empty(p(2026, 10))
        };
        assert_eq!(check_alerts(&mut u, Some(u64::MAX)), vec![Alert::Full]);
        // u64::MAX is a multiple of 5, so this is exactly 80 %
        let mut u = Usage {
            rx: u64::MAX / 5 * 4,
            ..Usage::empty(p(2026, 10))
        };
        assert_eq!(check_alerts(&mut u, Some(u64::MAX)), vec![Alert::Warn]);
        let mut u = Usage {
            rx: u64::MAX / 5 * 4 - 1,
            ..Usage::empty(p(2026, 10))
        };
        assert!(check_alerts(&mut u, Some(u64::MAX)).is_empty());
    }

    #[test]
    fn fraction_is_clamped() {
        assert_eq!(fraction(0, Some(100)), Some(0.0));
        assert_eq!(fraction(50, Some(100)), Some(0.5));
        assert_eq!(fraction(250, Some(100)), Some(1.0));
    }

    #[test]
    fn parse_cap_accepts_valid_values() {
        assert_eq!(parse_cap_gb("50"), Ok(Some(50_000_000_000)));
        assert_eq!(parse_cap_gb(" 2,5 "), Ok(Some(2_500_000_000)));
        assert_eq!(parse_cap_gb("2.5"), Ok(Some(2_500_000_000)));
        assert_eq!(parse_cap_gb("0.001"), Ok(Some(1_000_000)));
    }

    #[test]
    fn parse_cap_empty_or_zero_means_none() {
        assert_eq!(parse_cap_gb(""), Ok(None));
        assert_eq!(parse_cap_gb("   "), Ok(None));
        assert_eq!(parse_cap_gb("0"), Ok(None));
        assert_eq!(parse_cap_gb("0,0"), Ok(None));
    }

    #[test]
    fn parse_cap_rejects_invalid_values() {
        for bad in [
            "abc", "-5", "1e99", "inf", "NaN", "5 Go", "1,2,3", "1000001", "1e-12",
        ] {
            assert_eq!(parse_cap_gb(bad), Err(InvalidCap), "{bad}");
        }
    }

    #[test]
    fn period_rejects_absurd_years() {
        assert!(Period::new(1999, 12).is_none());
        assert!(Period::new(10_000, 1).is_none());
        assert!(Period::new(i32::MAX, 12).is_none());
        assert!(Period::new(2000, 1).is_some());
        assert!(Period::new(9999, 12).is_some());
    }

    #[test]
    fn period_next_never_overflows() {
        let p = Period {
            year: i32::MAX,
            month: 12,
        };
        assert_eq!(p.next().year, i32::MAX);
    }

    #[test]
    fn restore_reports_rollover() {
        let saved = Usage {
            rx: 7,
            ..Usage::empty(p(2026, 9))
        };
        let (t, changed) = Tracker::restore(Some(saved.clone()), p(2026, 10));
        assert!(changed);
        assert_eq!(t.usage, Usage::empty(p(2026, 10)));
        let (t, changed) = Tracker::restore(
            Some(Usage {
                period: p(2026, 10),
                ..saved
            }),
            p(2026, 10),
        );
        assert!(!changed);
        assert_eq!(t.usage.rx, 7);
        let (_, changed) = Tracker::restore(None, p(2026, 10));
        assert!(!changed);
    }

    #[test]
    fn far_future_saved_period_is_relabelled_keeping_totals() {
        // e.g. RTC once at 2037: never freeze the monthly reset for years
        let saved = Usage {
            rx: 5,
            ..Usage::empty(p(2037, 1))
        };
        let (mut t, changed) = Tracker::restore(Some(saved), p(2026, 10));
        assert!(changed);
        assert_eq!((t.usage.period, t.usage.rx), (p(2026, 10), 5));
        assert!(t.roll(p(2026, 11)));
        assert_eq!(t.usage, Usage::empty(p(2026, 11)));
    }

    #[test]
    fn far_future_period_while_running_is_relabelled() {
        let mut t = Tracker::new(None, p(2037, 1));
        t.sample(Some(c(0, 0)), p(2037, 1));
        t.sample(Some(c(9, 0)), p(2037, 1));
        assert!(t.roll(p(2026, 10)));
        assert_eq!((t.usage.period, t.usage.rx), (p(2026, 10), 9));
    }

    #[test]
    fn adopt_keeps_baseline() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(100, 0)), p(2026, 10));
        let theirs = Usage {
            rx: 1_000,
            ..Usage::empty(p(2026, 10))
        };
        assert!(!t.adopt(theirs, p(2026, 10)));
        assert_eq!(t.sample(Some(c(150, 0)), p(2026, 10)), c(50, 0));
        assert_eq!(t.usage.rx, 1_050);
    }

    #[test]
    fn adopt_rolls_stale_usage() {
        let mut t = Tracker::new(None, p(2026, 11));
        assert!(t.adopt(
            Usage {
                rx: 1_000,
                ..Usage::empty(p(2026, 10))
            },
            p(2026, 11),
        ));
        assert_eq!(t.usage, Usage::empty(p(2026, 11)));
    }

    #[test]
    fn parse_cap_rejects_sub_byte_even_when_rounding_up() {
        assert_eq!(parse_cap_gb("0.0000000005"), Err(InvalidCap));
        assert_eq!(parse_cap_gb("0,0000000009"), Err(InvalidCap));
        assert_eq!(parse_cap_gb("0.000000001"), Ok(Some(1)));
    }

    #[test]
    fn clock_behind_boundary_is_exactly_one_month() {
        let saved = |period| Usage {
            rx: 7,
            ..Usage::empty(period)
        };
        let (t, changed) = Tracker::restore(Some(saved(p(2026, 11))), p(2026, 10));
        assert!(!changed);
        assert_eq!(t.usage, saved(p(2026, 11)));
        let (t, changed) = Tracker::restore(Some(saved(p(2026, 12))), p(2026, 10));
        assert!(changed);
        assert_eq!(t.usage, saved(p(2026, 10)));
        let (t, changed) = Tracker::restore(Some(saved(p(2027, 1))), p(2026, 12));
        assert!(!changed);
        assert_eq!(t.usage, saved(p(2027, 1)));
        let (t, changed) = Tracker::restore(Some(saved(p(2027, 2))), p(2026, 12));
        assert!(changed);
        assert_eq!(t.usage, saved(p(2026, 12)));
    }

    #[test]
    fn adopt_relabels_far_future_usage_keeping_totals_flags_and_baseline() {
        let mut t = Tracker::new(None, p(2026, 10));
        t.sample(Some(c(100, 0)), p(2026, 10));
        let theirs = Usage {
            rx: 9,
            alerted_warn: true,
            ..Usage::empty(p(2037, 1))
        };
        assert!(t.adopt(theirs, p(2026, 10)));
        assert_eq!(
            t.usage,
            Usage {
                rx: 9,
                alerted_warn: true,
                ..Usage::empty(p(2026, 10))
            }
        );
        assert_eq!(t.sample(Some(c(130, 0)), p(2026, 10)), c(30, 0));
    }

    #[test]
    fn parse_cap_edges() {
        assert_eq!(parse_cap_gb("1000000"), Ok(Some(1_000_000_000_000_000)));
        assert_eq!(parse_cap_gb("1000000,0000001"), Err(InvalidCap));
        assert_eq!(parse_cap_gb("-0"), Ok(None));
        assert_eq!(parse_cap_gb(",5"), Ok(Some(500_000_000)));
        assert_eq!(parse_cap_gb("\u{a0}5\u{a0}"), Ok(Some(5_000_000_000)));
        // French thousands separators are accepted
        assert_eq!(parse_cap_gb("1\u{202f}000"), Ok(Some(1_000_000_000_000)));
        assert_eq!(parse_cap_gb("1 000"), Ok(Some(1_000_000_000_000)));
    }

    #[test]
    fn parse_cap_rejects_letters_including_exponents() {
        for bad in ["1e3", "1E3", "5e-1", "0x10", "\u{200b}5"] {
            assert_eq!(parse_cap_gb(bad), Err(InvalidCap), "{bad:?}");
        }
    }

    #[test]
    fn parse_cap_ignores_any_whitespace_in_thousands() {
        for s in ["1 000", "1\u{a0}000", "1\u{202f}000", "1\t000", " 1 000,5 "] {
            assert!(
                matches!(parse_cap_gb(s), Ok(Some(b)) if b >= 1_000_000_000_000),
                "{s:?}"
            );
        }
    }

    #[test]
    fn parse_cap_bounds() {
        assert_eq!(parse_cap_gb("1 000 000"), Ok(Some(1_000_000_000_000_000)));
        assert_eq!(parse_cap_gb("1000000,000001"), Err(InvalidCap));
        assert_eq!(parse_cap_gb("0,000000001"), Ok(Some(1)));
    }
}
