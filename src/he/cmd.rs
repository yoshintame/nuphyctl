//! `nuphyctl he ...` command handlers.

use crate::app::is_retryable_rgb_error;
use crate::backup::{default_backup_dir, write_backup};
use crate::cli::{
    DeviceSelector, HeCheckArgs, HeDumpArgs, HeExportArgs, HeInfoArgs, HeReadArgs, HeWriteArgs,
};
use crate::he::actions::Action;
use crate::he::config::{check_config, export_config, load_config};
use crate::he::layout::{KeyRef, TravelRef, key_count, layer_name, mode_label, position_name};
use crate::he::pending;
use crate::he::plan::{Plan, plan_changes, plan_restore};
use crate::he::protocol::{Base, FirmwareInfo, HeProtocol};
use crate::he::region::{LAYERS_PER_MODE, MODES, Region};
use crate::he::snapshot::{self, Identity};
use crate::he::state::{Matrix, State, TravelTable};
use crate::he::travel::{FIELDS, mm};
use crate::hid_transport::{clear_input_reports, open_selected_device};
use crate::output::print_json;
use anyhow::{Context, Result, bail};
use hidapi::{HidApi, HidDevice};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::{thread, time::Duration};

const NUPHY_VID: u16 = 0x19f5;
/// The board whose slot layout `layout::AIR60_HE` describes.
const AIR60_HE_PID: u16 = 0xfee0;
const CONNECT_MAX_ATTEMPTS: usize = 4;
const CONNECT_RETRY_BASE_DELAY_MS: u64 = 120;

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

    fn state(&self) -> Result<State> {
        let read = |r| self.protocol.read_region(r);
        State::from_regions([
            read(Region::Matrix)?,
            read(Region::Travel)?,
            read(Region::Macros)?,
        ])
    }

    fn factory_matrix(&self) -> Result<Matrix> {
        Matrix::from_bytes(self.protocol.read_factory_matrix()?)
    }

    /// Back up `current`, mark the write unfinished, send the plan's writes,
    /// read everything back and require it to equal the wanted state.
    fn write_and_verify(&self, current: &State, plan: &Plan) -> Result<PathBuf> {
        let writes = plan.writes(current)?;
        let backup = write_backup(
            &default_backup_dir()?,
            "he",
            &snapshot::encode(self.identity(), current),
        )
        .context("failed to save a backup; nothing was written")?;
        eprintln!("backup: {}", backup.display());
        let restore_hint = || {
            format!(
                "restore with: nuphyctl he restore-backup {}",
                backup.display()
            )
        };
        pending::begin(&backup)?;
        for (region, chunk) in &writes {
            self.protocol
                .write(*region, chunk)
                .with_context(|| format!("write failed; {}", restore_hint()))?;
        }
        let after = self.state().context("failed to read the keyboard back")?;
        if after != plan.want {
            bail!(
                "verification failed: the keyboard does not hold what was written; {}",
                restore_hint()
            );
        }
        pending::end()?;
        Ok(backup)
    }
}

fn connect(api: &HidApi, selector: &DeviceSelector) -> Result<HidDevice> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        let err = match open_selected_device(api, selector) {
            Ok(dev) => match clear_input_reports(&dev) {
                Ok(()) => return Ok(dev),
                Err(err) => err,
            },
            Err(err) => err,
        };
        if !is_retryable_rgb_error(&err) || attempt >= CONNECT_MAX_ATTEMPTS {
            return Err(err);
        }
        eprintln!(
            "transient HID error (attempt {attempt}/{CONNECT_MAX_ATTEMPTS}): {err} -- retrying"
        );
        thread::sleep(Duration::from_millis(
            CONNECT_RETRY_BASE_DELAY_MS * attempt as u64,
        ));
    }
}

fn with_keyboard<T>(
    api: &HidApi,
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
    let dev = connect(api, &selector)?;
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
            "warning: a previous write did not finish; the keyboard may hold half-written data (restore with: nuphyctl he restore-backup {})",
            backup.display()
        );
    }
    Ok(())
}

pub fn run_info(api: &HidApi, args: &HeInfoArgs) -> Result<()> {
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

pub fn run_dump(api: &HidApi, args: &HeDumpArgs) -> Result<()> {
    warn_if_unfinished()?;
    let read = &args.read;
    with_keyboard(api, &read.device, |kb| {
        let matrix = Matrix::from_bytes(kb.protocol.read_region(Region::Matrix)?)?;
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
                                "raw": format!("{:02x}{:02x}{:02x}", r.now.0[0], r.now.0[1], r.now.0[2]),
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

pub fn run_travel(api: &HidApi, args: &HeReadArgs) -> Result<()> {
    warn_if_unfinished()?;
    with_keyboard(api, &args.device, |kb| {
        let table = TravelTable::from_bytes(kb.protocol.read_region(Region::Travel)?)?;
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
                        "raw": r.0.iter().map(|b| format!("{b:02x}")).collect::<String>(),
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

pub fn run_export(api: &HidApi, args: &HeExportArgs) -> Result<()> {
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

pub fn run_apply(api: &HidApi, args: &HeWriteArgs) -> Result<()> {
    pending::ensure_none()?;
    let config = load_config(&args.file)?;
    let notes =
        check_config(&config).with_context(|| format!("invalid config {}", args.file.display()))?;
    with_keyboard(api, &args.device, |kb| {
        let current = kb.state()?;
        let mut plan = plan_changes(&config, &current)?;
        plan.notes.extend(notes);
        finish(kb, args, &current, &plan)
    })
}

pub fn run_restore_backup(api: &HidApi, args: &HeWriteArgs) -> Result<()> {
    let bytes = std::fs::read(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    let (identity, saved) =
        snapshot::decode(&bytes).with_context(|| format!("cannot use {}", args.file.display()))?;
    with_keyboard(api, &args.device, |kb| {
        let here = kb.identity();
        if (identity.vid, identity.pid) != (here.vid, here.pid) {
            bail!(
                "{} is a backup of {:04x}:{:04x}, not of this {:04x}:{:04x}",
                args.file.display(),
                identity.vid,
                identity.pid,
                here.vid,
                here.pid
            );
        }
        let current = kb.state()?;
        let mut plan = plan_restore(&current, &saved);
        if identity.firmware != here.firmware {
            plan.notes.push(format!(
                "the backup was taken on firmware {}, the keyboard runs {}",
                identity.firmware, here.firmware
            ));
        }
        finish(kb, args, &current, &plan)?;
        if !args.dry_run {
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

/// Validate the plan's writes and the firmware in every case, so a dry run
/// fails exactly where a real run would; then write unless it is a dry run.
fn finish(kb: &Keyboard, args: &HeWriteArgs, current: &State, plan: &Plan) -> Result<()> {
    if !plan.is_empty() {
        kb.firmware.version.check_writes()?;
        plan.writes(current)?;
    }
    let outcome = if plan.is_empty() {
        Outcome::NoChanges
    } else if args.dry_run {
        Outcome::DryRun
    } else {
        Outcome::Written(kb.write_and_verify(current, plan)?)
    };
    report(args.json, &args.file, plan, &outcome)
}

fn report(as_json: bool, file: &Path, plan: &Plan, outcome: &Outcome) -> Result<()> {
    let changes: Vec<String> = plan.changes.iter().map(ToString::to_string).collect();
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
            "notes": plan.notes,
            "backup": backup,
        }));
    }
    for note in &plan.notes {
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
