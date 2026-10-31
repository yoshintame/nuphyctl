//! `nuphyctl he ...` command handlers.

use crate::backup::{default_backup_dir, write_backup};
use crate::cli::{
    DeviceSelector, HeApplyArgs, HeCheckArgs, HeDumpArgs, HeExportArgs, HeInfoArgs, HeReadArgs,
    HeRestoreArgs, HeWriteArgs,
};
use crate::he::actions::Action;
use crate::he::config::{check_config, export_config, load_config};
use crate::he::layout::{KeyRef, TravelRef, key_count, layer_name, mode_label, position_name};
use crate::he::pending;
use crate::he::plan::{Plan, plan_changes, plan_restore};
use crate::he::protocol::{Base, FirmwareInfo, HeProtocol};
use crate::he::region::{LAYERS_PER_MODE, MODES, Region, WriteChunk};
use crate::he::snapshot::{self, Header, Identity};
use crate::he::state::{MacroBank, Matrix, State, TravelTable};
use crate::he::travel::{FIELDS, mm};
use crate::hid_transport::connect_with_retry;
use crate::output::print_json;
use anyhow::{Context, Result, bail};
use hidapi::HidApi;
use serde_json::json;
use std::path::{Path, PathBuf};

const NUPHY_VID: u16 = 0x19f5;
/// The board whose slot layout `layout::AIR60_HE` describes.
const AIR60_HE_PID: u16 = 0xfee0;

struct Keyboard<'a> {
    protocol: HeProtocol<'a>,
    firmware: FirmwareInfo,
    base: Base,
    product: Option<String>,
}

impl Keyboard<'_> {
    fn identity(&self) -> Identity {
        Identity {
            vid: NUPHY_VID,
            pid: AIR60_HE_PID,
            firmware: self.firmware.version,
        }
    }

    fn matrix(&self) -> Result<Matrix> {
        Matrix::from_bytes(self.protocol.read_region(Region::Matrix)?)
    }

    fn travel(&self) -> Result<TravelTable> {
        TravelTable::from_bytes(self.protocol.read_region(Region::Travel)?)
    }

    fn state(&self) -> Result<State> {
        Ok(State {
            matrix: self.matrix()?,
            travel: self.travel()?,
            macros: MacroBank::from_bytes(self.protocol.read_region(Region::Macros)?)?,
        })
    }

    fn factory_matrix(&self) -> Result<Matrix> {
        Matrix::from_bytes(self.protocol.read_factory_matrix()?)
    }

    fn write(&self, plan: &Plan) -> Result<PathBuf> {
        write_and_verify(
            &default_backup_dir()?,
            self.identity(),
            plan,
            |region, chunk| self.protocol.write(region, chunk),
            || self.state(),
        )
    }
}

/// Back up the plan's current state into `dir`, mark the write unfinished,
/// `send` the plan's writes, `read_back` everything and require it to equal
/// the wanted state. Returns the new backup; on failure the error names the
/// backup to restore. Only a restore may start while an earlier write is
/// unfinished: it keeps that write's backup as the one to restore, and its
/// own backup, of possibly half-written data, is marked so it is never
/// restored.
fn write_and_verify(
    dir: &Path,
    identity: Identity,
    plan: &Plan,
    mut send: impl FnMut(Region, &WriteChunk) -> Result<()>,
    read_back: impl FnOnce() -> Result<State>,
) -> Result<PathBuf> {
    identity.firmware.check_writes()?;
    if !plan.is_restore() {
        pending::ensure_none_in(dir)?;
    }
    let over_unfinished = pending::unfinished_in(dir)?.is_some();
    let header = Header {
        identity,
        over_unfinished,
    };
    let backup = write_backup(dir, "he", &snapshot::encode(header, plan.current()))
        .context("failed to save a backup; nothing was written")?;
    let restore = pending::begin_in(dir, &backup, plan.is_restore())?;
    if over_unfinished {
        eprintln!(
            "backup of the half-written state (never restored): {}",
            backup.display()
        );
    } else {
        eprintln!("backup: {}", backup.display());
    }
    let restore_hint = || pending::restore_hint(&restore);
    for (region, chunk) in plan.writes() {
        send(*region, chunk).with_context(|| format!("write failed; {}", restore_hint()))?;
    }
    let after = read_back()
        .with_context(|| format!("failed to read the keyboard back; {}", restore_hint()))?;
    if &after != plan.want() {
        bail!(
            "verification failed: the keyboard does not hold what was written; {}",
            restore_hint()
        );
    }
    pending::end_in(dir)?;
    Ok(backup)
}

fn with_keyboard<T>(
    api: &mut HidApi,
    selector: &DeviceSelector,
    f: impl FnOnce(&Keyboard) -> Result<T>,
) -> Result<T> {
    if selector.vid.is_some_and(|v| v != NUPHY_VID)
        || selector.pid.is_some_and(|p| p != AIR60_HE_PID)
    {
        bail!(
            "only the Air60 HE ({NUPHY_VID:04x}:{AIR60_HE_PID:04x}) has a known key layout; other boards are not supported yet"
        );
    }
    let selector = DeviceSelector {
        vid: Some(NUPHY_VID),
        pid: Some(AIR60_HE_PID),
        ..selector.clone()
    };
    let dev = connect_with_retry(api, &selector)?;
    let protocol = HeProtocol::new(&dev);
    let firmware = protocol.firmware_info()?;
    let base = protocol.base(&firmware)?;
    let product = dev.get_product_string().ok().flatten();
    f(&Keyboard {
        protocol,
        firmware,
        base,
        product,
    })
}

fn modes(mode: Option<u8>) -> Vec<u8> {
    mode.map_or_else(|| (0..MODES).collect(), |m| vec![m])
}

/// Reading commands still work after an unfinished write, but say so.
fn warn_if_unfinished() -> Result<()> {
    if let Some(backup) = pending::unfinished()? {
        eprintln!(
            "warning: a previous write did not finish; the keyboard may hold half-written data ({})",
            pending::restore_hint(&backup)
        );
    }
    Ok(())
}

pub fn run_info(api: &mut HidApi, args: &HeInfoArgs) -> Result<()> {
    warn_if_unfinished()?;
    with_keyboard(api, &args.device, |kb| {
        let version = kb.firmware.version;
        if args.json {
            return print_json(&json!({
                "product": kb.product,
                "firmware": version.to_string(),
                "built": kb.firmware.built,
                "current_mode": mode_label(kb.base.current_mode),
                "layers_per_mode": kb.base.layers_per_mode,
                "writes_supported": version.writes_supported(),
            }));
        }
        println!("product: {}", kb.product.as_deref().unwrap_or("unknown"));
        println!("firmware: {version} ({})", kb.firmware.built);
        println!("current mode: {}", mode_label(kb.base.current_mode));
        println!("layers per mode: {}", kb.base.layers_per_mode);
        if !version.writes_supported() {
            println!("note: writes are refused on this firmware");
        }
        Ok(())
    })
}

struct DumpRow {
    at: KeyRef,
    now: Action,
    factory: Action,
}

pub fn run_dump(api: &mut HidApi, args: &HeDumpArgs) -> Result<()> {
    warn_if_unfinished()?;
    let read = &args.read;
    with_keyboard(api, &read.device, |kb| {
        let matrix = kb.matrix()?;
        let factory = kb.factory_matrix()?;
        let mut layers = Vec::new();
        for mode in modes(read.mode) {
            for layer in 0..LAYERS_PER_MODE {
                let rows: Vec<DumpRow> = (0..key_count())
                    .map(|slot| {
                        let at = KeyRef { mode, layer, slot };
                        DumpRow {
                            at,
                            now: matrix.action(at),
                            factory: factory.action(at),
                        }
                    })
                    .filter(|r| {
                        if args.changed {
                            r.now != r.factory
                        } else {
                            layer == 0 || r.now != Action::NONE
                        }
                    })
                    .collect();
                layers.push((
                    format!("{}.{}", mode_label(mode), layer_name(mode, layer)),
                    rows,
                ));
            }
        }
        if read.json {
            let layers: Vec<_> = layers
                .iter()
                .map(|(name, rows)| {
                    let keys: Vec<_> = rows
                        .iter()
                        .map(|r| {
                            json!({
                                "slot": r.at.slot,
                                "position": position_name(r.at.slot),
                                "action": r.now.to_string(),
                                "raw": r.now.raw(),
                                "factory": r.factory.to_string(),
                            })
                        })
                        .collect();
                    json!({ "layer": name, "keys": keys })
                })
                .collect();
            return print_json(&json!({ "changed_only": args.changed, "layers": layers }));
        }
        for (name, rows) in layers.iter().filter(|(_, rows)| !rows.is_empty()) {
            println!("{name}");
            for r in rows {
                let mark = if r.now == r.factory {
                    String::new()
                } else {
                    format!("  (factory {})", r.factory)
                };
                println!("  {:<6} {}{mark}", position_name(r.at.slot), r.now);
            }
        }
        Ok(())
    })
}

pub fn run_travel(api: &mut HidApi, args: &HeReadArgs) -> Result<()> {
    warn_if_unfinished()?;
    with_keyboard(api, &args.device, |kb| {
        let table = kb.travel()?;
        let rows: Vec<TravelRef> = modes(args.mode)
            .into_iter()
            .flat_map(|mode| (0..key_count()).map(move |slot| TravelRef { mode, slot }))
            .collect();
        if args.json {
            let keys: Vec<_> = rows
                .iter()
                .map(|&at| {
                    let r = table.record(at);
                    let mut entry = json!({
                        "mode": mode_label(at.mode),
                        "slot": at.slot,
                        "position": position_name(at.slot),
                        "switch": r.switch_type(),
                        "trigger": r.trigger().to_string(),
                        "raw": r.hex(),
                    });
                    for field in &FIELDS {
                        entry[field.name] = json!(f64::from((field.get)(&r)) / 1000.0);
                    }
                    entry
                })
                .collect();
            return print_json(&json!({ "keys": keys }));
        }
        for (i, &at) in rows.iter().enumerate() {
            if i == 0 || rows[i - 1].mode != at.mode {
                println!("{}", mode_label(at.mode));
            }
            let r = table.record(at);
            let lengths: Vec<String> = FIELDS
                .iter()
                .map(|f| format!("{} {}", f.name, mm((f.get)(&r))))
                .collect();
            println!(
                "  {:<6} {:<10} {}",
                position_name(at.slot),
                r.trigger().to_string(),
                lengths.join("  ")
            );
        }
        Ok(())
    })
}

pub fn run_export(api: &mut HidApi, args: &HeExportArgs) -> Result<()> {
    pending::ensure_none()?;
    with_keyboard(api, &args.device, |kb| {
        let state = kb.state()?;
        let factory = kb.factory_matrix()?;
        print!("{}", export_config(&modes(args.mode), &state, &factory));
        Ok(())
    })
}

pub fn run_check(args: &HeCheckArgs) -> Result<()> {
    let config = load_config(&args.file)?;
    let notes =
        check_config(&config).with_context(|| format!("invalid config {}", args.file.display()))?;
    for note in &notes {
        println!("note: {note}");
    }
    println!("ok: {}", config.summary());
    Ok(())
}

pub fn run_apply(api: &mut HidApi, args: &HeApplyArgs) -> Result<()> {
    pending::ensure_none()?;
    let config = load_config(&args.config)?;
    let notes = check_config(&config)
        .with_context(|| format!("invalid config {}", args.config.display()))?;
    with_keyboard(api, &args.write.device, |kb| {
        let mut plan = plan_changes(&config, &kb.state()?)?;
        plan.add_notes(notes);
        finish(kb, &args.write, &args.config, &plan).map(drop)
    })
}

pub fn run_restore_backup(api: &mut HidApi, args: &HeRestoreArgs) -> Result<()> {
    let file = &args.backup;
    let bytes =
        std::fs::read(file).with_context(|| format!("failed to read {}", file.display()))?;
    let (header, saved) =
        snapshot::decode(&bytes).with_context(|| format!("cannot use {}", file.display()))?;
    if header.over_unfinished {
        let instead = match pending::unfinished()? {
            Some(earlier) => pending::restore_hint(&earlier),
            None => "restore the backup the interrupted write printed first".to_string(),
        };
        bail!(
            "{} was taken over an unfinished write and may hold half-written data; {instead}",
            file.display()
        );
    }
    let identity = header.identity;
    with_keyboard(api, &args.write.device, |kb| {
        let here = kb.identity();
        if (identity.vid, identity.pid) != (here.vid, here.pid) {
            bail!(
                "{} is a backup of {:04x}:{:04x}, not of this {:04x}:{:04x}",
                file.display(),
                identity.vid,
                identity.pid,
                here.vid,
                here.pid
            );
        }
        let mut plan = plan_restore(&kb.state()?, &saved)?;
        if identity.firmware != here.firmware {
            plan.add_notes([format!(
                "the backup was taken on firmware {}, the keyboard runs {}",
                identity.firmware, here.firmware
            )]);
        }
        let outcome = finish(kb, &args.write, file, &plan)?;
        if matches!(outcome, Outcome::NoChanges) && !args.write.dry_run {
            // The keyboard now holds the backup: any unfinished write is undone.
            pending::end()?;
        }
        Ok(())
    })
}

enum Outcome {
    NoChanges,
    DryRun,
    Written(PathBuf),
}

/// The plan's writes were checked when it was made; check the firmware too,
/// so a dry run fails exactly where a real run would; then write unless it
/// is a dry run.
fn finish(kb: &Keyboard, args: &HeWriteArgs, file: &Path, plan: &Plan) -> Result<Outcome> {
    let outcome = if plan.is_empty() {
        Outcome::NoChanges
    } else {
        kb.firmware.version.check_writes()?;
        if args.dry_run {
            Outcome::DryRun
        } else {
            Outcome::Written(kb.write(plan)?)
        }
    };
    report(args.json, file, plan, &outcome)?;
    Ok(outcome)
}

fn report(as_json: bool, file: &Path, plan: &Plan, outcome: &Outcome) -> Result<()> {
    let changes: Vec<String> = plan.changes().iter().map(ToString::to_string).collect();
    if as_json {
        let (status, backup) = match outcome {
            Outcome::NoChanges => ("no_changes", None),
            Outcome::DryRun => ("dry_run", None),
            Outcome::Written(path) => ("written", Some(path.display().to_string())),
        };
        return print_json(&json!({
            "file": file.display().to_string(),
            "status": status,
            "changes": changes,
            "notes": plan.notes(),
            "backup": backup,
        }));
    }
    for note in plan.notes() {
        eprintln!("note: {note}");
    }
    for line in &changes {
        println!("{line}");
    }
    match outcome {
        Outcome::NoChanges => {
            println!(
                "no changes: the keyboard already matches {}",
                file.display()
            )
        }
        Outcome::DryRun => println!("dry run: {} change(s), nothing written", changes.len()),
        Outcome::Written(_) => println!("{} change(s) written and verified", changes.len()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::he::config::parse_config;
    use crate::he::protocol::FirmwareVersion;
    use crate::he::testutil::{SAMPLE, blank_state};
    use std::cell::{Cell, RefCell};

    const ID: Identity = Identity {
        vid: NUPHY_VID,
        pid: AIR60_HE_PID,
        firmware: FirmwareVersion::Bcd(0x120),
    };

    /// A keyboard in memory whose link drops once `budget` packets are sent.
    struct FakeKeyboard {
        state: RefCell<State>,
        budget: Cell<usize>,
    }

    impl FakeKeyboard {
        fn write(&self, dir: &Path, plan: &Plan) -> Result<PathBuf> {
            write_and_verify(
                dir,
                ID,
                plan,
                |region, chunk| {
                    let Some(left) = self.budget.get().checked_sub(1) else {
                        bail!("link dropped");
                    };
                    self.budget.set(left);
                    let at = usize::from(chunk.addr);
                    self.state.borrow_mut().region_mut(region)[at..at + chunk.data.len()]
                        .copy_from_slice(&chunk.data);
                    Ok(())
                },
                || Ok(self.state.borrow().clone()),
            )
        }

        fn now(&self) -> State {
            self.state.borrow().clone()
        }
    }

    fn saved(path: &Path) -> State {
        snapshot::decode(&std::fs::read(path).unwrap()).unwrap().1
    }

    fn header(path: &Path) -> Header {
        snapshot::decode(&std::fs::read(path).unwrap()).unwrap().0
    }

    /// The backups in `dir`, oldest first.
    fn backups(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "bin"))
            .collect();
        paths.sort_by_key(|p| std::fs::metadata(p).unwrap().modified().unwrap());
        paths
    }

    /// An apply stops half way, then the restore it points to stops half way
    /// too: the marker and every hint keep naming the backup taken before
    /// the damage, the restore's own backup is marked as half-written, no
    /// apply may start, and restoring the first backup brings the keyboard
    /// back.
    #[test]
    fn an_interrupted_restore_keeps_the_first_backup() {
        let dir = std::env::temp_dir().join(format!("nuphyctl-chain-{}", std::process::id()));
        let good = blank_state();
        let kb = FakeKeyboard {
            state: RefCell::new(good.clone()),
            budget: Cell::new(0),
        };
        let apply = plan_changes(&parse_config(SAMPLE).unwrap(), &good).unwrap();
        assert!(apply.writes().len() >= 3, "{:?}", apply.writes());
        kb.budget.set(apply.writes().len() - 1);
        let err = format!("{:#}", kb.write(&dir, &apply).unwrap_err());
        let first = pending::unfinished_in(&dir).unwrap().unwrap();
        assert!(err.contains(&first.display().to_string()), "{err}");
        assert_eq!(saved(&first), good);

        let restore = plan_restore(&kb.now(), &saved(&first)).unwrap();
        assert!(restore.writes().len() >= 2, "{:?}", restore.writes());
        kb.budget.set(1);
        let err = format!("{:#}", kb.write(&dir, &restore).unwrap_err());
        assert!(err.contains(&first.display().to_string()), "{err}");
        assert_eq!(pending::unfinished_in(&dir).unwrap(), Some(first.clone()));
        let marked: Vec<bool> = backups(&dir)
            .iter()
            .map(|p| header(p).over_unfinished)
            .collect();
        assert_eq!(marked, [false, true], "only the restore's backup is marked");

        let half = kb.now();
        let esc = parse_config("[m2.base]\nESC = \"F1\"").unwrap();
        let again = plan_changes(&esc, &half).unwrap();
        kb.budget.set(usize::MAX);
        assert!(kb.write(&dir, &again).is_err(), "no apply over the marker");
        assert_eq!(kb.now(), half);
        assert_eq!(backups(&dir).len(), 2, "and no backup either");

        kb.budget.set(usize::MAX);
        let restore = plan_restore(&kb.now(), &saved(&first)).unwrap();
        kb.write(&dir, &restore).unwrap();
        assert_eq!(kb.now(), good);
        assert_eq!(pending::unfinished_in(&dir).unwrap(), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_unchecked_firmware_before_anything() {
        let dir = std::env::temp_dir().join(format!("nuphyctl-firmware-{}", std::process::id()));
        let good = blank_state();
        let plan = plan_changes(&parse_config(SAMPLE).unwrap(), &good).unwrap();
        let old = Identity {
            firmware: FirmwareVersion::Bcd(0x105),
            ..ID
        };
        let sent = Cell::new(0);
        let err = write_and_verify(
            &dir,
            old,
            &plan,
            |_, _| {
                sent.set(sent.get() + 1);
                Ok(())
            },
            || Ok(good.clone()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("firmware"), "{err}");
        assert_eq!(sent.get(), 0);
        assert!(!dir.exists(), "no backup, no marker");
    }
}
