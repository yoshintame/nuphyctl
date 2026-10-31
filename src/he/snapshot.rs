//! Backup files of `nuphyctl he`: a header naming the keyboard, then every
//! region of the memory map in order.
//!
//! ```text
//! 0   12  magic "nuphyctl-he\0"
//! 12  1   format version (1)
//! 13  1   flags: bit 0 set when taken over an unfinished write
//! 14  2   USB vendor id, little endian
//! 16  2   USB product id
//! 18  2   firmware version
//! 20  ..  key matrix, travel, macros
//! ```

use crate::he::protocol::FirmwareVersion;
use crate::he::region::Region;
use crate::he::state::State;
use anyhow::{Result, bail};

const MAGIC: &[u8; 12] = b"nuphyctl-he\0";
const FORMAT: u8 = 1;
const HEADER: usize = 20;

const OVER_UNFINISHED: u8 = 1;

/// What a backup's header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub identity: Identity,
    /// Taken while an earlier write was still unfinished, so it may hold
    /// half-written data: not a state to restore.
    pub over_unfinished: bool,
}

/// The keyboard a backup was taken from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub vid: u16,
    pub pid: u16,
    pub firmware: FirmwareVersion,
}

pub fn encode(header: Header, state: &State) -> Vec<u8> {
    let identity = header.identity;
    let mut out = Vec::with_capacity(HEADER + Region::ALL.iter().map(|r| r.len()).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.push(FORMAT);
    out.push(if header.over_unfinished {
        OVER_UNFINISHED
    } else {
        0
    });
    out.extend_from_slice(&identity.vid.to_le_bytes());
    out.extend_from_slice(&identity.pid.to_le_bytes());
    out.extend_from_slice(&identity.firmware.to_u16().to_le_bytes());
    for region in Region::ALL {
        out.extend_from_slice(state.region(region));
    }
    out
}

pub fn decode(bytes: &[u8]) -> Result<(Header, State)> {
    if bytes.len() < HEADER || &bytes[..MAGIC.len()] != MAGIC {
        bail!("not an nuphyctl he backup (no header)");
    }
    if bytes[12] != FORMAT {
        bail!(
            "backup format {} is not supported (expected {FORMAT})",
            bytes[12]
        );
    }
    let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let identity = Identity {
        vid: word(14),
        pid: word(16),
        firmware: FirmwareVersion::from_u16(word(18)),
    };
    let body = &bytes[HEADER..];
    let expected: usize = Region::ALL.iter().map(|r| r.len()).sum();
    if body.len() != expected {
        bail!("backup body is {} bytes, expected {expected}", body.len());
    }
    let mut at = 0;
    let regions = Region::ALL.map(|r| {
        let part = body[at..at + r.len()].to_vec();
        at += r.len();
        part
    });
    if bytes[13] & !OVER_UNFINISHED != 0 {
        bail!(
            "backup flags {:#04x} are not known to this version",
            bytes[13]
        );
    }
    let header = Header {
        identity,
        over_unfinished: bytes[13] & OVER_UNFINISHED != 0,
    };
    Ok((header, State::from_regions(regions)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::he::testutil::blank_state;

    #[test]
    fn round_trips_and_rejects_strangers() {
        let header = Header {
            identity: Identity {
                vid: 0x19f5,
                pid: 0xfee0,
                firmware: FirmwareVersion::Bcd(0x120),
            },
            over_unfinished: false,
        };
        let state = blank_state();
        let bytes = encode(header, &state);
        assert_eq!(&bytes[..12], b"nuphyctl-he\0");
        assert_eq!(bytes[13], 0, "a plain backup reads as before the flag");
        assert_eq!(decode(&bytes).unwrap(), (header, state.clone()));
        let marked = Header {
            over_unfinished: true,
            ..header
        };
        assert_eq!(decode(&encode(marked, &state)).unwrap().0, marked);
        let mut unknown = bytes.clone();
        unknown[13] = 0x02;
        assert!(decode(&unknown).is_err(), "unknown flags are refused");

        let mut wrong_format = bytes.clone();
        wrong_format[12] = 2;
        assert!(decode(&wrong_format).is_err());
        assert!(decode(&bytes[20..]).is_err(), "headerless");
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
    }
}
