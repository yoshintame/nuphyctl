//! The declarative config: parsing, offline checks and export.
//!
//! ```toml
//! [m1.base]                 # mode m1..m3 . layer, numbered across modes as in NuPhy IO
//! CAPS = "FN(2)"            # momentary layer, absolute number
//! "#58" = "LCTL+C"
//!
//! [m2.fn5]                  # m2's layers: base (fn4), fn5, fn6, fn7
//! I = "UP"
//!
//! [m1.travel]               # millimetres; unset fields keep the keyboard's value
//! "*" = { trigger = "normal", actuation = 1.5 }
//! W = { trigger = "rapid", actuation = 0.5, press = 0.1, release = 0.1 }
//!
//! [m1.macros]               # 1..32 as in NuPhy IO; a chord, or steps with delays
//! 1 = "LCTL+Z"
//! 3 = ["LGUI down 1", "SPC down 1", "SPC up 1", "LGUI up"]
//! ```

use crate::he::actions::Action;
use crate::he::layout::{
    KeyRef, MacroRef, TravelRef, key_count, layer_name, mode_label, parse_layer, parse_mode,
    parse_position, position_name,
};
use crate::he::macros::{Macro, encode_region};
use crate::he::region::{LAYERS_PER_MODE, MACRO_COUNT, MODES};
use crate::he::state::{Matrix, State};
use crate::he::travel::TravelSpec;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

/// A mode's travel section: `"*"` for every key, and keys of their own on
/// top of it, field by field.
#[derive(Debug, Default)]
pub struct TravelSection {
    pub all: TravelSpec,
    pub keys: BTreeMap<usize, TravelSpec>,
}

impl TravelSection {
    /// What the section asks of one key, if anything.
    pub fn spec(&self, slot: usize) -> Option<TravelSpec> {
        let spec = self
            .all
            .merged(self.keys.get(&slot).copied().unwrap_or_default());
        (spec != TravelSpec::default()).then_some(spec)
    }

    /// The written entries with their labels: `"*"`, then each key.
    fn entries(&self, mode: u8) -> impl Iterator<Item = (String, &TravelSpec)> {
        let all = (self.all != TravelSpec::default())
            .then(|| (format!("{}.travel \"*\"", mode_label(mode)), &self.all));
        all.into_iter().chain(
            self.keys
                .iter()
                .map(move |(&slot, spec)| (TravelRef { mode, slot }.to_string(), spec)),
        )
    }
}

#[derive(Debug, Default)]
pub struct Config {
    pub actions: BTreeMap<KeyRef, Action>,
    pub travel: BTreeMap<u8, TravelSection>,
    pub macros: BTreeMap<MacroRef, Macro>,
}

impl Config {
    /// One line for `check`: what the file sets.
    pub fn summary(&self) -> String {
        let travel: usize = self
            .travel
            .iter()
            .map(|(&mode, section)| section.entries(mode).count())
            .sum();
        format!(
            "{} key action(s), {travel} travel entr(ies), {} macro(s)",
            self.actions.len(),
            self.macros.len()
        )
    }
}

pub fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_config(&text).with_context(|| format!("invalid config {}", path.display()))
}

/// Two spellings of one thing (`ESC` and `ESCAPE`, `1` and `01`, `[m1.travel]`
/// and `[1.travel]`) are an error naming both.
fn claim<K: Ord>(seen: &mut BTreeMap<K, String>, key: K, source: &str) -> Result<()> {
    if let Some(previous) = seen.insert(key, source.to_string()) {
        bail!("{source} sets the same thing as {previous}");
    }
    Ok(())
}

pub fn parse_config(text: &str) -> Result<Config> {
    let table: toml::Table = text.parse().context("invalid TOML")?;
    let mut config = Config::default();
    let mut actions_seen = BTreeMap::new();
    let mut macros_seen = BTreeMap::new();
    let mut travel_seen = BTreeMap::new();
    for (mode_key, sections) in &table {
        let mode = parse_mode(mode_key).with_context(|| format!("in [{mode_key}]"))?;
        let sections = sections
            .as_table()
            .with_context(|| format!("[{mode_key}] must be a table"))?;
        for (section, entries) in sections {
            let entries = entries
                .as_table()
                .with_context(|| format!("[{mode_key}.{section}] must be a table"))?;
            let at = format!("{mode_key}.{section}");
            match section.as_str() {
                "travel" => {
                    claim(&mut travel_seen, mode, &format!("[{at}]"))?;
                    config.travel.insert(mode, parse_travel(&at, entries)?);
                }
                "macros" => {
                    for (key, value) in entries {
                        let source = format!("{at}.{key}");
                        let index = key
                            .parse::<usize>()
                            .ok()
                            .filter(|n| (1..=MACRO_COUNT).contains(n))
                            .with_context(|| {
                                format!("{source}: macro numbers are 1..={MACRO_COUNT}")
                            })?
                            - 1;
                        let at = MacroRef { mode, index };
                        claim(&mut macros_seen, at, &source)?;
                        config.macros.insert(at, Macro::parse(value, &source)?);
                    }
                }
                _ => {
                    let layer = parse_layer(mode, section).with_context(|| format!("in [{at}]"))?;
                    for (key, value) in entries {
                        let source = format!("{at}.{key}");
                        let slot = parse_position(key).with_context(|| format!("in {source}"))?;
                        let at = KeyRef { mode, layer, slot };
                        claim(&mut actions_seen, at, &source)?;
                        let text = value.as_str().with_context(|| {
                            format!("{source} must be an action string like \"ESC\"")
                        })?;
                        let action: Action =
                            text.parse().with_context(|| format!("in {source}"))?;
                        config.actions.insert(at, action);
                    }
                }
            }
        }
    }
    Ok(config)
}

fn parse_travel(at: &str, entries: &toml::Table) -> Result<TravelSection> {
    let mut section = TravelSection::default();
    for (key, value) in entries {
        let source = format!("{at}.{key}");
        let spec = TravelSpec::parse(value, &source)?;
        if key == "*" {
            section.all = spec;
            continue;
        }
        let slot = parse_position(key).with_context(|| format!("in {source}"))?;
        if section.keys.insert(slot, spec).is_some() {
            bail!("{source} names a key twice");
        }
    }
    Ok(section)
}

/// Offline checks: travel entries must fit a record, macros must fit their
/// region. Returns notes about what only the keyboard can settle.
pub fn check_config(config: &Config) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for (&mode, section) in &config.travel {
        for (at, spec) in section.entries(mode) {
            if let Some(note) = spec.check_offline().with_context(|| at.clone())? {
                notes.push(format!("{at}: {note}"));
            }
        }
    }
    for mode in 0..MODES {
        let mut list = vec![Macro::default(); MACRO_COUNT];
        for (at, m) in config.macros.iter().filter(|(at, _)| at.mode == mode) {
            list[at.index] = m.clone();
        }
        encode_region(&list).with_context(|| format!("{}.macros", mode_label(mode)))?;
    }
    for (at, a) in &config.actions {
        if !a.may_assign() {
            notes.push(format!(
                "{at} = {a}: {}; apply passes only if the keyboard already holds it",
                a.unmanaged_reason()
            ));
        }
        if let Some(target) = a.layer()
            && target / LAYERS_PER_MODE != at.mode
        {
            notes.push(format!(
                "{at} = {a}: FN(n) is absolute and layer {target} belongs to {}",
                mode_label(target / LAYERS_PER_MODE)
            ));
        }
    }
    Ok(notes)
}

/// The given modes as a config: actions that differ from the factory matrix,
/// macros, and the travel table as a `"*"` default plus per-key differences.
pub fn export_config(modes: &[u8], state: &State, factory: &Matrix) -> String {
    let mut out = String::from(
        "# nuphyctl he: key actions that differ from the factory matrix, macros, and key travel (mm)\n# apply with: nuphyctl he apply <this file>\n",
    );
    for &mode in modes {
        let m = mode_label(mode);
        for layer in 0..LAYERS_PER_MODE {
            let changed: Vec<KeyRef> = (0..key_count())
                .map(|slot| KeyRef { mode, layer, slot })
                .filter(|&at| state.matrix.action(at) != factory.action(at))
                .collect();
            if changed.is_empty() {
                continue;
            }
            out.push_str(&format!("\n[{m}.{}]\n", layer_name(mode, layer)));
            for at in changed {
                out.push_str(&format!(
                    "{} = \"{}\"  # factory {}\n",
                    toml_key(&position_name(at.slot)),
                    state.matrix.action(at),
                    factory.action(at)
                ));
            }
        }
        match state.macros.macros(mode) {
            Ok(list) if list.iter().any(|mac| !mac.0.is_empty()) => {
                out.push_str(&format!("\n[{m}.macros]\n"));
                for (index, mac) in list.iter().enumerate().filter(|(_, mac)| !mac.0.is_empty()) {
                    out.push_str(&format!("{} = {}\n", index + 1, mac.to_toml()));
                }
            }
            Ok(_) => {}
            Err(err) => out.push_str(&format!("\n# [{m}.macros] left out: {err:#}\n")),
        }
        let specs: Vec<TravelSpec> = (0..key_count())
            .map(|slot| TravelSpec::of(&state.travel.record(TravelRef { mode, slot })))
            .collect();
        let mut counts: Vec<(TravelSpec, usize)> = Vec::new();
        for spec in &specs {
            match counts.iter_mut().find(|(s, _)| s == spec) {
                Some(entry) => entry.1 += 1,
                None => counts.push((*spec, 1)),
            }
        }
        out.push_str(&format!("\n[{m}.travel]\n"));
        if let Some(&(common, _)) = counts.iter().max_by_key(|(_, n)| *n) {
            out.push_str(&format!("\"*\" = {}\n", common.render(None)));
            for (slot, spec) in specs.iter().enumerate().filter(|(_, s)| **s != common) {
                out.push_str(&format!(
                    "{} = {}\n",
                    toml_key(&position_name(slot)),
                    spec.render(Some(&common))
                ));
            }
        }
    }
    out
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
    use crate::he::plan::plan_changes;
    use crate::he::region::Region;
    use crate::he::testutil::{SAMPLE, blank_state};
    use crate::he::travel::{FieldId, Trigger};

    #[test]
    fn parses_config() {
        let c = parse_config(SAMPLE).unwrap();
        let key = |mode, layer, slot| c.actions[&KeyRef { mode, layer, slot }];
        assert_eq!(key(0, 0, 28), Action([0xf0, 0xff, 2]));
        assert_eq!(key(0, 0, 58), Action([0x10, 0x01, 0x06]));
        assert_eq!(key(0, 2, 22).to_string(), "UP");
        let travel = &c.travel[&0];
        assert_eq!(travel.keys.len(), 1);
        let w = travel.spec(16).unwrap();
        assert_eq!(w.length(FieldId::Actuation), Some(500));
        assert_eq!(w.trigger, Some(Trigger::Rapid));
        assert_eq!(w.length(FieldId::Press), Some(100));
        assert_eq!(
            travel.spec(0).unwrap().length(FieldId::Actuation),
            Some(1500)
        );
        assert_eq!(travel.spec(0).unwrap().trigger, None);
        assert_eq!(
            c.macros[&MacroRef { mode: 0, index: 0 }],
            Macro::chord("LCTL+Z").unwrap()
        );
        assert_eq!(
            c.summary(),
            "3 key action(s), 2 travel entr(ies), 1 macro(s)"
        );
    }

    #[test]
    fn layers_are_numbered_across_modes() {
        let c = parse_config("[m2.fn5]\nI = \"UP\"\n[m2.base]\nQ = \"A\"").unwrap();
        assert!(c.actions.contains_key(&KeyRef {
            mode: 1,
            layer: 1,
            slot: 22
        }));
        assert!(c.actions.contains_key(&KeyRef {
            mode: 1,
            layer: 0,
            slot: 15
        }));
        assert!(parse_config("[m2.fn1]\nI = \"UP\"").is_err());
        assert!(parse_config("[m2.fn4]\nQ = \"A\"\n[m2.base]\nQ = \"B\"").is_err());
    }

    #[test]
    fn config_errors() {
        for bad in [
            "[m4.base]\nESC = \"ESC\"",
            "[m1.fn9]\nESC = \"ESC\"",
            "[m1.base]\nF1 = \"ESC\"",
            "[m1.base]\nESC = \"NOPE\"",
            "[m1.base]\nESC = 41",
            "[m1.base]\nESC = \"ESC\"\n\"#0\" = \"F1\"",
            "[m1.base]\nESC = \"ESC\"\n[1.fn0]\nESC = \"F1\"",
            "[m1.travel]\nW = { actuation = \"x\" }",
            "[m1.travel]\nW = { speed = 1 }",
            "[m1.travel]\nW = { trigger = 16 }",
            "[m1.travel]\nW = { actuation = 1 }\nESCAPE = { actuation = 1 }\nESC = { actuation = 2 }",
            "[m1.travel]\nW = { actuation = 1 }\n[1.travel]\nQ = { actuation = 1 }",
            "[m1.macros]\n33 = \"LCTL+C\"",
            "[m1.macros]\n0 = \"LCTL+C\"",
            "[m1.macros]\n1 = \"LCTL+Z\"\n01 = \"LCTL+C\"",
            "[m1.macros]\n1 = [\"C sideways\"]",
        ] {
            assert!(parse_config(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn checks_offline() {
        let check = |text: &str| check_config(&parse_config(text).unwrap());
        assert!(check(SAMPLE).unwrap().is_empty());
        assert_eq!(check("[m2.base]\nS = \"0x950020\"").unwrap().len(), 1);
        assert!(check("[m2.base]\nCAPS = \"FN(1)\"").unwrap()[0].contains("belongs to m1"));
        assert!(check("[m2.base]\nCAPS = \"FN(5)\"").unwrap().is_empty());
        let notes = check("[m1.travel]\n\"*\" = { press = 0.105 }").unwrap();
        assert_eq!(notes.len(), 1, "a \"*\" entry is checked once: {notes:?}");
        assert!(notes[0].contains("finer"));
        assert!(check("[m1.travel]\nW = { actuation = 3.5 }").is_err());
        assert!(parse_config("[m1.travel]\nW = { press = 0.0013 }").is_err());
    }

    #[test]
    fn export_round_trips() {
        let factory = blank_state();
        let mut state = plan_changes(&parse_config(SAMPLE).unwrap(), &factory)
            .unwrap()
            .want;
        let one = TravelRef { mode: 0, slot: 1 };
        let mut odd = state.travel.record(one);
        odd.set_trigger(Trigger::Raw(3));
        state.travel.set_record(one, odd);
        state.matrix.set_action(
            KeyRef {
                mode: 0,
                layer: 3,
                slot: 0,
            },
            Action([0xf0, 0xff, 12]),
        );
        state.matrix.set_action(
            KeyRef {
                mode: 0,
                layer: 3,
                slot: 1,
            },
            Action([0x70, 40, 0xff]),
        );
        state.matrix.set_action(
            KeyRef {
                mode: 1,
                layer: 1,
                slot: 0,
            },
            Action([0x10, 0, 0x2b]),
        );

        let text = export_config(&[0, 1], &state, &factory.matrix);
        assert!(text.contains("[m1.base]\nCAPS = \"FN(2)\""), "{text}");
        assert!(text.contains("[m1.macros]\n1 = \"LCTL+Z\""), "{text}");
        assert!(
            text.contains("\"*\" = { trigger = \"normal\", actuation = 1.5"),
            "{text}"
        );
        assert!(
            text.contains("W = { trigger = \"rapid\", actuation = 0.5, press = 0.1 }"),
            "{text}"
        );
        assert!(text.contains("\"1\" = { trigger = 3 }"), "{text}");
        assert!(text.contains("ESC = \"0xf0ff0c\""), "{text}");
        assert!(text.contains("[m2.fn5]\nESC = \"TAB\""), "{text}");

        let config = parse_config(&text).unwrap();
        assert!(plan_changes(&config, &state).unwrap().is_empty());
    }

    #[test]
    fn export_notes_an_undecodable_macro_region() {
        let mut state = blank_state();
        let mut list = vec![Macro::default(); MACRO_COUNT];
        list[0] = Macro::chord("LCTL+C").unwrap();
        state.macros.set_macros(1, &list).unwrap();
        state.region_mut(Region::Macros)[0] = 3;
        let text = export_config(&[0, 1], &state, &state.matrix);
        assert!(text.contains("# [m1.macros] left out"), "{text}");
        assert!(text.contains("[m2.macros]\n1 = \"LCTL+C\""), "{text}");
    }
}
