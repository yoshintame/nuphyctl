//! Per-key travel: the 8-byte record of the keyboard, and the declarative
//! spec a config file gives for it. Lengths are whole micrometres inside and
//! millimetres at the TOML boundary.

use crate::he::region::TRAVEL_RECORD;
use anyhow::{Context, Result, bail};
use std::fmt;
use std::ops::RangeInclusive;

/// One key's travel settings. Setters change only their own bits, so
/// unknown bits (the switch type, the firmware's check mark, Snap Tap
/// priority) survive a round trip.
///
/// ```text
/// 0      bits 0-4 switch type, bits 5-7 a mark the firmware expects
/// 1      bits 0-3 trigger, bits 4-7 Snap Tap priority
/// 2-3    bits 0-8 actuation point; bits 9-10 / 11-12 press / release step
/// 4-5    bits 0-8 press sensitivity, bits 9-15 upper dead zone
/// 6-7    bits 0-8 release sensitivity, bits 9-15 lower dead zone
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TravelRecord(pub [u8; TRAVEL_RECORD]);

const NINE_BITS: u16 = 0x1ff;
const TRAVEL_STEP_UM: u32 = 10;
const DEAD_ZONE_MAX: u16 = 0x7f;
/// What a note says about a value this tool does not write.
pub const ONLY_IF_HELD: &str = "apply passes only if the keyboard already holds it";

/// Rapid Trigger sensitivity step selected by two bits of byte 3.
fn step_um(bits: u8) -> u32 {
    match bits & 3 {
        1 => 5,
        2 => 1,
        _ => 10,
    }
}

impl TravelRecord {
    fn word(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.0[at], self.0[at + 1]])
    }

    fn set_word(&mut self, at: usize, v: u16) {
        [self.0[at], self.0[at + 1]] = v.to_le_bytes();
    }

    /// The record's bytes in hex, as `travel --json` shows them.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn switch_type(&self) -> u8 {
        self.0[0] & 0x1f
    }

    pub fn trigger(&self) -> Trigger {
        Trigger::from_bits(self.0[1] & 0x0f)
    }

    pub fn set_trigger(&mut self, trigger: Trigger) {
        self.0[1] = (self.0[1] & 0xf0) | trigger.bits();
    }

    fn press_step(&self) -> u32 {
        step_um(self.0[3] >> 1)
    }

    fn release_step(&self) -> u32 {
        step_um(self.0[3] >> 3)
    }

    pub fn actuation_um(&self) -> u32 {
        (u32::from(self.word(2) & NINE_BITS) + 1) * TRAVEL_STEP_UM
    }

    pub fn press_um(&self) -> u32 {
        (u32::from(self.word(4) & NINE_BITS) + 1) * self.press_step()
    }

    pub fn release_um(&self) -> u32 {
        (u32::from(self.word(6) & NINE_BITS) + 1) * self.release_step()
    }

    pub fn upper_deadzone_um(&self) -> u32 {
        dead_um(self.word(4) >> 9)
    }

    pub fn lower_deadzone_um(&self) -> u32 {
        dead_um(self.word(6) >> 9)
    }

    fn set_low9(&mut self, at: usize, value: u16) {
        let w = self.word(at);
        self.set_word(at, (w & !NINE_BITS) | value);
    }

    fn set_high7(&mut self, at: usize, value: u16) {
        let w = self.word(at);
        self.set_word(at, (w & NINE_BITS) | (value << 9));
    }

    fn store_actuation_um(&mut self, um: u32) -> Result<()> {
        self.set_low9(2, units(um, TRAVEL_STEP_UM)?);
        Ok(())
    }

    fn store_press_um(&mut self, um: u32) -> Result<()> {
        self.set_low9(4, units(um, self.press_step())?);
        Ok(())
    }

    fn store_release_um(&mut self, um: u32) -> Result<()> {
        self.set_low9(6, units(um, self.release_step())?);
        Ok(())
    }

    fn store_upper_deadzone_um(&mut self, um: u32) -> Result<()> {
        self.set_high7(4, dead_units(um)?);
        Ok(())
    }

    fn store_lower_deadzone_um(&mut self, um: u32) -> Result<()> {
        self.set_high7(6, dead_units(um)?);
        Ok(())
    }
}

/// Records store `value - 1` steps: 1.50 mm at 0.01 mm steps is 149.
fn units(um: u32, step: u32) -> Result<u16> {
    if !um.is_multiple_of(step) {
        bail!(
            "{} mm is not a multiple of the {} mm step",
            mm(um),
            mm(step)
        );
    }
    let v = um / step;
    if !(1..=u32::from(NINE_BITS) + 1).contains(&v) {
        bail!("{} mm does not fit the record", mm(um));
    }
    Ok((v - 1) as u16)
}

fn dead_um(v: u16) -> u32 {
    if v == 0 {
        0
    } else {
        (u32::from(v) + 1) * TRAVEL_STEP_UM
    }
}

/// Dead zones store `value - 1` steps in 7 bits, and 0 for none.
fn dead_units(um: u32) -> Result<u16> {
    if um == 0 {
        return Ok(0);
    }
    let v = units(um, TRAVEL_STEP_UM)?;
    if !(1..=DEAD_ZONE_MAX).contains(&v) {
        bail!("dead zone {} mm does not fit the record", mm(um));
    }
    Ok(v)
}

/// Micrometres as a millimetre literal without trailing zeros: `1.5`, `0.01`, `0`.
pub fn mm(um: u32) -> String {
    let text = format!("{}.{:03}", um / 1000, um % 1000);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn um_from_toml(value: &toml::Value, source: &str) -> Result<u32> {
    let mm = match value {
        toml::Value::Float(f) => *f,
        toml::Value::Integer(i) => *i as f64,
        _ => bail!("{source} must be a number of millimetres"),
    };
    let um = (mm * 1000.0).round();
    if !(0.0..=f64::from(u32::MAX)).contains(&um) || (mm * 1000.0 - um).abs() > 1e-6 {
        bail!("{source}: {mm} mm is negative or finer than 0.001 mm");
    }
    Ok(um as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Normal,
    Rapid,
    Continuous,
    /// A value without a name; kept as it is.
    Raw(u8),
}

impl Trigger {
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x0f {
            0 => Self::Normal,
            1 => Self::Rapid,
            2 => Self::Continuous,
            n => Self::Raw(n),
        }
    }

    pub fn bits(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Rapid => 1,
            Self::Continuous => 2,
            Self::Raw(n) => n & 0x0f,
        }
    }

    fn name(self) -> Option<&'static str> {
        match self {
            Self::Normal => Some("normal"),
            Self::Rapid => Some("rapid"),
            Self::Continuous => Some("continuous"),
            Self::Raw(_) => None,
        }
    }

    /// The TOML literal: a quoted name, or the raw number.
    fn to_toml(self) -> String {
        match self.name() {
            Some(name) => format!("\"{name}\""),
            None => self.bits().to_string(),
        }
    }

    fn from_toml(value: &toml::Value, source: &str) -> Result<Self> {
        let wanted = || format!("{source} must be normal, rapid, continuous or a raw 0..=15");
        match value {
            toml::Value::String(name) => [Self::Normal, Self::Rapid, Self::Continuous]
                .into_iter()
                .find(|t| t.name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
                .with_context(wanted),
            toml::Value::Integer(n) if (0..=15).contains(n) => Ok(Self::from_bits(*n as u8)),
            _ => bail!(wanted()),
        }
    }
}

impl fmt::Display for Trigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "{}", self.bits()),
        }
    }
}

/// The length fields of a travel record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldId {
    Actuation,
    Press,
    Release,
    UpperDeadzone,
    LowerDeadzone,
}

/// A length field of a travel record, by its config name.
pub struct Field {
    pub id: FieldId,
    pub name: &'static str,
    pub get: fn(&TravelRecord) -> u32,
    /// Stores any value the record can hold, inside `writable` or not.
    store: fn(&mut TravelRecord, u32) -> Result<()>,
    /// Values this tool writes. The keyboard may hold others, which `apply`
    /// leaves alone as long as the config asks for what is already there.
    writable: RangeInclusive<u32>,
    /// Counted in Rapid Trigger steps, whose size the record itself selects.
    pub sensitivity: bool,
}

impl Field {
    pub fn set(&self, record: &mut TravelRecord, um: u32) -> Result<()> {
        if !self.writable.contains(&um) {
            bail!("{}", self.outside_writable(um));
        }
        (self.store)(record, um).with_context(|| self.name)
    }

    fn outside_writable(&self, um: u32) -> String {
        format!(
            "{} {} mm is outside what this tool writes ({}..={} mm)",
            self.name,
            mm(um),
            mm(*self.writable.start()),
            mm(*self.writable.end())
        )
    }
}

pub static FIELDS: [Field; 5] = [
    Field {
        id: FieldId::Actuation,
        name: "actuation",
        get: TravelRecord::actuation_um,
        store: TravelRecord::store_actuation_um,
        writable: 100..=3300,
        sensitivity: false,
    },
    Field {
        id: FieldId::Press,
        name: "press",
        get: TravelRecord::press_um,
        store: TravelRecord::store_press_um,
        writable: 1..=3300,
        sensitivity: true,
    },
    Field {
        id: FieldId::Release,
        name: "release",
        get: TravelRecord::release_um,
        store: TravelRecord::store_release_um,
        writable: 1..=3300,
        sensitivity: true,
    },
    Field {
        id: FieldId::UpperDeadzone,
        name: "upper_deadzone",
        get: TravelRecord::upper_deadzone_um,
        store: TravelRecord::store_upper_deadzone_um,
        writable: 0..=1280,
        sensitivity: false,
    },
    Field {
        id: FieldId::LowerDeadzone,
        name: "lower_deadzone",
        get: TravelRecord::lower_deadzone_um,
        store: TravelRecord::store_lower_deadzone_um,
        writable: 0..=1280,
        sensitivity: false,
    },
];

/// Rapid Trigger step sizes a record can select, as byte 3 encodes them for
/// both press and release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Um10,
    Um5,
    Um1,
}

impl TravelRecord {
    /// A stock record (switch 2, 1.50 mm, normal trigger) with `step` for
    /// both press and release: what offline checks validate against.
    pub fn stock(step: Step) -> Self {
        let bits = match step {
            Step::Um10 => 0,
            Step::Um5 => 1,
            Step::Um1 => 2,
        };
        Self([0xa2, 0, 0x95, (bits << 1) | (bits << 3), 0, 0, 0, 0])
    }
}

/// What a config travel entry asks for; unset fields keep the keyboard's value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TravelSpec {
    pub trigger: Option<Trigger>,
    /// Micrometres, in `FIELDS` order.
    pub lengths: [Option<u32>; 5],
}

impl TravelSpec {
    /// Everything `record` holds.
    pub fn of(record: &TravelRecord) -> Self {
        Self {
            trigger: Some(record.trigger()),
            lengths: FIELDS.each_ref().map(|f| Some((f.get)(record))),
        }
    }

    /// `over` field by field on top of `self`.
    pub fn merged(self, over: Self) -> Self {
        let mut lengths = self.lengths;
        for (l, o) in lengths.iter_mut().zip(over.lengths) {
            *l = o.or(*l);
        }
        Self {
            trigger: over.trigger.or(self.trigger),
            lengths,
        }
    }

    pub fn length(&self, id: FieldId) -> Option<u32> {
        self.lengths[id as usize]
    }

    /// `record` with this spec's fields set. A field that already holds the
    /// asked value is left alone, so values outside the ranges this tool
    /// writes survive an export and apply untouched.
    pub fn apply_to(&self, mut record: TravelRecord) -> Result<TravelRecord> {
        if let Some(t) = self.trigger
            && t != record.trigger()
        {
            if let Trigger::Raw(n) = t {
                bail!("trigger {n} has no name, and this tool writes only named ones");
            }
            record.set_trigger(t);
        }
        for field in &FIELDS {
            if let Some(um) = self.length(field.id)
                && um != (field.get)(&record)
            {
                field.set(&mut record, um)?;
            }
        }
        Ok(record)
    }

    /// Checks without a keyboard: every value must fit a record, or the
    /// entry is an error. What only the keyboard can settle comes back as
    /// notes: a sensitivity that needs a finer step than 0.01 mm (which step
    /// a key uses is known at apply), and a value outside what this tool
    /// writes, or a trigger without a name (apply passes only if the key
    /// already holds it), and a sensitivity too coarse for the finer steps.
    pub fn check_offline(&self) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        if let Some(Trigger::Raw(n)) = self.trigger {
            notes.push(format!("trigger {n} has no name; {ONLY_IF_HELD}"));
        }
        for field in &FIELDS {
            let Some(um) = self.length(field.id) else {
                continue;
            };
            let fits = |step| (field.store)(&mut TravelRecord::stock(step), um);
            if let Err(err) = fits(Step::Um10) {
                if !field.sensitivity
                    || [Step::Um5, Step::Um1].into_iter().all(|s| fits(s).is_err())
                {
                    return Err(err.context(field.name));
                }
                notes.push(format!(
                    "{} needs a finer Rapid Trigger step than 0.01 mm; apply fails unless the key uses one",
                    field.name
                ));
            } else if field.sensitivity
                && [Step::Um5, Step::Um1].into_iter().any(|s| fits(s).is_err())
            {
                notes.push(format!(
                    "{} {} mm does not fit a key with a finer Rapid Trigger step; apply fails on such keys",
                    field.name,
                    mm(um)
                ));
            }
            if !field.writable.contains(&um) {
                notes.push(format!("{}; {ONLY_IF_HELD}", field.outside_writable(um)));
            }
        }
        Ok(notes)
    }

    pub fn parse(value: &toml::Value, source: &str) -> Result<Self> {
        let table = value.as_table().with_context(|| {
            format!("{source} must be an inline table like {{ actuation = 1.5 }}")
        })?;
        let mut spec = Self::default();
        for (key, v) in table {
            let at = format!("{source}.{key}");
            if key == "trigger" {
                spec.trigger = Some(Trigger::from_toml(v, &at)?);
                continue;
            }
            let field = FIELDS.iter().find(|f| f.name == key).with_context(|| {
                let names: Vec<&str> = FIELDS.iter().map(|f| f.name).collect();
                format!(
                    "unknown field {key:?} in {source} (expected trigger, {})",
                    names.join(", ")
                )
            })?;
            spec.lengths[field.id as usize] = Some(um_from_toml(v, &at)?);
        }
        Ok(spec)
    }

    /// An inline table with the fields that differ from `base` (all set
    /// fields when there is no base).
    pub fn render(&self, base: Option<&Self>) -> String {
        let mut parts = Vec::new();
        if let Some(t) = self.trigger
            && base.is_none_or(|b| b.trigger != self.trigger)
        {
            parts.push(format!("trigger = {}", t.to_toml()));
        }
        for field in &FIELDS {
            if let Some(um) = self.length(field.id)
                && base.is_none_or(|b| b.length(field.id) != self.length(field.id))
            {
                parts.push(format!("{} = {}", field.name, mm(um)));
            }
        }
        format!("{{ {} }}", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(bytes: [u8; 8]) -> TravelRecord {
        TravelRecord(bytes)
    }

    fn set(record: &mut TravelRecord, id: FieldId, um: u32) -> Result<()> {
        FIELDS[id as usize].set(record, um)
    }

    /// Records from a real Air60 HE export: switch 2 (Jade Pro), 1.50 mm
    /// normal and 0.50 mm Rapid Trigger with 0.01 mm sensitivity.
    #[test]
    fn decodes_real_records() {
        let normal = record([0x02, 0x00, 0x95, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(normal.switch_type(), 2);
        assert_eq!(normal.trigger(), Trigger::Normal);
        assert_eq!(normal.actuation_um(), 1500);
        assert_eq!(normal.press_um(), 10);
        assert_eq!(normal.upper_deadzone_um(), 0);

        let rapid = record([0x02, 0x01, 0x31, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(rapid.trigger(), Trigger::Rapid);
        assert_eq!(rapid.actuation_um(), 500);
    }

    #[test]
    fn setters_keep_other_bits() {
        let mut r = record([0xa2, 0x21, 0x95, 0xe0, 0x09, 0x06, 0x00, 0x00]);
        set(&mut r, FieldId::Actuation, 2000).unwrap();
        set(&mut r, FieldId::Press, 100).unwrap();
        r.set_trigger(Trigger::Continuous);
        assert_eq!(r.0[0], 0xa2);
        assert_eq!(r.0[1], 0x22);
        assert_eq!(r.0[3] & 0xfe, 0xe0);
        assert_eq!(r.actuation_um(), 2000);
        assert_eq!(r.press_um(), 100);
        assert_eq!(r.upper_deadzone_um(), 40);
        set(&mut r, FieldId::UpperDeadzone, 0).unwrap();
        assert_eq!(r.upper_deadzone_um(), 0);
        assert_eq!(r.press_um(), 100);
        for (id, um) in [
            (FieldId::Actuation, 50),
            (FieldId::Actuation, 3400),
            (FieldId::Actuation, 1234),
            (FieldId::LowerDeadzone, 1300),
            (FieldId::LowerDeadzone, 10),
        ] {
            assert!(set(&mut r, id, um).is_err(), "{id:?} {um}");
        }
    }

    #[test]
    fn finer_steps() {
        let mut r = record([0x02, 0, 0x95, 0x04, 0, 0, 0, 0]);
        set(&mut r, FieldId::Press, 105).unwrap();
        assert_eq!(r.press_um(), 105);
        assert!(
            set(&mut r, FieldId::Press, 600).is_err(),
            "599 steps do not fit 9 bits"
        );
    }

    #[test]
    fn triggers() {
        assert_eq!(Trigger::from_bits(3), Trigger::Raw(3));
        assert_eq!(Trigger::Raw(3).bits(), 3);
        let mut r = record([0x02, 0xf3, 0x95, 0, 0, 0, 0, 0]);
        assert_eq!(r.trigger(), Trigger::Raw(3));
        r.set_trigger(Trigger::Rapid);
        assert_eq!(r.0[1], 0xf1);
    }

    #[test]
    fn fields_line_up_with_their_ids() {
        for (i, f) in FIELDS.iter().enumerate() {
            assert_eq!(f.id as usize, i, "{}", f.name);
        }
    }

    /// A value the keyboard holds outside the ranges this tool writes (here
    /// press 4.01 mm) exports and applies back without being touched.
    #[test]
    fn unchanged_out_of_range_values_round_trip() {
        let mut r = record([0x02, 0, 0x95, 0, 0x90, 0x01, 0, 0]);
        assert_eq!(r.press_um(), 4010);
        let spec = TravelSpec::of(&r);
        assert_eq!(spec.apply_to(r).unwrap(), r);
        set(&mut r, FieldId::Actuation, 1000).unwrap();
        assert!(
            TravelSpec {
                lengths: [None, Some(4020), None, None, None],
                ..Default::default()
            }
            .apply_to(r)
            .is_err()
        );
    }

    #[test]
    fn offline_checks() {
        let spec = |id: FieldId, um| {
            let mut lengths = [None; 5];
            lengths[id as usize] = Some(um);
            TravelSpec {
                lengths,
                ..Default::default()
            }
        };
        assert!(
            spec(FieldId::Press, 100)
                .check_offline()
                .unwrap()
                .is_empty()
        );
        let finer = spec(FieldId::Press, 105).check_offline().unwrap();
        assert!(finer.len() == 1 && finer[0].contains("finer"), "{finer:?}");
        assert_eq!(spec(FieldId::Press, 3).check_offline().unwrap().len(), 1);
        let coarse = spec(FieldId::Press, 600).check_offline().unwrap();
        assert!(
            coarse.len() == 1 && coarse[0].contains("finer Rapid Trigger step;"),
            "{coarse:?}"
        );
        let raw = TravelSpec {
            trigger: Some(Trigger::Raw(7)),
            ..Default::default()
        };
        let notes = raw.check_offline().unwrap();
        assert!(
            notes.len() == 1 && notes[0].contains("already holds"),
            "{notes:?}"
        );
        let mut held = record([0x02, 0x07, 0x95, 0, 0, 0, 0, 0]);
        assert_eq!(raw.apply_to(held).unwrap(), held, "kept when already held");
        held.set_trigger(Trigger::Normal);
        assert!(raw.apply_to(held).is_err(), "never written");

        for (id, um) in [(FieldId::Actuation, 3500), (FieldId::Press, 4010)] {
            let notes = spec(id, um).check_offline().unwrap();
            assert!(
                notes.iter().any(|n| n.contains("already holds")),
                "{notes:?}"
            );
        }
        for (id, um) in [
            (FieldId::Actuation, 5130),
            (FieldId::Actuation, 1234),
            (FieldId::Press, 5130),
            (FieldId::UpperDeadzone, 1290),
            (FieldId::UpperDeadzone, 10),
        ] {
            assert!(spec(id, um).check_offline().is_err(), "{id:?} {um}");
        }
        assert_eq!(TravelRecord::stock(Step::Um1).press_um(), 1);
        assert_eq!(TravelRecord::stock(Step::Um5).release_um(), 5);
    }

    #[test]
    fn millimetres() {
        assert_eq!(mm(1500), "1.5");
        assert_eq!(mm(10), "0.01");
        assert_eq!(mm(0), "0");
        assert_eq!(mm(1000), "1");
        assert_eq!(mm(105), "0.105");
        let t: toml::Table = "a = 1.5\nb = 2\nc = 0.0105\nd = -1.0".parse().unwrap();
        assert_eq!(um_from_toml(&t["a"], "a").unwrap(), 1500);
        assert_eq!(um_from_toml(&t["b"], "b").unwrap(), 2000);
        assert!(um_from_toml(&t["c"], "c").is_err());
        assert!(um_from_toml(&t["d"], "d").is_err());
    }

    #[test]
    fn specs_parse_render_and_apply() {
        let t: toml::Table =
            "w = { actuation = 0.5, trigger = \"rapid\", press = 0.1 }\nx = { trigger = 5 }"
                .parse()
                .unwrap();
        let w = TravelSpec::parse(&t["w"], "w").unwrap();
        assert_eq!(w.trigger, Some(Trigger::Rapid));
        assert_eq!(w.length(FieldId::Actuation), Some(500));
        assert_eq!(w.length(FieldId::Press), Some(100));
        assert_eq!(w.length(FieldId::Release), None);
        assert_eq!(
            w.render(None),
            "{ trigger = \"rapid\", actuation = 0.5, press = 0.1 }"
        );
        let x = TravelSpec::parse(&t["x"], "x").unwrap();
        assert_eq!(x.render(None), "{ trigger = 5 }");

        let stock = record([0x02, 0, 0x95, 0, 0, 0, 0, 0]);
        let applied = w.apply_to(stock).unwrap();
        assert_eq!(
            TravelSpec::of(&applied).render(Some(&TravelSpec::of(&stock))),
            "{ trigger = \"rapid\", actuation = 0.5, press = 0.1 }"
        );
        assert_eq!(w.merged(x).trigger, Some(Trigger::Raw(5)));
        assert_eq!(w.merged(x).length(FieldId::Actuation), Some(500));

        let bad: toml::Table = "a = { speed = 1 }\nb = { trigger = 16 }\nc = { actuation = \"x\" }"
            .parse()
            .unwrap();
        for k in ["a", "b", "c"] {
            assert!(TravelSpec::parse(&bad[k], k).is_err(), "{k}");
        }
    }
}
