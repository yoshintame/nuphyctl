//! HE board model: named positions, the key matrix and travel tables as
//! buffers, the declarative TOML config, diffs and write planning. Nothing
//! here touches the device.

use crate::he::actions::Action;
use crate::he::macros::{MACRO_BYTES, MACRO_COUNT, Macro, decode_region, encode_region};
use crate::he::protocol::{
    ACTION_LEN, CHUNK, LAYER_BYTES, LAYERS, LAYERS_PER_MODE, MODES, TRAVEL_BYTES, TRAVEL_RECORD,
    matrix_addr, travel_addr,
};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};

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
fn parse_layer(mode: u8, input: &str) -> Result<u8> {
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

/// All 12 layers of the key matrix, as read with `GetUseKeyMatrix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matrix(pub Vec<u8>);

impl Matrix {
    pub const BYTES: usize = LAYERS as usize * LAYER_BYTES;

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != Self::BYTES {
            bail!(
                "key matrix is {} bytes, expected {}",
                bytes.len(),
                Self::BYTES
            );
        }
        Ok(Self(bytes))
    }

    pub fn get(&self, mode: u8, layer: u8, slot: usize) -> Action {
        let at = usize::from(matrix_addr(mode, layer, slot));
        Action([self.0[at], self.0[at + 1], self.0[at + 2]])
    }

    pub fn set(&mut self, mode: u8, layer: u8, slot: usize, action: Action) {
        let at = usize::from(matrix_addr(mode, layer, slot));
        self.0[at..at + ACTION_LEN].copy_from_slice(&action.0);
    }
}

/// Rapid Trigger sensitivity step selected by two bits of a travel record.
fn step_mm(bits: u8) -> f64 {
    match bits & 3 {
        1 => 0.005,
        2 => 0.001,
        _ => 0.01,
    }
}

pub const TRIGGERS: [&str; 3] = ["normal", "rapid", "continuous"];

/// One key's travel settings (8 bytes). Setters change only their own bits,
/// so unknown bits (the switch type, the firmware's check mark, Snap Tap
/// priority) survive a round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TravelRecord(pub [u8; TRAVEL_RECORD]);

impl TravelRecord {
    fn word(&self, at: usize) -> u16 {
        u16::from(self.0[at]) | (u16::from(self.0[at + 1]) << 8)
    }

    fn set_word(&mut self, at: usize, v: u16) {
        self.0[at] = v as u8;
        self.0[at + 1] = (v >> 8) as u8;
    }

    pub fn switch_type(&self) -> u8 {
        self.0[0] & 0x1f
    }

    pub fn trigger(&self) -> u8 {
        self.0[1] & 0x0f
    }

    pub fn actuation_mm(&self) -> f64 {
        round3(f64::from((self.word(2) & 0x1ff) + 1) * 0.01)
    }

    fn press_step(&self) -> f64 {
        step_mm(self.0[3] >> 1)
    }

    fn release_step(&self) -> f64 {
        step_mm(self.0[3] >> 3)
    }

    pub fn press_mm(&self) -> f64 {
        round3(f64::from((self.word(4) & 0x1ff) + 1) * self.press_step())
    }

    pub fn release_mm(&self) -> f64 {
        round3(f64::from((self.word(6) & 0x1ff) + 1) * self.release_step())
    }

    pub fn upper_deadzone_mm(&self) -> f64 {
        dead_mm(self.word(4) >> 9)
    }

    pub fn lower_deadzone_mm(&self) -> f64 {
        dead_mm(self.word(6) >> 9)
    }

    pub fn set_trigger(&mut self, trigger: u8) {
        self.0[1] = (self.0[1] & 0xf0) | (trigger & 0x0f);
    }

    pub fn set_actuation_mm(&mut self, mm: f64) -> Result<()> {
        check_range("actuation", mm, 0.1, 3.3)?;
        let units = units(mm, 0.01)?;
        let w = self.word(2);
        self.set_word(2, (w & !0x1ff) | units);
        Ok(())
    }

    pub fn set_press_mm(&mut self, mm: f64) -> Result<()> {
        check_range("press", mm, 0.001, 3.3)?;
        let units = units(mm, self.press_step())?;
        let w = self.word(4);
        self.set_word(4, (w & !0x1ff) | units);
        Ok(())
    }

    pub fn set_release_mm(&mut self, mm: f64) -> Result<()> {
        check_range("release", mm, 0.001, 3.3)?;
        let units = units(mm, self.release_step())?;
        let w = self.word(6);
        self.set_word(6, (w & !0x1ff) | units);
        Ok(())
    }

    pub fn set_upper_deadzone_mm(&mut self, mm: f64) -> Result<()> {
        let v = dead_units(mm)?;
        let w = self.word(4);
        self.set_word(4, (w & 0x1ff) | (v << 9));
        Ok(())
    }

    pub fn set_lower_deadzone_mm(&mut self, mm: f64) -> Result<()> {
        let v = dead_units(mm)?;
        let w = self.word(6);
        self.set_word(6, (w & 0x1ff) | (v << 9));
        Ok(())
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn check_range(what: &str, mm: f64, min: f64, max: f64) -> Result<()> {
    if !(min - 1e-9..=max + 1e-9).contains(&mm) {
        bail!("{what} {mm} mm out of range ({min}..={max} mm)");
    }
    Ok(())
}

/// Records store `value - 1` steps: 1.50 mm at 0.01 mm steps is 149.
fn units(mm: f64, step: f64) -> Result<u16> {
    let steps = (mm / step).round();
    if ((steps * step) - mm).abs() > step / 10.0 {
        bail!("{mm} mm is not a multiple of the {step} mm step");
    }
    let v = steps as i64 - 1;
    if !(0..=0x1ff).contains(&v) {
        bail!("{mm} mm does not fit the record");
    }
    Ok(v as u16)
}

fn dead_mm(v: u16) -> f64 {
    if v == 0 {
        0.0
    } else {
        round3(f64::from(v + 1) * 0.01)
    }
}

fn dead_units(mm: f64) -> Result<u16> {
    if mm.abs() < 1e-9 {
        return Ok(0);
    }
    check_range("dead zone", mm, 0.02, 1.28)?;
    units(mm, 0.01)
}

/// All three modes' travel tables, as read with `GetKeyRTInfo`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Travel(pub Vec<u8>);

impl Travel {
    pub const BYTES: usize = MODES as usize * TRAVEL_BYTES;

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != Self::BYTES {
            bail!(
                "travel table is {} bytes, expected {}",
                bytes.len(),
                Self::BYTES
            );
        }
        Ok(Self(bytes))
    }

    pub fn get(&self, mode: u8, slot: usize) -> TravelRecord {
        let at = usize::from(travel_addr(mode, slot));
        let mut r = [0u8; TRAVEL_RECORD];
        r.copy_from_slice(&self.0[at..at + TRAVEL_RECORD]);
        TravelRecord(r)
    }

    pub fn set(&mut self, mode: u8, slot: usize, record: TravelRecord) {
        let at = usize::from(travel_addr(mode, slot));
        self.0[at..at + TRAVEL_RECORD].copy_from_slice(&record.0);
    }
}

/// All three modes' macro regions, as read with `GetMacro`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Macros(pub Vec<u8>);

impl Macros {
    pub const BYTES: usize = MODES as usize * MACRO_BYTES;

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != Self::BYTES {
            bail!(
                "macro regions are {} bytes, expected {}",
                bytes.len(),
                Self::BYTES
            );
        }
        Ok(Self(bytes))
    }

    pub fn region(&self, mode: u8) -> &[u8] {
        let at = usize::from(mode) * MACRO_BYTES;
        &self.0[at..at + MACRO_BYTES]
    }

    pub fn set_region(&mut self, mode: u8, region: &[u8]) {
        let at = usize::from(mode) * MACRO_BYTES;
        self.0[at..at + MACRO_BYTES].copy_from_slice(region);
    }

    pub fn decode(&self, mode: u8) -> Result<Vec<Macro>> {
        decode_region(self.region(mode))
            .with_context(|| format!("cannot read the macros of {}", mode_label(mode)))
    }
}

/// What a TOML travel entry asks for; unset fields keep the device's value.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TravelSpec {
    pub actuation: Option<f64>,
    pub trigger: Option<u8>,
    pub press: Option<f64>,
    pub release: Option<f64>,
    pub upper_deadzone: Option<f64>,
    pub lower_deadzone: Option<f64>,
}

impl TravelSpec {
    fn merged(self, over: Self) -> Self {
        Self {
            actuation: over.actuation.or(self.actuation),
            trigger: over.trigger.or(self.trigger),
            press: over.press.or(self.press),
            release: over.release.or(self.release),
            upper_deadzone: over.upper_deadzone.or(self.upper_deadzone),
            lower_deadzone: over.lower_deadzone.or(self.lower_deadzone),
        }
    }

    pub fn apply_to(&self, mut r: TravelRecord) -> Result<TravelRecord> {
        if let Some(t) = self.trigger {
            r.set_trigger(t);
        }
        if let Some(v) = self.actuation {
            r.set_actuation_mm(v)?;
        }
        if let Some(v) = self.press {
            r.set_press_mm(v)?;
        }
        if let Some(v) = self.release {
            r.set_release_mm(v)?;
        }
        if let Some(v) = self.upper_deadzone {
            r.set_upper_deadzone_mm(v)?;
        }
        if let Some(v) = self.lower_deadzone {
            r.set_lower_deadzone_mm(v)?;
        }
        Ok(r)
    }

    fn of(r: &TravelRecord) -> Self {
        Self {
            actuation: Some(r.actuation_mm()),
            trigger: Some(r.trigger()),
            press: Some(r.press_mm()),
            release: Some(r.release_mm()),
            upper_deadzone: Some(r.upper_deadzone_mm()),
            lower_deadzone: Some(r.lower_deadzone_mm()),
        }
    }

    fn parse(value: &toml::Value, source: &str) -> Result<Self> {
        let table = value.as_table().with_context(|| {
            format!("{source} must be an inline table like {{ actuation = 1.5 }}")
        })?;
        let mut spec = Self::default();
        for (field, v) in table {
            let mm = || -> Result<f64> {
                match v {
                    toml::Value::Float(f) => Ok(*f),
                    toml::Value::Integer(i) => Ok(*i as f64),
                    _ => bail!("{source}.{field} must be a number of millimetres"),
                }
            };
            match field.as_str() {
                "actuation" => spec.actuation = Some(mm()?),
                "press" => spec.press = Some(mm()?),
                "release" => spec.release = Some(mm()?),
                "upper_deadzone" => spec.upper_deadzone = Some(mm()?),
                "lower_deadzone" => spec.lower_deadzone = Some(mm()?),
                "trigger" => {
                    let name = v
                        .as_str()
                        .with_context(|| format!("{source}.trigger must be a string"))?;
                    let t = TRIGGERS
                        .iter()
                        .position(|t| t.eq_ignore_ascii_case(name))
                        .with_context(|| {
                            format!(
                                "{source}.trigger {name:?} (expected normal, rapid or continuous)"
                            )
                        })?;
                    spec.trigger = Some(t as u8);
                }
                other => bail!(
                    "unknown field {other:?} in {source} (expected actuation, trigger, press, release, upper_deadzone, lower_deadzone)"
                ),
            }
        }
        Ok(spec)
    }

    fn render(&self, base: Option<&Self>) -> String {
        let mut parts = Vec::new();
        let differs = |a: Option<f64>, b: Option<f64>| base.is_none() || a != b;
        if let Some(v) = self
            .actuation
            .filter(|_| differs(self.actuation, base.and_then(|b| b.actuation)))
        {
            parts.push(format!("actuation = {v}"));
        }
        if let Some(t) = self.trigger
            && (base.is_none() || base.and_then(|b| b.trigger) != Some(t))
        {
            parts.push(format!(
                "trigger = \"{}\"",
                TRIGGERS.get(usize::from(t)).unwrap_or(&"normal")
            ));
        }
        let fields = [
            ("press", self.press, base.and_then(|b| b.press)),
            ("release", self.release, base.and_then(|b| b.release)),
            (
                "upper_deadzone",
                self.upper_deadzone,
                base.and_then(|b| b.upper_deadzone),
            ),
            (
                "lower_deadzone",
                self.lower_deadzone,
                base.and_then(|b| b.lower_deadzone),
            ),
        ];
        for (name, v, b) in fields {
            if let Some(v) = v.filter(|_| differs(v, b)) {
                parts.push(format!("{name} = {v}"));
            }
        }
        format!("{{ {} }}", parts.join(", "))
    }
}

/// A parsed config: key actions per (mode, layer, slot) and travel per
/// (mode, slot), each remembering where in the file it came from.
#[derive(Debug, Default)]
pub struct Config {
    pub actions: BTreeMap<(u8, u8, usize), Action>,
    pub travel: BTreeMap<(u8, usize), TravelSpec>,
    pub macros: BTreeMap<(u8, usize), Macro>,
}

/// Parse a declarative config:
///
/// ```toml
/// [m1.base]                 # m1..m3 . base | fn1 | fn2 | fn3
/// CAPS = "FN(2)"
/// "#58" = "LCTL+C"
///
/// [m1.travel]               # millimetres; unset fields stay as they are
/// "*" = { actuation = 1.5, trigger = "normal" }
/// W = { actuation = 0.5, trigger = "rapid", press = 0.1, release = 0.1 }
///
/// [m1.macros]               # 1..32 as in NuPhy IO; a chord, or steps with delays
/// 1 = "LCTL+Z"
/// 3 = ["LGUI down 1", "SPC down 1", "SPC up 1", "LGUI up"]
/// ```
pub fn parse_config(text: &str) -> Result<Config> {
    let table: toml::Table = text.parse().context("invalid TOML")?;
    let mut config = Config::default();
    let mut sources: HashMap<(u8, u8, usize), String> = HashMap::new();
    for (mode_key, sections) in &table {
        let mode = parse_mode(mode_key).with_context(|| format!("in [{mode_key}]"))?;
        let sections = sections
            .as_table()
            .with_context(|| format!("[{mode_key}] must be a table"))?;
        for (section, keys) in sections {
            let keys = keys
                .as_table()
                .with_context(|| format!("[{mode_key}.{section}] must be a table"))?;
            if section == "travel" {
                parse_travel(mode, mode_key, keys, &mut config)?;
                continue;
            }
            if section == "macros" {
                for (key, value) in keys {
                    let source = format!("{mode_key}.macros.{key}");
                    let index: usize = key
                        .parse::<usize>()
                        .ok()
                        .filter(|&n| (1..=MACRO_COUNT).contains(&n))
                        .map(|n| n - 1)
                        .with_context(|| {
                            format!("{source}: macro numbers are 1..={MACRO_COUNT}")
                        })?;
                    config
                        .macros
                        .insert((mode, index), Macro::parse(value, &source)?);
                }
                continue;
            }
            let layer =
                parse_layer(mode, section).with_context(|| format!("in [{mode_key}.{section}]"))?;
            for (key, value) in keys {
                let source = format!("{mode_key}.{section}.{key}");
                let slot = parse_position(key).with_context(|| format!("in {source}"))?;
                let text = value
                    .as_str()
                    .with_context(|| format!("{source} must be an action string like \"ESC\""))?;
                let action: Action = text.parse().with_context(|| format!("in {source}"))?;
                if let Some(previous) = sources.insert((mode, layer, slot), source.clone()) {
                    bail!("{source} assigns the same key as {previous}");
                }
                config.actions.insert((mode, layer, slot), action);
            }
        }
    }
    Ok(config)
}

fn parse_travel(mode: u8, mode_key: &str, keys: &toml::Table, config: &mut Config) -> Result<()> {
    let all = match keys.get("*") {
        Some(v) => TravelSpec::parse(v, &format!("{mode_key}.travel.\"*\""))?,
        None => TravelSpec::default(),
    };
    let mut per_key: BTreeMap<usize, TravelSpec> = BTreeMap::new();
    for (key, value) in keys.iter().filter(|(k, _)| k.as_str() != "*") {
        let source = format!("{mode_key}.travel.{key}");
        let slot = parse_position(key).with_context(|| format!("in {source}"))?;
        if per_key
            .insert(slot, TravelSpec::parse(value, &source)?)
            .is_some()
        {
            bail!("{source} names a key twice");
        }
    }
    if all == TravelSpec::default() && per_key.is_empty() {
        return Ok(());
    }
    for slot in 0..key_count() {
        let spec = all.merged(per_key.get(&slot).copied().unwrap_or_default());
        if spec != TravelSpec::default() {
            config.travel.insert((mode, slot), spec);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionChange {
    pub mode: u8,
    pub layer: u8,
    pub slot: usize,
    pub from: Action,
    pub to: Action,
}

impl ActionChange {
    pub fn describe(&self) -> String {
        format!(
            "{}.{} {}: {} -> {}",
            mode_label(self.mode),
            LAYER_NAMES[usize::from(self.layer)],
            position_name(self.slot),
            self.from,
            self.to
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MacroChange {
    pub mode: u8,
    pub index: usize,
    pub from: Macro,
    pub to: Macro,
}

impl MacroChange {
    pub fn describe(&self) -> String {
        let show = |m: &Macro| {
            if m.0.is_empty() {
                "(empty)".to_string()
            } else {
                m.to_toml()
            }
        };
        format!(
            "{}.macros {}: {} -> {}",
            mode_label(self.mode),
            self.index + 1,
            show(&self.from),
            show(&self.to)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TravelChange {
    pub mode: u8,
    pub slot: usize,
    pub from: TravelRecord,
    pub to: TravelRecord,
}

impl TravelChange {
    pub fn describe(&self) -> String {
        format!(
            "{}.travel {}: {} -> {}",
            mode_label(self.mode),
            position_name(self.slot),
            TravelSpec::of(&self.from).render(None),
            TravelSpec::of(&self.to).render(Some(&TravelSpec::of(&self.from)))
        )
    }
}

/// Changes that turn the device state into what `config` asks for. Refuses
/// to write actions this tool does not manage, or to overwrite them.
pub fn plan_changes(
    config: &Config,
    matrix: &Matrix,
    travel: &Travel,
    macros: &Macros,
) -> Result<Plan> {
    let mut actions = Vec::new();
    for (&(mode, layer, slot), &to) in &config.actions {
        let from = matrix.get(mode, layer, slot);
        if from == to {
            continue;
        }
        let change = ActionChange {
            mode,
            layer,
            slot,
            from,
            to,
        };
        if !to.is_writable() {
            bail!(
                "{}: this tool does not assign {to}; set it in NuPhy IO",
                change.describe()
            );
        }
        if !from.is_writable() {
            bail!(
                "{}: the key holds {from}, which points into tables this tool does not edit; change it in NuPhy IO first",
                change.describe()
            );
        }
        actions.push(change);
    }
    let mut records = Vec::new();
    for (&(mode, slot), spec) in &config.travel {
        let from = travel.get(mode, slot);
        let to = spec
            .apply_to(from)
            .with_context(|| format!("{}.travel {}", mode_label(mode), position_name(slot)))?;
        if from != to {
            records.push(TravelChange {
                mode,
                slot,
                from,
                to,
            });
        }
    }
    let mut macro_changes = Vec::new();
    let mut want_macros = macros.clone();
    for mode in 0..MODES {
        let wanted: Vec<_> = config
            .macros
            .range((mode, 0)..(mode, MACRO_COUNT))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let mut list = macros.decode(mode)?;
        for (&(_, index), to) in wanted {
            if list[index] != *to {
                macro_changes.push(MacroChange {
                    mode,
                    index,
                    from: list[index].clone(),
                    to: to.clone(),
                });
                list[index] = to.clone();
            }
        }
        if macro_changes.iter().any(|c| c.mode == mode) {
            let region =
                encode_region(&list).with_context(|| format!("{}.macros", mode_label(mode)))?;
            want_macros.set_region(mode, &region);
        }
    }
    Ok(Plan {
        actions,
        travel: records,
        macros: macro_changes,
        want_macros,
    })
}

/// What `apply` would change, and the macro regions it would write.
#[derive(Debug)]
pub struct Plan {
    pub actions: Vec<ActionChange>,
    pub travel: Vec<TravelChange>,
    pub macros: Vec<MacroChange>,
    pub want_macros: Macros,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.travel.is_empty() && self.macros.is_empty()
    }

    pub fn describe(&self) -> Vec<String> {
        self.actions
            .iter()
            .map(ActionChange::describe)
            .chain(self.travel.iter().map(TravelChange::describe))
            .chain(self.macros.iter().map(MacroChange::describe))
            .collect()
    }
}

/// Offline checks of a parsed config: every travel entry must fit a record
/// (ranges and steps of a stock record), and actions this tool does not write
/// are listed, since `apply` accepts them only where the keyboard already
/// holds them.
pub fn check_config(config: &Config) -> Result<Vec<String>> {
    let stock = TravelRecord([0xa2, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00]);
    for (&(mode, slot), spec) in &config.travel {
        spec.apply_to(stock)
            .with_context(|| format!("{}.travel {}", mode_label(mode), position_name(slot)))?;
    }
    for mode in 0..MODES {
        let mut list = vec![Macro::default(); MACRO_COUNT];
        for (&(_, index), m) in config.macros.range((mode, 0)..(mode, MACRO_COUNT)) {
            list[index] = m.clone();
        }
        encode_region(&list).with_context(|| format!("{}.macros", mode_label(mode)))?;
    }
    Ok(config
        .actions
        .iter()
        .filter(|(_, a)| !a.is_writable())
        .map(|(&(mode, layer, slot), a)| {
            format!(
                "{}.{} {} = {a}: not written by this tool; apply passes only if the keyboard already holds it",
                mode_label(mode),
                LAYER_NAMES[usize::from(layer)],
                position_name(slot)
            )
        })
        .collect())
}

/// One write: the region's byte address and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteChunk {
    pub addr: u16,
    pub data: Vec<u8>,
}

/// Group fixed-size entries into packets: only changed entries are written;
/// adjacent ones share a packet.
fn coalesce(mut entries: Vec<(u16, Vec<u8>)>) -> Vec<WriteChunk> {
    entries.sort_by_key(|(addr, _)| *addr);
    let mut chunks: Vec<WriteChunk> = Vec::new();
    for (addr, data) in entries {
        match chunks.last_mut() {
            Some(c)
                if usize::from(c.addr) + c.data.len() == usize::from(addr)
                    && c.data.len() + data.len() <= CHUNK
                    && usize::from(addr) % LAYER_BYTES != 0 =>
            {
                c.data.extend_from_slice(&data)
            }
            _ => chunks.push(WriteChunk { addr, data }),
        }
    }
    chunks
}

pub fn plan_action_writes(changes: &[ActionChange]) -> Vec<WriteChunk> {
    coalesce(
        changes
            .iter()
            .map(|c| (matrix_addr(c.mode, c.layer, c.slot), c.to.0.to_vec()))
            .collect(),
    )
}

/// Packets that turn `current` macro regions into `want`, skipping the
/// packet-sized windows that already match.
pub fn plan_macro_writes(current: &Macros, want: &Macros) -> Vec<WriteChunk> {
    let mut out = Vec::new();
    for mode in 0..MODES {
        let base = usize::from(mode) * MACRO_BYTES;
        for at in (0..MACRO_BYTES).step_by(CHUNK) {
            let end = (at + CHUNK).min(MACRO_BYTES);
            let (a, b) = (
                &current.0[base + at..base + end],
                &want.0[base + at..base + end],
            );
            if a != b {
                out.push(WriteChunk {
                    addr: (base + at) as u16,
                    data: b.to_vec(),
                });
            }
        }
    }
    out
}

pub fn plan_travel_writes(changes: &[TravelChange]) -> Vec<WriteChunk> {
    coalesce(
        changes
            .iter()
            .map(|c| (travel_addr(c.mode, c.slot), c.to.0.to_vec()))
            .collect(),
    )
}

/// Render the given modes as a config: actions that differ from the factory
/// matrix, and the travel table as a `"*"` default plus per-key differences.
pub fn export_config(
    modes: &[u8],
    current: &Matrix,
    factory: &Matrix,
    travel: &Travel,
    macros: &Macros,
) -> Result<String> {
    let mut out = String::from(
        "# nuphyctl he: key actions that differ from the factory matrix, and key travel (mm)\n# apply with: nuphyctl he apply <this file>\n",
    );
    for &mode in modes {
        let m = mode_label(mode);
        for layer in 0..LAYERS_PER_MODE {
            let changed: Vec<usize> = (0..key_count())
                .filter(|&s| current.get(mode, layer, s) != factory.get(mode, layer, s))
                .collect();
            if changed.is_empty() {
                continue;
            }
            out.push_str(&format!("\n[{m}.{}]\n", LAYER_NAMES[usize::from(layer)]));
            for slot in changed {
                out.push_str(&format!(
                    "{} = \"{}\"  # factory {}\n",
                    toml_key(&position_name(slot)),
                    current.get(mode, layer, slot),
                    factory.get(mode, layer, slot)
                ));
            }
        }
        let list = macros.decode(mode)?;
        if list.iter().any(|m| !m.0.is_empty()) {
            out.push_str(&format!("\n[{m}.macros]\n"));
            for (index, mac) in list.iter().enumerate() {
                if !mac.0.is_empty() {
                    out.push_str(&format!("{} = {}\n", index + 1, mac.to_toml()));
                }
            }
        }
        let specs: Vec<TravelSpec> = (0..key_count())
            .map(|s| TravelSpec::of(&travel.get(mode, s)))
            .collect();
        let mut counts: Vec<(String, usize, TravelSpec)> = Vec::new();
        for spec in &specs {
            let key = spec.render(None);
            match counts.iter_mut().find(|(k, _, _)| *k == key) {
                Some(entry) => entry.1 += 1,
                None => counts.push((key, 1, *spec)),
            }
        }
        let common = counts.iter().max_by_key(|(_, n, _)| *n).map(|(_, _, s)| *s);
        out.push_str(&format!("\n[{m}.travel]\n"));
        if let Some(common) = common {
            out.push_str(&format!("\"*\" = {}\n", common.render(None)));
            for (slot, spec) in specs.iter().enumerate() {
                if *spec != common {
                    out.push_str(&format!(
                        "{} = {}\n",
                        toml_key(&position_name(slot)),
                        spec.render(Some(&common))
                    ));
                }
            }
        }
    }
    Ok(out)
}

fn toml_key(name: &str) -> String {
    if name.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(bytes: [u8; 8]) -> TravelRecord {
        TravelRecord(bytes)
    }

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

    /// Records from a real Air60 HE export: switch 2 (Jade Pro), 1.50 mm
    /// normal and 0.50 mm Rapid Trigger with 0.01 mm sensitivity.
    #[test]
    fn decodes_travel_records() {
        let normal = record([0x02, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(normal.switch_type(), 2);
        assert_eq!(normal.trigger(), 0);
        assert_eq!(normal.actuation_mm(), 1.5);
        assert_eq!(normal.press_mm(), 0.01);
        assert_eq!(normal.upper_deadzone_mm(), 0.0);

        let rapid = record([0x02, 0x01, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(rapid.trigger(), 1);
        assert_eq!(rapid.actuation_mm(), 0.5);
    }

    #[test]
    fn travel_setters_keep_other_bits() {
        let mut r = record([0xa2, 0x21, 0x95, 0xe0, 0x09, 0x06, 0x00, 0x00]);
        r.set_actuation_mm(2.0).unwrap();
        r.set_press_mm(0.1).unwrap();
        r.set_trigger(2);
        assert_eq!(r.0[0], 0xa2);
        assert_eq!(r.0[1], 0x22);
        assert_eq!(r.0[3] & 0xfe, 0xe0);
        assert_eq!(r.actuation_mm(), 2.0);
        assert_eq!(r.press_mm(), 0.1);
        assert_eq!(r.upper_deadzone_mm(), 0.04);
        r.set_upper_deadzone_mm(0.0).unwrap();
        assert_eq!(r.upper_deadzone_mm(), 0.0);
        assert_eq!(r.press_mm(), 0.1);
        assert!(r.set_actuation_mm(0.05).is_err());
        assert!(r.set_actuation_mm(3.4).is_err());
        assert!(r.set_actuation_mm(1.234).is_err());
        assert!(r.set_lower_deadzone_mm(1.3).is_err());
    }

    fn blank_matrix() -> Matrix {
        let mut m = Matrix(vec![0; Matrix::BYTES]);
        for mode in 0..MODES {
            for layer in 0..LAYERS_PER_MODE {
                for slot in 0..key_count() {
                    m.set(mode, layer, slot, Action::NONE);
                }
            }
        }
        m
    }

    fn blank_macros() -> Macros {
        let region = encode_region(&vec![Macro::default(); MACRO_COUNT]).unwrap();
        Macros(region.repeat(usize::from(MODES)))
    }

    fn blank_travel() -> Travel {
        let mut t = Travel(vec![0; Travel::BYTES]);
        for mode in 0..MODES {
            for slot in 0..key_count() {
                t.set(mode, slot, record([0x02, 0, 0x95, 0, 0, 0, 0, 0]));
            }
        }
        t
    }

    const SAMPLE: &str = r##"
        [m1.base]
        CAPS = "FN(2)"
        "#58" = "LCTL+C"

        [m1.fn2]
        I = "UP"

        [m1.travel]
        "*" = { actuation = 1.5 }
        W = { actuation = 0.5, trigger = "rapid", press = 0.1 }

        [m1.macros]
        1 = "LCTL+Z"
    "##;

    #[test]
    fn parses_config() {
        let c = parse_config(SAMPLE).unwrap();
        assert_eq!(c.actions[&(0, 0, 28)], Action([0xf0, 0xff, 2]));
        assert_eq!(c.actions[&(0, 0, 58)], Action([0x10, 0x01, 0x06]));
        assert_eq!(c.actions[&(0, 2, 22)].to_string(), "UP");
        assert_eq!(c.travel.len(), 61);
        let w = c.travel[&(0, 16)];
        assert_eq!(w.actuation, Some(0.5));
        assert_eq!(w.trigger, Some(1));
        assert_eq!(w.press, Some(0.1));
        assert_eq!(c.travel[&(0, 0)].actuation, Some(1.5));
        assert_eq!(c.travel[&(0, 0)].trigger, None);
    }

    #[test]
    fn config_errors() {
        assert!(parse_config("[m4.base]\nESC = \"ESC\"").is_err());
        assert!(parse_config("[m1.fn9]\nESC = \"ESC\"").is_err());
        assert!(parse_config("[m1.base]\nF1 = \"ESC\"").is_err());
        assert!(parse_config("[m1.base]\nESC = \"NOPE\"").is_err());
        assert!(parse_config("[m1.base]\nESC = 41").is_err());
        assert!(parse_config("[m1.base]\nESC = \"ESC\"\n\"#0\" = \"F1\"").is_err());
        assert!(parse_config("[m1.travel]\nW = { actuation = \"x\" }").is_err());
        assert!(parse_config("[m1.travel]\nW = { speed = 1 }").is_err());
        assert!(parse_config("[m1.travel]\nW = { trigger = \"fast\" }").is_err());
        assert!(parse_config("[m1.macros]\n33 = \"LCTL+C\"").is_err());
        assert!(parse_config("[m1.macros]\n0 = \"LCTL+C\"").is_err());
        assert!(parse_config("[m1.macros]\n0 = [\"C sideways\"]").is_err());
    }

    #[test]
    fn plans_and_is_idempotent() {
        let config = parse_config(SAMPLE).unwrap();
        let matrix = blank_matrix();
        let travel = blank_travel();
        let macros = blank_macros();
        let plan = plan_changes(&config, &matrix, &travel, &macros).unwrap();
        assert_eq!(plan.actions.len(), 3);
        assert_eq!(plan.travel.len(), 1);
        assert_eq!(plan.travel[0].slot, 16);
        assert_eq!(plan.travel[0].to.trigger(), 1);
        assert_eq!(plan.macros.len(), 1);
        assert!(!plan_macro_writes(&macros, &plan.want_macros).is_empty());

        let mut m2 = matrix.clone();
        for c in &plan.actions {
            m2.set(c.mode, c.layer, c.slot, c.to);
        }
        let mut t2 = travel.clone();
        for c in &plan.travel {
            t2.set(c.mode, c.slot, c.to);
        }
        let again = plan_changes(&config, &m2, &t2, &plan.want_macros).unwrap();
        assert!(again.is_empty(), "{:?}", again.describe());
    }

    #[test]
    fn refuses_unmanaged_actions() {
        let mut matrix = blank_matrix();
        let travel = blank_travel();
        let macros = blank_macros();
        let config = parse_config("[m1.base]\nESC = \"0x950020\"").unwrap();
        assert!(plan_changes(&config, &matrix, &travel, &macros).is_err());
        let config = parse_config("[m1.base]\nESC = \"FACTORY_RESET\"").unwrap();
        assert!(plan_changes(&config, &matrix, &travel, &macros).is_err());
        matrix.set(0, 0, 0, Action([0x95, 0, 0x20]));
        let config = parse_config("[m1.base]\nESC = \"ESC\"").unwrap();
        assert!(plan_changes(&config, &matrix, &travel, &macros).is_err());
        let config = parse_config("[m1.base]\nESC = \"0x950020\"").unwrap();
        assert!(
            plan_changes(&config, &matrix, &travel, &macros)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn checks_config_offline() {
        let notes = check_config(&parse_config(SAMPLE).unwrap()).unwrap();
        assert!(notes.is_empty());
        let notes = check_config(&parse_config("[m2.base]\nS = \"0x950020\"").unwrap()).unwrap();
        assert_eq!(notes.len(), 1);
        assert!(
            check_config(&parse_config("[m1.travel]\nW = { actuation = 3.5 }").unwrap()).is_err()
        );
        assert!(
            check_config(&parse_config("[m1.travel]\nW = { press = 0.015 }").unwrap()).is_err()
        );
    }

    #[test]
    fn coalesces_writes() {
        let ch = |slot, to: [u8; 3]| ActionChange {
            mode: 0,
            layer: 0,
            slot,
            from: Action::NONE,
            to: Action(to),
        };
        let plan = plan_action_writes(&[
            ch(2, [0x10, 0, 5]),
            ch(1, [0x10, 0, 4]),
            ch(5, [0x10, 0, 6]),
        ]);
        assert_eq!(
            plan,
            vec![
                WriteChunk {
                    addr: 3,
                    data: vec![0x10, 0, 4, 0x10, 0, 5]
                },
                WriteChunk {
                    addr: 15,
                    data: vec![0x10, 0, 6]
                },
            ]
        );
        let all: Vec<ActionChange> = (0..61).map(|s| ch(s, [0x10, 0, 4])).collect();
        let plan = plan_action_writes(&all);
        assert!(
            plan.iter()
                .all(|c| c.data.len() <= CHUNK && c.data.len() % 3 == 0)
        );
        assert_eq!(plan.iter().map(|c| c.data.len()).sum::<usize>(), 183);
    }

    #[test]
    fn export_round_trips() {
        let factory = blank_matrix();
        let mut current = blank_matrix();
        current.set(0, 0, 28, Action([0xf0, 0xff, 2]));
        current.set(0, 1, 0, Action([0x10, 0, 0x35]));
        let mut travel = blank_travel();
        let mut w = travel.get(0, 16);
        w.set_actuation_mm(0.5).unwrap();
        w.set_trigger(1);
        travel.set(0, 16, w);

        let mut macros = blank_macros();
        let mut list = vec![Macro::default(); MACRO_COUNT];
        list[4] = Macro::chord("LCTL+C").unwrap();
        macros.set_region(0, &encode_region(&list).unwrap());

        let text = export_config(&[0], &current, &factory, &travel, &macros).unwrap();
        assert!(text.contains("[m1.base]\nCAPS = \"FN(2)\""), "{text}");
        assert!(text.contains("[m1.macros]\n5 = \"LCTL+C\""), "{text}");
        assert!(
            text.contains("\"*\" = { actuation = 1.5, trigger = \"normal\""),
            "{text}"
        );
        assert!(
            text.contains("W = { actuation = 0.5, trigger = \"rapid\" }"),
            "{text}"
        );

        let config = parse_config(&text).unwrap();
        let plan = plan_changes(&config, &current, &travel, &macros).unwrap();
        assert!(plan.is_empty(), "{:?}", plan.describe());
        let plan = plan_changes(&config, &factory, &blank_travel(), &blank_macros()).unwrap();
        assert_eq!(plan.actions.len(), 2);
        assert_eq!(plan.travel.len(), 1);
        assert_eq!(plan.macros.len(), 1);
    }
}
