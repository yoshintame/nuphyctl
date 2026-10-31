//! Air60 HE key positions and the names of modes and layers.

use crate::he::region::{LAYERS, LAYERS_PER_MODE, MODES};
use anyhow::{Context, Result, bail};

pub const LAYER_NAMES: [&str; 4] = ["base", "fn1", "fn2", "fn3"];

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

/// A layer within `mode`: `base`, `fn1`..`fn3`, or NuPhy IO's numbering
/// across modes (`fn8`..`fn11` are m3's four layers).
pub fn parse_layer(mode: u8, input: &str) -> Result<u8> {
    let s = input.trim().to_ascii_lowercase();
    if let Some(i) = LAYER_NAMES.iter().position(|n| *n == s) {
        return Ok(i as u8);
    }
    if let Some(n) = s.strip_prefix("fn").and_then(|n| n.parse::<u8>().ok())
        && n < LAYERS
    {
        if n / LAYERS_PER_MODE != mode {
            bail!(
                "layer {input:?} belongs to {}, not {}",
                mode_label(n / LAYERS_PER_MODE),
                mode_label(mode)
            );
        }
        return Ok(n % LAYERS_PER_MODE);
    }
    bail!("unknown layer {input:?} (expected base, fn1, fn2 or fn3)")
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
        assert_eq!(parse_layer(2, "fn8").unwrap(), 0);
        assert_eq!(parse_layer(2, "fn11").unwrap(), 3);
        assert_eq!(parse_layer(2, "fn3").unwrap(), 3);
        assert!(parse_layer(1, "fn11").is_err());
        assert!(parse_layer(0, "fn12").is_err());
    }
}
