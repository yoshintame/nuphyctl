//! Plans: the state the keyboard should hold afterwards. The changes to
//! report and the packets to send both come from diffing it against the
//! current state, so they cannot disagree.

use crate::he::config::Config;
use crate::he::layout::{TravelRef, key_count};
use crate::he::region::{MODES, Region, WriteChunk, diff_writes, unwritable_differences};
use crate::he::state::{Change, State};
use anyhow::{Context, Result, bail};

#[derive(Debug)]
pub struct Plan {
    pub want: State,
    pub changes: Vec<Change>,
    /// Things worth knowing that the plan does not change.
    pub notes: Vec<String>,
}

impl Plan {
    fn new(current: &State, want: State, notes: Vec<String>) -> Self {
        let changes = current.changes_to(&want);
        Self {
            want,
            changes,
            notes,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The packets that turn `current` into the wanted state, every one
    /// checked against its region's write rules before any is sent.
    pub fn writes(&self, current: &State) -> Result<Vec<(Region, WriteChunk)>> {
        let mut out = Vec::new();
        for region in Region::ALL {
            for chunk in diff_writes(region, current.region(region), self.want.region(region)) {
                region.check_write(chunk.addr, &chunk.data)?;
                out.push((region, chunk));
            }
        }
        Ok(out)
    }
}

/// The plan that makes `current` hold what `config` lists. Refuses actions
/// this tool does not assign, and keys whose action it may not replace.
pub fn plan_changes(config: &Config, current: &State) -> Result<Plan> {
    let mut want = current.clone();
    for (&at, &to) in &config.actions {
        let from = current.matrix.action(at);
        if from == to {
            continue;
        }
        if !to.may_assign() {
            bail!(
                "{at}: cannot assign {to}: {}; set it in NuPhy IO",
                to.unmanaged_reason()
            );
        }
        if !from.may_replace() {
            bail!(
                "{at}: the key holds {from}: {}; change it in NuPhy IO first",
                from.unmanaged_reason()
            );
        }
        want.matrix.set_action(at, to);
    }
    for (&mode, section) in &config.travel {
        for slot in 0..key_count() {
            let Some(spec) = section.spec(slot) else {
                continue;
            };
            let at = TravelRef { mode, slot };
            let record = spec
                .apply_to(current.travel.record(at))
                .with_context(|| at.to_string())?;
            want.travel.set_record(at, record);
        }
    }
    for mode in 0..MODES {
        let wanted: Vec<_> = config
            .macros
            .iter()
            .filter(|(at, _)| at.mode == mode)
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let mut list = current.macros.macros(mode).context(
            "an interrupted write? restore the backup it printed with `he restore-backup`, or rewrite the macros in NuPhy IO",
        )?;
        if wanted.iter().all(|(at, m)| list[at.index] == **m) {
            continue;
        }
        for (at, m) in wanted {
            list[at.index] = m.clone();
        }
        want.macros.set_macros(mode, &list)?;
    }
    Ok(Plan::new(current, want, Vec::new()))
}

/// The plan that brings `current` back to `backup`, byte for byte wherever
/// this tool writes.
pub fn plan_restore(current: &State, backup: &State) -> Plan {
    let mut want = backup.clone();
    let mut notes = Vec::new();
    let matrix = Region::Matrix;
    for range in unwritable_differences(matrix, current.region(matrix), backup.region(matrix)) {
        notes.push(format!(
            "key matrix bytes {}..{} (G keys) differ from the backup; this tool does not write them",
            range.start, range.end
        ));
        want.region_mut(matrix)[range.clone()].copy_from_slice(&current.region(matrix)[range]);
    }
    for mode in 0..MODES {
        let same_macros = matches!(
            (current.macros.macros(mode), want.macros.macros(mode)),
            (Ok(now), Ok(then)) if now == then
        );
        if same_macros {
            // Same macros in another layout: nothing worth writing.
            want.macros.copy_region(mode, &current.macros);
        }
    }
    Plan::new(current, want, notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::he::actions::Action;
    use crate::he::config::parse_config;
    use crate::he::layout::{KeyRef, MacroRef};
    use crate::he::region::{MACRO_BYTES, MACRO_COUNT, macro_offset};
    use crate::he::testutil::{SAMPLE, blank_state};

    #[test]
    fn plans_writes_and_is_idempotent() {
        let current = blank_state();
        let plan = plan_changes(&parse_config(SAMPLE).unwrap(), &current).unwrap();
        let count = |f: fn(&Change) -> bool| plan.changes.iter().filter(|c| f(c)).count();
        assert_eq!(count(|c| matches!(c, Change::Action { .. })), 3);
        assert_eq!(count(|c| matches!(c, Change::Travel { .. })), 1);
        assert_eq!(count(|c| matches!(c, Change::Macro { .. })), 1);
        let writes = plan.writes(&current).unwrap();
        assert!(writes.iter().any(|(r, _)| *r == Region::Macros));

        let again = plan_changes(&parse_config(SAMPLE).unwrap(), &plan.want).unwrap();
        assert!(again.is_empty(), "{:?}", again.changes);
        assert!(again.writes(&plan.want).unwrap().is_empty());
    }

    #[test]
    fn unmanaged_actions() {
        let mut current = blank_state();
        let esc = KeyRef {
            mode: 0,
            layer: 0,
            slot: 0,
        };
        for text in [
            "[m1.base]\nESC = \"0x950020\"",
            "[m1.base]\nESC = \"FACTORY_RESET\"",
        ] {
            assert!(
                plan_changes(&parse_config(text).unwrap(), &current).is_err(),
                "{text}"
            );
        }
        current.matrix.set_action(esc, Action([0x95, 0, 0x20]));
        let over = parse_config("[m1.base]\nESC = \"ESC\"").unwrap();
        let err = plan_changes(&over, &current).unwrap_err().to_string();
        assert!(err.contains("Snap Tap"), "{err}");
        let same = parse_config("[m1.base]\nESC = \"0x950020\"").unwrap();
        assert!(plan_changes(&same, &current).unwrap().is_empty());

        // A factory Fn-layer function this tool does not assign can still be
        // replaced.
        let fn1_c = KeyRef {
            mode: 0,
            layer: 1,
            slot: 44,
        };
        current.matrix.set_action(fn1_c, Action([0xf0, 0xfe, 0]));
        let remap = parse_config("[m1.fn1]\nC = \"LCTL+C\"").unwrap();
        assert_eq!(plan_changes(&remap, &current).unwrap().changes.len(), 1);
    }

    fn break_macros(state: &mut State, mode: u8) {
        let at = macro_offset(mode);
        let region = &mut state.region_mut(Region::Macros)[at..at + MACRO_BYTES];
        region.fill(0);
        for i in 0..MACRO_COUNT {
            region[2 * i] = 80;
        }
    }

    /// A half-written macro region: apply refuses with a pointer to
    /// restore-backup, other modes still apply, and restore rewrites it raw.
    #[test]
    fn undecodable_macro_region() {
        let good = blank_state();
        let mut broken = good.clone();
        break_macros(&mut broken, 0);
        let m1 = parse_config("[m1.macros]\n1 = \"LCTL+C\"").unwrap();
        let err = plan_changes(&m1, &broken).unwrap_err();
        assert!(format!("{err:#}").contains("restore-backup"), "{err:#}");
        let m2 = parse_config("[m2.macros]\n1 = \"LCTL+C\"").unwrap();
        assert!(plan_changes(&m2, &broken).is_ok());

        let plan = plan_restore(&broken, &good);
        assert_eq!(plan.changes, vec![Change::MacroRegion { mode: 0 }]);
        assert_eq!(plan.want, good);
        assert!(!plan.writes(&broken).unwrap().is_empty());
    }

    #[test]
    fn restore_round_trips_and_keeps_g_keys() {
        let backup = blank_state();
        let mut current = plan_changes(&parse_config(SAMPLE).unwrap(), &backup)
            .unwrap()
            .want;
        current.region_mut(Region::Matrix)[470] = 9;
        let plan = plan_restore(&current, &backup);
        assert_eq!(plan.changes.len(), 5, "{:?}", plan.changes);
        assert_eq!(plan.notes.len(), 1);
        assert_eq!(plan.want.region(Region::Matrix)[470], 9);
        assert_eq!(plan.want.travel, backup.travel);
        assert_eq!(plan.want.macros, backup.macros);
        assert!(plan_restore(&backup, &backup).is_empty());
    }

    #[test]
    fn restore_refuses_never_written_actions() {
        let current = blank_state();
        let mut backup = current.clone();
        backup.matrix.set_action(
            KeyRef {
                mode: 0,
                layer: 1,
                slot: 5,
            },
            Action([0xf0, 0x08, 0]),
        );
        assert!(plan_restore(&current, &backup).writes(&current).is_err());
    }

    /// Changes and writes come from the same diff: whenever one is empty,
    /// so is the other.
    #[test]
    fn changes_and_writes_agree() {
        let base = blank_state();
        let mut other = plan_changes(&parse_config(SAMPLE).unwrap(), &base)
            .unwrap()
            .want;
        for (a, b) in [(&base, &other), (&other, &base), (&base, &base)] {
            let plan = plan_restore(a, b);
            assert_eq!(plan.is_empty(), plan.writes(a).unwrap().is_empty());
        }
        let mut list = other.macros.macros(0).unwrap();
        list.swap(0, 3);
        other.macros.set_macros(0, &list).unwrap();
        let plan = plan_restore(&base, &other);
        assert!(plan.changes.iter().any(|c| matches!(
            c,
            Change::Macro {
                at: MacroRef { mode: 0, .. },
                ..
            }
        )));
    }
}
