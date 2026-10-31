//! Key actions of the HE key matrix: three bytes `[kind, a, b]` per slot.
//!
//! - `10 mods usage`: a keyboard key, `mods` a modifier bitmask, `usage` a HID
//!   keyboard usage (`10 02 00` is Left Shift alone, `10 01 06` Ctrl+C).
//! - `30 lo hi`: a consumer-control usage (`30 e2 00` Mute).
//! - `70 n r`: macro slot `n`, shown 1-based as NuPhy IO does, with its loop
//!   mode `r`: `01` plays once (`MACRO(1)`), `00` repeats while the key is
//!   held (`MACRO(1, hold)`), `ff` repeats until the key is pressed again
//!   (`MACRO(1, toggle)`, NuPhy IO's "Press key to stop"). Other values do
//!   not play a matching number of times and stay raw.
//! - `f0 ff n`: momentary layer `n` (absolute layer number, 0..=11).
//! - `f0 f 00`: a keyboard function (lighting, sleep, mode switch, ...).
//! - `90`..`95`: DKS, Toggle, Mod-Tap and Snap Tap keys, which point into
//!   tables of their own.
//! - Anything without a name is shown and accepted as raw hex `0xKKAABB`.

use crate::he::names::{call_args, modifier_names, parse_chord, parse_number, usage_name};
use crate::he::region::{LAYERS, MACRO_COUNT};
use anyhow::{Context, Result, bail};
use std::fmt;

const KIND_KEY: u8 = 0x10;
const KIND_CONSUMER: u8 = 0x30;
const KIND_MACRO: u8 = 0x70;
const KIND_FUNCTION: u8 = 0xf0;
const LINKED_KINDS: std::ops::RangeInclusive<u8> = 0x90..=0x95;
const FN_LAYER: u8 = 0xff;
const MACRO_ONCE: u8 = 0x01;
const MACRO_HOLD: u8 = 0x00;
const MACRO_TOGGLE: u8 = 0xff;
const FIRMWARE_LOADER: u8 = 0x07;
const FACTORY_RESET: u8 = 0x08;

/// The three bytes of a matrix slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action(pub [u8; 3]);

/// What an action means, decoded by its kind byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    None,
    Key {
        mods: u8,
        usage: u8,
    },
    Consumer(u16),
    /// A macro slot, 0-based, and how it loops.
    Macro {
        slot: u8,
        mode: MacroMode,
    },
    /// A momentary layer, absolute (0..=11).
    Layer(u8),
    /// A keyboard function by code, named or not.
    Function(u8),
    /// DKS, Toggle, Mod-Tap or Snap Tap: points into a table of its own.
    Linked([u8; 3]),
    /// A kind this tool does not know (mouse, system, out-of-range slots, ...).
    Unknown([u8; 3]),
}

/// What `apply` and `restore-backup` may do with an action. Each step down
/// allows less; whatever `apply` may replace, `restore-backup` may write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Assigned and replaced.
    Free,
    /// Replaced, but not assigned.
    ReplaceOnly(&'static str),
    /// Neither assigned nor replaced; `restore-backup` still writes it back.
    Kept(&'static str),
    /// The firmware loader and factory reset, whatever the third byte holds:
    /// never written by any command, so never replaced either.
    Frozen(&'static str),
}

impl Policy {
    fn reason(self) -> Option<&'static str> {
        match self {
            Self::Free => None,
            Self::ReplaceOnly(why) | Self::Kept(why) | Self::Frozen(why) => Some(why),
        }
    }
}

/// How a macro key plays its macro, checked on an Air60 HE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroMode {
    Once,
    /// Repeats while the key is held.
    Hold,
    /// Repeats until the key is pressed again. Stopping it can cut a macro
    /// off half way and leave a modifier held.
    Toggle,
}

impl MacroMode {
    const ALL: [Self; 3] = [Self::Once, Self::Hold, Self::Toggle];

    fn byte(self) -> u8 {
        match self {
            Self::Once => MACRO_ONCE,
            Self::Hold => MACRO_HOLD,
            Self::Toggle => MACRO_TOGGLE,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.byte() == byte)
    }

    /// The name after the macro number; `MACRO(n)` alone plays once.
    fn name(self) -> Option<&'static str> {
        match self {
            Self::Once => None,
            Self::Hold => Some("hold"),
            Self::Toggle => Some("toggle"),
        }
    }

    fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.eq_ignore_ascii_case("once") {
            return Ok(Self::Once);
        }
        Self::ALL
            .into_iter()
            .find(|m| m.name().is_some_and(|n| n.eq_ignore_ascii_case(input)))
            .with_context(|| format!("macro mode {input:?} is not once, hold or toggle"))
    }
}

/// A keyboard function (`f0 code 00`).
#[derive(Debug, PartialEq, Eq)]
pub struct Function {
    pub code: u8,
    pub name: &'static str,
    /// Whether `apply` may put it on a key.
    pub assignable: bool,
}

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

fn function(code: u8) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|f| f.code == code)
}

impl Action {
    pub const NONE: Self = Self([KIND_KEY, 0, 0]);

    pub fn kind(self) -> ActionKind {
        match self.0 {
            [KIND_KEY, 0, 0] => ActionKind::None,
            [KIND_KEY, mods, usage] => ActionKind::Key { mods, usage },
            [KIND_CONSUMER, lo, hi] => ActionKind::Consumer(u16::from_le_bytes([lo, hi])),
            [KIND_MACRO, slot, mode] if usize::from(slot) < MACRO_COUNT => {
                match MacroMode::from_byte(mode) {
                    Some(mode) => ActionKind::Macro { slot, mode },
                    None => ActionKind::Unknown(self.0),
                }
            }
            [KIND_FUNCTION, FN_LAYER, layer] if layer < LAYERS => ActionKind::Layer(layer),
            [KIND_FUNCTION, code, 0] if code != FN_LAYER => ActionKind::Function(code),
            bytes @ [kind, ..] if LINKED_KINDS.contains(&kind) => ActionKind::Linked(bytes),
            bytes => ActionKind::Unknown(bytes),
        }
    }

    /// What `apply` and `restore-backup` may do with this action.
    pub fn policy(self) -> Policy {
        if matches!(self.0, [KIND_FUNCTION, FIRMWARE_LOADER | FACTORY_RESET, _]) {
            return Policy::Frozen("no command writes it, so restore-backup could not put it back");
        }
        match self.kind() {
            ActionKind::Function(code) if !function(code).is_some_and(|f| f.assignable) => {
                Policy::ReplaceOnly("this tool does not assign this keyboard function")
            }
            ActionKind::Linked(_) => Policy::Kept(
                "it points into a DKS, Toggle, Mod-Tap or Snap Tap table this tool does not edit",
            ),
            ActionKind::Unknown(_) => Policy::Kept("this tool does not know what it does"),
            _ => Policy::Free,
        }
    }

    /// Why `apply` may not put this action on a key, if it may not.
    pub fn assign_refusal(self) -> Option<&'static str> {
        self.policy().reason()
    }

    /// Why `apply` may not replace this action with another, if it may not.
    pub fn replace_refusal(self) -> Option<&'static str> {
        match self.policy() {
            Policy::ReplaceOnly(_) => None,
            policy => policy.reason(),
        }
    }

    /// Never written to the keyboard by any command, `restore-backup`
    /// included.
    pub fn never_write(self) -> bool {
        matches!(self.policy(), Policy::Frozen(_))
    }

    /// The absolute layer of a momentary-layer action.
    pub fn layer(self) -> Option<u8> {
        match self.kind() {
            ActionKind::Layer(layer) => Some(layer),
            _ => None,
        }
    }

    /// The raw literal `0xKKAABB`, as configs accept it.
    pub fn raw(self) -> String {
        let [k, a, b] = self.0;
        format!("0x{k:02x}{a:02x}{b:02x}")
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            ActionKind::None => f.write_str("NO"),
            ActionKind::Key { mods, usage } if usage == 0 || usage_name(usage).is_some() => {
                let names: Vec<&str> = modifier_names(mods).chain(usage_name(usage)).collect();
                f.write_str(&names.join("+"))
            }
            ActionKind::Consumer(usage) => match CONSUMER.iter().find(|(u, _)| *u == usage) {
                Some((_, name)) => f.write_str(name),
                None => write!(f, "CONSUMER(0x{usage:03x})"),
            },
            ActionKind::Macro { slot, mode } => {
                write!(f, "MACRO({}", u16::from(slot) + 1)?;
                if let Some(name) = mode.name() {
                    write!(f, ", {name}")?;
                }
                f.write_str(")")
            }
            ActionKind::Layer(layer) => write!(f, "FN({layer})"),
            ActionKind::Function(code) => match function(code) {
                Some(func) => f.write_str(func.name),
                None => f.write_str(&self.raw()),
            },
            _ => f.write_str(&self.raw()),
        }
    }
}

impl std::str::FromStr for Action {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self> {
        let s = input.trim();
        if s.is_empty() {
            bail!("empty action");
        }
        if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            if hex.len() != 6 {
                bail!("raw action {s:?} must be 6 hex digits, like 0x10002c");
            }
            let v = u32::from_str_radix(hex, 16)
                .with_context(|| format!("invalid raw action {s:?}"))?;
            return Ok(Self([(v >> 16) as u8, (v >> 8) as u8, v as u8]));
        }
        if let Some(n) = call_args(s, "FN") {
            let layer = parse_number(n)?;
            if layer >= u32::from(LAYERS) {
                bail!("layer {layer} out of range in {s:?} (0..=11)");
            }
            return Ok(Self([KIND_FUNCTION, FN_LAYER, layer as u8]));
        }
        if let Some(args) = call_args(s, "MACRO") {
            let (n, mode) = match args.split_once(',') {
                Some((n, mode)) => (n, MacroMode::parse(mode)?),
                None => (args, MacroMode::Once),
            };
            let number = parse_number(n)?;
            if !(1..=MACRO_COUNT as u32).contains(&number) {
                bail!("macro number out of range in {s:?} (1..={MACRO_COUNT})");
            }
            return Ok(Self([KIND_MACRO, (number - 1) as u8, mode.byte()]));
        }
        if let Some(n) = call_args(s, "CONSUMER") {
            let usage = u16::try_from(parse_number(n)?)
                .ok()
                .with_context(|| format!("consumer usage out of range in {s:?}"))?;
            let [lo, hi] = usage.to_le_bytes();
            return Ok(Self([KIND_CONSUMER, lo, hi]));
        }
        let upper = s.to_ascii_uppercase();
        if matches!(upper.as_str(), "NO" | "NONE" | "XXXXXXX") {
            return Ok(Self::NONE);
        }
        if let Some((usage, _)) = CONSUMER.iter().find(|(_, n)| *n == upper) {
            let [lo, hi] = usage.to_le_bytes();
            return Ok(Self([KIND_CONSUMER, lo, hi]));
        }
        if let Some(func) = FUNCTIONS.iter().find(|f| f.name == upper) {
            return Ok(Self([KIND_FUNCTION, func.code, 0]));
        }
        let chord = parse_chord(s)?;
        Ok(Self([
            KIND_KEY,
            chord.modifier_mask(),
            chord.usage.unwrap_or(0),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::he::region::{Region, matrix_offset};

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
        assert_eq!(a([0x70, 3, 0x01]).to_string(), "MACRO(4)");
        assert_eq!(a([0x70, 3, 0x00]).to_string(), "MACRO(4, hold)");
        assert_eq!(a([0x70, 3, 0xff]).to_string(), "MACRO(4, toggle)");
        assert_eq!(a([0x70, 3, 0x02]).to_string(), "0x700302");
        assert_eq!(a([0xf0, 0xff, 2]).to_string(), "FN(2)");
        assert_eq!(a([0xf0, 0x52, 0]).to_string(), "SLEEP");
        assert_eq!(a([0xf0, 0x08, 0]).to_string(), "FACTORY_RESET");
        assert_eq!(a([0x95, 0x00, 0x20]).to_string(), "0x950020");
        assert_eq!(a([0x10, 0, 0xc0]).to_string(), "0x1000c0");
    }

    #[test]
    fn parses_names() {
        assert_eq!("esc".parse::<Action>().unwrap(), a([0x10, 0, 0x29]));
        assert_eq!("LSFT".parse::<Action>().unwrap(), a([0x10, 0x02, 0]));
        assert_eq!("LCTL+C".parse::<Action>().unwrap(), a([0x10, 0x01, 0x06]));
        assert_eq!("kc_lshift".parse::<Action>().unwrap(), a([0x10, 0x02, 0]));
        assert_eq!("VOLU".parse::<Action>().unwrap(), a([0x30, 0xe9, 0]));
        assert_eq!("FN(9)".parse::<Action>().unwrap(), a([0xf0, 0xff, 9]));
        assert_eq!("macro(1)".parse::<Action>().unwrap(), a([0x70, 0, 1]));
        assert_eq!("MACRO(32)".parse::<Action>().unwrap(), a([0x70, 31, 1]));
        assert_eq!("MACRO(5, hold)".parse::<Action>().unwrap(), a([0x70, 4, 0]));
        assert_eq!("MACRO(5, once)".parse::<Action>().unwrap(), a([0x70, 4, 1]));
        assert_eq!(
            "macro(5,Toggle)".parse::<Action>().unwrap(),
            a([0x70, 4, 0xff])
        );
        assert_eq!("0x950020".parse::<Action>().unwrap(), a([0x95, 0, 0x20]));
        assert_eq!("NO".parse::<Action>().unwrap(), Action::NONE);
        for bad in [
            "FN(12)",
            "A+B",
            "NOPE",
            "0x1234",
            "MACRO(0)",
            "MACRO(33)",
            "MACRO(1, 2)",
            "MACRO(1, loop)",
            "MACRO(1, )",
            "",
            "VOLU+A",
        ] {
            assert!(bad.parse::<Action>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn display_round_trips_every_action_of_a_kind() {
        for kind in [0x00, 0x10, 0x30, 0x70, 0x95, 0xf0, 0xff] {
            for a_byte in 0..=255u8 {
                for b_byte in [0u8, 1, 0x29, 0x7f, 0xc0, 0xff] {
                    let action = a([kind, a_byte, b_byte]);
                    let text = action.to_string();
                    assert_eq!(text.parse::<Action>().unwrap(), action, "{text}");
                }
            }
        }
    }

    #[test]
    fn policy() {
        for ok in [
            [0x10, 0, 4],
            [0x10, 0, 0xc0],
            [0x30, 0xe2, 0],
            [0xf0, 0xff, 3],
            [0xf0, 0x52, 0],
            [0x70, 0, 0xff],
        ] {
            assert_eq!(a(ok).assign_refusal(), None, "{ok:02x?}");
            assert_eq!(a(ok).replace_refusal(), None, "{ok:02x?}");
        }
        // Factory Fn layers hold functions this tool does not assign; a key
        // holding one can still be given something else.
        for f in [[0xf0, 0xfe, 0], [0xf0, 0x04, 0], [0xf0, 0x99, 0]] {
            assert!(a(f).assign_refusal().is_some(), "{f:02x?}");
            assert_eq!(a(f).replace_refusal(), None, "{f:02x?}");
        }
        for unmanaged in [
            [0x95, 0, 0x20],
            [0x92, 1, 2],
            [0x20, 1, 0],
            [0x70, 32, 0xff],
            [0xf0, 0xff, 12],
            [0xf0, 0x52, 7],
            [0xf0, 0x07, 0],
            [0xf0, 0x08, 0],
            [0xf0, 0x08, 5],
        ] {
            assert!(
                a(unmanaged).assign_refusal().is_some() && a(unmanaged).replace_refusal().is_some(),
                "{unmanaged:02x?}"
            );
        }
        assert!(
            a([0xf0, 0x08, 0])
                .replace_refusal()
                .unwrap()
                .contains("restore-backup")
        );
        assert!(a([0xf0, 0x07, 0]).never_write());
        assert!(a([0xf0, 0x08, 5]).never_write());
        assert!(!a([0xf0, 0x52, 0]).never_write());
        assert_eq!(a([0xf0, 0xff, 9]).layer(), Some(9));
        assert_eq!(a([0x10, 0, 4]).layer(), None);
    }

    /// Whatever `apply` may replace, `restore-backup` may write back.
    #[test]
    fn replaceable_actions_can_be_restored() {
        let at = matrix_offset(0, 1, 5) as u16;
        for kind in [0x00, 0x10, 0x30, 0x70, 0x90, 0x95, 0xf0, 0xff] {
            for x in 0..=255u8 {
                for y in 0..=255u8 {
                    let action = a([kind, x, y]);
                    if action.replace_refusal().is_none() {
                        assert!(
                            Region::Matrix.check_write(at, &action.0).is_ok(),
                            "{action}"
                        );
                    }
                }
            }
        }
    }
}
