//! A marker that a write to the keyboard started and has not been verified.
//!
//! A write that stops half way can leave macros that still decode but are
//! wrong; re-running `apply` on them would make the damage permanent. So the
//! marker, naming the backup taken before the write, is created before the
//! first packet and removed only after verification passes or a backup is
//! restored. While it exists, `apply` and `export` refuse.
//!
//! A `restore-backup` that starts while the marker exists leaves it alone:
//! the backup it names was taken before the damage, the restore's own backup
//! after it, and is marked as such so it is never restored. Any other write
//! refuses while the marker exists.

use crate::backup::default_backup_dir;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

const NAME: &str = "he-pending";

/// The backup of an unfinished write, if there is one.
pub fn unfinished() -> Result<Option<PathBuf>> {
    unfinished_in(&default_backup_dir()?)
}

/// What to run to undo an unfinished write.
pub fn restore_hint(backup: &Path) -> String {
    format!(
        "restore with: nuphyctl he restore-backup {}",
        backup.display()
    )
}

fn refusal(backup: &Path) -> String {
    format!(
        "a previous write to the keyboard did not finish, so it may hold half-written data; {}",
        restore_hint(backup)
    )
}

/// Refuse to go on while a write is unfinished.
pub fn ensure_none() -> Result<()> {
    ensure_none_in(&default_backup_dir()?)
}

pub fn ensure_none_in(dir: &Path) -> Result<()> {
    if let Some(backup) = unfinished_in(dir)? {
        bail!(refusal(&backup));
    }
    Ok(())
}

/// Mark a write in `dir` unfinished and return the backup the marker names,
/// the one to restore if this write fails. A restore joins an earlier
/// unfinished write and keeps its backup; any other write refuses to.
pub fn begin_in(dir: &Path, backup: &Path, restoring: bool) -> Result<PathBuf> {
    let path = dir.join(NAME);
    std::fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(backup.display().to_string().as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            Ok(backup.to_path_buf())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let earlier = unfinished_in(dir)?
                .with_context(|| format!("{} vanished while starting a write", path.display()))?;
            if !restoring {
                bail!(refusal(&earlier));
            }
            Ok(earlier)
        }
        Err(e) => Err(e).with_context(|| format!("failed to create {}", path.display())),
    }
}

pub fn end() -> Result<()> {
    end_in(&default_backup_dir()?)
}

pub fn unfinished_in(dir: &Path) -> Result<Option<PathBuf>> {
    let path = dir.join(NAME);
    match std::fs::read_to_string(&path) {
        Ok(backup) => Ok(Some(PathBuf::from(backup.trim()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

pub fn end_in(dir: &Path) -> Result<()> {
    let path = dir.join(NAME);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("failed to remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_clears_an_unfinished_write() {
        let dir = std::env::temp_dir().join(format!("nuphyctl-pending-{}", std::process::id()));
        assert_eq!(unfinished_in(&dir).unwrap(), None);
        let first = Path::new("/state/he-20261101T000000Z.bin");
        let second = Path::new("/state/he-20261101T000100Z.bin");
        assert_eq!(begin_in(&dir, first, false).unwrap(), first);
        assert_eq!(unfinished_in(&dir).unwrap().as_deref(), Some(first));
        assert!(
            begin_in(&dir, second, false).is_err(),
            "only a restore joins"
        );
        assert_eq!(
            begin_in(&dir, second, true).unwrap(),
            first,
            "the earliest backup stays"
        );
        assert_eq!(unfinished_in(&dir).unwrap().as_deref(), Some(first));
        end_in(&dir).unwrap();
        assert_eq!(unfinished_in(&dir).unwrap(), None);
        end_in(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
