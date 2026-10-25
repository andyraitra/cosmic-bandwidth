// SPDX-License-Identifier: GPL-3.0-only

//! Settings persisted with cosmic-config in `~/.config/cosmic/<APP_ID>/v1/`.

use cosmic::cosmic_config::{
    self, ConfigGet, ConfigSet, CosmicConfigEntry, cosmic_config_derive::CosmicConfigEntry,
};
use serde::{Deserialize, Serialize};

pub const DEFAULT_INTERFACE: &str = "wlp2s0f0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, CosmicConfigEntry)]
#[version = 1]
pub struct BandwidthConfig {
    /// Interface whose traffic is counted.
    pub interface: String,
    /// Monthly cap in bytes, 0 for none.
    pub cap_bytes: u64,
    /// Bumped by any instance to ask the counting instance to reset the month.
    pub reset_requests: u64,
}

impl Default for BandwidthConfig {
    fn default() -> Self {
        Self {
            interface: DEFAULT_INTERFACE.into(),
            cap_bytes: 0,
            reset_requests: 0,
        }
    }
}

impl BandwidthConfig {
    pub fn cap(&self) -> Option<u64> {
        (self.cap_bytes > 0).then_some(self.cap_bytes)
    }
}

/// Writes only the cap, so other keys changed by other instances are kept.
pub fn write_cap(handler: &cosmic_config::Config, cap: u64) -> Result<(), cosmic_config::Error> {
    handler.set("cap_bytes", cap)
}

/// Requests a month reset from every instance: writes one more than the highest
/// of the value on disk and `seen`, and returns it.
pub fn request_reset(
    handler: &cosmic_config::Config,
    seen: u64,
) -> Result<u64, cosmic_config::Error> {
    let on_disk = handler.get::<u64>("reset_requests").unwrap_or(0);
    let next = on_disk.max(seen).saturating_add(1);
    handler.set("reset_requests", next)?;
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_cap() {
        let c = BandwidthConfig::default();
        assert_eq!(c.interface, "wlp2s0f0");
        assert_eq!(c.reset_requests, 0);
        assert_eq!(c.cap(), None);
        assert_eq!(BandwidthConfig { cap_bytes: 1, ..c }.cap(), Some(1));
    }

    fn handler(dir: &std::path::Path) -> cosmic_config::Config {
        cosmic_config::Config::with_custom_path("io.test.Bw", BandwidthConfig::VERSION, dir.into())
            .unwrap()
    }

    #[test]
    fn stale_instance_requesting_reset_does_not_revert_cap() {
        let dir = tempfile::tempdir().unwrap();
        let h = handler(dir.path());
        write_cap(&h, 5).unwrap(); // instance A
        // instance B never saw A's cap nor any reset
        assert_eq!(request_reset(&h, 0).unwrap(), 1);
        let c = BandwidthConfig::get_entry(&h).unwrap();
        assert_eq!((c.cap_bytes, c.reset_requests), (5, 1));
    }

    #[test]
    fn request_reset_bumps_above_disk_and_seen() {
        let dir = tempfile::tempdir().unwrap();
        let h = handler(dir.path());
        assert_eq!(request_reset(&h, 0).unwrap(), 1);
        assert_eq!(request_reset(&h, 0).unwrap(), 2, "stale seen: disk wins");
        assert_eq!(request_reset(&h, 10).unwrap(), 11, "disk behind: seen wins");
        assert_eq!(BandwidthConfig::get_entry(&h).unwrap().reset_requests, 11);
    }

    #[test]
    fn write_cap_writes_even_if_equal_to_a_stale_local_copy() {
        let dir = tempfile::tempdir().unwrap();
        let h = handler(dir.path());
        write_cap(&h, 7).unwrap();
        write_cap(&h, 0).unwrap();
        assert_eq!(BandwidthConfig::get_entry(&h).unwrap().cap_bytes, 0);
    }

    #[test]
    fn legacy_config_without_reset_requests_keeps_other_fields() {
        let dir = tempfile::tempdir().unwrap();
        let h = handler(dir.path());
        let v1 = dir.path().join("cosmic/io.test.Bw/v1");
        std::fs::write(v1.join("interface"), "\"eth0\"").unwrap();
        std::fs::write(v1.join("cap_bytes"), "5").unwrap();
        let config = match BandwidthConfig::get_entry(&h) {
            Ok(c) => c,
            Err((errs, c)) => {
                assert!(errs.iter().all(|e| !e.is_err()), "{errs:?}");
                c
            }
        };
        assert_eq!(
            config,
            BandwidthConfig {
                interface: "eth0".into(),
                cap_bytes: 5,
                reset_requests: 0
            }
        );
    }

    #[test]
    fn write_cap_keeps_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let h = handler(dir.path());
        h.set("interface", "eth0".to_string()).unwrap();
        assert_eq!(request_reset(&h, 0).unwrap(), 1);
        write_cap(&h, 9).unwrap();
        let c = BandwidthConfig::get_entry(&h).unwrap();
        assert_eq!(
            c,
            BandwidthConfig {
                interface: "eth0".into(),
                cap_bytes: 9,
                reset_requests: 1
            }
        );
    }

    #[test]
    fn request_reset_with_garbage_on_disk_uses_seen() {
        let dir = tempfile::tempdir().unwrap();
        let h = handler(dir.path());
        request_reset(&h, 0).unwrap();
        let v1 = dir.path().join("cosmic/io.test.Bw/v1");
        std::fs::write(v1.join("reset_requests"), "garbage").unwrap();
        assert_eq!(request_reset(&h, 4).unwrap(), 5);
        assert_eq!(h.get::<u64>("reset_requests").unwrap(), 5);
    }
}
