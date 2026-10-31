//! Plans: the state the keyboard should hold afterwards. The changes to
//! report and the packets to send both come from diffing it against the
//! current state, once, so they cannot disagree.

use crate::he::actions::{ActionKind, MacroMode};
use crate::he::config::Config;
use crate::he::layout::{KeyRef, TravelRef, key_count};
use crate::he::region::{
    LAYERS_PER_MODE, MATRIX_SLOTS, MODES, Region, WriteChunk, diff_writes, unwritable_differences,
};
use crate::he::state::{Change, State};
use anyhow::{Context, Result, bail};

#[derive(Debug)]
pub struct Plan {
    current: State,
    want: State,
    changes: Vec<Change>,
    writes: Vec<(Region, WriteChunk)>,
    /// Things worth knowing that the plan does not change.
    notes: Vec<String>,
    purpose: Purpose,
}

/// What a plan is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Purpose {
    Apply,
    /// Brings back a backup: the one write that may start while an earlier
    /// one is unfinished.
    Restore,
}

impl Plan {
    /// Diff `want` against `current` into changes and packets, every packet
    /// checked against its region's write rules.
    fn new(current: State, want: State, notes: Vec<String>, purpose: Purpose) -> Result<Self> {
        let changes = current.changes_to(&want);
        let mut writes = Vec::new();
        for region in Region::ALL {
            for chunk in diff_writes(region, current.region(region), want.region(region)) {
                region.check_write(chunk.addr, &chunk.data)?;
                writes.push((region, chunk));
            }
        }
        Ok(Self {
            current,
            want,
            changes,
            writes,
            notes,
            purpose,
        })
    }

    pub fn is_restore(&self) -> bool {
        self.purpose == Purpose::Restore
    }

    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The state the plan starts from: what is backed up before writing.
    pub fn current(&self) -> &State {
        &self.current
    }

    pub fn want(&self) -> &State {
        &self.want
    }

    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// The packets that turn the current state into the wanted one.
    pub fn writes(&self) -> &[(Region, WriteChunk)] {
        &self.writes
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    pub fn add_notes(&mut self, notes: impl IntoIterator<Item = String>) {
        self.notes.extend(notes);
    }
}

/// The plan that makes `current` hold what `config` lists. Refuses actions
/// this tool does not assign, and keys whose action it may not replace.
pub fn plan_changes(config: &Config, current: &State) -> Result<Plan> {
    let mut want = current.clone();
    let mut notes = Vec::new();
    for (&at, &to) in &config.actions {
        let from = current.matrix.action(at);
        if from == to {
            continue;
        }
        if let Some(why) = to.assign_refusal() {
            bail!("{at}: cannot assign {to}: {why}; set it in NuPhy IO");
        }
        if let Some(why) = from.replace_refusal() {
            bail!("{at}: the key holds {from}: {why}; change it in NuPhy IO first");
        }
        if let (
            ActionKind::Macro {
                slot,
                mode: MacroMode::Toggle,
            },
            ActionKind::Macro {
                slot: to_slot,
                mode: MacroMode::Once,
            },
        ) = (from.kind(), to.kind())
            && slot == to_slot
        {
            notes.push(format!(
                "{at}: {to} plays once; the key repeats {from} until pressed again, so write {from} to keep that (MACRO(n) meant toggle before nuphyctl 9616150)"
            ));
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
    for mode in (0..MODES).filter(|&m| config.sets_macros(m)) {
        let list = current.macros.macros(mode).context(
            "an interrupted write? restore the backup it printed with `he restore-backup`, or rewrite the macros in NuPhy IO",
        )?;
        let wanted = config.macros_over(mode, list.clone());
        if wanted != list {
            want.macros.set_macros(mode, &wanted)?;
        }
    }
    Plan::new(current.clone(), want, notes, Purpose::Apply)
}

/// The plan that brings `current` back to `backup`, byte for byte wherever
/// this tool writes, except for actions no command writes: where the backup
/// holds one the keyboard no longer does, the key keeps what it has.
pub fn plan_restore(current: &State, backup: &State) -> Result<Plan> {
    let mut want = backup.clone();
    let mut notes = Vec::new();
    let matrix = Region::Matrix;
    for range in unwritable_differences(matrix, current.region(matrix), backup.region(matrix)) {
        notes.push(format!(
            "key matrix bytes {}..{} (past the 128 slots: unused bytes and G keys) differ from the backup; this tool does not write them",
            range.start, range.end
        ));
        want.region_mut(matrix)[range.clone()].copy_from_slice(&current.region(matrix)[range]);
    }
    for mode in 0..MODES {
        for layer in 0..LAYERS_PER_MODE {
            for slot in 0..MATRIX_SLOTS {
                let at = KeyRef { mode, layer, slot };
                let (now, then) = (current.matrix.action(at), backup.matrix.action(at));
                if now == then {
                    continue;
                }
                if then.never_write() {
                    notes.push(format!(
                        "{at}: the backup holds {then}, which no command writes; the key keeps {now} (set it in NuPhy IO)"
                    ));
                    want.matrix.set_action(at, now);
                } else if now.never_write() {
                    notes.push(format!(
                        "{at}: the key holds {now}, which no command writes; restoring puts {then} there for good"
                    ));
                }
            }
        }
        let same_macros = matches!(
            (current.macros.macros(mode), want.macros.macros(mode)),
            (Ok(now), Ok(then)) if now == then
        );
        if same_macros {
            // Same macros in another layout: nothing worth writing.
            want.macros.copy_region(mode, &current.macros);
        }
    }
    Plan::new(current.clone(), want, notes, Purpose::Restore)
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
        let count = |f: fn(&Change) -> bool| plan.changes().iter().filter(|c| f(c)).count();
        assert_eq!(count(|c| matches!(c, Change::Action { .. })), 3);
        assert_eq!(count(|c| matches!(c, Change::Travel { .. })), 1);
        assert_eq!(count(|c| matches!(c, Change::Macro { .. })), 1);
        assert!(plan.writes().iter().any(|(r, _)| *r == Region::Macros));

        let again = plan_changes(&parse_config(SAMPLE).unwrap(), plan.want()).unwrap();
        assert!(again.is_empty(), "{:?}", again.changes());
        assert!(again.writes().is_empty());
    }

    #[test]
    fn notes_a_toggle_macro_turning_into_once() {
        let mut current = blank_state();
        let w = KeyRef {
            mode: 0,
            layer: 2,
            slot: 16,
        };
        current.matrix.set_action(w, Action([0x70, 4, 0xff]));
        let once = parse_config("[m1.fn2]\nW = \"MACRO(5)\"").unwrap();
        let plan = plan_changes(&once, &current).unwrap();
        assert_eq!(plan.changes().len(), 1);
        assert!(
            plan.notes().len() == 1 && plan.notes()[0].contains("MACRO(5, toggle)"),
            "{:?}",
            plan.notes()
        );
        let kept = parse_config("[m1.fn2]\nW = \"MACRO(5, toggle)\"").unwrap();
        assert!(plan_changes(&kept, &current).unwrap().is_empty());
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
        assert_eq!(plan_changes(&remap, &current).unwrap().changes().len(), 1);

        let fn1_lbrc = KeyRef {
            mode: 0,
            layer: 1,
            slot: 26,
        };
        current.matrix.set_action(fn1_lbrc, Action([0xf0, 0x08, 0]));
        let over = parse_config("[m1.fn1]\n\"#26\" = \"F6\"").unwrap();
        let err = plan_changes(&over, &current).unwrap_err().to_string();
        assert!(err.contains("FACTORY_RESET"), "{err}");
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

        let plan = plan_restore(&broken, &good).unwrap();
        assert_eq!(plan.changes(), [Change::MacroRegion { mode: 0 }]);
        assert_eq!(plan.want(), &good);
        assert!(!plan.writes().is_empty());
    }

    #[test]
    fn restore_round_trips_and_keeps_g_keys() {
        let backup = blank_state();
        let mut current = plan_changes(&parse_config(SAMPLE).unwrap(), &backup)
            .unwrap()
            .want()
            .clone();
        current.region_mut(Region::Matrix)[470] = 9;
        let plan = plan_restore(&current, &backup).unwrap();
        assert_eq!(plan.changes().len(), 5, "{:?}", plan.changes());
        assert_eq!(plan.notes().len(), 1);
        assert!(plan.notes()[0].contains("unused bytes and G keys"));
        assert_eq!(plan.want().region(Region::Matrix)[470], 9);
        assert_eq!(plan.want().travel, backup.travel);
        assert_eq!(plan.want().macros, backup.macros);
        assert!(plan_restore(&backup, &backup).unwrap().is_empty());
    }

    /// A backup holding an action no command writes, where the keyboard now
    /// holds something else: the rest restores, that key keeps its action.
    #[test]
    fn restore_goes_around_never_written_actions() {
        let current = blank_state();
        let mut backup = current.clone();
        let at = KeyRef {
            mode: 0,
            layer: 1,
            slot: 26,
        };
        backup.matrix.set_action(at, Action([0xf0, 0x08, 0]));
        let esc = KeyRef {
            mode: 0,
            layer: 0,
            slot: 0,
        };
        backup.matrix.set_action(esc, Action([0x10, 0, 0x29]));
        let plan = plan_restore(&current, &backup).unwrap();
        assert_eq!(plan.changes().len(), 1, "{:?}", plan.changes());
        assert_eq!(plan.want().matrix.action(at), Action::NONE);
        assert_eq!(plan.want().matrix.action(esc), Action([0x10, 0, 0x29]));
        assert!(
            plan.notes().len() == 1 && plan.notes()[0].contains("FACTORY_RESET"),
            "{:?}",
            plan.notes()
        );

        let mut same = current.clone();
        same.matrix.set_action(at, Action([0xf0, 0x08, 0]));
        let plan = plan_restore(&same, &backup).unwrap();
        assert!(plan.notes().is_empty(), "the key still holds it");
        assert_eq!(plan.changes().len(), 1);

        let plan = plan_restore(&backup, &current).unwrap();
        assert_eq!(plan.want().matrix.action(at), Action::NONE);
        assert!(
            plan.notes().iter().any(|n| n.contains("for good")),
            "{:?}",
            plan.notes()
        );
    }

    /// Changes and writes come from the same diff: whenever one is empty,
    /// so is the other.
    #[test]
    fn changes_and_writes_agree() {
        let base = blank_state();
        let mut other = plan_changes(&parse_config(SAMPLE).unwrap(), &base)
            .unwrap()
            .want()
            .clone();
        for (a, b) in [(&base, &other), (&other, &base), (&base, &base)] {
            let plan = plan_restore(a, b).unwrap();
            assert_eq!(plan.is_empty(), plan.writes().is_empty());
        }
        let mut list = other.macros.macros(0).unwrap();
        list.swap(0, 3);
        other.macros.set_macros(0, &list).unwrap();
        let plan = plan_restore(&base, &other).unwrap();
        assert!(plan.changes().iter().any(|c| matches!(
            c,
            Change::Macro {
                at: MacroRef { mode: 0, .. },
                ..
            }
        )));
    }
}
