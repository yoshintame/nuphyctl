//! Macros of the HE boards: one 2048-byte region per mode.
//!
//! The region starts with 32 little-endian `u16` offsets, one per macro.
//! A macro is a run of 4-byte steps from its offset to the step with the
//! "last" bit: `delay_lo delay_hi flags code`, where `flags` carries the
//! step kind in its low bits (1 modifier, `code` a modifier bitmask; 2 key,
//! `code` a HID usage; 3 mouse button), bit 6 for "down" and bit 7 for
//! "last". An empty macro is the single step `00 00 80 00`.
//!
//! NuPhy IO lays macros out one after another from byte 64 and points unused
//! entries at an empty step; [`encode_region`] does the same.

use crate::he::names::{Key, call_args, modifier_name, parse_byte, parse_key, usage_name};
pub use crate::he::region::{MACRO_BYTES, MACRO_COUNT};
use anyhow::{Context, Result, bail};
use std::fmt;

const HEADER: usize = MACRO_COUNT * 2;
const STEP: usize = 4;

const KIND_MODIFIER: u8 = 1;
const KIND_KEY: u8 = 2;
const KIND_MOUSE: u8 = 3;
/// The flags byte keeps the step kind in its low 6 bits.
const KIND_MASK: u8 = 0x3f;
const DOWN: u8 = 0x40;
const LAST: u8 = 0x80;

/// The delay NuPhy IO puts after each step of a macro it creates, except the
/// last; `"LCTL+C"` shorthand compiles to it.
const CHORD_DELAY: u16 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub kind: u8,
    pub down: bool,
    pub code: u8,
    /// Wait after this step, in the firmware's units (milliseconds).
    pub delay: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Macro(pub Vec<Step>);

impl Step {
    /// What the step presses: `LCTL`, `C`, `MOUSE(1)` or raw `CODE(kind,code)`.
    pub fn target_name(&self) -> String {
        match (self.kind, self.code) {
            (KIND_MODIFIER, mask) if mask.count_ones() == 1 => {
                modifier_name(mask.trailing_zeros()).to_string()
            }
            (KIND_KEY, usage) if usage_name(usage).is_some() => {
                usage_name(usage).unwrap_or_default().to_string()
            }
            (KIND_MOUSE, button) => format!("MOUSE({button})"),
            (kind, code) => format!("CODE({kind},{code})"),
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = if self.down { "down" } else { "up" };
        write!(f, "{} {direction}", self.target_name())?;
        if self.delay != 0 {
            write!(f, " {}", self.delay)?;
        }
        Ok(())
    }
}

fn parse_target(input: &str) -> Result<(u8, u8)> {
    if let Some(args) = call_args(input, "CODE") {
        let (kind, code) = args
            .split_once(',')
            .with_context(|| format!("{input:?}: expected CODE(kind,code)"))?;
        let kind = parse_byte(kind)?;
        if !(1..=KIND_MASK).contains(&kind) {
            bail!("{input:?}: a step kind is 1..={KIND_MASK}");
        }
        return Ok((kind, parse_byte(code)?));
    }
    if let Some(args) = call_args(input, "MOUSE") {
        return Ok((KIND_MOUSE, parse_byte(args)?));
    }
    Ok(match parse_key(input)? {
        Key::Modifier(bit) => (KIND_MODIFIER, bit),
        Key::Usage(usage) => (KIND_KEY, usage),
    })
}

impl std::str::FromStr for Step {
    type Err = anyhow::Error;

    /// `<key> down|up [delay]`, e.g. `LCTL down`, `C up 10`, `MOUSE(1) down`.
    fn from_str(input: &str) -> Result<Self> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        let (target, direction, delay) = match parts.as_slice() {
            [t, d] => (*t, *d, 0),
            [t, d, delay] => (
                *t,
                *d,
                delay
                    .parse::<u16>()
                    .with_context(|| format!("invalid delay in {input:?}"))?,
            ),
            _ => bail!("macro step {input:?} must be `<key> down|up [delay]`"),
        };
        let down = match direction.to_ascii_lowercase().as_str() {
            "down" => true,
            "up" => false,
            _ => bail!("macro step {input:?}: expected down or up"),
        };
        let (kind, code) = parse_target(target).with_context(|| format!("in {input:?}"))?;
        Ok(Self {
            kind,
            down,
            code,
            delay,
        })
    }
}

impl Macro {
    /// `"LCTL+C"`: modifiers down in order, the key down and up, modifiers up
    /// in reverse, with NuPhy IO's step delay and none after the last step.
    pub fn chord(input: &str) -> Result<Self> {
        let mut mods = Vec::new();
        let mut key = None;
        for part in input.split('+') {
            match parse_target(part.trim()).with_context(|| format!("in {input:?}"))? {
                (KIND_MODIFIER, bit) => mods.push(bit),
                (KIND_KEY, code) if key.is_none() => key = Some(code),
                _ => bail!("{input:?}: a chord is modifiers and at most one key"),
            }
        }
        let step = |kind, down, code| Step {
            kind,
            down,
            code,
            delay: CHORD_DELAY,
        };
        let mut steps: Vec<Step> = mods.iter().map(|&m| step(KIND_MODIFIER, true, m)).collect();
        if let Some(code) = key {
            steps.push(step(KIND_KEY, true, code));
            steps.push(step(KIND_KEY, false, code));
        }
        steps.extend(mods.iter().rev().map(|&m| step(KIND_MODIFIER, false, m)));
        if let Some(last) = steps.last_mut() {
            last.delay = 0;
        }
        Ok(Self(steps))
    }

    /// The shortest config value that reproduces this macro exactly.
    pub fn to_toml(&self) -> String {
        if !self.0.is_empty() {
            let pressed: Vec<String> = self
                .0
                .iter()
                .filter(|s| s.down)
                .map(Step::target_name)
                .collect();
            let shorthand = pressed.join("+");
            if Self::chord(&shorthand).is_ok_and(|m| m == *self) {
                return format!("\"{shorthand}\"");
            }
        }
        let steps: Vec<String> = self.0.iter().map(|s| format!("\"{s}\"")).collect();
        format!("[{}]", steps.join(", "))
    }

    pub fn parse(value: &toml::Value, source: &str) -> Result<Self> {
        match value {
            toml::Value::String(s) => Self::chord(s).with_context(|| format!("in {source}")),
            toml::Value::Array(items) => {
                let steps = items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .with_context(|| format!("{source}: steps are strings"))?
                            .parse::<Step>()
                    })
                    .collect::<Result<Vec<_>>>()
                    .with_context(|| format!("in {source}"))?;
                Ok(Self(steps))
            }
            _ => bail!("{source} must be a chord like \"LCTL+C\" or a list of steps"),
        }
    }
}

fn step_bytes(s: &Step, last: bool) -> [u8; STEP] {
    let flags = (s.kind & KIND_MASK) | if s.down { DOWN } else { 0 } | if last { LAST } else { 0 };
    [s.delay as u8, (s.delay >> 8) as u8, flags, s.code]
}

/// Decode a mode's macro region into its 32 macros.
pub fn decode_region(region: &[u8]) -> Result<Vec<Macro>> {
    if region.len() != MACRO_BYTES {
        bail!(
            "macro region is {} bytes, expected {MACRO_BYTES}",
            region.len()
        );
    }
    let mut out = Vec::with_capacity(MACRO_COUNT);
    for i in 0..MACRO_COUNT {
        let mut at = usize::from(region[2 * i]) | usize::from(region[2 * i + 1]) << 8;
        if at == 0 {
            out.push(Macro::default());
            continue;
        }
        if at < HEADER || at % STEP != 0 {
            bail!("macro {i} starts at {at}, outside the step area");
        }
        let mut steps = Vec::new();
        loop {
            if at + STEP > MACRO_BYTES {
                bail!("macro {i} runs past the end of the region");
            }
            let b = &region[at..at + STEP];
            steps.push(Step {
                kind: b[2] & KIND_MASK,
                down: b[2] & DOWN != 0,
                code: b[3],
                delay: u16::from(b[0]) | u16::from(b[1]) << 8,
            });
            if b[2] & LAST != 0 {
                break;
            }
            at += STEP;
        }
        let empty = steps.len() == 1 && steps[0].kind == 0 && steps[0].code == 0;
        out.push(Macro(if empty { Vec::new() } else { steps }));
    }
    Ok(out)
}

/// Lay out 32 macros the way NuPhy IO does: one after another from byte 64,
/// in index order, unused entries pointing at a shared empty step.
pub fn encode_region(macros: &[Macro]) -> Result<Vec<u8>> {
    if macros.len() != MACRO_COUNT {
        bail!("expected {MACRO_COUNT} macros, got {}", macros.len());
    }
    let mut region = vec![0u8; MACRO_BYTES];
    let used = macros
        .iter()
        .rposition(|m| !m.0.is_empty())
        .map_or(0, |i| i + 1);
    let mut at = HEADER;
    let mut empty_at = None;
    let mut offsets = [0usize; MACRO_COUNT];
    for (i, m) in macros.iter().enumerate().take(used) {
        offsets[i] = at;
        let steps: Vec<[u8; STEP]> = if m.0.is_empty() {
            empty_at.get_or_insert(at);
            vec![[0, 0, LAST, 0]]
        } else {
            m.0.iter()
                .enumerate()
                .map(|(n, s)| step_bytes(s, n + 1 == m.0.len()))
                .collect()
        };
        for b in steps {
            if at + STEP > MACRO_BYTES {
                bail!("macros do not fit the {MACRO_BYTES}-byte region");
            }
            region[at..at + STEP].copy_from_slice(&b);
            at += STEP;
        }
    }
    let empty_at = match empty_at {
        Some(e) => e,
        None => {
            if at + STEP > MACRO_BYTES {
                bail!("macros do not fit the {MACRO_BYTES}-byte region");
            }
            region[at..at + STEP].copy_from_slice(&[0, 0, LAST, 0]);
            at
        }
    };
    for off in offsets.iter_mut().skip(used) {
        *off = empty_at;
    }
    for (i, off) in offsets.iter().enumerate() {
        region[2 * i] = *off as u8;
        region[2 * i + 1] = (*off >> 8) as u8;
    }
    Ok(region)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start of the macro region of a real Air60 HE mode: Ctrl+Z,
    /// Ctrl+Shift+Z, Win+Space, an empty macro, Ctrl+C.
    fn sample() -> Vec<u8> {
        let mut r = vec![0u8; MACRO_BYTES];
        let header = [64u16, 80, 104, 120, 136];
        for (i, off) in header
            .iter()
            .chain(std::iter::repeat(&120))
            .take(32)
            .enumerate()
        {
            r[2 * i] = *off as u8;
            r[2 * i + 1] = (*off >> 8) as u8;
        }
        let steps: &[[u8; 4]] = &[
            [0, 0, 0x41, 0x01],
            [0, 0, 0x42, 0x1d],
            [0, 0, 0x02, 0x1d],
            [0, 0, 0x81, 0x01],
            [1, 0, 0x41, 0x01],
            [1, 0, 0x41, 0x02],
            [1, 0, 0x42, 0x1d],
            [1, 0, 0x02, 0x1d],
            [1, 0, 0x01, 0x02],
            [1, 0, 0x81, 0x01],
            [1, 0, 0x41, 0x08],
            [1, 0, 0x42, 0x2c],
            [1, 0, 0x02, 0x2c],
            [1, 0, 0x81, 0x08],
            [0, 0, 0x80, 0x00],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [1, 0, 0x41, 0x01],
            [1, 0, 0x42, 0x06],
            [1, 0, 0x02, 0x06],
            [0, 0, 0x81, 0x01],
        ];
        for (n, s) in steps.iter().enumerate() {
            r[64 + 4 * n..68 + 4 * n].copy_from_slice(s);
        }
        r
    }

    #[test]
    fn decodes_a_real_region() {
        let m = decode_region(&sample()).unwrap();
        assert_eq!(
            m[0].to_toml(),
            r#"["LCTL down", "Z down", "Z up", "LCTL up"]"#
        );
        assert_eq!(
            m[2].to_toml(),
            r#"["LGUI down 1", "SPC down 1", "SPC up 1", "LGUI up 1"]"#
        );
        assert!(m[3].0.is_empty());
        assert_eq!(m[4].to_toml(), r#""LCTL+C""#);
        assert!(m[20].0.is_empty());
    }

    #[test]
    fn chords() {
        let m = Macro::chord("LCTL+LSFT+Z").unwrap();
        let text: Vec<String> = m.0.iter().map(Step::to_string).collect();
        assert_eq!(
            text,
            [
                "LCTL down 1",
                "LSFT down 1",
                "Z down 1",
                "Z up 1",
                "LSFT up 1",
                "LCTL up"
            ]
        );
        assert_eq!(m.to_toml(), r#""LCTL+LSFT+Z""#);
        assert!(Macro::chord("A+B").is_err());
        assert!(Macro::chord("NOPE").is_err());
    }

    #[test]
    fn steps_round_trip() {
        for text in [
            "LCTL down",
            "C up 10",
            "MOUSE(1) down",
            "CODE(2,0) down 1",
            "RGUI up",
        ] {
            assert_eq!(text.parse::<Step>().unwrap().to_string(), text);
        }
        assert!("C sideways".parse::<Step>().is_err());
        assert!("CODE(64,1) down".parse::<Step>().is_err());
        assert!("CODE(0,0) up".parse::<Step>().is_err());
        assert!("C down x".parse::<Step>().is_err());
    }

    #[test]
    fn encodes_like_nuphy_io_and_round_trips() {
        let macros = decode_region(&sample()).unwrap();
        let region = encode_region(&macros).unwrap();
        assert_eq!(decode_region(&region).unwrap(), macros);
        assert_eq!(&region[..6], &[64, 0, 80, 0, 104, 0]);
        assert_eq!(&region[120..124], &[0, 0, 0x80, 0]);
        assert_eq!(region[2 * 31], 120);

        let mut only_one = vec![Macro::default(); MACRO_COUNT];
        only_one[0] = Macro::chord("LCTL+C").unwrap();
        let region = encode_region(&only_one).unwrap();
        assert_eq!(&region[64 + 16..64 + 20], &[0, 0, 0x80, 0]);
        assert_eq!(region[2], 80);
        assert_eq!(decode_region(&region).unwrap(), only_one);

        let empty = encode_region(&vec![Macro::default(); MACRO_COUNT]).unwrap();
        assert_eq!(&empty[..2], &[64, 0]);
        assert_eq!(&empty[64..68], &[0, 0, 0x80, 0]);
    }

    #[test]
    fn rejects_oversized_and_broken_regions() {
        let mut big = vec![Macro::default(); MACRO_COUNT];
        big[0] = Macro(vec!["A down".parse().unwrap(); 600]);
        assert!(encode_region(&big).is_err());
        let mut broken = sample();
        broken[0] = 3;
        assert!(decode_region(&broken).is_err());
        let never_written = decode_region(&vec![0u8; MACRO_BYTES]).unwrap();
        assert!(never_written.iter().all(|m| m.0.is_empty()));
    }
}
