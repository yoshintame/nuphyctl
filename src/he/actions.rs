//! Key actions of the HE key matrix: three bytes `[kind, a, b]` per slot.
//!
//! - `10 mods hid`: a keyboard key, `mods` a modifier bitmask, `hid` a HID
//!   keyboard usage (`10 02 00` is Left Shift alone, `10 01 06` Ctrl+C).
//! - `30 lo hi`: a consumer-control usage (`30 e2 00` Mute).
//! - `70 n ff`: macro slot `n`.
//! - `f0 ff n`: momentary layer `n` (absolute layer number, 0..=11).
//! - `f0 f 00`: a keyboard function (lighting, sleep, mode switch, ...).
//! - Anything else (DKS, Toggle, Mod-Tap, Snap Tap, mouse, ...) is shown and
//!   accepted as raw hex `0xKKAABB`.

use crate::keycodes::{KeycodeGroup, lookup_code, lookup_name};
use anyhow::{Context, Result, bail};
use std::fmt;

pub const KIND_KEY: u8 = 0x10;
pub const KIND_CONSUMER: u8 = 0x30;
pub const KIND_MACRO: u8 = 0x70;
pub const KIND_FUNCTION: u8 = 0xf0;
const FN_LAYER: u8 = 0xff;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Action(pub [u8; 3]);

impl Action {
    pub const NONE: Self = Self([KIND_KEY, 0, 0]);

    /// Actions this tool may write: their meaning lives in these three bytes
    /// (a macro slot only points at the macro). DKS, Mod-Tap, Toggle and Snap
    /// Tap keys point into tables this tool does not edit, and factory reset
    /// or the firmware loader are never assigned.
    pub fn is_writable(self) -> bool {
        match self.0 {
            [KIND_KEY | KIND_CONSUMER, ..] => true,
            [KIND_MACRO, _, 0xff] => true,
            [KIND_FUNCTION, FN_LAYER, layer] => layer < crate::he::protocol::LAYERS,
            [KIND_FUNCTION, f, _] => FUNCTIONS.iter().any(|x| x.0 == f && x.2),
            _ => false,
        }
    }
}

const MODIFIERS: &[(u8, &str)] = &[
    (0x01, "LCTL"),
    (0x02, "LSFT"),
    (0x04, "LALT"),
    (0x08, "LGUI"),
    (0x10, "RCTL"),
    (0x20, "RSFT"),
    (0x40, "RALT"),
    (0x80, "RGUI"),
];

/// Consumer usages offered by NuPhy IO.
const CONSUMER: &[(u16, &str)] = &[
    (0x00cd, "MPLY"),
    (0x00b5, "MNXT"),
    (0x00b6, "MPRV"),
    (0x00b7, "MSTP"),
    (0x00e2, "MUTE"),
    (0x00e9, "VOLU"),
    (0x00ea, "VOLD"),
    (0x006f, "BRIU"),
    (0x0070, "BRID"),
    (0x0192, "CALC"),
    (0x018a, "MAIL"),
    (0x0194, "MYCM"),
    (0x0183, "MSEL"),
    (0x0223, "WWW_HOME"),
    (0x0224, "WWW_BACK"),
    (0x0225, "WWW_FORWARD"),
    (0x0227, "WWW_REFRESH"),
    (0x0221, "WWW_SEARCH"),
    (0x029f, "MISSION_CONTROL"),
    (0x02a0, "LAUNCHPAD"),
    (0x00cf, "SIRI"),
];

/// Keyboard functions (`f0 f 00`): code, name, safe to assign. Factory reset
/// and the firmware loader are never assigned by this tool.
const FUNCTIONS: &[(u8, &str, bool)] = &[
    (0x01, "KBD_LOCK", true),
    (0x02, "WIN_LOCK", true),
    (0x04, "WIN_MODE", false),
    (0x05, "MAC_MODE", false),
    (0x06, "WIN_MAC", false),
    (0x07, "FIRMWARE_LOADER", false),
    (0x08, "FACTORY_RESET", false),
    (0x10, "SIDE_BRIGHTER", true),
    (0x11, "SIDE_DIMMER", true),
    (0x12, "SIDE_SLOWER", false),
    (0x13, "SIDE_FASTER", false),
    (0x15, "SIDE_COLOR", true),
    (0x16, "SIDE_EFFECT", true),
    (0x2e, "LIGHT_NEXT", true),
    (0x2f, "LIGHT_PREV", true),
    (0x30, "LIGHT_TOGGLE", true),
    (0x32, "LIGHT_BRIGHTER", true),
    (0x33, "LIGHT_DIMMER", true),
    (0x34, "LIGHT_LOOP", false),
    (0x35, "LIGHT_OFF", false),
    (0x36, "LIGHT_FASTER", true),
    (0x37, "LIGHT_SLOWER", true),
    (0x39, "LIGHT_LEFT", false),
    (0x3a, "LIGHT_RIGHT", false),
    (0x3c, "COLOR_NEXT", true),
    (0x3d, "COLOR_PREV", true),
    (0x3e, "COLOR_LOOP", true),
    (0x51, "MAC_SEARCH", false),
    (0x52, "SLEEP", true),
    (0x53, "SCREENSHOT_AREA", true),
    (0x54, "SCREENSHOT", true),
    (0x55, "MAC_SCREENSHOT_AREA", false),
    (0x56, "MAC_SCREENSHOT", false),
    (0xfa, "MODE1", true),
    (0xfb, "MODE2", true),
    (0xfc, "MODE3", true),
    (0xfe, "LAYER_SWITCH", false),
];

fn hid_name(hid: u8) -> Option<&'static str> {
    lookup_code(u16::from(hid))
        .filter(|k| matches!(k.group, KeycodeGroup::Basic))
        .map(|k| k.name)
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [kind, a, b] = self.0;
        match kind {
            KIND_KEY if a == 0 && b == 0 => f.write_str("NO"),
            KIND_KEY if b == 0 || hid_name(b).is_some() => {
                let mut parts: Vec<&str> = MODIFIERS
                    .iter()
                    .filter(|(bit, _)| a & bit != 0)
                    .map(|(_, n)| *n)
                    .collect();
                if let Some(name) = hid_name(b) {
                    parts.push(name);
                }
                f.write_str(&parts.join("+"))
            }
            KIND_CONSUMER => {
                let usage = u16::from(a) | (u16::from(b) << 8);
                match CONSUMER.iter().find(|(u, _)| *u == usage) {
                    Some((_, name)) => f.write_str(name),
                    None => write!(f, "CONSUMER(0x{usage:03x})"),
                }
            }
            KIND_MACRO if b == 0xff => write!(f, "MACRO({a})"),
            KIND_FUNCTION if a == FN_LAYER => write!(f, "FN({b})"),
            KIND_FUNCTION if b == 0 => match FUNCTIONS.iter().find(|x| x.0 == a) {
                Some((_, name, _)) => f.write_str(name),
                None => write!(f, "0x{kind:02x}{a:02x}{b:02x}"),
            },
            _ => write!(f, "0x{kind:02x}{a:02x}{b:02x}"),
        }
    }
}

fn call<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input
        .get(..name.len())?
        .eq_ignore_ascii_case(name)
        .then(|| &input[name.len()..])?;
    rest.strip_prefix('(')?.strip_suffix(')').map(str::trim)
}

fn parse_number(input: &str) -> Result<u32> {
    let parsed = match input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => input.parse(),
    };
    parsed.with_context(|| format!("invalid number {input:?}"))
}

impl std::str::FromStr for Action {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let s = input.trim();
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            if hex.len() != 6 {
                bail!("raw action {s:?} must be 6 hex digits, like 0x10002c");
            }
            let v = u32::from_str_radix(hex, 16)
                .with_context(|| format!("invalid raw action {s:?}"))?;
            return Ok(Self([(v >> 16) as u8, (v >> 8) as u8, v as u8]));
        }
        if let Some(n) = call(s, "FN") {
            let layer = parse_number(n)?;
            if layer >= u32::from(crate::he::protocol::LAYERS) {
                bail!("layer {layer} out of range in {s:?} (0..=11)");
            }
            return Ok(Self([KIND_FUNCTION, FN_LAYER, layer as u8]));
        }
        if let Some(n) = call(s, "MACRO") {
            let slot = parse_number(n)?;
            let slot = u8::try_from(slot).ok().filter(|&n| n < 0xff);
            let slot = slot.with_context(|| format!("macro slot out of range in {s:?}"))?;
            return Ok(Self([KIND_MACRO, slot, 0xff]));
        }
        if let Some(n) = call(s, "CONSUMER") {
            let usage = u16::try_from(parse_number(n)?)
                .ok()
                .with_context(|| format!("consumer usage out of range in {s:?}"))?;
            return Ok(Self([KIND_CONSUMER, usage as u8, (usage >> 8) as u8]));
        }
        let upper = s.to_ascii_uppercase();
        if matches!(upper.as_str(), "NO" | "NONE" | "XXXXXXX") {
            return Ok(Self::NONE);
        }
        if let Some((usage, _)) = CONSUMER.iter().find(|(_, n)| *n == upper) {
            return Ok(Self([KIND_CONSUMER, *usage as u8, (*usage >> 8) as u8]));
        }
        if let Some((f, _, _)) = FUNCTIONS.iter().find(|x| x.1 == upper) {
            return Ok(Self([KIND_FUNCTION, *f, 0]));
        }

        let mut mods = 0u8;
        let mut hid = 0u8;
        for part in upper.split('+').map(str::trim) {
            let part = part.strip_prefix("KC_").unwrap_or(part);
            if let Some((bit, _)) = MODIFIERS.iter().find(|(_, n)| *n == part) {
                mods |= bit;
                continue;
            }
            let k = lookup_name(part)
                .filter(|k| k.code <= 0xff)
                .with_context(|| format!("unknown key {part:?} in {s:?}"))?;
            if (0xe0..=0xe7).contains(&k.code) {
                mods |= 1 << (k.code - 0xe0);
                continue;
            }
            if !matches!(k.group, KeycodeGroup::Basic) || hid != 0 {
                bail!("{s:?}: at most one key besides modifiers");
            }
            hid = k.code as u8;
        }
        if mods == 0 && hid == 0 {
            bail!("empty action {s:?}");
        }
        Ok(Self([KIND_KEY, mods, hid]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(bytes: [u8; 3]) -> Action {
        Action(bytes)
    }

    #[test]
    fn names_actions_from_the_air60_he() {
        assert_eq!(a([0x10, 0, 0x29]).to_string(), "ESC");
        assert_eq!(a([0x10, 0x02, 0]).to_string(), "LSFT");
        assert_eq!(a([0x10, 0x40, 0]).to_string(), "RALT");
        assert_eq!(a([0x10, 0x01, 0x06]).to_string(), "LCTL+C");
        assert_eq!(a([0x10, 0, 0]).to_string(), "NO");
        assert_eq!(a([0x10, 0, 0x65]).to_string(), "APP");
        assert_eq!(a([0x30, 0xe2, 0]).to_string(), "MUTE");
        assert_eq!(a([0x30, 0x9f, 0x02]).to_string(), "MISSION_CONTROL");
        assert_eq!(a([0x70, 3, 0xff]).to_string(), "MACRO(3)");
        assert_eq!(a([0xf0, 0xff, 2]).to_string(), "FN(2)");
        assert_eq!(a([0xf0, 0x52, 0]).to_string(), "SLEEP");
        assert_eq!(a([0xf0, 0x08, 0]).to_string(), "FACTORY_RESET");
        assert_eq!(a([0x95, 0x00, 0x20]).to_string(), "0x950020");
    }

    #[test]
    fn parses_names() {
        assert_eq!("esc".parse::<Action>().unwrap(), a([0x10, 0, 0x29]));
        assert_eq!("LSFT".parse::<Action>().unwrap(), a([0x10, 0x02, 0]));
        assert_eq!("LCTL+C".parse::<Action>().unwrap(), a([0x10, 0x01, 0x06]));
        assert_eq!("kc_lshift".parse::<Action>().unwrap(), a([0x10, 0x02, 0]));
        assert_eq!("VOLU".parse::<Action>().unwrap(), a([0x30, 0xe9, 0]));
        assert_eq!("FN(9)".parse::<Action>().unwrap(), a([0xf0, 0xff, 9]));
        assert_eq!("macro(0)".parse::<Action>().unwrap(), a([0x70, 0, 0xff]));
        assert_eq!("0x950020".parse::<Action>().unwrap(), a([0x95, 0, 0x20]));
        assert_eq!("NO".parse::<Action>().unwrap(), Action::NONE);
        assert!("FN(12)".parse::<Action>().is_err());
        assert!("A+B".parse::<Action>().is_err());
        assert!("NOPE".parse::<Action>().is_err());
        assert!("0x1234".parse::<Action>().is_err());
    }

    #[test]
    fn display_round_trips() {
        for bytes in [
            [0x10, 0, 0x29],
            [0x10, 0x05, 0x04],
            [0x10, 0x80, 0],
            [0x30, 0xcd, 0],
            [0x30, 0x34, 0x12],
            [0x70, 7, 0xff],
            [0xf0, 0xff, 11],
            [0xf0, 0xfe, 0],
            [0x92, 1, 2],
            [0x10, 0, 0xfe],
        ] {
            let action = a(bytes);
            assert_eq!(
                action.to_string().parse::<Action>().unwrap(),
                action,
                "{action}"
            );
        }
    }

    #[test]
    fn writable_kinds() {
        assert!(a([0x10, 0, 4]).is_writable());
        assert!(a([0x30, 0xe2, 0]).is_writable());
        assert!(a([0xf0, 0xff, 3]).is_writable());
        assert!(a([0xf0, 0x52, 0]).is_writable());
        assert!(a([0x70, 0, 0xff]).is_writable());
        assert!(!a([0xf0, 0xff, 12]).is_writable());
        assert!(!a([0xf0, 0x07, 0]).is_writable());
        assert!(!a([0xf0, 0x08, 0]).is_writable());
        assert!(!a([0x95, 0, 0x20]).is_writable());
    }
}
