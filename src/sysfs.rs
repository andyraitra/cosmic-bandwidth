// SPDX-License-Identifier: GPL-3.0-only

//! Reads interface byte counters from `/sys/class/net/<iface>/statistics`.

use std::path::Path;

use crate::usage::Counters;

pub const SYS_CLASS_NET: &str = "/sys/class/net";

/// Linux interface names: 1 to 15 bytes, no `/`, no whitespace, not `.` or `..`.
pub fn valid_interface(name: &str) -> bool {
    (1..=15).contains(&name.len())
        && name != "."
        && name != ".."
        && !name
            .bytes()
            .any(|b| b == b'/' || b.is_ascii_whitespace() || b.is_ascii_control())
}

/// `None` when the name is invalid, the interface is absent, or a counter is unreadable.
pub fn read_counters(root: &Path, iface: &str) -> Option<Counters> {
    if !valid_interface(iface) {
        return None;
    }
    let stats = root.join(iface).join("statistics");
    let read = |name: &str| -> Option<u64> {
        std::fs::read_to_string(stats.join(name))
            .ok()?
            .trim_end()
            .parse()
            .ok()
    };
    Some(Counters {
        rx: read("rx_bytes")?,
        tx: read("tx_bytes")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(root: &Path, iface: &str, rx: &str, tx: &str) {
        let dir = root.join(iface).join("statistics");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rx_bytes"), rx).unwrap();
        std::fs::write(dir.join("tx_bytes"), tx).unwrap();
    }

    #[test]
    fn validates_names() {
        for ok in ["wlp2s0f0", "eth0", "a", "enx0123456789ab"] {
            assert!(valid_interface(ok), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "../etc",
            "a/b",
            "wl p",
            "wlan0\n",
            "0123456789abcdef",
        ] {
            assert!(!valid_interface(bad), "{bad:?}");
        }
    }

    #[test]
    fn reads_counters_with_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        fake(dir.path(), "wlp2s0f0", "123\n", "456\n");
        assert_eq!(
            read_counters(dir.path(), "wlp2s0f0"),
            Some(Counters { rx: 123, tx: 456 })
        );
    }

    #[test]
    fn reads_max_u64() {
        let dir = tempfile::tempdir().unwrap();
        fake(dir.path(), "w", "18446744073709551615", "0");
        assert_eq!(
            read_counters(dir.path(), "w"),
            Some(Counters {
                rx: u64::MAX,
                tx: 0
            })
        );
    }

    #[test]
    fn missing_interface() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_counters(dir.path(), "wlp2s0f0"), None);
    }

    #[test]
    fn garbage_counters() {
        for (rx, tx) in [
            ("", "1"),
            ("abc", "1"),
            ("1", "-2"),
            ("1", "18446744073709551616"),
            ("1 2", "1"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            fake(dir.path(), "w", rx, tx);
            assert_eq!(read_counters(dir.path(), "w"), None, "{rx:?} {tx:?}");
        }
    }

    #[test]
    fn invalid_name_never_touches_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        // a crafted statistics dir one level up must not be reachable
        fake(dir.path(), "statistics_parent", "1", "1");
        let root = dir.path().join("net");
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(read_counters(&root, "../statistics_parent"), None);
    }
}
