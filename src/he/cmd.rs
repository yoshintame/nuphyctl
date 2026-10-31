//! `nuphyctl he ...` command handlers.

use crate::backup::{default_backup_dir, write_backup};
use crate::cli::{
    DeviceSelector, HeCheckArgs, HeDumpArgs, HeExportArgs, HeInfoArgs, HeReadArgs, HeWriteArgs,
};
use crate::he::actions::Action;
use crate::he::config::{check_config, export_config, load_config};
use crate::he::layout::{LAYER_NAMES, key_count, mode_label, position_name};
use crate::he::plan::{Plan, State, action_in, plan_changes, plan_restore};
use crate::he::protocol::{Base, FirmwareInfo, HeProtocol};
use crate::he::region::{LAYERS_PER_MODE, MODES, Region};
use crate::he::snapshot::{self, Identity};
use crate::he::travel::{FIELDS, mm};
use crate::hid_transport::{clear_input_reports, open_selected_device};
use crate::keymap_cmd::print_json;
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

    fn state(&self) -> Result<State> {
        let [matrix, travel, macros] = Region::ALL;
        State::new([
            self.protocol.read_region(matrix)?,
            self.protocol.read_region(travel)?,
            self.protocol.read_region(macros)?,
        ])
    }

    /// Back up `current`, send the plan's writes, read everything back and
    /// require it to equal the wanted state.
    fn write_and_verify(&self, current: &State, plan: &Plan) -> Result<PathBuf> {
        self.firmware.version.check_writes()?;
        let writes = plan.writes(current)?;
        let backup = write_backup(
            &default_backup_dir()?,
            "he",
            &snapshot::encode(self.identity(), current),
        )
        .context("failed to save a backup; nothing was written")?;
        eprintln!("backup: {}", backup.display());
        for (region, chunk) in &writes {
            self.protocol.write(*region, chunk).with_context(|| {
                format!(
                    "write failed; restore with: nuphyctl he restore-backup {}",
                    backup.display()
                )
            })?;
        }
        let after = self.state().context("failed to read the keyboard back")?;
        if after != plan.want {
            bail!(
                "verification failed: the keyboard does not hold what was written; restore with: nuphyctl he restore-backup {}",
                backup.display()
            );
        }
        Ok(backup)
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
    let dev = open_selected_device(api, &selector)?;
    clear_input_reports(&dev)?;
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

pub fn run_info(api: &HidApi, args: &HeInfoArgs) -> Result<()> {
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

pub fn run_dump(api: &HidApi, args: &HeDumpArgs) -> Result<()> {
    let read = &args.read;
    with_keyboard(api, &read.device, |kb| {
        let matrix = kb.protocol.read_region(Region::Matrix)?;
        let factory = kb.protocol.read_factory_matrix()?;
        let mut layers_json = Vec::new();
        for mode in modes(read.mode) {
            for layer in 0..LAYERS_PER_MODE {
                let name = format!("{}.{}", mode_label(mode), LAYER_NAMES[usize::from(layer)]);
                let shown: Vec<(usize, Action, Action)> = (0..key_count())
                    .map(|slot| {
                        let now = action_in(&matrix, mode, layer, slot);
                        (slot, now, action_in(&factory, mode, layer, slot))
                    })
                    .filter(|(_, now, factory)| {
                        if args.changed {
                            now != factory
                        } else {
                            layer == 0 || *now != Action::NONE
                        }
                    })
                    .collect();
                if read.json {
                    let keys: Vec<_> = shown
                        .iter()
                        .map(|(slot, now, factory)| {
                            json!({
                                "slot": slot,
                                "position": position_name(*slot),
                                "action": now.to_string(),
                                "raw": format!("{:02x}{:02x}{:02x}", now.0[0], now.0[1], now.0[2]),
                                "factory": factory.to_string(),
                            })
                        })
                        .collect();
                    layers_json.push(json!({ "layer": name, "keys": keys }));
                    continue;
                }
                if shown.is_empty() {
                    continue;
                }
                println!("{name}");
                for (slot, now, factory) in shown {
                    let mark = if now == factory {
                        String::new()
                    } else {
                        format!("  (factory {factory})")
                    };
                    println!("  {:<6} {now}{mark}", position_name(slot));
                }
            }
        }
        if read.json {
            print_json(&json!({ "changed_only": args.changed, "layers": layers_json }))?;
        }
        Ok(())
    })
}

pub fn run_travel(api: &HidApi, args: &HeReadArgs) -> Result<()> {
    with_keyboard(api, &args.device, |kb| {
        let state = kb.state()?;
        let mut keys = Vec::new();
        for mode in modes(args.mode) {
            if !args.json {
                println!("{}", mode_label(mode));
            }
            for slot in 0..key_count() {
                let r = state.travel(mode, slot);
                if args.json {
                    let mut entry = json!({
                        "mode": mode_label(mode),
                        "slot": slot,
                        "position": position_name(slot),
                        "switch": r.switch_type(),
                        "trigger": r.trigger().to_string(),
                        "raw": r.0.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    });
                    for field in &FIELDS {
                        entry[field.name] = json!(f64::from((field.get)(&r)) / 1000.0);
                    }
                    keys.push(entry);
                    continue;
                }
                let lengths: Vec<String> = FIELDS
                    .iter()
                    .map(|f| format!("{} {}", f.name, mm((f.get)(&r))))
                    .collect();
                println!(
                    "  {:<6} {:<10} {}",
                    position_name(slot),
                    r.trigger().to_string(),
                    lengths.join("  ")
                );
            }
        }
        if args.json {
            print_json(&json!({ "keys": keys }))?;
        }
        Ok(())
    })
}

pub fn run_export(api: &HidApi, args: &HeExportArgs) -> Result<()> {
    with_keyboard(api, &args.device, |kb| {
        let state = kb.state()?;
        let factory = kb.protocol.read_factory_matrix()?;
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
    println!(
        "ok: {} key action(s), {} travel entr(ies), {} macro(s)",
        config.actions.len(),
        config.travel.len(),
        config.macros.len()
    );
    Ok(())
}

pub fn run_apply(api: &HidApi, args: &HeWriteArgs) -> Result<()> {
    let config = load_config(&args.file)?;
    for note in check_config(&config)? {
        eprintln!("note: {note}");
    }
    with_keyboard(api, &args.device, |kb| {
        let current = kb.state()?;
        let plan = plan_changes(&config, &current)?;
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
        if identity.firmware != here.firmware {
            eprintln!(
                "note: the backup was taken on firmware {}, the keyboard runs {}",
                identity.firmware, here.firmware
            );
        }
        let current = kb.state()?;
        let plan = plan_restore(&current, &saved);
        finish(kb, args, &current, &plan)
    })
}

fn finish(kb: &Keyboard, args: &HeWriteArgs, current: &State, plan: &Plan) -> Result<()> {
    for note in &plan.notes {
        eprintln!("note: {note}");
    }
    let backup = if args.dry_run || plan.is_empty() {
        None
    } else {
        Some(kb.write_and_verify(current, plan)?)
    };
    report(args.json, &args.file, plan, args.dry_run, backup)
}

fn report(
    as_json: bool,
    file: &Path,
    plan: &Plan,
    dry_run: bool,
    backup: Option<PathBuf>,
) -> Result<()> {
    let changes = plan.describe();
    if as_json {
        return print_json(&json!({
            "file": file.display().to_string(),
            "changes": changes,
            "notes": plan.notes,
            "dry_run": dry_run,
            "written": backup.is_some(),
            "backup": backup.map(|p| p.display().to_string()),
        }));
    }
    if changes.is_empty() {
        println!(
            "no changes: the keyboard already matches {}",
            file.display()
        );
        return Ok(());
    }
    for line in &changes {
        println!("{line}");
    }
    if dry_run {
        println!("dry run: {} change(s), nothing written", changes.len());
    } else {
        println!("{} change(s) written and verified", changes.len());
    }
    Ok(())
}
