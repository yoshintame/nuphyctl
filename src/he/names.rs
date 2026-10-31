//! Key and modifier names shared by key actions and macro steps, on top of
//! the keycode catalogue (`KC_` prefixes and aliases come from there).

use crate::keycodes::{KeycodeGroup, lookup_code, lookup_name};
use anyhow::{Context, Result, bail};

const FIRST_MODIFIER: u16 = 0xe0;

/// A name resolved to what the HE firmware stores for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// One modifier as a bitmask bit (`LCTL` = 0x01 .. `RGUI` = 0x80).
    Modifier(u8),
    /// A HID keyboard usage.
    Usage(u8),
}

/// Name of a HID keyboard usage, if the catalogue has one.
pub fn usage_name(usage: u8) -> Option<&'static str> {
    lookup_code(u16::from(usage))
        .filter(|k| matches!(k.group, KeycodeGroup::Basic))
        .map(|k| k.name)
}

/// Name of the modifier at `bit` (0 = `LCTL` .. 7 = `RGUI`).
pub fn modifier_name(bit: u32) -> &'static str {
    lookup_code(FIRST_MODIFIER + bit as u16).map_or("?", |k| k.name)
}

/// Names of the modifiers set in `mask`, in bit order.
pub fn modifier_names(mask: u8) -> impl Iterator<Item = &'static str> {
    (0..8)
        .filter(move |b| mask & (1 << b) != 0)
        .map(modifier_name)
}

pub fn parse_key(input: &str) -> Result<Key> {
    let k = lookup_name(input.trim())
        .filter(|k| k.code <= 0xff)
        .with_context(|| format!("unknown key {input:?}"))?;
    if (FIRST_MODIFIER..FIRST_MODIFIER + 8).contains(&k.code) {
        return Ok(Key::Modifier(1 << (k.code - FIRST_MODIFIER)));
    }
    if !matches!(k.group, KeycodeGroup::Basic) {
        bail!("{input:?} is not a keyboard key");
    }
    Ok(Key::Usage(k.code as u8))
}

/// `NAME(args)` with a case-insensitive name: the trimmed `args`.
pub fn call_args<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input
        .get(..name.len())?
        .eq_ignore_ascii_case(name)
        .then(|| &input[name.len()..])?;
    rest.strip_prefix('(')?.strip_suffix(')').map(str::trim)
}

/// A decimal or `0x` hexadecimal number.
pub fn parse_number(input: &str) -> Result<u32> {
    let s = input.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    }
    .with_context(|| format!("invalid number {s:?}"))
}

pub fn parse_byte(input: &str) -> Result<u8> {
    let n = parse_number(input)?;
    u8::try_from(n).with_context(|| format!("{n} does not fit a byte"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_modifiers() {
        assert_eq!(parse_key("esc").unwrap(), Key::Usage(0x29));
        assert_eq!(parse_key("KC_LSHIFT").unwrap(), Key::Modifier(0x02));
        assert_eq!(parse_key("RGUI").unwrap(), Key::Modifier(0x80));
        assert!(parse_key("NOPE").is_err());
        assert!(parse_key("VOLU").is_err());
        assert_eq!(usage_name(0x2c), Some("SPC"));
        assert_eq!(modifier_names(0x05).collect::<Vec<_>>(), ["LCTL", "LALT"]);
    }

    #[test]
    fn calls_and_numbers() {
        assert_eq!(call_args("fn(3)", "FN"), Some("3"));
        assert_eq!(call_args("FNX(3)", "FN"), None);
        assert_eq!(call_args("é(3)", "FN"), None);
        assert_eq!(parse_number("0x1f").unwrap(), 31);
        assert_eq!(parse_byte("255").unwrap(), 255);
        assert!(parse_byte("256").is_err());
    }
}
