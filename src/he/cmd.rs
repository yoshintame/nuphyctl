//! `nuphyctl he ...` command handlers.

use crate::cli::{
    DeviceSelector, HeApplyArgs, HeCheckArgs, HeDumpArgs, HeExportArgs, HeInfoArgs,
    HeRestoreBackupArgs, HeTravelArgs,
};
use crate::he::model::{
    ActionChange, LAYER_NAMES, MacroChange, Macros, Matrix, Plan, TRIGGERS, Travel, TravelChange,
    check_config, export_config, key_count, mode_label, parse_config, parse_mode,
    plan_action_writes, plan_changes, plan_macro_writes, plan_travel_writes, position_name,
};
use crate::he::protocol::{
    Base, CMD_GET_DEFAULT_MATRIX, CMD_GET_MACRO, CMD_GET_TRAVEL, CMD_GET_USE_MATRIX, CMD_SET_MACRO,
    CMD_SET_TRAVEL, CMD_SET_USE_MATRIX, FirmwareInfo, HeProtocol, LAYERS_PER_MODE, MODES,
};
use crate::hid_transport::{clear_input_reports, open_selected_device};
use crate::keymap::{default_backup_dir, write_backup};
use anyhow::{Context, Result, bail};
use hidapi::HidApi;
use serde_json::{Value, json};
use std::path::PathBuf;

const NUPHY_VID: u16 = 0x19f5;
/// The board whose slot layout `model::AIR60_HE` describes.
const AIR60_HE_PID: u16 = 0xfee0;
/// Firmware versions the write path was checked against.
const WRITE_FIRMWARE: std::ops::RangeInclusive<u32> = 110..=139;

struct Keyboard<'a> {
    protocol: HeProtocol<'a>,
    firmware: FirmwareInfo,
    base: Base,
    product: Option<String>,
}

impl Keyboard<'_> {
    fn matrix(&self, factory: bool) -> Result<Matrix> {
        let cmd = if factory {
            CMD_GET_DEFAULT_MATRIX
        } else {
            CMD_GET_USE_MATRIX
        };
        Matrix::from_bytes(self.protocol.read(cmd, 0, Matrix::BYTES)?)
    }

    fn travel(&self) -> Result<Travel> {
        Travel::from_bytes(self.protocol.read(CMD_GET_TRAVEL, 0, Travel::BYTES)?)
    }

    fn macros(&self) -> Result<Macros> {
        Macros::from_bytes(self.protocol.read(CMD_GET_MACRO, 0, Macros::BYTES)?)
    }

    fn state(&self) -> Result<State> {
        Ok(State {
            matrix: self.matrix(false)?,
            travel: self.travel()?,
            macros: self.macros()?,
        })
    }

    /// Back up the current state, write the plan, read everything back and
    /// require it to match.
    fn write_and_verify(&self, current: &State, plan: &Plan) -> Result<PathBuf> {
        let number = self.firmware.number().unwrap_or(0);
        if !WRITE_FIRMWARE.contains(&number) {
            bail!(
                "refusing to write: firmware {} is outside the checked range {}..={}",
                self.firmware.version,
                WRITE_FIRMWARE.start(),
                WRITE_FIRMWARE.end()
            );
        }
        if let Some(c) = plan.actions.iter().find(|c| c.to.is_forbidden()) {
            bail!(
                "refusing to write: {} assigns a forbidden action",
                c.describe()
            );
        }
        let backup = backup(current)?;
        eprintln!("backup: {}", backup.display());
        let restore_hint = || {
            format!(
                "write failed; restore with: nuphyctl he restore-backup {}",
                backup.display()
            )
        };

        let mut want = State {
            matrix: current.matrix.clone(),
            travel: current.travel.clone(),
            macros: plan.want_macros.clone(),
        };
        for c in &plan.actions {
            want.matrix.set(c.mode, c.layer, c.slot, c.to);
        }
        for c in &plan.travel {
            want.travel.set(c.mode, c.slot, c.to);
        }
        for chunk in plan_action_writes(&plan.actions) {
            self.protocol
                .write(CMD_SET_USE_MATRIX, chunk.addr, &chunk.data)
                .with_context(restore_hint)?;
        }
        for chunk in plan_travel_writes(&plan.travel) {
            self.protocol
                .write(CMD_SET_TRAVEL, chunk.addr, &chunk.data)
                .with_context(restore_hint)?;
        }
        for chunk in plan_macro_writes(&current.macros, &want.macros) {
            self.protocol
                .write(CMD_SET_MACRO, chunk.addr, &chunk.data)
                .with_context(restore_hint)?;
        }

        let after = self.state().context("failed to read the keyboard back")?;
        if after != want {
            bail!(
                "verification failed: the keyboard does not hold what was written; restore with: nuphyctl he restore-backup {}",
                backup.display()
            );
        }
        Ok(backup)
    }
}

/// Everything `apply` manages: key matrix, travel tables, macro regions.
#[derive(Debug, PartialEq, Eq)]
struct State {
    matrix: Matrix,
    travel: Travel,
    macros: Macros,
}

const BACKUP_BYTES: usize = Matrix::BYTES + Travel::BYTES + Macros::BYTES;
/// Backups written before macros were managed hold matrix and travel only.
const LEGACY_BACKUP_BYTES: usize = Matrix::BYTES + Travel::BYTES;

fn backup(state: &State) -> Result<PathBuf> {
    let mut bytes = state.matrix.0.clone();
    bytes.extend_from_slice(&state.travel.0);
    bytes.extend_from_slice(&state.macros.0);
    write_backup(&default_backup_dir()?, "he", &bytes)
        .context("failed to save a backup; nothing was written")
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

fn print_json(value: &Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn selected_modes(mode: Option<&str>) -> Result<Vec<u8>> {
    match mode {
        Some(m) => Ok(vec![parse_mode(m)?]),
        None => Ok((0..MODES).collect()),
    }
}

pub fn run_info(api: &HidApi, args: &HeInfoArgs) -> Result<()> {
    with_keyboard(api, &args.device, |kb| {
        let writable = kb
            .firmware
            .number()
            .is_some_and(|n| WRITE_FIRMWARE.contains(&n));
        if args.json {
            return print_json(&json!({
                "product": kb.product,
                "firmware": kb.firmware.version,
                "built": kb.firmware.built,
                "current_mode": mode_label(kb.base.current_mode),
                "layers_per_mode": kb.base.layers_per_mode,
                "writes_supported": writable,
            }));
        }
        println!("product: {}", kb.product.as_deref().unwrap_or("unknown"));
        println!("firmware: {} ({})", kb.firmware.version, kb.firmware.built);
        println!("current mode: {}", mode_label(kb.base.current_mode));
        println!("layers per mode: {}", kb.base.layers_per_mode);
        if !writable {
            println!("note: writes are refused on this firmware");
        }
        Ok(())
    })
}

pub fn run_dump(api: &HidApi, args: &HeDumpArgs) -> Result<()> {
    let modes = selected_modes(args.mode.as_deref())?;
    with_keyboard(api, &args.device, |kb| {
        let current = kb.matrix(false)?;
        let factory = kb.matrix(true)?;
        let mut layers_json = Vec::new();
        for &mode in &modes {
            for layer in 0..LAYERS_PER_MODE {
                let mut keys = Vec::new();
                let mut lines = Vec::new();
                for slot in 0..key_count() {
                    let action = current.get(mode, layer, slot);
                    let default = factory.get(mode, layer, slot);
                    if args.changed && action == default {
                        continue;
                    }
                    if !args.changed && layer > 0 && action == crate::he::actions::Action::NONE {
                        continue;
                    }
                    keys.push(json!({
                        "slot": slot,
                        "position": position_name(slot),
                        "action": action.to_string(),
                        "raw": format!("{:02x}{:02x}{:02x}", action.0[0], action.0[1], action.0[2]),
                        "factory": default.to_string(),
                    }));
                    let mark = if action == default {
                        String::new()
                    } else {
                        format!("  (factory {default})")
                    };
                    lines.push(format!("  {:<6} {}{}", position_name(slot), action, mark));
                }
                let name = format!("{}.{}", mode_label(mode), LAYER_NAMES[usize::from(layer)]);
                if args.json {
                    layers_json.push(json!({ "layer": name, "keys": keys }));
                } else if !lines.is_empty() {
                    println!("{name}");
                    for line in lines {
                        println!("{line}");
                    }
                }
            }
        }
        if args.json {
            print_json(&json!({ "changed_only": args.changed, "layers": layers_json }))?;
        }
        Ok(())
    })
}

pub fn run_travel(api: &HidApi, args: &HeTravelArgs) -> Result<()> {
    let modes = selected_modes(args.mode.as_deref())?;
    with_keyboard(api, &args.device, |kb| {
        let travel = kb.travel()?;
        let mut out = Vec::new();
        for &mode in &modes {
            if !args.json {
                println!("{}", mode_label(mode));
            }
            for slot in 0..key_count() {
                let r = travel.get(mode, slot);
                let trigger = TRIGGERS
                    .get(usize::from(r.trigger()))
                    .copied()
                    .unwrap_or("?");
                if args.json {
                    out.push(json!({
                        "mode": mode_label(mode),
                        "slot": slot,
                        "position": position_name(slot),
                        "switch": r.switch_type(),
                        "trigger": trigger,
                        "actuation": r.actuation_mm(),
                        "press": r.press_mm(),
                        "release": r.release_mm(),
                        "upper_deadzone": r.upper_deadzone_mm(),
                        "lower_deadzone": r.lower_deadzone_mm(),
                        "raw": r.0.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                    }));
                    continue;
                }
                println!(
                    "  {:<6} {:<10} actuation {:.2}  press {:.3}  release {:.3}  dead {:.2}/{:.2}",
                    position_name(slot),
                    trigger,
                    r.actuation_mm(),
                    r.press_mm(),
                    r.release_mm(),
                    r.upper_deadzone_mm(),
                    r.lower_deadzone_mm()
                );
            }
        }
        if args.json {
            print_json(&json!({ "keys": out }))?;
        }
        Ok(())
    })
}

pub fn run_export(api: &HidApi, args: &HeExportArgs) -> Result<()> {
    let modes = selected_modes(args.mode.as_deref())?;
    with_keyboard(api, &args.device, |kb| {
        let state = kb.state()?;
        let factory = kb.matrix(true)?;
        print!(
            "{}",
            export_config(
                &modes,
                &state.matrix,
                &factory,
                &state.travel,
                &state.macros
            )?
        );
        Ok(())
    })
}

pub fn run_check(args: &HeCheckArgs) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    let config =
        parse_config(&text).with_context(|| format!("invalid config {}", args.file.display()))?;
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

pub fn run_apply(api: &HidApi, args: &HeApplyArgs) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    let config =
        parse_config(&text).with_context(|| format!("invalid config {}", args.file.display()))?;
    for note in check_config(&config)? {
        eprintln!("note: {note}");
    }
    with_keyboard(api, &args.device, |kb| {
        let state = kb.state()?;
        let plan = plan_changes(&config, &state.matrix, &state.travel, &state.macros)?;
        let backup = if args.dry_run || plan.is_empty() {
            None
        } else {
            Some(kb.write_and_verify(&state, &plan)?)
        };
        report(args.json, &args.file, &plan, args.dry_run, backup)
    })
}

pub fn run_restore_backup(api: &HidApi, args: &HeRestoreBackupArgs) -> Result<()> {
    let bytes = std::fs::read(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    if bytes.len() != BACKUP_BYTES && bytes.len() != LEGACY_BACKUP_BYTES {
        bail!(
            "{} is {} bytes, not an HE backup ({BACKUP_BYTES} bytes, or {LEGACY_BACKUP_BYTES} without macros)",
            args.file.display(),
            bytes.len()
        );
    }
    let (m, rest) = bytes.split_at(Matrix::BYTES);
    let (t, saved_macros) = rest.split_at(Travel::BYTES);
    let want_matrix = Matrix::from_bytes(m.to_vec())?;
    let want_travel = Travel::from_bytes(t.to_vec())?;
    with_keyboard(api, &args.device, |kb| {
        let state = kb.state()?;
        let mut plan = Plan {
            actions: Vec::new(),
            travel: Vec::new(),
            macros: Vec::new(),
            raw_macro_regions: Vec::new(),
            want_macros: state.macros.clone(),
        };
        for layer in 0..crate::he::protocol::LAYERS {
            let at = usize::from(layer) * crate::he::protocol::LAYER_BYTES;
            let tail =
                at + crate::he::protocol::MATRIX_SLOTS * 3..at + crate::he::protocol::LAYER_BYTES;
            if state.matrix.0[tail.clone()] != want_matrix.0[tail] {
                eprintln!(
                    "note: bytes 384..512 of layer {layer} (G keys) differ from the backup; this tool does not write them"
                );
            }
        }
        for mode in 0..MODES {
            for slot in 0..crate::he::protocol::MATRIX_SLOTS {
                for layer in 0..LAYERS_PER_MODE {
                    let from = state.matrix.get(mode, layer, slot);
                    let to = want_matrix.get(mode, layer, slot);
                    if from != to {
                        plan.actions.push(ActionChange {
                            mode,
                            layer,
                            slot,
                            from,
                            to,
                        });
                    }
                }
                let (from, to) = (state.travel.get(mode, slot), want_travel.get(mode, slot));
                if from != to {
                    plan.travel.push(TravelChange {
                        mode,
                        slot,
                        from,
                        to,
                    });
                }
            }
        }
        if !saved_macros.is_empty() {
            let saved = Macros::from_bytes(saved_macros.to_vec())?;
            for mode in 0..MODES {
                if state.macros.region(mode) == saved.region(mode) {
                    continue;
                }
                match (state.macros.decode(mode), saved.decode(mode)) {
                    (Ok(now), Ok(then)) => {
                        let changed: Vec<_> = (0..now.len())
                            .filter(|&i| now[i] != then[i])
                            .map(|index| MacroChange {
                                mode,
                                index,
                                from: now[index].clone(),
                                to: then[index].clone(),
                            })
                            .collect();
                        if changed.is_empty() {
                            continue;
                        }
                        plan.macros.extend(changed);
                    }
                    _ => plan.raw_macro_regions.push(mode),
                }
                plan.want_macros.set_region(mode, saved.region(mode));
            }
        }
        let backup = if args.dry_run || plan.is_empty() {
            None
        } else {
            Some(kb.write_and_verify(&state, &plan)?)
        };
        report(args.json, &args.file, &plan, args.dry_run, backup)
    })
}

fn report(
    as_json: bool,
    file: &std::path::Path,
    plan: &Plan,
    dry_run: bool,
    backup: Option<PathBuf>,
) -> Result<()> {
    let changes = plan.describe();
    if as_json {
        return print_json(&json!({
            "file": file.display().to_string(),
            "changes": changes,
            "dry_run": dry_run,
            "written": backup.is_some(),
            "verified": backup.is_some(),
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
