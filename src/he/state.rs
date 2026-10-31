//! The keyboard's memory as typed views over the bytes of each region, and
//! the changes between two such states.

use crate::he::actions::Action;
use crate::he::layout::{KeyRef, MacroRef, TravelRef, mode_label};
use crate::he::macros::{Macro, decode_region, encode_region};
use crate::he::region::{
    ACTION_LEN, LAYERS_PER_MODE, MACRO_BYTES, MACRO_COUNT, MATRIX_SLOTS, MODES, Region,
    TRAVEL_RECORD, macro_offset, matrix_offset, travel_offset,
};
use crate::he::travel::{TravelRecord, TravelSpec};
use anyhow::{Context, Result, bail};
use std::fmt;

fn checked(region: Region, bytes: Vec<u8>) -> Result<Vec<u8>> {
    if bytes.len() != region.len() {
        bail!(
            "{} is {} bytes, expected {}",
            region.name(),
            bytes.len(),
            region.len()
        );
    }
    Ok(bytes)
}

/// Key actions of all 12 layers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix(Vec<u8>);

impl Matrix {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self(checked(Region::Matrix, bytes)?))
    }

    pub fn action(&self, at: KeyRef) -> Action {
        let o = matrix_offset(at.mode, at.layer, at.slot);
        Action([self.0[o], self.0[o + 1], self.0[o + 2]])
    }

    pub fn set_action(&mut self, at: KeyRef, action: Action) {
        let o = matrix_offset(at.mode, at.layer, at.slot);
        self.0[o..o + ACTION_LEN].copy_from_slice(&action.0);
    }
}

/// Travel records of all three modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TravelTable(Vec<u8>);

impl TravelTable {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self(checked(Region::Travel, bytes)?))
    }

    pub fn record(&self, at: TravelRef) -> TravelRecord {
        let o = travel_offset(at.mode, at.slot);
        let mut r = [0u8; TRAVEL_RECORD];
        r.copy_from_slice(&self.0[o..o + TRAVEL_RECORD]);
        TravelRecord(r)
    }

    pub fn set_record(&mut self, at: TravelRef, record: TravelRecord) {
        let o = travel_offset(at.mode, at.slot);
        self.0[o..o + TRAVEL_RECORD].copy_from_slice(&record.0);
    }
}

/// Macro regions of all three modes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroBank(Vec<u8>);

impl MacroBank {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        Ok(Self(checked(Region::Macros, bytes)?))
    }

    pub fn region(&self, mode: u8) -> &[u8] {
        let o = macro_offset(mode);
        &self.0[o..o + MACRO_BYTES]
    }

    pub fn copy_region(&mut self, mode: u8, from: &Self) {
        let o = macro_offset(mode);
        self.0[o..o + MACRO_BYTES].copy_from_slice(from.region(mode));
    }

    pub fn macros(&self, mode: u8) -> Result<Vec<Macro>> {
        decode_region(self.region(mode))
            .with_context(|| format!("the macros of {} do not decode", mode_label(mode)))
    }

    pub fn set_macros(&mut self, mode: u8, macros: &[Macro]) -> Result<()> {
        let region =
            encode_region(macros).with_context(|| format!("{}.macros", mode_label(mode)))?;
        let o = macro_offset(mode);
        self.0[o..o + MACRO_BYTES].copy_from_slice(&region);
        Ok(())
    }
}

/// Everything `nuphyctl he` manages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State {
    pub matrix: Matrix,
    pub travel: TravelTable,
    pub macros: MacroBank,
}

impl State {
    pub fn from_regions([matrix, travel, macros]: [Vec<u8>; 3]) -> Result<Self> {
        Ok(Self {
            matrix: Matrix::from_bytes(matrix)?,
            travel: TravelTable::from_bytes(travel)?,
            macros: MacroBank::from_bytes(macros)?,
        })
    }

    pub fn region(&self, region: Region) -> &[u8] {
        match region {
            Region::Matrix => &self.matrix.0,
            Region::Travel => &self.travel.0,
            Region::Macros => &self.macros.0,
        }
    }

    pub fn region_mut(&mut self, region: Region) -> &mut [u8] {
        match region {
            Region::Matrix => &mut self.matrix.0,
            Region::Travel => &mut self.travel.0,
            Region::Macros => &mut self.macros.0,
        }
    }

    /// Every difference between this state and `want` over what this tool
    /// writes: key actions of the 128 slots, travel records, macros.
    pub fn changes_to(&self, want: &Self) -> Vec<Change> {
        let mut changes = Vec::new();
        for mode in 0..MODES {
            for layer in 0..LAYERS_PER_MODE {
                for slot in 0..MATRIX_SLOTS {
                    let at = KeyRef { mode, layer, slot };
                    let (from, to) = (self.matrix.action(at), want.matrix.action(at));
                    if from != to {
                        changes.push(Change::Action { at, from, to });
                    }
                }
            }
            for slot in 0..MATRIX_SLOTS {
                let at = TravelRef { mode, slot };
                let (from, to) = (self.travel.record(at), want.travel.record(at));
                if from != to {
                    changes.push(Change::Travel { at, from, to });
                }
            }
            if self.macros.region(mode) == want.macros.region(mode) {
                continue;
            }
            match (self.macros.macros(mode), want.macros.macros(mode)) {
                (Ok(now), Ok(then)) if now != then => {
                    for index in (0..MACRO_COUNT).filter(|&i| now[i] != then[i]) {
                        changes.push(Change::Macro {
                            at: MacroRef { mode, index },
                            from: now[index].clone(),
                            to: then[index].clone(),
                        });
                    }
                }
                _ => changes.push(Change::MacroRegion { mode }),
            }
        }
        changes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Action {
        at: KeyRef,
        from: Action,
        to: Action,
    },
    Travel {
        at: TravelRef,
        from: TravelRecord,
        to: TravelRecord,
    },
    Macro {
        at: MacroRef,
        from: Macro,
        to: Macro,
    },
    /// A macro region rewritten byte for byte: one side does not decode, or
    /// the same macros sit in another layout.
    MacroRegion { mode: u8 },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let show = |m: &Macro| {
            if m.0.is_empty() {
                "(empty)".to_string()
            } else {
                m.to_toml()
            }
        };
        match self {
            Self::Action { at, from, to } => write!(f, "{at}: {from} -> {to}"),
            Self::Travel { at, from, to } => {
                let before = TravelSpec::of(from);
                write!(
                    f,
                    "{at}: {} -> {}",
                    before.render(None),
                    TravelSpec::of(to).render(Some(&before))
                )
            }
            Self::Macro { at, from, to } => write!(f, "{at}: {} -> {}", show(from), show(to)),
            Self::MacroRegion { mode } => write!(
                f,
                "{}.macros: whole region rewritten byte for byte",
                mode_label(*mode)
            ),
        }
    }
}
