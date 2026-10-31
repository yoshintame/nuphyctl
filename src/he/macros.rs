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

use crate::he::names::{
    Key, call_args, modifier_name, parse_byte, parse_chord, parse_key, usage_name,
};
use crate::he::region::{MACRO_BYTES, MACRO_COUNT};
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
/// The one step of an empty macro.
const EMPTY_STEP: [u8; STEP] = [0, 0, LAST, 0];

/// The delay NuPhy IO puts after each step of a macro it creates, except the
/// last; `"LCTL+C"` shorthand compiles to it.
const CHORD_DELAY: u16 = 1;

/// What a macro step presses or releases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// One modifier, as its bitmask bit.
    Modifier(u8),
    /// A HID keyboard usage.
    Key(u8),
    /// A mouse button mask.
    Mouse(u8),
    /// Any other kind and code, kept as they are.
    Raw { kind: u8, code: u8 },
}

impl Target {
    /// The canonical target for a step's kind and code bytes.
    pub fn from_bytes(kind: u8, code: u8) -> Self {
        match (kind & KIND_MASK, code) {
            (KIND_MODIFIER, bit) if bit.count_ones() == 1 => Self::Modifier(bit),
            (KIND_KEY, usage) => Self::Key(usage),
            (KIND_MOUSE, buttons) => Self::Mouse(buttons),
            (kind, code) => Self::Raw { kind, code },
        }
    }

    fn bytes(self) -> (u8, u8) {
        match self {
            Self::Modifier(bit) => (KIND_MODIFIER, bit),
            Self::Key(usage) => (KIND_KEY, usage),
            Self::Mouse(buttons) => (KIND_MOUSE, buttons),
            Self::Raw { kind, code } => (kind, code),
        }
    }

    /// A name `parse` reads back: `LCTL`, `C`, `MOUSE(1)` or `CODE(kind,code)`.
    pub fn name(self) -> String {
        match self {
            Self::Modifier(bit) => modifier_name(bit.trailing_zeros()).to_string(),
            Self::Key(usage) => usage_name(usage)
                .map_or_else(|| format!("CODE({KIND_KEY},{usage})"), str::to_string),
            Self::Mouse(buttons) => format!("MOUSE({buttons})"),
            Self::Raw { kind, code } => format!("CODE({kind},{code})"),
        }
    }

    /// Whether a chord (`LCTL+C`) can name it.
    fn in_chord(self) -> bool {
        match self {
            Self::Modifier(_) => true,
            Self::Key(usage) => usage_name(usage).is_some(),
            _ => false,
        }
    }

    fn parse(input: &str) -> Result<Self> {
        if let Some(args) = call_args(input, "CODE") {
            let (kind, code) = args
                .split_once(',')
                .with_context(|| format!("{input:?}: expected CODE(kind,code)"))?;
            let kind = parse_byte(kind)?;
            if kind > KIND_MASK {
                bail!("{input:?}: a step kind is 0..={KIND_MASK}");
            }
            return Ok(Self::from_bytes(kind, parse_byte(code)?));
        }
        if let Some(args) = call_args(input, "MOUSE") {
            return Ok(Self::Mouse(parse_byte(args)?));
        }
        Ok(match parse_key(input)? {
            Key::Modifier(bit) => Self::Modifier(bit),
            Key::Usage(usage) => Self::Key(usage),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub target: Target,
    pub down: bool,
    /// Wait after this step, in the firmware's units (milliseconds).
    pub delay: u16,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let direction = if self.down { "down" } else { "up" };
        write!(f, "{} {direction}", self.target.name())?;
        if self.delay != 0 {
            write!(f, " {}", self.delay)?;
        }
        Ok(())
    }
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
        Ok(Self {
            target: Target::parse(target).with_context(|| format!("in {input:?}"))?,
            down,
            delay,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Macro(pub Vec<Step>);

impl Macro {
    /// `steps` as a macro; the one step an empty macro is stored as reads as
    /// no steps at all.
    pub fn new(steps: Vec<Step>) -> Self {
        let is_empty_step = |s: &Step| step_bytes(s, true) == EMPTY_STEP;
        if steps.len() == 1 && is_empty_step(&steps[0]) {
            return Self::default();
        }
        Self(steps)
    }

    /// `"LCTL+C"`: modifiers down in order, the key down and up, modifiers up
    /// in reverse, with NuPhy IO's step delay and none after the last step.
    pub fn chord(input: &str) -> Result<Self> {
        let chord = parse_chord(input)?;
        let step = |target, down| Step {
            target,
            down,
            delay: CHORD_DELAY,
        };
        let mods = chord.modifiers.iter().map(|&bit| Target::Modifier(bit));
        let mut steps: Vec<Step> = mods.clone().map(|t| step(t, true)).collect();
        if let Some(usage) = chord.usage {
            steps.push(step(Target::Key(usage), true));
            steps.push(step(Target::Key(usage), false));
        }
        steps.extend(mods.rev().map(|t| step(t, false)));
        if let Some(last) = steps.last_mut() {
            last.delay = 0;
        }
        Ok(Self::new(steps))
    }

    /// The shortest config value that reproduces this macro exactly.
    pub fn to_toml(&self) -> String {
        let pressed: Vec<Target> = self.0.iter().filter(|s| s.down).map(|s| s.target).collect();
        if !pressed.is_empty() && pressed.iter().all(|t| t.in_chord()) {
            let names: Vec<String> = pressed.iter().map(|t| t.name()).collect();
            let shorthand = names.join("+");
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
                Ok(Self::new(steps))
            }
            _ => bail!("{source} must be a chord like \"LCTL+C\" or a list of steps"),
        }
    }
}

fn step_bytes(s: &Step, last: bool) -> [u8; STEP] {
    let (kind, code) = s.target.bytes();
    let flags = (kind & KIND_MASK) | if s.down { DOWN } else { 0 } | if last { LAST } else { 0 };
    let [lo, hi] = s.delay.to_le_bytes();
    [lo, hi, flags, code]
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
                target: Target::from_bytes(b[2], b[3]),
                down: b[2] & DOWN != 0,
                delay: u16::from_le_bytes([b[0], b[1]]),
            });
            if b[2] & LAST != 0 {
                break;
            }
            at += STEP;
        }
        out.push(Macro::new(steps));
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
    let mut offsets = [0usize; MACRO_COUNT];
    let mut at = HEADER;
    let mut push = |step: [u8; STEP]| -> Result<usize> {
        if at + STEP > MACRO_BYTES {
            bail!("macros do not fit the {MACRO_BYTES}-byte region");
        }
        region[at..at + STEP].copy_from_slice(&step);
        at += STEP;
        Ok(at - STEP)
    };
    let mut empty_at = None;
    for (i, m) in macros.iter().enumerate().take(used) {
        offsets[i] = if m.0.is_empty() {
            let start = push(EMPTY_STEP)?;
            empty_at.get_or_insert(start);
            start
        } else {
            let mut start = None;
            for (n, s) in m.0.iter().enumerate() {
                let placed = push(step_bytes(s, n + 1 == m.0.len()))?;
                start.get_or_insert(placed);
            }
            start.expect("a macro with steps")
        };
    }
    if used < MACRO_COUNT {
        let empty_at = match empty_at {
            Some(e) => e,
            None => push(EMPTY_STEP)?,
        };
        offsets[used..].fill(empty_at);
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
        assert!("C down x".parse::<Step>().is_err());
    }

    /// Every kind and code a region can hold prints as a step that parses
    /// back to the same bytes, kind 0 included.
    #[test]
    fn every_step_round_trips() {
        for kind in 0..=KIND_MASK {
            for code in [0u8, 1, 2, 0x29, 0x80, 0xc0, 0xff] {
                for down in [false, true] {
                    let step = Step {
                        target: Target::from_bytes(kind, code),
                        down,
                        delay: 7,
                    };
                    let back: Step = step.to_string().parse().unwrap();
                    assert_eq!(step_bytes(&back, false), step_bytes(&step, false), "{step}");
                }
            }
        }
    }

    #[test]
    fn empty_is_only_the_exact_empty_step() {
        let parse = |steps: &[&str]| {
            let list = toml::Value::Array(
                steps
                    .iter()
                    .map(|s| toml::Value::String(s.to_string()))
                    .collect(),
            );
            Macro::parse(&list, "m").unwrap()
        };
        assert!(parse(&["CODE(0,0) up"]).0.is_empty());
        assert_eq!(parse(&["CODE(0,0) up 5"]).0.len(), 1);
        assert_eq!(parse(&["CODE(0,0) up", "A up"]).0.len(), 2);
        let mut region = encode_region(&vec![Macro::default(); MACRO_COUNT]).unwrap();
        // A lone kind-0 step with a delay is a step, not an empty macro.
        region[64..68].copy_from_slice(&[5, 0, LAST, 0]);
        assert_eq!(decode_region(&region).unwrap()[0].0.len(), 1);
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

    /// Every empty macro before the last used one gets an empty step of its
    /// own; the unused ones after it share the first.
    #[test]
    fn empty_macros_keep_their_own_steps() {
        let mut list = vec![Macro::default(); MACRO_COUNT];
        list[2] = Macro::chord("LCTL+C").unwrap();
        let region = encode_region(&list).unwrap();
        let offset = |i: usize| usize::from(region[2 * i]) | usize::from(region[2 * i + 1]) << 8;
        assert_eq!([offset(0), offset(1), offset(2)], [64, 68, 72]);
        assert!((3..MACRO_COUNT).all(|i| offset(i) == 64));
        assert_eq!(decode_region(&region).unwrap(), list);
    }

    /// 32 macros that fill the region to the last byte fit; one step more
    /// does not.
    #[test]
    fn a_full_region_fits_exactly() {
        let tap = |n: usize| {
            Macro::new(
                (0..n)
                    .map(|i| Step {
                        target: Target::Key(4),
                        down: i % 2 == 0,
                        delay: 1,
                    })
                    .collect(),
            )
        };
        let mut list: Vec<Macro> = (0..MACRO_COUNT)
            .map(|i| tap(if i < 16 { 16 } else { 15 }))
            .collect();
        let region = encode_region(&list).unwrap();
        assert_eq!(decode_region(&region).unwrap(), list);
        list[31] = tap(16);
        assert!(encode_region(&list).is_err());
    }
}
