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
//! refuses while the marker exists. Commands that read or write the keyboard
//! state hold a lock in the same directory, so only one runs at a time.

use crate::backup::default_backup_dir;
use anyhow::{Context, Result, bail};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const NAME: &str = "he-pending";
const LOCK: &str = "he.lock";

/// The marker of an unfinished write and the lock, in a backup directory.
pub struct Pending {
    dir: PathBuf,
}

/// Held while a command works with the keyboard state; released on drop.
pub struct Lock {
    _file: File,
}

impl Pending {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn in_default_dir() -> Result<Self> {
        Ok(Self::new(default_backup_dir()?))
    }

    /// The directory backups are written to.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self) -> PathBuf {
        self.dir.join(NAME)
    }

    /// Take the lock, or refuse while another command holds it.
    pub fn lock(&self) -> Result<Lock> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create {}", self.dir.display()))?;
        let path = self.dir.join(LOCK);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        if file.try_lock().is_err() {
            bail!(
                "another nuphyctl he command is working with the keyboard; try again when it ends"
            );
        }
        Ok(Lock { _file: file })
    }

    /// The backup of an unfinished write, if there is one.
    pub fn unfinished(&self) -> Result<Option<PathBuf>> {
        let path = self.path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("failed to read {}", path.display())),
        };
        let backup = text.trim();
        if backup.is_empty() {
            bail!(
                "{} exists but names no backup: a previous write did not finish; restore the backup that write printed first, then delete the file",
                path.display()
            );
        }
        Ok(Some(PathBuf::from(backup)))
    }

    /// Refuse to go on while a write is unfinished.
    pub fn ensure_none(&self) -> Result<()> {
        if let Some(backup) = self.unfinished()? {
            bail!(refusal(&backup));
        }
        Ok(())
    }

    /// Mark a write unfinished, naming `backup`. The marker appears whole or
    /// not at all, and only if no other one exists.
    pub fn begin(&self, backup: &Path) -> Result<()> {
        let path = self.path();
        let staged = self.dir.join(format!("{NAME}.{}.tmp", std::process::id()));
        let mut file = File::create(&staged)
            .with_context(|| format!("failed to create {}", staged.display()))?;
        file.write_all(backup.display().to_string().as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("failed to write {}", staged.display()))?;
        let linked = std::fs::hard_link(&staged, &path);
        let _ = std::fs::remove_file(&staged);
        match linked {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!("{} appeared while starting a write", path.display())
            }
            Err(e) => Err(e).with_context(|| format!("failed to create {}", path.display())),
        }
    }

    pub fn end(&self) -> Result<()> {
        let path = self.path();
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("failed to remove {}", path.display()))
            }
            _ => Ok(()),
        }
    }
}

/// What to run to undo an unfinished write.
pub fn restore_hint(backup: &Path) -> String {
    format!(
        "restore with: nuphyctl he restore-backup {}",
        backup.display()
    )
}

pub fn refusal(backup: &Path) -> String {
    format!(
        "a previous write to the keyboard did not finish, so it may hold half-written data; {}",
        restore_hint(backup)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> Pending {
        let dir = std::env::temp_dir().join(format!("nuphyctl-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Pending::new(dir)
    }

    #[test]
    fn marks_and_clears_an_unfinished_write() {
        let pending = scratch("pending");
        assert_eq!(pending.unfinished().unwrap(), None);
        let first = Path::new("/state/he-20261101T000000Z.bin");
        pending.begin(first).unwrap();
        assert_eq!(pending.unfinished().unwrap().as_deref(), Some(first));
        assert!(pending.ensure_none().is_err());
        assert!(
            pending.begin(Path::new("/state/later.bin")).is_err(),
            "never two markers"
        );
        assert_eq!(pending.unfinished().unwrap().as_deref(), Some(first));
        pending.end().unwrap();
        assert_eq!(pending.unfinished().unwrap(), None);
        pending.end().unwrap();
        std::fs::remove_dir_all(pending.dir()).unwrap();
    }

    #[test]
    fn an_empty_marker_is_an_error_not_a_path() {
        let pending = scratch("empty-marker");
        std::fs::write(pending.path(), "").unwrap();
        let err = pending.unfinished().unwrap_err().to_string();
        assert!(err.contains("names no backup"), "{err}");
        std::fs::remove_dir_all(pending.dir()).unwrap();
    }

    #[test]
    fn one_command_at_a_time() {
        let pending = scratch("lock");
        let held = pending.lock().unwrap();
        assert!(pending.lock().is_err());
        drop(held);
        assert!(pending.lock().is_ok());
        std::fs::remove_dir_all(pending.dir()).unwrap();
    }
}
