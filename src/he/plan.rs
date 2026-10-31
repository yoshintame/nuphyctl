//! The keyboard's state as bytes, and plans that turn it into another state.
//!
//! A [`Plan`] is the state the keyboard should hold afterwards plus the
//! changes for the report. Writing it is one byte diff per region, and
//! verifying it is reading the regions back and comparing with that state.

use crate::he::actions::Action;
use crate::he::config::Config;
use crate::he::layout::{LAYER_NAMES, mode_label, position_name};
use crate::he::macros::{Macro, decode_region, encode_region};
use crate::he::region::{
    LAYERS_PER_MODE, MACRO_BYTES, MACRO_COUNT, MATRIX_SLOTS, MODES, Region, TRAVEL_RECORD,
    WriteChunk, diff_writes, macro_offset, matrix_offset, travel_offset, unwritable_differences,
};
use crate::he::travel::{TravelRecord, TravelSpec};
use anyhow::{Context, Result, bail};

/// Everything `nuphyctl he` manages, region by region.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    regions: [Vec<u8>; 3],
}

/// The action in a raw key matrix (the factory one has no `State`).
pub fn action_in(matrix: &[u8], mode: u8, layer: u8, slot: usize) -> Action {
    let at = matrix_offset(mode, layer, slot);
    Action([matrix[at], matrix[at + 1], matrix[at + 2]])
}

impl State {
    pub fn new(regions: [Vec<u8>; 3]) -> Result<Self> {
        for (region, bytes) in Region::ALL.iter().zip(&regions) {
            if bytes.len() != region.len() {
                bail!(
                    "{} is {} bytes, expected {}",
                    region.name(),
                    bytes.len(),
                    region.len()
                );
            }
        }
        Ok(Self { regions })
    }

    pub fn region(&self, region: Region) -> &[u8] {
        &self.regions[region.index()]
    }

    fn region_mut(&mut self, region: Region) -> &mut [u8] {
        &mut self.regions[region.index()]
    }

    pub fn action(&self, mode: u8, layer: u8, slot: usize) -> Action {
        action_in(self.region(Region::Matrix), mode, layer, slot)
    }

    pub fn set_action(&mut self, mode: u8, layer: u8, slot: usize, action: Action) {
        let at = matrix_offset(mode, layer, slot);
        self.region_mut(Region::Matrix)[at..at + 3].copy_from_slice(&action.0);
    }

    pub fn travel(&self, mode: u8, slot: usize) -> TravelRecord {
        let at = travel_offset(mode, slot);
        let mut r = [0u8; TRAVEL_RECORD];
        r.copy_from_slice(&self.region(Region::Travel)[at..at + TRAVEL_RECORD]);
        TravelRecord(r)
    }

    pub fn set_travel(&mut self, mode: u8, slot: usize, record: TravelRecord) {
        let at = travel_offset(mode, slot);
        self.region_mut(Region::Travel)[at..at + TRAVEL_RECORD].copy_from_slice(&record.0);
    }

    pub fn macro_region(&self, mode: u8) -> &[u8] {
        let at = macro_offset(mode);
        &self.region(Region::Macros)[at..at + MACRO_BYTES]
    }

    fn copy_macro_region(&mut self, mode: u8, from: &Self) {
        let at = macro_offset(mode);
        self.region_mut(Region::Macros)[at..at + MACRO_BYTES]
            .copy_from_slice(from.macro_region(mode));
    }

    pub fn macros(&self, mode: u8) -> Result<Vec<Macro>> {
        decode_region(self.macro_region(mode))
            .with_context(|| format!("the macros of {} do not decode", mode_label(mode)))
    }

    pub fn set_macros(&mut self, mode: u8, macros: &[Macro]) -> Result<()> {
        let region =
            encode_region(macros).with_context(|| format!("{}.macros", mode_label(mode)))?;
        let at = macro_offset(mode);
        self.region_mut(Region::Macros)[at..at + MACRO_BYTES].copy_from_slice(&region);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Action {
        mode: u8,
        layer: u8,
        slot: usize,
        from: Action,
        to: Action,
    },
    Travel {
        mode: u8,
        slot: usize,
        from: TravelRecord,
        to: TravelRecord,
    },
    Macro {
        mode: u8,
        index: usize,
        from: Macro,
        to: Macro,
    },
    /// A macro region restored byte for byte, without a per-macro diff.
    MacroRegion { mode: u8 },
}

impl Change {
    pub fn describe(&self) -> String {
        match self {
            Self::Action {
                mode,
                layer,
                slot,
                from,
                to,
            } => format!(
                "{}.{} {}: {from} -> {to}",
                mode_label(*mode),
                LAYER_NAMES[usize::from(*layer)],
                position_name(*slot)
            ),
            Self::Travel {
                mode,
                slot,
                from,
                to,
            } => {
                let before = TravelSpec::of(from);
                format!(
                    "{}.travel {}: {} -> {}",
                    mode_label(*mode),
                    position_name(*slot),
                    before.render(None),
                    TravelSpec::of(to).render(Some(&before))
                )
            }
            Self::Macro {
                mode,
                index,
                from,
                to,
            } => {
                let show = |m: &Macro| {
                    if m.0.is_empty() {
                        "(empty)".to_string()
                    } else {
                        m.to_toml()
                    }
                };
                format!(
                    "{}.macros {}: {} -> {}",
                    mode_label(*mode),
                    index + 1,
                    show(from),
                    show(to)
                )
            }
            Self::MacroRegion { mode } => format!(
                "{}.macros: whole region restored byte for byte (a side does not decode)",
                mode_label(*mode)
            ),
        }
    }
}

#[derive(Debug)]
pub struct Plan {
    pub want: State,
    pub changes: Vec<Change>,
    /// Things the plan leaves alone that the user should know about.
    pub notes: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    pub fn describe(&self) -> Vec<String> {
        self.changes.iter().map(Change::describe).collect()
    }

    /// The packets that turn `current` into the wanted state, every one
    /// checked against the region's write rules before any is sent.
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

/// The plan that makes `current` hold what `config` lists. Refuses to write
/// actions this tool does not manage, or to overwrite them.
pub fn plan_changes(config: &Config, current: &State) -> Result<Plan> {
    let mut want = current.clone();
    let mut changes = Vec::new();
    for (&(mode, layer, slot), &to) in &config.actions {
        let from = current.action(mode, layer, slot);
        if from == to {
            continue;
        }
        let change = Change::Action {
            mode,
            layer,
            slot,
            from,
            to,
        };
        if !to.is_writable() {
            bail!(
                "{}: this tool does not assign {to}; set it in NuPhy IO",
                change.describe()
            );
        }
        if !from.is_writable() {
            bail!(
                "{}: the key holds {from}, which points into tables this tool does not edit; change it in NuPhy IO first",
                change.describe()
            );
        }
        want.set_action(mode, layer, slot, to);
        changes.push(change);
    }
    for (&(mode, slot), spec) in &config.travel {
        let from = current.travel(mode, slot);
        let to = spec
            .apply_to(from)
            .with_context(|| format!("{}.travel {}", mode_label(mode), position_name(slot)))?;
        if from != to {
            want.set_travel(mode, slot, to);
            changes.push(Change::Travel {
                mode,
                slot,
                from,
                to,
            });
        }
    }
    for mode in 0..MODES {
        let wanted: Vec<_> = config
            .macros
            .range((mode, 0)..(mode, MACRO_COUNT))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let mut list = current.macros(mode).context(
            "an interrupted write? restore the backup it printed with `he restore-backup`, or rewrite the macros in NuPhy IO",
        )?;
        let before = changes.len();
        for (&(_, index), to) in wanted {
            if list[index] != *to {
                changes.push(Change::Macro {
                    mode,
                    index,
                    from: list[index].clone(),
                    to: to.clone(),
                });
                list[index] = to.clone();
            }
        }
        if changes.len() > before {
            want.set_macros(mode, &list)?;
        }
    }
    Ok(Plan {
        want,
        changes,
        notes: Vec::new(),
    })
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

    let mut changes = Vec::new();
    for mode in 0..MODES {
        for layer in 0..LAYERS_PER_MODE {
            for slot in 0..MATRIX_SLOTS {
                let (from, to) = (
                    current.action(mode, layer, slot),
                    want.action(mode, layer, slot),
                );
                if from != to {
                    changes.push(Change::Action {
                        mode,
                        layer,
                        slot,
                        from,
                        to,
                    });
                }
            }
        }
        for slot in 0..MATRIX_SLOTS {
            let (from, to) = (current.travel(mode, slot), want.travel(mode, slot));
            if from != to {
                changes.push(Change::Travel {
                    mode,
                    slot,
                    from,
                    to,
                });
            }
        }
        if current.macro_region(mode) == want.macro_region(mode) {
            continue;
        }
        match (current.macros(mode), want.macros(mode)) {
            (Ok(now), Ok(then)) => {
                let differing: Vec<usize> =
                    (0..MACRO_COUNT).filter(|&i| now[i] != then[i]).collect();
                if differing.is_empty() {
                    // Same macros, another layout: nothing worth writing.
                    want.copy_macro_region(mode, current);
                }
                for index in differing {
                    changes.push(Change::Macro {
                        mode,
                        index,
                        from: now[index].clone(),
                        to: then[index].clone(),
                    });
                }
            }
            _ => changes.push(Change::MacroRegion { mode }),
        }
    }
    Plan {
        want,
        changes,
        notes,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::he::config::parse_config;
    use crate::he::layout::key_count;

    pub fn blank_state() -> State {
        let mut state = State::new(Region::ALL.map(|r| vec![0u8; r.len()])).unwrap();
        for mode in 0..MODES {
            for layer in 0..LAYERS_PER_MODE {
                for slot in 0..key_count() {
                    state.set_action(mode, layer, slot, Action::NONE);
                }
            }
            for slot in 0..key_count() {
                state.set_travel(mode, slot, TravelRecord([0x02, 0, 0x95, 0, 0, 0, 0, 0]));
            }
            state
                .set_macros(mode, &vec![Macro::default(); MACRO_COUNT])
                .unwrap();
        }
        state
    }

    pub const SAMPLE: &str = r##"
        [m1.base]
        CAPS = "FN(2)"
        "#58" = "LCTL+C"

        [m1.fn2]
        I = "UP"

        [m1.travel]
        "*" = { actuation = 1.5 }
        W = { actuation = 0.5, trigger = "rapid", press = 0.1 }

        [m1.macros]
        1 = "LCTL+Z"
    "##;

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
        assert!(again.is_empty(), "{:?}", again.describe());
        assert!(again.writes(&plan.want).unwrap().is_empty());
    }

    #[test]
    fn refuses_unmanaged_actions() {
        let mut current = blank_state();
        for text in [
            "[m1.base]\nESC = \"0x950020\"",
            "[m1.base]\nESC = \"FACTORY_RESET\"",
        ] {
            assert!(
                plan_changes(&parse_config(text).unwrap(), &current).is_err(),
                "{text}"
            );
        }
        current.set_action(0, 0, 0, Action([0x95, 0, 0x20]));
        let over = parse_config("[m1.base]\nESC = \"ESC\"").unwrap();
        assert!(plan_changes(&over, &current).is_err());
        let same = parse_config("[m1.base]\nESC = \"0x950020\"").unwrap();
        assert!(plan_changes(&same, &current).unwrap().is_empty());
    }

    /// A half-written macro region: apply refuses with a pointer to
    /// restore-backup, other modes still apply, and restore rewrites it raw.
    #[test]
    fn undecodable_macro_region() {
        let good = blank_state();
        let mut broken = good.clone();
        let at = macro_offset(0);
        let region = &mut broken.region_mut(Region::Macros)[at..at + MACRO_BYTES];
        region.fill(0);
        for i in 0..MACRO_COUNT {
            region[2 * i] = 80;
        }
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
        let config = parse_config(SAMPLE).unwrap();
        let mut current = plan_changes(&config, &backup).unwrap().want;
        current.region_mut(Region::Matrix)[470] = 9;
        let plan = plan_restore(&current, &backup);
        assert_eq!(plan.changes.len(), 5, "{:?}", plan.describe());
        assert_eq!(plan.notes.len(), 1);
        assert_eq!(plan.want.region(Region::Matrix)[470], 9);
        assert_eq!(
            plan.want.region(Region::Travel),
            backup.region(Region::Travel)
        );
        assert_eq!(
            plan.want.region(Region::Macros),
            backup.region(Region::Macros)
        );
        assert!(plan_restore(&backup, &backup).is_empty());
    }

    #[test]
    fn restore_refuses_forbidden_actions() {
        let current = blank_state();
        let mut backup = current.clone();
        backup.set_action(0, 1, 5, Action([0xf0, 0x08, 0]));
        let plan = plan_restore(&current, &backup);
        assert!(plan.writes(&current).is_err());
    }
}
