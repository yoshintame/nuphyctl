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

use crate::he::region::{CHUNK, LAYERS_PER_MODE, MODES, Region, WriteChunk};
use crate::hid_transport::{HidResponder, REPORT_LEN, Report};
use anyhow::{Context, Result, bail};
use hidapi::HidDevice;
use std::fmt;
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(1000);
const ATTEMPTS: usize = 3;
const BASE_BYTES: usize = 16;

const FLAG_HOST: u8 = 0x55;
const FLAG_REPLY: u8 = 0xaa;

const CMD_GET_INFO: u8 = 3;
const CMD_GET_BASE: u8 = 4;
const CMD_GET_DEFAULT_MATRIX: u8 = 7;

/// Commands this tool may ever send: reads, and the writes of the regions in
/// the memory map. Factory reset, calibration, mode settings (whose byte 0
/// enters the firmware loader) and the loader itself are refused before a
/// packet is built.
fn is_allowed(cmd: u8) -> bool {
    [CMD_GET_INFO, CMD_GET_BASE, CMD_GET_DEFAULT_MATRIX].contains(&cmd)
        || Region::ALL
            .iter()
            .any(|r| r.get_cmd() == cmd || r.set_cmd() == cmd)
}

/// Firmware version: two bytes read as hexadecimal digits (`20 01` is 120).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirmwareVersion(pub u16);

impl FirmwareVersion {
    /// Versions the write path was checked against.
    const WRITES: std::ops::RangeInclusive<u32> = 110..=139;

    /// The digits as a decimal number, if every one is 0..=9.
    pub fn number(self) -> Option<u32> {
        (0..4).rev().try_fold(0u32, |acc, i| {
            let digit = u32::from((self.0 >> (4 * i)) & 0xf);
            (digit < 10).then_some(acc * 10 + digit)
        })
    }

    pub fn writes_supported(self) -> bool {
        self.number().is_some_and(|n| Self::WRITES.contains(&n))
    }

    pub fn check_writes(self) -> Result<()> {
        if !self.writes_supported() {
            bail!(
                "refusing to write: firmware {self} is outside the checked range {}..={}",
                Self::WRITES.start(),
                Self::WRITES.end()
            );
        }
        Ok(())
    }
}

impl fmt::Display for FirmwareVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareInfo {
    pub version: FirmwareVersion,
    pub built: String,
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
        parse_base(&data, base_offset(firmware.version))
    }

    pub fn read_region(&self, region: Region) -> Result<Vec<u8>> {
        self.read(region.get_cmd(), 0, region.len())
            .with_context(|| format!("failed to read the {}", region.name()))
    }

    pub fn read_factory_matrix(&self) -> Result<Vec<u8>> {
        self.read(CMD_GET_DEFAULT_MATRIX, 0, Region::Matrix.len())
            .context("failed to read the factory key matrix")
    }

    pub fn write(&self, region: Region, chunk: &WriteChunk) -> Result<()> {
        region.check_write(chunk.addr, &chunk.data)?;
        self.transact(
            region.set_cmd(),
            chunk.addr,
            chunk.data.len() as u8,
            &chunk.data,
        )
        .with_context(|| format!("{} write at {} failed", region.name(), chunk.addr))?;
        Ok(())
    }

    /// Read `size` bytes from `addr`, in packet-sized chunks.
    fn read(&self, cmd: u8, addr: u16, size: usize) -> Result<Vec<u8>> {
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
    if !is_allowed(cmd) {
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
            version: FirmwareVersion(0x100),
            built: text(&d),
        };
    }
    FirmwareInfo {
        version: FirmwareVersion(u16::from(d[1]) << 8 | u16::from(d[0])),
        built: text(&d[2..]),
    }
}

/// Firmware 100..=109 keeps the mode byte of `GetBase` 8 bytes further in.
fn base_offset(version: FirmwareVersion) -> usize {
    match version.number() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_read_packet() {
        let p = build(8, 0x0810, 56, &[]).unwrap();
        assert_eq!(&p[..8], &[0x55, 8, 0, 56 + 0x10 + 0x08, 56, 0x10, 0x08, 0]);
        assert!(p[8..].iter().all(|&b| b == 0));
    }

    #[test]
    fn refuses_unknown_commands() {
        for cmd in [238, 6, 168, 169, 242] {
            assert!(build(cmd, 0, 0, &[]).is_err(), "cmd {cmd}");
        }
        for cmd in [3, 4, 7, 8, 9, 12, 13, 160, 161] {
            assert!(build(cmd, 0, 0, &[]).is_ok(), "cmd {cmd}");
        }
    }

    #[test]
    fn decodes_reply_data_with_key() {
        let mut r = [0u8; REPORT_LEN];
        r[0] = FLAG_REPLY;
        r[1] = 8;
        r[2] = 0x5a;
        r[8] = 0x10 ^ 0x5a;
        r[9] = 0x5a;
        r[10] = 0x29 ^ 0x5a;
        r[3] = checksum(&r);
        assert!(check_reply(&r, 8).is_ok());
        assert_eq!(reply_data(&r, 3), vec![0x10, 0, 0x29]);
    }

    #[test]
    fn parses_info_and_base() {
        let mut r = [0u8; REPORT_LEN];
        r[8] = 0x20;
        r[9] = 0x01;
        r[10..30].copy_from_slice(b"Jan 27 2026,10:56:00");
        let info = parse_info(&r);
        assert_eq!(info.version, FirmwareVersion(0x120));
        assert_eq!(info.version.to_string(), "120");
        assert_eq!(info.built, "Jan 27 2026,10:56:00");
        assert_eq!(base_offset(info.version), 0);

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
    fn firmware_versions() {
        assert_eq!(FirmwareVersion(0x120).number(), Some(120));
        assert!(FirmwareVersion(0x120).writes_supported());
        assert!(!FirmwareVersion(0x109).writes_supported());
        assert!(!FirmwareVersion(0x140).writes_supported());
        assert_eq!(FirmwareVersion(0x10a).number(), None);
        assert!(FirmwareVersion(0x10a).check_writes().is_err());
        assert_eq!(base_offset(FirmwareVersion(0x105)), 8);
    }
}
