//! Key actions of the HE key matrix: three bytes `[kind, a, b]` per slot.
//!
//! - `10 mods hid`: a keyboard key, `mods` a modifier bitmask, `hid` a HID
//!   keyboard usage (`10 02 00` is Left Shift alone, `10 01 06` Ctrl+C).
//! - `30 lo hi`: a consumer-control usage (`30 e2 00` Mute).
//! - `70 n ff`: macro slot `n`, shown 1-based as NuPhy IO does (`MACRO(1)`).
//! - `f0 ff n`: momentary layer `n` (absolute layer number, 0..=11).
//! - `f0 f 00`: a keyboard function (lighting, sleep, mode switch, ...).
//! - Anything else (DKS, Toggle, Mod-Tap, Snap Tap, mouse, ...) is shown and
//!   accepted as raw hex `0xKKAABB`.

use crate::he::names::{Key, call_args, modifier_names, parse_key, parse_number, usage_name};
use crate::he::region::{LAYERS, MACRO_COUNT};
use anyhow::{Context, Result, bail};
use std::fmt;

const KIND_KEY: u8 = 0x10;
const KIND_CONSUMER: u8 = 0x30;
const KIND_MACRO: u8 = 0x70;
const KIND_FUNCTION: u8 = 0xf0;
const FN_LAYER: u8 = 0xff;
const MACRO_TAIL: u8 = 0xff;

/// The three bytes of a matrix slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Action(pub [u8; 3]);

/// What an action means, decoded once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    None,
    Key {
        mods: u8,
        usage: u8,
    },
    Consumer(u16),
    /// A macro slot, 0-based.
    Macro(u8),
    /// A momentary layer, absolute (0..=11).
    Layer(u8),
    Function(&'static Function),
    /// Anything without a meaning of its own here: DKS, Toggle, Mod-Tap,
    /// Snap Tap, mouse, unknown functions or out-of-range slots.
    Raw([u8; 3]),
}

/// A keyboard function (`f0 code 00`).
#[derive(Debug, PartialEq, Eq)]
pub struct Function {
    pub code: u8,
    pub name: &'static str,
    /// Whether `apply` may put it on a key.
    pub assignable: bool,
}

const FIRMWARE_LOADER: u8 = 0x07;
const FACTORY_RESET: u8 = 0x08;

macro_rules! f {
    ($code:expr, $name:expr, $assignable:expr) => {
        Function {
            code: $code,
            name: $name,
            assignable: $assignable,
        }
    };
}

static FUNCTIONS: &[Function] = &[
    f!(0x01, "KBD_LOCK", true),
    f!(0x02, "WIN_LOCK", true),
    f!(0x04, "WIN_MODE", false),
    f!(0x05, "MAC_MODE", false),
    f!(0x06, "WIN_MAC", false),
    f!(FIRMWARE_LOADER, "FIRMWARE_LOADER", false),
    f!(FACTORY_RESET, "FACTORY_RESET", false),
    f!(0x10, "SIDE_BRIGHTER", true),
    f!(0x11, "SIDE_DIMMER", true),
    f!(0x12, "SIDE_SLOWER", false),
    f!(0x13, "SIDE_FASTER", false),
    f!(0x15, "SIDE_COLOR", true),
    f!(0x16, "SIDE_EFFECT", true),
    f!(0x2e, "LIGHT_NEXT", true),
    f!(0x2f, "LIGHT_PREV", true),
    f!(0x30, "LIGHT_TOGGLE", true),
    f!(0x32, "LIGHT_BRIGHTER", true),
    f!(0x33, "LIGHT_DIMMER", true),
    f!(0x34, "LIGHT_LOOP", false),
    f!(0x35, "LIGHT_OFF", false),
    f!(0x36, "LIGHT_FASTER", true),
    f!(0x37, "LIGHT_SLOWER", true),
    f!(0x39, "LIGHT_LEFT", false),
    f!(0x3a, "LIGHT_RIGHT", false),
    f!(0x3c, "COLOR_NEXT", true),
    f!(0x3d, "COLOR_PREV", true),
    f!(0x3e, "COLOR_LOOP", true),
    f!(0x51, "MAC_SEARCH", false),
    f!(0x52, "SLEEP", true),
    f!(0x53, "SCREENSHOT_AREA", true),
    f!(0x54, "SCREENSHOT", true),
    f!(0x55, "MAC_SCREENSHOT_AREA", false),
    f!(0x56, "MAC_SCREENSHOT", false),
    f!(0xfa, "MODE1", true),
    f!(0xfb, "MODE2", true),
    f!(0xfc, "MODE3", true),
    f!(0xfe, "LAYER_SWITCH", false),
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

impl Action {
    pub const NONE: Self = Self([KIND_KEY, 0, 0]);

    pub fn kind(self) -> ActionKind {
        match self.0 {
            [KIND_KEY, 0, 0] => ActionKind::None,
            [KIND_KEY, mods, usage] if usage == 0 || usage_name(usage).is_some() => {
                ActionKind::Key { mods, usage }
            }
            [KIND_CONSUMER, lo, hi] => ActionKind::Consumer(u16::from_le_bytes([lo, hi])),
            [KIND_MACRO, slot, MACRO_TAIL] if usize::from(slot) < MACRO_COUNT => {
                ActionKind::Macro(slot)
            }
            [KIND_FUNCTION, FN_LAYER, layer] if layer < LAYERS => ActionKind::Layer(layer),
            [KIND_FUNCTION, code, 0] => match FUNCTIONS.iter().find(|f| f.code == code) {
                Some(f) => ActionKind::Function(f),
                None => ActionKind::Raw(self.0),
            },
            bytes => ActionKind::Raw(bytes),
        }
    }

    /// Actions `apply` may write: their meaning lives in these three bytes
    /// (a macro slot only points at the macro). DKS, Mod-Tap, Toggle and Snap
    /// Tap keys point into tables this tool does not edit.
    pub fn is_writable(self) -> bool {
        match self.kind() {
            ActionKind::Function(f) => f.assignable,
            ActionKind::Raw(_) => false,
            _ => true,
        }
    }

    /// Never written to the keyboard by any command, `restore-backup`
    /// included: the firmware loader and factory reset, whatever the third
    /// byte holds.
    pub fn is_forbidden(self) -> bool {
        matches!(self.0, [KIND_FUNCTION, FIRMWARE_LOADER | FACTORY_RESET, _])
    }

    /// The absolute layer of a momentary-layer action.
    pub fn layer(self) -> Option<u8> {
        match self.kind() {
            ActionKind::Layer(layer) => Some(layer),
            _ => None,
        }
    }
}

impl From<ActionKind> for Action {
    fn from(kind: ActionKind) -> Self {
        Self(match kind {
            ActionKind::None => [KIND_KEY, 0, 0],
            ActionKind::Key { mods, usage } => [KIND_KEY, mods, usage],
            ActionKind::Consumer(usage) => {
                let [lo, hi] = usage.to_le_bytes();
                [KIND_CONSUMER, lo, hi]
            }
            ActionKind::Macro(slot) => [KIND_MACRO, slot, MACRO_TAIL],
            ActionKind::Layer(layer) => [KIND_FUNCTION, FN_LAYER, layer],
            ActionKind::Function(f) => [KIND_FUNCTION, f.code, 0],
            ActionKind::Raw(bytes) => bytes,
        })
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            ActionKind::None => f.write_str("NO"),
            ActionKind::Key { mods, usage } => {
                let names: Vec<&str> = modifier_names(mods).chain(usage_name(usage)).collect();
                f.write_str(&names.join("+"))
            }
            ActionKind::Consumer(usage) => match CONSUMER.iter().find(|(u, _)| *u == usage) {
                Some((_, name)) => f.write_str(name),
                None => write!(f, "CONSUMER(0x{usage:03x})"),
            },
            ActionKind::Macro(slot) => write!(f, "MACRO({})", u16::from(slot) + 1),
            ActionKind::Layer(layer) => write!(f, "FN({layer})"),
            ActionKind::Function(func) => f.write_str(func.name),
            ActionKind::Raw([k, a, b]) => write!(f, "0x{k:02x}{a:02x}{b:02x}"),
        }
    }
}

fn parse_kind(s: &str) -> Result<ActionKind> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if hex.len() != 6 {
            bail!("raw action {s:?} must be 6 hex digits, like 0x10002c");
        }
        let v =
            u32::from_str_radix(hex, 16).with_context(|| format!("invalid raw action {s:?}"))?;
        return Ok(Action([(v >> 16) as u8, (v >> 8) as u8, v as u8]).kind());
    }
    if let Some(n) = call_args(s, "FN") {
        let layer = parse_number(n)?;
        if layer >= u32::from(LAYERS) {
            bail!("layer {layer} out of range in {s:?} (0..=11)");
        }
        return Ok(ActionKind::Layer(layer as u8));
    }
    if let Some(n) = call_args(s, "MACRO") {
        let number = parse_number(n)?;
        if !(1..=MACRO_COUNT as u32).contains(&number) {
            bail!("macro number out of range in {s:?} (1..={MACRO_COUNT})");
        }
        return Ok(ActionKind::Macro((number - 1) as u8));
    }
    if let Some(n) = call_args(s, "CONSUMER") {
        let usage = u16::try_from(parse_number(n)?)
            .ok()
            .with_context(|| format!("consumer usage out of range in {s:?}"))?;
        return Ok(ActionKind::Consumer(usage));
    }
    let upper = s.to_ascii_uppercase();
    if matches!(upper.as_str(), "NO" | "NONE" | "XXXXXXX") {
        return Ok(ActionKind::None);
    }
    if let Some((usage, _)) = CONSUMER.iter().find(|(_, n)| *n == upper) {
        return Ok(ActionKind::Consumer(*usage));
    }
    if let Some(func) = FUNCTIONS.iter().find(|f| f.name == upper) {
        return Ok(ActionKind::Function(func));
    }
    let mut mods = 0u8;
    let mut usage = 0u8;
    for part in s.split('+') {
        match parse_key(part).with_context(|| format!("in {s:?}"))? {
            Key::Modifier(bit) => mods |= bit,
            Key::Usage(_) if usage != 0 => bail!("{s:?}: at most one key besides modifiers"),
            Key::Usage(u) => usage = u,
        }
    }
    Ok(ActionKind::Key { mods, usage })
}

impl std::str::FromStr for Action {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let s = input.trim();
        if s.is_empty() {
            bail!("empty action");
        }
        Ok(parse_kind(s)?.into())
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
        assert_eq!(a([0x70, 3, 0xff]).to_string(), "MACRO(4)");
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
        assert_eq!("macro(1)".parse::<Action>().unwrap(), a([0x70, 0, 0xff]));
        assert_eq!("MACRO(32)".parse::<Action>().unwrap(), a([0x70, 31, 0xff]));
        assert_eq!("0x950020".parse::<Action>().unwrap(), a([0x95, 0, 0x20]));
        assert_eq!("NO".parse::<Action>().unwrap(), Action::NONE);
        for bad in [
            "FN(12)",
            "A+B",
            "NOPE",
            "0x1234",
            "MACRO(0)",
            "MACRO(33)",
            "",
            "VOLU+A",
        ] {
            assert!(bad.parse::<Action>().is_err(), "{bad:?}");
        }
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
            [0xf0, 0xff, 12],
            [0x70, 0xff, 0xff],
            [0x70, 32, 0xff],
            [0xf0, 0x52, 7],
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
    fn writable_and_forbidden() {
        for w in [
            [0x10, 0, 4],
            [0x30, 0xe2, 0],
            [0xf0, 0xff, 3],
            [0xf0, 0x52, 0],
            [0x70, 0, 0xff],
        ] {
            assert!(a(w).is_writable(), "{w:02x?}");
        }
        for nw in [
            [0xf0, 0xff, 12],
            [0xf0, 0x07, 0],
            [0xf0, 0x08, 0],
            [0x95, 0, 0x20],
            [0x70, 32, 0xff],
            [0xf0, 0x52, 7],
        ] {
            assert!(!a(nw).is_writable(), "{nw:02x?}");
        }
        assert!(a([0xf0, 0x07, 0]).is_forbidden());
        assert!(a([0xf0, 0x08, 5]).is_forbidden());
        assert!(!a([0xf0, 0x52, 0]).is_forbidden());
        assert_eq!(a([0xf0, 0xff, 9]).layer(), Some(9));
        assert_eq!(a([0x10, 0, 4]).layer(), None);
    }
}
