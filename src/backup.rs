//! Timestamped backup files under `$XDG_STATE_HOME/nuphyctl`, shared by the
//! Air75 V3 keymap commands and the Hall-effect commands.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `$XDG_STATE_HOME/nuphyctl`, falling back to `~/.local/state/nuphyctl`.
pub fn default_backup_dir() -> Result<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(state).join("nuphyctl"));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .context("neither XDG_STATE_HOME nor HOME is set; cannot choose a backup directory")?;
    Ok(PathBuf::from(home).join(".local/state/nuphyctl"))
}

/// Write a backup as `<prefix>-YYYYMMDDTHHMMSSZ.bin` and return its path.
pub fn write_backup(dir: &Path, prefix: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create backup directory {}", dir.display()))?;
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamp = utc_timestamp(secs);
    for n in 0..1000 {
        let name = if n == 0 {
            format!("{prefix}-{stamp}.bin")
        } else {
            format!("{prefix}-{stamp}-{n}.bin")
        };
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(bytes)
                    .with_context(|| format!("failed to write backup {}", path.display()))?;
                f.sync_all()
                    .with_context(|| format!("failed to sync backup {}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to create backup {}", path.display()));
            }
        }
    }
    bail!(
        "could not find a free backup file name in {}",
        dir.display()
    )
}

/// `YYYYMMDDTHHMMSSZ` for a Unix timestamp (UTC).
pub fn utc_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(utc_timestamp(0), "19700101T000000Z");
        assert_eq!(utc_timestamp(951_782_400), "20000229T000000Z");
        assert_eq!(utc_timestamp(1_791_990_000), "20261014T150000Z");
    }

    #[test]
    fn backups_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("nuphyctl-test-{}", std::process::id()));
        let a = write_backup(&dir, "keymap", &[1, 2, 3]).unwrap();
        let b = write_backup(&dir, "keymap", &[4, 5, 6]).unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), vec![1, 2, 3]);
        assert_eq!(std::fs::read(&b).unwrap(), vec![4, 5, 6]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
