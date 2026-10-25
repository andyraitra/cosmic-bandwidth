// SPDX-License-Identifier: GPL-3.0-only

//! Human readable sizes, in decimal units (1 Ko = 1000 o) like macOS and carriers.

use crate::usage::Period;

const UNITS: [&str; 5] = ["o", "Ko", "Mo", "Go", "To"];

/// Splits a byte count into a rounded number and its unit, e.g. `("12.4", "Go")`.
/// Bytes have no decimals; larger units have one, dropped when `trim` and it is `.0`.
pub fn split_bytes(bytes: u64, trim: bool) -> (String, &'static str) {
    if bytes < 1000 {
        return (bytes.to_string(), UNITS[0]);
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    // promote while the value, rounded to one decimal, would reach 1000
    while unit < UNITS.len() - 1 && (value * 10.0).round() >= 10_000.0 {
        value /= 1000.0;
        unit += 1;
    }
    let mut number = format!("{value:.1}");
    if trim && number.ends_with(".0") {
        number.truncate(number.len() - 2);
    }
    (number, UNITS[unit])
}

/// `12.4 Go`
pub fn bytes(bytes: u64) -> String {
    let (number, unit) = split_bytes(bytes, false);
    format!("{number} {unit}")
}

/// `1.2 Mo/s`. Negative or non finite rates show as `0 o/s`.
pub fn rate(bytes_per_sec: f64) -> String {
    let bytes_per_sec = if bytes_per_sec.is_finite() && bytes_per_sec > 0.0 {
        bytes_per_sec.round() as u64
    } else {
        0
    };
    format!("{}/s", bytes(bytes_per_sec))
}

/// `12.4/50 Go` when both share a unit, else `532.1 Mo/50 Go`; `12.4 Go` without cap.
pub fn usage(total: u64, cap: Option<u64>) -> String {
    let Some(cap) = cap.filter(|&c| c > 0) else {
        return bytes(total);
    };
    let (used, used_unit) = split_bytes(total, false);
    let (cap, cap_unit) = split_bytes(cap, true);
    if used_unit == cap_unit {
        format!("{used}/{cap} {cap_unit}")
    } else {
        format!("{used} {used_unit}/{cap} {cap_unit}")
    }
}

/// `62 %`, or `> 100 %` once the cap is exceeded. `None` without a cap.
pub fn percent(total: u64, cap: Option<u64>) -> Option<String> {
    let cap = cap.filter(|&c| c > 0)?;
    if total > cap {
        return Some("> 100 %".into());
    }
    let pct = u128::from(total) * 100 / u128::from(cap);
    Some(format!("{pct} %"))
}

/// Popup rate line: `↓1.2 Mo/s ↑40.0 Ko/s`, `—` while the interface is missing.
pub fn live_rate(rate: Option<(f64, f64)>) -> String {
    match rate {
        Some((rx, tx)) => format!("↓{} ↑{}", self::rate(rx), self::rate(tx)),
        None => "—".into(),
    }
}

/// Value shown in the cap field: gigabytes without trailing zeros, empty for no cap.
pub fn cap_input(cap: u64) -> String {
    if cap == 0 {
        return String::new();
    }
    let (whole, frac) = (cap / 1_000_000_000, cap % 1_000_000_000);
    let frac = format!("{frac:09}");
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{frac}")
    }
}

/// `1er novembre 2026`
pub fn reset_date(period: Period) -> String {
    const MONTHS: [&str; 12] = [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ];
    format!("1er {} {}", MONTHS[period.month as usize - 1], period.year)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_under_one_kilo_have_no_decimal() {
        assert_eq!(bytes(0), "0 o");
        assert_eq!(bytes(999), "999 o");
    }

    #[test]
    fn decimal_units() {
        assert_eq!(bytes(1_000), "1.0 Ko");
        assert_eq!(bytes(1_234_567), "1.2 Mo");
        assert_eq!(bytes(12_400_000_000), "12.4 Go");
        assert_eq!(bytes(3_000_000_000_000), "3.0 To");
    }

    #[test]
    fn rounding_promotes_to_next_unit() {
        assert_eq!(bytes(999_960), "1.0 Mo");
        assert_eq!(bytes(999_949), "999.9 Ko");
    }

    #[test]
    fn huge_values_stay_in_terabytes() {
        assert_eq!(bytes(u64::MAX), "18446744.1 To");
    }

    #[test]
    fn trim_drops_dot_zero() {
        assert_eq!(split_bytes(50_000_000_000, true), ("50".into(), "Go"));
        assert_eq!(split_bytes(2_500_000_000, true), ("2.5".into(), "Go"));
        assert_eq!(split_bytes(50_000_000_000, false), ("50.0".into(), "Go"));
    }

    #[test]
    fn rates() {
        assert_eq!(rate(0.0), "0 o/s");
        assert_eq!(rate(1_200_000.0), "1.2 Mo/s");
        assert_eq!(rate(40_400.4), "40.4 Ko/s");
        assert_eq!(rate(-3.0), "0 o/s");
        assert_eq!(rate(f64::NAN), "0 o/s");
        assert_eq!(rate(f64::INFINITY), "0 o/s");
    }

    #[test]
    fn usage_with_and_without_cap() {
        assert_eq!(usage(12_400_000_000, None), "12.4 Go");
        assert_eq!(usage(12_400_000_000, Some(0)), "12.4 Go");
        assert_eq!(usage(12_400_000_000, Some(50_000_000_000)), "12.4/50 Go");
        assert_eq!(usage(532_100_000, Some(50_000_000_000)), "532.1 Mo/50 Go");
    }

    #[test]
    fn percents() {
        assert_eq!(percent(1, None), None);
        assert_eq!(percent(1, Some(0)), None);
        assert_eq!(percent(0, Some(100)), Some("0 %".into()));
        assert_eq!(percent(624, Some(1000)), Some("62 %".into()));
        assert_eq!(percent(100, Some(100)), Some("100 %".into()));
        assert_eq!(percent(101, Some(100)), Some("> 100 %".into()));
        assert_eq!(percent(u64::MAX, Some(1)), Some("> 100 %".into()));
    }

    #[test]
    fn cap_input_values() {
        assert_eq!(cap_input(0), "");
        assert_eq!(cap_input(50_000_000_000), "50");
        assert_eq!(cap_input(2_500_000_000), "2.5");
        assert_eq!(cap_input(1_000_000), "0.001");
        assert_eq!(cap_input(1), "0.000000001");
    }

    #[test]
    fn cap_input_roundtrips_through_parse() {
        use crate::usage::parse_cap_gb;
        for cap in [
            1,
            1_000_000,
            2_500_000_000,
            50_000_000_000,
            123_456_789_012,
            1_000_000_000_000_000,
        ] {
            assert_eq!(parse_cap_gb(&cap_input(cap)), Ok(Some(cap)), "{cap}");
        }
    }

    #[test]
    fn reset_dates_in_french() {
        assert_eq!(
            reset_date(Period::new(2026, 11).unwrap()),
            "1er novembre 2026"
        );
        assert_eq!(
            reset_date(Period::new(2027, 1).unwrap()),
            "1er janvier 2027"
        );
        assert_eq!(reset_date(Period::new(2027, 8).unwrap()), "1er août 2027");
    }

    #[test]
    fn live_rates() {
        assert_eq!(
            live_rate(Some((1_200_000.0, 40_000.0))),
            "↓1.2 Mo/s ↑40.0 Ko/s"
        );
        assert_eq!(live_rate(Some((0.0, 0.0))), "↓0 o/s ↑0 o/s");
        assert_eq!(live_rate(None), "—");
    }
}
