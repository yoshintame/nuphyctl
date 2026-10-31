//! Memory map of the HE boards: the regions `nuphyctl he` reads and writes,
//! where each entry lives, and which byte ranges may be written.

use crate::he::actions::Action;
use anyhow::{Result, bail};
use std::ops::Range;

pub const MODES: u8 = 3;
pub const LAYERS_PER_MODE: u8 = 4;
pub const LAYERS: u8 = MODES * LAYERS_PER_MODE;
pub const MATRIX_SLOTS: usize = 128;
pub const ACTION_LEN: usize = 3;
/// One layer: 128 slots of 3 bytes, then 16 G keys from byte 464.
pub const LAYER_BYTES: usize = 512;
pub const TRAVEL_RECORD: usize = 8;
/// One mode's travel table: 128 records of 8 bytes.
pub const TRAVEL_BYTES: usize = MATRIX_SLOTS * TRAVEL_RECORD;
/// One mode's macro region: 32 `u16` offsets, then 4-byte steps.
pub const MACRO_BYTES: usize = 2048;
pub const MACRO_COUNT: usize = 32;
/// Largest payload of one packet.
pub const CHUNK: usize = 56;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// Key actions, 12 layers.
    Matrix,
    /// Per-key travel records, 3 modes.
    Travel,
    /// Macro regions, 3 modes.
    Macros,
}

impl Region {
    pub const ALL: [Self; 3] = [Self::Matrix, Self::Travel, Self::Macros];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn len(self) -> usize {
        self.segment() * self.segments()
    }

    pub const fn get_cmd(self) -> u8 {
        match self {
            Self::Matrix => 8,
            Self::Macros => 12,
            Self::Travel => 160,
        }
    }

    pub const fn set_cmd(self) -> u8 {
        match self {
            Self::Matrix => 9,
            Self::Macros => 13,
            Self::Travel => 161,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Matrix => "key matrix",
            Self::Travel => "travel",
            Self::Macros => "macros",
        }
    }

    /// One layer or one mode.
    const fn segment(self) -> usize {
        match self {
            Self::Matrix => LAYER_BYTES,
            Self::Travel => TRAVEL_BYTES,
            Self::Macros => MACRO_BYTES,
        }
    }

    const fn segments(self) -> usize {
        match self {
            Self::Matrix => LAYERS as usize,
            Self::Travel | Self::Macros => MODES as usize,
        }
    }

    /// Writes cover whole entries: a key action, a travel record, a macro step.
    const fn unit(self) -> usize {
        match self {
            Self::Matrix => ACTION_LEN,
            Self::Travel => TRAVEL_RECORD,
            Self::Macros => 4,
        }
    }

    /// Bytes of a segment this tool writes. Matrix bytes past the 128 slots
    /// (the G keys) are never written.
    const fn writable(self) -> Range<usize> {
        match self {
            Self::Matrix => 0..MATRIX_SLOTS * ACTION_LEN,
            Self::Travel | Self::Macros => 0..self.segment(),
        }
    }

    pub fn is_writable_at(self, offset: usize) -> bool {
        offset < self.len() && self.writable().contains(&(offset % self.segment()))
    }

    /// The rules every write to this region must pass, whoever planned it.
    pub fn check_write(self, addr: u16, data: &[u8]) -> Result<()> {
        let addr = usize::from(addr);
        let unit = self.unit();
        if data.is_empty() || data.len() > CHUNK {
            bail!(
                "{} write of {} bytes (1..={CHUNK})",
                self.name(),
                data.len()
            );
        }
        let end = addr + data.len();
        let segment = self.segment();
        let start_in = addr % segment;
        // Entries are aligned within their layer or mode: a 512-byte layer
        // is not a multiple of a 3-byte action.
        if !start_in.is_multiple_of(unit) || !data.len().is_multiple_of(unit) {
            bail!("{} writes cover whole {unit}-byte entries", self.name());
        }
        if end > self.len()
            || addr / segment != (end - 1) / segment
            || !self.writable().contains(&start_in)
            || start_in + data.len() > self.writable().end
        {
            bail!(
                "{} write at {addr}..{end} leaves the writable bytes",
                self.name()
            );
        }
        if self == Self::Matrix {
            for b in data.chunks(ACTION_LEN) {
                let action = Action([b[0], b[1], b[2]]);
                if action.is_forbidden() {
                    bail!("refusing to assign {action} to a key");
                }
            }
        }
        Ok(())
    }
}

pub fn matrix_offset(mode: u8, layer: u8, slot: usize) -> usize {
    usize::from(mode * LAYERS_PER_MODE + layer) * LAYER_BYTES + slot * ACTION_LEN
}

pub fn travel_offset(mode: u8, slot: usize) -> usize {
    usize::from(mode) * TRAVEL_BYTES + slot * TRAVEL_RECORD
}

pub fn macro_offset(mode: u8) -> usize {
    usize::from(mode) * MACRO_BYTES
}

/// One write: the region's byte address and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteChunk {
    pub addr: u16,
    pub data: Vec<u8>,
}

/// Packets that turn `current` into `want` within the writable bytes: only
/// entries that differ, adjacent ones sharing a packet, never across a layer
/// or mode.
pub fn diff_writes(region: Region, current: &[u8], want: &[u8]) -> Vec<WriteChunk> {
    let unit = region.unit();
    let max = CHUNK / unit * unit;
    let mut chunks: Vec<WriteChunk> = Vec::new();
    let writable = region.writable();
    let entries = (0..region.segments()).flat_map(|s| {
        let base = s * region.segment();
        (base + writable.start..base + writable.end).step_by(unit)
    });
    for at in entries {
        if current[at..at + unit] == want[at..at + unit] {
            continue;
        }
        let entry = &want[at..at + unit];
        match chunks.last_mut() {
            Some(c)
                if usize::from(c.addr) + c.data.len() == at
                    && c.data.len() + unit <= max
                    && usize::from(c.addr) / region.segment() == at / region.segment() =>
            {
                c.data.extend_from_slice(entry)
            }
            _ => chunks.push(WriteChunk {
                addr: at as u16,
                data: entry.to_vec(),
            }),
        }
    }
    chunks
}

/// Byte ranges where `current` and `want` differ but this tool never writes.
pub fn unwritable_differences(region: Region, current: &[u8], want: &[u8]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    for at in 0..region.len() {
        if region.is_writable_at(at) || current[at] == want[at] {
            continue;
        }
        match out.last_mut() {
            Some(r) if r.end == at => r.end = at + 1,
            _ => out.push(at..at + 1),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_commands() {
        assert_eq!(Region::Matrix.len(), 6144);
        assert_eq!(Region::Travel.len(), 3072);
        assert_eq!(Region::Macros.len(), 6144);
        assert_eq!(matrix_offset(1, 0, 1), 4 * 512 + 3);
        assert_eq!(matrix_offset(2, 3, 60), 11 * 512 + 180);
        assert_eq!(travel_offset(1, 2), 1024 + 16);
    }

    #[test]
    fn write_rules() {
        let m = Region::Matrix;
        let a = matrix_offset(1, 0, 2) as u16;
        assert!(m.check_write(a, &[0x10, 0, 0x29]).is_ok());
        assert!(m.check_write(a + 1, &[0x10, 0, 0x29]).is_err());
        assert!(
            m.check_write(a - 2, &[0x10, 0, 0x29]).is_err(),
            "aligned to the region, not the layer"
        );
        assert!(m.check_write(a, &[0x10, 0]).is_err());
        assert!(m.check_write(a, &[]).is_err());
        assert!(m.check_write(a, &[0xf0, 0x07, 0]).is_err());
        assert!(m.check_write(a, &[0xf0, 0x08, 0]).is_err());
        assert!(
            m.check_write(matrix_offset(0, 0, 128) as u16, &[0x10, 0, 4])
                .is_err()
        );
        assert!(
            m.check_write(matrix_offset(2, 3, 127) as u16, &[0x10, 0, 4])
                .is_ok()
        );

        let t = Region::Travel;
        assert!(t.check_write(travel_offset(2, 127) as u16, &[0; 8]).is_ok());
        assert!(
            t.check_write(travel_offset(2, 127) as u16 + 8, &[0; 8])
                .is_err()
        );
        assert!(t.check_write(4, &[0; 8]).is_err());
        assert!(t.check_write(1024 - 8, &[0; 16]).is_err());

        let mc = Region::Macros;
        assert!(mc.check_write(2048 - 56, &[0; 56]).is_ok());
        assert!(mc.check_write(2048 - 8, &[0; 16]).is_err());
        assert!(mc.check_write(3 * 2048 - 4, &[0; 4]).is_ok());
        assert!(mc.check_write(3 * 2048, &[0; 4]).is_err());
        assert!(mc.check_write(0, &[0; 60]).is_err());
    }

    #[test]
    fn diffs_into_coalesced_writes() {
        let r = Region::Matrix;
        let current = vec![0u8; r.len()];
        let mut want = current.clone();
        for slot in [1, 2, 5] {
            let at = matrix_offset(0, 0, slot);
            want[at..at + 3].copy_from_slice(&[0x10, 0, 4 + slot as u8]);
        }
        // G-key bytes differ too, but are never written.
        want[470] = 1;
        let plan = diff_writes(r, &current, &want);
        assert_eq!(
            plan,
            vec![
                WriteChunk {
                    addr: 3,
                    data: vec![0x10, 0, 5, 0x10, 0, 6]
                },
                WriteChunk {
                    addr: 15,
                    data: vec![0x10, 0, 9]
                },
            ]
        );
        assert_eq!(unwritable_differences(r, &current, &want), vec![470..471]);

        let mut second_layer = current.clone();
        let at = matrix_offset(0, 1, 0);
        second_layer[at..at + 3].copy_from_slice(&[0x10, 0, 4]);
        assert_eq!(
            diff_writes(r, &current, &second_layer),
            vec![WriteChunk {
                addr: 512,
                data: vec![0x10, 0, 4]
            }]
        );

        let mut all = current.clone();
        all[..61 * 3].fill(7);
        let plan = diff_writes(r, &current, &all);
        assert!(
            plan.iter()
                .all(|c| c.data.len() <= 54 && r.check_write(c.addr, &c.data).is_ok())
        );
        assert_eq!(plan.iter().map(|c| c.data.len()).sum::<usize>(), 183);

        let m = Region::Macros;
        let blank = vec![0u8; m.len()];
        let mut want = blank.clone();
        want[2040..2052].fill(1);
        let plan = diff_writes(m, &blank, &want);
        assert_eq!(plan.len(), 2, "never across a mode: {plan:?}");
        assert!(plan.iter().all(|c| m.check_write(c.addr, &c.data).is_ok()));
    }
}
