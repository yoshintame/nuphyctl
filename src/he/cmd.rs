//! `nuphyctl he ...` command handlers.

use crate::cli::{
    DeviceSelector, HeApplyArgs, HeDumpArgs, HeExportArgs, HeInfoArgs, HeRestoreBackupArgs,
    HeTravelArgs,
};
use crate::he::model::{
    ActionChange, LAYER_NAMES, Matrix, TRIGGERS, Travel, TravelChange, export_config, key_count,
    mode_label, parse_config, parse_mode, plan_action_writes, plan_changes, plan_travel_writes,
    position_name,
};
use crate::he::protocol::{
    Base, CMD_GET_DEFAULT_MATRIX, CMD_GET_TRAVEL, CMD_GET_USE_MATRIX, CMD_SET_TRAVEL,
    CMD_SET_USE_MATRIX, FirmwareInfo, HeProtocol, LAYERS_PER_MODE, MODES,
};
use crate::hid_transport::{clear_input_reports, open_selected_device};
use crate::keymap::{default_backup_dir, write_backup};
use anyhow::{Context, Result, bail};
use hidapi::HidApi;
use serde_json::{Value, json};
use std::path::PathBuf;

const NUPHY_VID: u16 = 0x19f5;
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

    /// Back up matrix and travel, write the changes, read both back and
    /// require them to match.
    fn write_and_verify(
        &self,
        matrix: &Matrix,
        travel: &Travel,
        actions: &[ActionChange],
        records: &[TravelChange],
    ) -> Result<PathBuf> {
        let number = self.firmware.number().unwrap_or(0);
        if !WRITE_FIRMWARE.contains(&number) {
            bail!(
                "refusing to write: firmware {} is outside the checked range {}..={}",
                self.firmware.version,
                WRITE_FIRMWARE.start(),
                WRITE_FIRMWARE.end()
            );
        }
        let backup = backup(matrix, travel)?;
        eprintln!("backup: {}", backup.display());
        let restore_hint = || {
            format!(
                "write failed; restore with: nuphyctl he restore-backup {}",
                backup.display()
            )
        };

        let mut want_matrix = matrix.clone();
        for c in actions {
            want_matrix.set(c.mode, c.layer, c.slot, c.to);
        }
        let mut want_travel = travel.clone();
        for c in records {
            want_travel.set(c.mode, c.slot, c.to);
        }
        for chunk in plan_action_writes(actions) {
            self.protocol
                .write(CMD_SET_USE_MATRIX, chunk.addr, &chunk.data)
                .with_context(restore_hint)?;
        }
        for chunk in plan_travel_writes(records) {
            self.protocol
                .write(CMD_SET_TRAVEL, chunk.addr, &chunk.data)
                .with_context(restore_hint)?;
        }

        let after_matrix = self
            .matrix(false)
            .context("failed to read the matrix back")?;
        let after_travel = self.travel().context("failed to read travel back")?;
        if after_matrix != want_matrix || after_travel != want_travel {
            bail!(
                "verification failed: the keyboard does not hold what was written; restore with: nuphyctl he restore-backup {}",
                backup.display()
            );
        }
        Ok(backup)
    }
}

fn backup(matrix: &Matrix, travel: &Travel) -> Result<PathBuf> {
    let mut bytes = matrix.0.clone();
    bytes.extend_from_slice(&travel.0);
    write_backup(&default_backup_dir()?, "he", &bytes)
        .context("failed to save a backup; nothing was written")
}

fn with_keyboard<T>(
    api: &HidApi,
    selector: &DeviceSelector,
    f: impl FnOnce(&Keyboard) -> Result<T>,
) -> Result<T> {
    let selector = DeviceSelector {
        vid: selector.vid.or(Some(NUPHY_VID)),
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
        let current = kb.matrix(false)?;
        let factory = kb.matrix(true)?;
        let travel = kb.travel()?;
        print!("{}", export_config(&modes, &current, &factory, &travel));
        Ok(())
    })
}

pub fn run_apply(api: &HidApi, args: &HeApplyArgs) -> Result<()> {
    let text = std::fs::read_to_string(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    let config =
        parse_config(&text).with_context(|| format!("invalid config {}", args.file.display()))?;
    with_keyboard(api, &args.device, |kb| {
        let matrix = kb.matrix(false)?;
        let travel = kb.travel()?;
        let (actions, records) = plan_changes(&config, &matrix, &travel)?;
        let backup = if args.dry_run || (actions.is_empty() && records.is_empty()) {
            None
        } else {
            Some(kb.write_and_verify(&matrix, &travel, &actions, &records)?)
        };
        report(
            args.json,
            &args.file,
            &actions,
            &records,
            args.dry_run,
            backup,
        )
    })
}

pub fn run_restore_backup(api: &HidApi, args: &HeRestoreBackupArgs) -> Result<()> {
    let bytes = std::fs::read(&args.file)
        .with_context(|| format!("failed to read {}", args.file.display()))?;
    if bytes.len() != Matrix::BYTES + Travel::BYTES {
        bail!(
            "{} is {} bytes, not an HE backup ({} bytes)",
            args.file.display(),
            bytes.len(),
            Matrix::BYTES + Travel::BYTES
        );
    }
    let want_matrix = Matrix::from_bytes(bytes[..Matrix::BYTES].to_vec())?;
    let want_travel = Travel::from_bytes(bytes[Matrix::BYTES..].to_vec())?;
    with_keyboard(api, &args.device, |kb| {
        let matrix = kb.matrix(false)?;
        let travel = kb.travel()?;
        let mut actions = Vec::new();
        let mut records = Vec::new();
        for mode in 0..MODES {
            for slot in 0..crate::he::protocol::MATRIX_SLOTS {
                for layer in 0..LAYERS_PER_MODE {
                    let (from, to) = (
                        matrix.get(mode, layer, slot),
                        want_matrix.get(mode, layer, slot),
                    );
                    if from != to {
                        actions.push(ActionChange {
                            mode,
                            layer,
                            slot,
                            from,
                            to,
                        });
                    }
                }
                let (from, to) = (travel.get(mode, slot), want_travel.get(mode, slot));
                if from != to {
                    records.push(TravelChange {
                        mode,
                        slot,
                        from,
                        to,
                    });
                }
            }
        }
        let backup = if args.dry_run || (actions.is_empty() && records.is_empty()) {
            None
        } else {
            Some(kb.write_and_verify(&matrix, &travel, &actions, &records)?)
        };
        report(
            args.json,
            &args.file,
            &actions,
            &records,
            args.dry_run,
            backup,
        )
    })
}

fn report(
    as_json: bool,
    file: &std::path::Path,
    actions: &[ActionChange],
    records: &[TravelChange],
    dry_run: bool,
    backup: Option<PathBuf>,
) -> Result<()> {
    if as_json {
        return print_json(&json!({
            "file": file.display().to_string(),
            "actions": actions.iter().map(ActionChange::describe).collect::<Vec<_>>(),
            "travel": records.iter().map(TravelChange::describe).collect::<Vec<_>>(),
            "dry_run": dry_run,
            "written": backup.is_some(),
            "verified": backup.is_some(),
            "backup": backup.map(|p| p.display().to_string()),
        }));
    }
    if actions.is_empty() && records.is_empty() {
        println!(
            "no changes: the keyboard already matches {}",
            file.display()
        );
        return Ok(());
    }
    for c in actions {
        println!("{}", c.describe());
    }
    for c in records {
        println!("{}", c.describe());
    }
    let n = actions.len() + records.len();
    if dry_run {
        println!("dry run: {n} change(s), nothing written");
    } else {
        println!("{n} change(s) written and verified");
    }
    Ok(())
}
