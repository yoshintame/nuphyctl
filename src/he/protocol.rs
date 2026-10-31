//! Vendor HID protocol of NuPhy's Hall-effect boards (Air60 HE, Air75 HE,
//! Halo65 HE, Field75 HE), as drive.nuphy.io speaks it.
//!
//! It shares the 64-byte framing with the Air75 V3 protocol but has no
//! session key exchange and its own command set: every `Get*` command reads a
//! region of the keyboard's memory by address and length, every `Set*` writes
//! one.
//!
//! ```text
//! request: 55 cmd 00  sum len addr_lo addr_hi 00 data...
//! reply:   aa cmd key sum len addr_lo addr_hi 00 data^key...
//! ```

use crate::hid_transport::{HidResponder, REPORT_LEN, Report};
use anyhow::{Context, Result, bail};
use hidapi::HidDevice;
use std::time::Duration;

pub const CHUNK: usize = REPORT_LEN - 8;

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
const BASE_BYTES: usize = 16;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1000);
const ATTEMPTS: usize = 3;

const FLAG_HOST: u8 = 0x55;
const FLAG_REPLY: u8 = 0xaa;

pub const CMD_GET_INFO: u8 = 3;
pub const CMD_GET_BASE: u8 = 4;
pub const CMD_GET_DEFAULT_MATRIX: u8 = 7;
pub const CMD_GET_USE_MATRIX: u8 = 8;
pub const CMD_SET_USE_MATRIX: u8 = 9;
pub const CMD_GET_TRAVEL: u8 = 160;
pub const CMD_SET_TRAVEL: u8 = 161;

/// The commands this tool may ever send. Everything else (factory reset,
/// calibration, mode settings whose byte 0 enters the firmware loader, the
/// loader itself) is refused before a packet is built.
const ALLOWED: &[u8] = &[
    CMD_GET_INFO,
    CMD_GET_BASE,
    CMD_GET_DEFAULT_MATRIX,
    CMD_GET_USE_MATRIX,
    CMD_SET_USE_MATRIX,
    CMD_GET_TRAVEL,
    CMD_SET_TRAVEL,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInfo {
    /// Two bytes read as a hexadecimal number, `14 01` -> `114`.
    pub version: String,
    pub built: String,
}

impl FirmwareInfo {
    pub fn number(&self) -> Option<u32> {
        self.version.parse().ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Base {
    pub current_mode: u8,
    pub layers_per_mode: u8,
}

pub struct HeProtocol<'a> {
    dev: &'a HidDevice,
    responder: HidResponder,
}

impl<'a> HeProtocol<'a> {
    pub fn new(dev: &'a HidDevice) -> Self {
        Self {
            dev,
            responder: HidResponder::with_timeout(DEFAULT_TIMEOUT),
        }
    }

    pub fn firmware_info(&self) -> Result<FirmwareInfo> {
        let reply = self
            .transact(CMD_GET_INFO, 0, 0, &[])
            .context("GetInfo (3) failed")?;
        Ok(parse_info(&reply))
    }

    pub fn base(&self, firmware: &FirmwareInfo) -> Result<Base> {
        let data = self
            .read(CMD_GET_BASE, 0, BASE_BYTES)
            .context("GetBase (4) failed")?;
        parse_base(&data, base_offset(firmware))
    }

    /// Read `size` bytes of a region starting at `addr`, in packet-sized chunks.
    pub fn read(&self, cmd: u8, addr: u16, size: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(size);
        while out.len() < size {
            let n = (size - out.len()).min(CHUNK);
            let at = usize::from(addr) + out.len();
            let at = u16::try_from(at).context("region address exceeds 16 bits")?;
            let reply = self
                .transact(cmd, at, n as u8, &[])
                .with_context(|| format!("read (cmd {cmd}) at {at} failed"))?;
            out.extend_from_slice(&reply_data(&reply, n));
        }
        Ok(out)
    }

    /// Write `data` (at most one packet) to a region at `addr`.
    pub fn write(&self, cmd: u8, addr: u16, data: &[u8]) -> Result<()> {
        guard_write(cmd, addr, data)?;
        self.transact(cmd, addr, data.len() as u8, data)
            .with_context(|| format!("write (cmd {cmd}) at {addr} failed"))?;
        Ok(())
    }

    fn transact(&self, cmd: u8, addr: u16, len: u8, data: &[u8]) -> Result<Report> {
        let packet = build(cmd, addr, len, data)?;
        let mut last_err = None;
        for _ in 0..ATTEMPTS {
            let reply = self
                .responder
                .send_and_expect_match(self.dev, 0, &packet, |r| {
                    r[0] == FLAG_REPLY && r[1] == cmd && reply_addr(r) == addr
                });
            match reply.and_then(|r| check_reply(&r, cmd).map(|()| r)) {
                Ok(r) => return Ok(r),
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err.expect("at least one attempt"))
    }
}

pub fn build(cmd: u8, addr: u16, len: u8, data: &[u8]) -> Result<Report> {
    if !ALLOWED.contains(&cmd) {
        bail!("command {cmd} is not one this tool sends");
    }
    if usize::from(len) > CHUNK || data.len() > CHUNK {
        bail!("at most {CHUNK} bytes per packet");
    }
    let mut p = [0u8; REPORT_LEN];
    p[0] = FLAG_HOST;
    p[1] = cmd;
    p[4] = len;
    p[5] = addr as u8;
    p[6] = (addr >> 8) as u8;
    p[8..8 + data.len()].copy_from_slice(data);
    p[3] = checksum(&p);
    Ok(p)
}

fn guard_write(cmd: u8, addr: u16, data: &[u8]) -> Result<()> {
    let addr = usize::from(addr);
    match cmd {
        CMD_SET_USE_MATRIX => {
            let within = addr % LAYER_BYTES;
            if !within.is_multiple_of(ACTION_LEN) || !data.len().is_multiple_of(ACTION_LEN) {
                bail!("key actions are written whole: 3 bytes from a slot's own address");
            }
            if within + data.len() > MATRIX_SLOTS * ACTION_LEN {
                bail!(
                    "write reaches past the matrix slots of layer {}",
                    addr / LAYER_BYTES
                );
            }
            if addr + data.len() > usize::from(LAYERS) * LAYER_BYTES {
                bail!("write reaches past the last layer");
            }
            for action in data.chunks(ACTION_LEN) {
                if action[0] == 0xf0 && action[1] == 0x07 {
                    bail!("refusing to assign the reboot-into-firmware-loader action");
                }
            }
        }
        CMD_SET_TRAVEL => {
            if !addr.is_multiple_of(TRAVEL_RECORD) || !data.len().is_multiple_of(TRAVEL_RECORD) {
                bail!("travel records are written whole: 8 bytes from a record's own address");
            }
            if addr + data.len() > usize::from(MODES) * TRAVEL_BYTES {
                bail!("write reaches past the last mode's travel table");
            }
        }
        _ => bail!("command {cmd} is not a write this tool makes"),
    }
    Ok(())
}

fn checksum(p: &Report) -> u8 {
    p[4..].iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

fn reply_addr(r: &Report) -> u16 {
    u16::from(r[5]) | (u16::from(r[6]) << 8)
}

fn check_reply(r: &Report, cmd: u8) -> Result<()> {
    if r[0] != FLAG_REPLY || r[1] != cmd {
        bail!("unexpected reply header {:02x} {:02x}", r[0], r[1]);
    }
    let sum = checksum(r);
    if r[3] != sum {
        bail!("reply checksum {:02x}, expected {:02x}", r[3], sum);
    }
    Ok(())
}

/// The first `n` payload bytes of a reply, un-XORed with the reply's key byte.
fn reply_data(r: &Report, n: usize) -> Vec<u8> {
    let key = r[2];
    r[8..8 + n.min(CHUNK)].iter().map(|b| b ^ key).collect()
}

fn parse_info(r: &Report) -> FirmwareInfo {
    let d = reply_data(r, CHUNK);
    let text = |bytes: &[u8]| {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).trim().to_string()
    };
    if d[..3].iter().all(u8::is_ascii_alphabetic) {
        // Old firmware answers with the build text alone.
        return FirmwareInfo {
            version: "100".into(),
            built: text(&d),
        };
    }
    FirmwareInfo {
        version: format!("{:x}", u16::from(d[1]) << 8 | u16::from(d[0])),
        built: text(&d[2..]),
    }
}

/// Firmware 100..=109 keeps the mode byte of `GetBase` 8 bytes further in.
fn base_offset(firmware: &FirmwareInfo) -> usize {
    match firmware.number() {
        Some(100..=109) => 8,
        _ => 0,
    }
}

fn parse_base(data: &[u8], offset: usize) -> Result<Base> {
    if data.len() < offset + 2 {
        bail!("GetBase reply too short: {} bytes", data.len());
    }
    let base = Base {
        current_mode: data[offset],
        layers_per_mode: match data[offset + 1] {
            0 => LAYERS_PER_MODE,
            n => n,
        },
    };
    if base.current_mode >= MODES || base.layers_per_mode != LAYERS_PER_MODE {
        bail!("unexpected GetBase reply {data:02x?}");
    }
    Ok(base)
}

pub fn matrix_addr(mode: u8, layer: u8, slot: usize) -> u16 {
    ((usize::from(mode * LAYERS_PER_MODE + layer)) * LAYER_BYTES + slot * ACTION_LEN) as u16
}

pub fn travel_addr(mode: u8, slot: usize) -> u16 {
    (usize::from(mode) * TRAVEL_BYTES + slot * TRAVEL_RECORD) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_read_packet() {
        let p = build(CMD_GET_USE_MATRIX, 0x0810, 56, &[]).unwrap();
        assert_eq!(&p[..8], &[0x55, 8, 0, 56 + 0x10 + 0x08, 56, 0x10, 0x08, 0]);
        assert!(p[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn refuses_unknown_commands() {
        assert!(build(238, 0, 0, &[]).is_err());
        assert!(build(6, 0, 0, &[]).is_err());
        assert!(build(168, 0, 0, &[]).is_err());
    }

    #[test]
    fn guards_matrix_writes() {
        let a = matrix_addr(1, 0, 2);
        assert!(guard_write(CMD_SET_USE_MATRIX, a, &[0x10, 0, 0x29]).is_ok());
        assert!(guard_write(CMD_SET_USE_MATRIX, a + 1, &[0x10, 0, 0x29]).is_err());
        assert!(guard_write(CMD_SET_USE_MATRIX, a, &[0x10, 0]).is_err());
        assert!(guard_write(CMD_SET_USE_MATRIX, a, &[0xf0, 0x07, 0]).is_err());
        assert!(guard_write(CMD_SET_USE_MATRIX, matrix_addr(0, 0, 128), &[0x10, 0, 4]).is_err());
        assert!(guard_write(CMD_SET_USE_MATRIX, matrix_addr(2, 3, 127), &[0x10, 0, 4]).is_ok());
    }

    #[test]
    fn guards_travel_writes() {
        assert!(guard_write(CMD_SET_TRAVEL, travel_addr(2, 127), &[0; 8]).is_ok());
        assert!(guard_write(CMD_SET_TRAVEL, travel_addr(2, 127) + 8, &[0; 8]).is_err());
        assert!(guard_write(CMD_SET_TRAVEL, 4, &[0; 8]).is_err());
        assert!(guard_write(CMD_GET_TRAVEL, 0, &[0; 8]).is_err());
    }

    #[test]
    fn decodes_reply_data_with_key() {
        let mut r = [0u8; REPORT_LEN];
        r[0] = FLAG_REPLY;
        r[1] = CMD_GET_USE_MATRIX;
        r[2] = 0x5a;
        r[8] = 0x10 ^ 0x5a;
        r[9] = 0x5a;
        r[10] = 0x29 ^ 0x5a;
        r[3] = checksum(&r);
        assert!(check_reply(&r, CMD_GET_USE_MATRIX).is_ok());
        assert_eq!(reply_data(&r, 3), vec![0x10, 0, 0x29]);
    }

    #[test]
    fn parses_info_and_base() {
        let mut r = [0u8; REPORT_LEN];
        r[8] = 0x14;
        r[9] = 0x01;
        r[10..14].copy_from_slice(b"2025");
        let info = parse_info(&r);
        assert_eq!(info.version, "114");
        assert_eq!(info.built, "2025");
        assert_eq!(base_offset(&info), 0);

        let mut data = [0u8; 16];
        data[0] = 1;
        data[1] = 4;
        assert_eq!(
            parse_base(&data, 0).unwrap(),
            Base {
                current_mode: 1,
                layers_per_mode: 4
            }
        );
        assert!(parse_base(&[5, 4], 0).is_err());
    }

    #[test]
    fn addresses() {
        assert_eq!(matrix_addr(0, 0, 0), 0);
        assert_eq!(matrix_addr(1, 0, 1), 4 * 512 + 3);
        assert_eq!(matrix_addr(2, 3, 60), 11 * 512 + 180);
        assert_eq!(travel_addr(1, 2), 1024 + 16);
    }
}
