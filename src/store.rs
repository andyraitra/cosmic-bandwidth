// SPDX-License-Identifier: GPL-3.0-only

//! Persistence of the monthly usage in `$XDG_STATE_HOME/cosmic-bandwidth/usage.json`.

use std::ffi::OsString;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::usage::{Period, Usage};

/// `$XDG_STATE_HOME/cosmic-bandwidth/usage.json`, falling back to
/// `$HOME/.local/state`. Relative XDG paths are ignored, as the spec requires.
pub fn state_path(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let absolute = |dir: Option<OsString>| dir.map(PathBuf::from).filter(|p| p.is_absolute());
    let base =
        absolute(xdg_state_home).or_else(|| absolute(home).map(|h| h.join(".local/state")))?;
    Some(base.join("cosmic-bandwidth").join("usage.json"))
}

/// Loads saved usage. A missing file gives `Ok(None)`; an unreadable or invalid one
/// is moved aside to `usage.json.corrupt-<unix time>` and also gives `Ok(None)`.
/// `Err` means a file exists that could not be loaded (possibly a transient error)
/// nor moved aside: the caller must not save over it.
pub fn load(path: &Path) -> io::Result<Option<Usage>> {
    let problem = match std::fs::read(path) {
        Ok(bytes) => match parse(&bytes) {
            Some(usage) => return Ok(Some(usage)),
            None => "invalid".to_string(),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        // the file itself is the problem: move it aside, it is kept
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::IsADirectory
            ) =>
        {
            err.to_string()
        }
        // possibly transient (EIO, EMFILE...): leave the file alone, do not save over it
        Err(err) => return Err(err),
    };
    let aside = move_aside(path)?;
    tracing::warn!(?path, ?aside, %problem, "saved usage unusable, moved aside");
    Ok(None)
}

/// Reads saved usage without any side effect, for instances that do not own it.
pub fn peek(path: &Path) -> Option<Usage> {
    parse(&std::fs::read(path).ok()?)
}

/// Takes the exclusive lock `usage.json.lock` that designates the one instance
/// counting and saving. `Ok(None)` when another instance holds it.
pub fn try_lock(path: &Path) -> io::Result<Option<File>> {
    let dir = parent(path)?;
    std::fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sibling(path, ".lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}

/// Whether `lock` still is the lock file at its path: if the file was deleted
/// or replaced, another instance may hold the new one and ownership is lost.
pub fn lock_is_current(lock: &File, path: &Path) -> bool {
    match (lock.metadata(), std::fs::metadata(sibling(path, ".lock"))) {
        (Ok(held), Ok(on_disk)) => (held.dev(), held.ino()) == (on_disk.dev(), on_disk.ino()),
        _ => false,
    }
}

/// Writes atomically: unique temp file in the same directory, fsync, rename,
/// then fsync of the directory so the rename itself survives a power loss.
pub fn save(path: &Path, usage: &Usage) -> io::Result<()> {
    let dir = parent(path)?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut tmp, usage)?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| err.error)?;
    File::open(dir)?.sync_all()
}

fn parse(bytes: &[u8]) -> Option<Usage> {
    serde_json::from_slice::<Usage>(bytes)
        .ok()
        .filter(|u| Period::new(u.period.year, u.period.month).is_some())
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::other("usage path has no parent"))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Renames to a fresh `.corrupt-<unix time>[-n]` name, never replacing an earlier copy.
fn move_aside(path: &Path) -> io::Result<PathBuf> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    for n in 0..1000 {
        let suffix = match n {
            0 => format!(".corrupt-{secs}"),
            n => format!(".corrupt-{secs}-{n}"),
        };
        let aside = sibling(path, &suffix);
        if aside.symlink_metadata().is_err() {
            std::fs::rename(path, &aside)?;
            return Ok(aside);
        }
    }
    Err(io::Error::other("too many corrupt usage files"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Usage {
        Usage {
            rx: 12,
            tx: 34,
            alerted_warn: true,
            ..Usage::empty(Period::new(2026, 10).unwrap())
        }
    }

    #[test]
    fn state_path_prefers_absolute_xdg() {
        assert_eq!(
            state_path(Some("/x/state".into()), Some("/home/u".into())),
            Some(PathBuf::from("/x/state/cosmic-bandwidth/usage.json"))
        );
    }

    #[test]
    fn state_path_falls_back_to_home() {
        let want = Some(PathBuf::from(
            "/home/u/.local/state/cosmic-bandwidth/usage.json",
        ));
        assert_eq!(state_path(None, Some("/home/u".into())), want);
        assert_eq!(state_path(Some("".into()), Some("/home/u".into())), want);
        assert_eq!(
            state_path(Some("rel/dir".into()), Some("/home/u".into())),
            want
        );
    }

    #[test]
    fn state_path_without_home() {
        assert_eq!(state_path(None, None), None);
        assert_eq!(state_path(None, Some("".into())), None);
        assert_eq!(state_path(None, Some("relative".into())), None);
    }

    #[test]
    fn roundtrip_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/usage.json");
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(sample()));
    }

    #[test]
    fn save_overwrites_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        save(&path, &Usage::empty(Period::new(2026, 9).unwrap())).unwrap();
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(sample()));
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![OsString::from("usage.json")]);
    }

    #[test]
    fn missing_file_is_none_without_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        assert_eq!(load(&path).unwrap(), None);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn corrupt_files_are_moved_aside() {
        for content in [
            &b"{not json"[..],
            b"",
            b"\xff\xfe",
            br#"{"period":{"year":2026,"month":13},"rx":1,"tx":1}"#,
            br#"{"period":{"year":2147483647,"month":12},"rx":1,"tx":1}"#,
            br#"{"period":{"year":1970,"month":1},"rx":1,"tx":1}"#,
            br#"{"period":{"year":2026,"month":10},"rx":-1,"tx":1}"#,
            br#"{"period":{"year":2026,"month":10}}"#,
            b"[]",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("usage.json");
            std::fs::write(&path, content).unwrap();
            assert_eq!(
                load(&path).unwrap(),
                None,
                "{:?}",
                String::from_utf8_lossy(content)
            );
            assert!(!path.exists());
            let aside = corrupt_files(dir.path());
            assert_eq!(aside.len(), 1);
            assert_eq!(std::fs::read(&aside[0]).unwrap(), content);
        }
    }

    #[test]
    fn legacy_file_without_alert_flags_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(
            &path,
            br#"{"period":{"year":2026,"month":10},"rx":5,"tx":6}"#,
        )
        .unwrap();
        let u = load(&path).unwrap().unwrap();
        assert_eq!(
            (u.rx, u.tx, u.alerted_warn, u.alerted_full),
            (5, 6, false, false)
        );
    }

    #[test]
    fn save_error_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(save(&blocker.join("usage.json"), &sample()).is_err());
    }

    fn corrupt_files(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("usage.json.corrupt")
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn second_corruption_keeps_the_first_copy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::write(&path, b"first").unwrap();
        assert_eq!(load(&path).unwrap(), None);
        std::fs::write(&path, b"second").unwrap();
        assert_eq!(load(&path).unwrap(), None);
        let mut contents: Vec<_> = corrupt_files(dir.path())
            .iter()
            .map(|p| std::fs::read(p).unwrap())
            .collect();
        contents.sort();
        assert_eq!(contents, vec![b"first".to_vec(), b"second".to_vec()]);
    }

    #[test]
    fn unreadable_file_is_kept_aside_not_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        save(&path, &sample()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(std::fs::read(&path).is_err(), "tests must not run as root");
        assert_eq!(load(&path).unwrap(), None);
        save(&path, &Usage::empty(Period::new(2026, 10).unwrap())).unwrap();
        let aside = corrupt_files(dir.path());
        assert_eq!(aside.len(), 1);
        std::fs::set_permissions(&aside[0], std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Usage>(&std::fs::read(&aside[0]).unwrap()).unwrap(),
            sample()
        );
    }

    #[test]
    fn directory_in_place_of_state_file_does_not_block_saving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::fs::create_dir(&path).unwrap();
        assert_eq!(load(&path).unwrap(), None);
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(sample()));
    }

    #[test]
    fn unloadable_file_that_cannot_be_moved_aside_is_an_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let path = state.join("usage.json");
        std::fs::write(&path, b"{broken").unwrap();
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = load(&path);
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    }

    #[test]
    fn peek_has_no_side_effect() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        assert_eq!(peek(&path), None);
        std::fs::write(&path, b"{broken").unwrap();
        assert_eq!(peek(&path), None);
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
        save(&path, &sample()).unwrap();
        assert_eq!(peek(&path), Some(sample()));
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/usage.json");
        let first = try_lock(&path).unwrap();
        assert!(first.is_some());
        assert!(try_lock(&path).unwrap().is_none());
        drop(first);
        assert!(try_lock(&path).unwrap().is_some());
    }

    #[test]
    fn concurrent_saves_never_publish_a_torn_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::sync::Arc::new(dir.path().join("usage.json"));
        let big = Usage {
            rx: 123_456_789_012,
            tx: 1,
            alerted_warn: true,
            alerted_full: true,
            ..Usage::empty(Period::new(2026, 10).unwrap())
        };
        let small = Usage::empty(Period::new(2026, 10).unwrap());
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                let u = if i % 2 == 0 {
                    big.clone()
                } else {
                    small.clone()
                };
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        save(&path, &u).unwrap();
                        let bytes = std::fs::read(&*path).unwrap();
                        assert!(
                            serde_json::from_slice::<Usage>(&bytes).is_ok(),
                            "torn file published: {:?}",
                            String::from_utf8_lossy(&bytes)
                        );
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, vec![OsString::from("usage.json")]);
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn move_aside_picks_a_numbered_name_when_the_plain_one_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let start = now_secs();
        for s in start..start + 5 {
            std::fs::write(sibling(&path, &format!(".corrupt-{s}")), format!("old {s}")).unwrap();
        }
        std::fs::write(&path, b"{broken").unwrap();
        assert_eq!(load(&path).unwrap(), None);
        for s in start..start + 5 {
            assert_eq!(
                std::fs::read_to_string(sibling(&path, &format!(".corrupt-{s}"))).unwrap(),
                format!("old {s}")
            );
        }
        let new: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with("-1"))
            .collect();
        assert_eq!(new.len(), 1, "{new:?}");
        assert_eq!(std::fs::read(dir.path().join(&new[0])).unwrap(), b"{broken");
    }

    #[test]
    fn dangling_symlink_with_the_aside_name_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let start = now_secs();
        for s in start..start + 5 {
            std::os::unix::fs::symlink("/nonexistent", sibling(&path, &format!(".corrupt-{s}")))
                .unwrap();
        }
        std::fs::write(&path, b"{broken").unwrap();
        assert_eq!(load(&path).unwrap(), None);
        for s in start..start + 5 {
            let link = sibling(&path, &format!(".corrupt-{s}"));
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        }
    }

    #[test]
    fn exhausted_aside_names_is_an_error_and_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let start = now_secs();
        for s in start..start + 3 {
            for n in 0..1000 {
                let suffix = if n == 0 {
                    format!(".corrupt-{s}")
                } else {
                    format!(".corrupt-{s}-{n}")
                };
                std::fs::write(sibling(&path, &suffix), b"").unwrap();
            }
        }
        std::fs::write(&path, b"{broken").unwrap();
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
    }

    #[test]
    fn try_lock_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        std::fs::write(&blocker, b"x").unwrap();
        assert!(try_lock(&blocker.join("usage.json")).is_err());
    }

    #[test]
    fn lock_file_does_not_disturb_load_or_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let _lock = try_lock(&path).unwrap().unwrap();
        assert_eq!(load(&path).unwrap(), None);
        let u = Usage::empty(Period::new(2026, 10).unwrap());
        save(&path, &u).unwrap();
        assert_eq!(peek(&path), Some(u));
        assert!(sibling(&path, ".lock").exists());
    }

    #[test]
    fn lock_is_current_until_its_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let lock = try_lock(&path).unwrap().unwrap();
        assert!(lock_is_current(&lock, &path));
        std::fs::remove_file(sibling(&path, ".lock")).unwrap();
        assert!(!lock_is_current(&lock, &path));
        let other = try_lock(&path).unwrap().unwrap();
        assert!(!lock_is_current(&lock, &path));
        assert!(lock_is_current(&other, &path));
    }

    #[test]
    fn transient_read_error_is_an_error_and_moves_nothing() {
        // ELOOP is neither NotFound, PermissionDenied nor IsADirectory
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        std::os::unix::fs::symlink(&path, &path).unwrap();
        let err = load(&path).unwrap_err();
        assert_ne!(err.kind(), io::ErrorKind::NotFound);
        assert!(path.symlink_metadata().unwrap().file_type().is_symlink());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![OsString::from("usage.json")]);
    }

    #[test]
    fn lock_is_not_current_once_replaced_by_rename_or_dir_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.json");
        let lock = try_lock(&path).unwrap().unwrap();
        let other = dir.path().join("other");
        std::fs::write(&other, b"").unwrap();
        std::fs::rename(&other, sibling(&path, ".lock")).unwrap();
        assert!(!lock_is_current(&lock, &path));
        assert!(
            try_lock(&path).unwrap().is_some(),
            "the new lock file is free"
        );

        let path = dir.path().join("sub/usage.json");
        let lock = try_lock(&path).unwrap().unwrap();
        std::fs::remove_dir_all(dir.path().join("sub")).unwrap();
        assert!(!lock_is_current(&lock, &path));
    }
}
