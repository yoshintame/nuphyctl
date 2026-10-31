//! Air60 HE key positions and the names of modes and layers.

use crate::he::region::{LAYERS, LAYERS_PER_MODE, MODES};
use anyhow::{Context, Result, bail};
use std::fmt;

/// A named physical key; its index in `AIR60_HE` is its matrix slot.
pub struct Position {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
}

macro_rules! p {
    ($name:expr) => {
        Position { name: $name, aliases: &[] }
    };
    ($name:expr, [$($alias:expr),*]) => {
        Position { name: $name, aliases: &[$($alias),*] }
    };
}

/// Air60 HE matrix slots 0..=60, in physical order.
pub static AIR60_HE: &[Position] = &[
    p!("ESC", ["ESCAPE"]),
    p!("1"),
    p!("2"),
    p!("3"),
    p!("4"),
    p!("5"),
    p!("6"),
    p!("7"),
    p!("8"),
    p!("9"),
    p!("0"),
    p!("MINS", ["MINUS"]),
    p!("EQL", ["EQUAL"]),
    p!("BSPC", ["BACKSPACE"]),
    p!("TAB"),
    p!("Q"),
    p!("W"),
    p!("E"),
    p!("R"),
    p!("T"),
    p!("Y"),
    p!("U"),
    p!("I"),
    p!("O"),
    p!("P"),
    p!("LBRC", ["LBRACKET"]),
    p!("RBRC", ["RBRACKET"]),
    p!("BSLS", ["BACKSLASH"]),
    p!("CAPS", ["CAPS_LOCK"]),
    p!("A"),
    p!("S"),
    p!("D"),
    p!("F"),
    p!("G"),
    p!("H"),
    p!("J"),
    p!("K"),
    p!("L"),
    p!("SCLN", ["SEMICOLON"]),
    p!("QUOT", ["QUOTE"]),
    p!("ENT", ["ENTER"]),
    p!("LSFT", ["LSHIFT"]),
    p!("Z"),
    p!("X"),
    p!("C"),
    p!("V"),
    p!("B"),
    p!("N"),
    p!("M"),
    p!("COMM", ["COMMA"]),
    p!("DOT"),
    p!("SLSH", ["SLASH"]),
    p!("RSFT", ["RSHIFT"]),
    p!("LCTL", ["LCTRL"]),
    p!("LGUI", ["LWIN", "LCMD"]),
    p!("LALT", ["LOPT"]),
    p!("SPC", ["SPACE"]),
    p!("RALT", ["ROPT"]),
    p!("MENU", ["APP"]),
    p!("RCTL", ["RCTRL"]),
    p!("FN"),
];

pub fn key_count() -> usize {
    AIR60_HE.len()
}

pub fn position_name(slot: usize) -> String {
    AIR60_HE
        .get(slot)
        .map(|p| p.name.to_string())
        .unwrap_or_else(|| format!("#{slot}"))
}

pub fn parse_position(input: &str) -> Result<usize> {
    let s = input.trim();
    if let Some(n) = s.strip_prefix('#') {
        let slot: usize = n.parse().with_context(|| format!("invalid slot {s:?}"))?;
        if slot >= key_count() {
            bail!("slot {slot} out of range (0..{})", key_count());
        }
        return Ok(slot);
    }
    let upper = s.to_ascii_uppercase();
    AIR60_HE
        .iter()
        .position(|p| p.name == upper || p.aliases.contains(&upper.as_str()))
        .with_context(|| format!("unknown key position {s:?}"))
}

/// A mode is `m1`..`m3` (as NuPhy IO labels them) or `1`..`3`.
pub fn parse_mode(input: &str) -> Result<u8> {
    let s = input.trim().to_ascii_lowercase();
    let n = s.strip_prefix('m').unwrap_or(&s);
    match n.parse::<u8>() {
        Ok(n @ 1..=MODES) => Ok(n - 1),
        _ => bail!("unknown mode {input:?} (expected m1, m2 or m3)"),
    }
}

pub fn mode_label(mode: u8) -> String {
    format!("m{}", mode + 1)
}

/// A layer section of `mode`, numbered across modes as NuPhy IO does: `base`
/// (or `fn0` / `fn4` / `fn8`) is the mode's first layer, `fn1`..`fn3` m1's
/// others, `fn5`..`fn7` m2's, `fn9`..`fn11` m3's. Returns the layer within
/// the mode.
pub fn parse_layer(mode: u8, input: &str) -> Result<u8> {
    let s = input.trim().to_ascii_lowercase();
    if s == "base" {
        return Ok(0);
    }
    let first = mode * LAYERS_PER_MODE;
    let names = || format!("base, fn{}, fn{} or fn{}", first + 1, first + 2, first + 3);
    let n = s
        .strip_prefix("fn")
        .and_then(|n| n.parse::<u8>().ok())
        .filter(|&n| n < LAYERS)
        .with_context(|| {
            format!(
                "unknown layer {input:?} (the layers of {} are {})",
                mode_label(mode),
                names()
            )
        })?;
    if n / LAYERS_PER_MODE != mode {
        bail!(
            "layer {input:?} is NuPhy IO's FN {n}, a layer of {}; the layers of {} are {}",
            mode_label(n / LAYERS_PER_MODE),
            mode_label(mode),
            names()
        );
    }
    Ok(n % LAYERS_PER_MODE)
}

/// The section name of a layer: `base`, else NuPhy IO's `fn<absolute>`.
pub fn layer_name(mode: u8, layer: u8) -> String {
    if layer == 0 {
        "base".to_string()
    } else {
        format!("fn{}", mode * LAYERS_PER_MODE + layer)
    }
}

/// One key of one layer: where a key action lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyRef {
    pub mode: u8,
    pub layer: u8,
    pub slot: usize,
}

impl fmt::Display for KeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{} {}",
            mode_label(self.mode),
            layer_name(self.mode, self.layer),
            position_name(self.slot)
        )
    }
}

/// One key's travel record in one mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TravelRef {
    pub mode: u8,
    pub slot: usize,
}

impl fmt::Display for TravelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.travel {}",
            mode_label(self.mode),
            position_name(self.slot)
        )
    }
}

/// One macro of one mode; `index` is 0-based, shown 1-based like NuPhy IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MacroRef {
    pub mode: u8,
    pub index: usize,
}

impl fmt::Display for MacroRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.macros {}", mode_label(self.mode), self.index + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions() {
        assert_eq!(key_count(), 61);
        assert_eq!(parse_position("esc").unwrap(), 0);
        assert_eq!(parse_position("CAPS").unwrap(), 28);
        assert_eq!(parse_position("W").unwrap(), 16);
        assert_eq!(parse_position("FN").unwrap(), 60);
        assert_eq!(parse_position("#58").unwrap(), 58);
        assert!(parse_position("#61").is_err());
        assert!(parse_position("F1").is_err());
        assert_eq!(position_name(41), "LSFT");
    }

    #[test]
    fn modes_and_layers() {
        assert_eq!(parse_mode("m1").unwrap(), 0);
        assert_eq!(parse_mode("M3").unwrap(), 2);
        assert_eq!(parse_mode("2").unwrap(), 1);
        assert!(parse_mode("m4").is_err());
        assert!(parse_mode("m0").is_err());
        assert_eq!(parse_layer(0, "base").unwrap(), 0);
        assert_eq!(parse_layer(0, "fn0").unwrap(), 0);
        assert_eq!(parse_layer(0, "FN3").unwrap(), 3);
        assert!(parse_layer(0, "fn4").is_err());
        assert_eq!(parse_layer(1, "base").unwrap(), 0);
        assert_eq!(parse_layer(1, "fn4").unwrap(), 0);
        assert_eq!(parse_layer(1, "fn5").unwrap(), 1);
        let err = parse_layer(1, "fn1").unwrap_err().to_string();
        assert!(
            err.contains("a layer of m1") && err.contains("fn5"),
            "{err}"
        );
        assert_eq!(parse_layer(2, "fn8").unwrap(), 0);
        assert_eq!(parse_layer(2, "fn11").unwrap(), 3);
        assert!(parse_layer(2, "fn3").is_err());
        assert!(parse_layer(0, "fn12").is_err());
        assert!(parse_layer(0, "layer1").is_err());
        assert_eq!(layer_name(1, 0), "base");
        assert_eq!(layer_name(1, 2), "fn6");
        assert_eq!(layer_name(0, 3), "fn3");
    }

    #[test]
    fn refs_display() {
        assert_eq!(
            KeyRef {
                mode: 1,
                layer: 1,
                slot: 16
            }
            .to_string(),
            "m2.fn5 W"
        );
        assert_eq!(TravelRef { mode: 0, slot: 0 }.to_string(), "m1.travel ESC");
        assert_eq!(MacroRef { mode: 2, index: 0 }.to_string(), "m3.macros 1");
    }
}
