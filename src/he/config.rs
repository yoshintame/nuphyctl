//! The declarative config: parsing, offline checks and export.
//!
//! ```toml
//! [m1.base]                 # mode m1..m3 . base | fn1 | fn2 | fn3 (or fn0..fn11)
//! CAPS = "FN(2)"            # momentary layer, absolute number
//! "#58" = "LCTL+C"
//!
//! [m1.travel]               # millimetres; unset fields keep the keyboard's value
//! "*" = { actuation = 1.5, trigger = "normal" }
//! W = { actuation = 0.5, trigger = "rapid", press = 0.1, release = 0.1 }
//!
//! [m1.macros]               # 1..32 as in NuPhy IO; a chord, or steps with delays
//! 1 = "LCTL+Z"
//! 3 = ["LGUI down 1", "SPC down 1", "SPC up 1", "LGUI up"]
//! ```

use crate::he::actions::Action;
use crate::he::layout::{
    LAYER_NAMES, key_count, mode_label, parse_layer, parse_mode, parse_position, position_name,
};
use crate::he::macros::{Macro, encode_region};
use crate::he::plan::{State, action_in};
use crate::he::region::{LAYERS_PER_MODE, MACRO_COUNT, MODES};
use crate::he::travel::{TravelRecord, TravelSpec};
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// What a config asks for, keyed by mode (0-based), layer, slot and macro
/// index (0-based).
#[derive(Debug, Default)]
pub struct Config {
    pub actions: BTreeMap<(u8, u8, usize), Action>,
    pub travel: BTreeMap<(u8, usize), TravelSpec>,
    pub macros: BTreeMap<(u8, usize), Macro>,
}

/// Where each canonical entry came from, so two spellings of the same key
/// (`1` and `01`, `[m1.travel]` and `[1.travel]`) are an error.
#[derive(Default)]
struct Sources(HashMap<String, String>);

impl Sources {
    fn claim(&mut self, canonical: String, source: &str) -> Result<()> {
        if let Some(previous) = self.0.insert(canonical, source.to_string()) {
            bail!("{source} sets the same thing as {previous}");
        }
        Ok(())
    }
}

pub fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_config(&text).with_context(|| format!("invalid config {}", path.display()))
}

pub fn parse_config(text: &str) -> Result<Config> {
    let table: toml::Table = text.parse().context("invalid TOML")?;
    let mut config = Config::default();
    let mut sources = Sources::default();
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
                    sources.claim(format!("{mode}.travel"), &format!("[{at}]"))?;
                    parse_travel(mode, &at, entries, &mut config)?;
                }
                "macros" => parse_macros(mode, &at, entries, &mut config, &mut sources)?,
                _ => {
                    let layer = parse_layer(mode, section).with_context(|| format!("in [{at}]"))?;
                    for (key, value) in entries {
                        let source = format!("{at}.{key}");
                        let slot = parse_position(key).with_context(|| format!("in {source}"))?;
                        sources.claim(format!("{mode}.{layer}.{slot}"), &source)?;
                        let text = value.as_str().with_context(|| {
                            format!("{source} must be an action string like \"ESC\"")
                        })?;
                        let action: Action =
                            text.parse().with_context(|| format!("in {source}"))?;
                        config.actions.insert((mode, layer, slot), action);
                    }
                }
            }
        }
    }
    Ok(config)
}

fn parse_macros(
    mode: u8,
    at: &str,
    entries: &toml::Table,
    config: &mut Config,
    sources: &mut Sources,
) -> Result<()> {
    for (key, value) in entries {
        let source = format!("{at}.{key}");
        let index = key
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MACRO_COUNT).contains(n))
            .with_context(|| format!("{source}: macro numbers are 1..={MACRO_COUNT}"))?
            - 1;
        sources.claim(format!("{mode}.macro.{index}"), &source)?;
        config
            .macros
            .insert((mode, index), Macro::parse(value, &source)?);
    }
    Ok(())
}

fn parse_travel(mode: u8, at: &str, entries: &toml::Table, config: &mut Config) -> Result<()> {
    let all = match entries.get("*") {
        Some(v) => TravelSpec::parse(v, &format!("{at}.\"*\""))?,
        None => TravelSpec::default(),
    };
    let mut per_key: BTreeMap<usize, TravelSpec> = BTreeMap::new();
    for (key, value) in entries.iter().filter(|(k, _)| k.as_str() != "*") {
        let source = format!("{at}.{key}");
        let slot = parse_position(key).with_context(|| format!("in {source}"))?;
        if per_key
            .insert(slot, TravelSpec::parse(value, &source)?)
            .is_some()
        {
            bail!("{source} names a key twice");
        }
    }
    for slot in 0..key_count() {
        let spec = all.merged(per_key.get(&slot).copied().unwrap_or_default());
        if spec != TravelSpec::default() {
            config.travel.insert((mode, slot), spec);
        }
    }
    Ok(())
}

/// Offline checks: travel entries must fit a record, macros must fit their
/// region. Returns notes about what only the keyboard can settle.
pub fn check_config(config: &Config) -> Result<Vec<String>> {
    // Stock records with 0.01, 0.005 and 0.001 mm Rapid Trigger steps: a
    // press or release value only has to fit one of them; which one the
    // keyboard uses is known only at apply.
    let stocks = [0x00, 0x0a, 0x14].map(|steps| TravelRecord([0xa2, 0, 0x95, steps, 0, 0, 0, 0]));
    let mut notes = Vec::new();
    for (&(mode, slot), spec) in &config.travel {
        let at = format!("{}.travel {}", mode_label(mode), position_name(slot));
        if let Err(err) = spec.apply_to(stocks[0]) {
            if !spec.sets_sensitivity() || !stocks[1..].iter().any(|r| spec.apply_to(*r).is_ok()) {
                return Err(err.context(at));
            }
            notes.push(format!(
                "{at}: press/release need a finer Rapid Trigger step than 0.01 mm; apply fails unless the key uses one"
            ));
        }
    }
    for mode in 0..MODES {
        let mut list = vec![Macro::default(); MACRO_COUNT];
        for (&(_, index), m) in config.macros.range((mode, 0)..(mode, MACRO_COUNT)) {
            list[index] = m.clone();
        }
        encode_region(&list).with_context(|| format!("{}.macros", mode_label(mode)))?;
    }
    for (&(mode, layer, slot), a) in &config.actions {
        let at = format!(
            "{}.{} {} = {a}",
            mode_label(mode),
            LAYER_NAMES[usize::from(layer)],
            position_name(slot)
        );
        if !a.is_writable() {
            notes.push(format!(
                "{at}: not written by this tool; apply passes only if the keyboard already holds it"
            ));
        }
        if let Some(target) = a.layer()
            && target / LAYERS_PER_MODE != mode
        {
            notes.push(format!(
                "{at}: FN(n) is absolute and layer {target} belongs to {}",
                mode_label(target / LAYERS_PER_MODE)
            ));
        }
    }
    Ok(notes)
}

/// The given modes as a config: actions that differ from the factory matrix,
/// macros, and the travel table as a `"*"` default plus per-key differences.
pub fn export_config(modes: &[u8], state: &State, factory_matrix: &[u8]) -> String {
    let mut out = String::from(
        "# nuphyctl he: key actions that differ from the factory matrix, macros, and key travel (mm)\n# apply with: nuphyctl he apply <this file>\n",
    );
    for &mode in modes {
        let m = mode_label(mode);
        for layer in 0..LAYERS_PER_MODE {
            let changed: Vec<usize> = (0..key_count())
                .filter(|&s| {
                    state.action(mode, layer, s) != action_in(factory_matrix, mode, layer, s)
                })
                .collect();
            if changed.is_empty() {
                continue;
            }
            out.push_str(&format!("\n[{m}.{}]\n", LAYER_NAMES[usize::from(layer)]));
            for slot in changed {
                out.push_str(&format!(
                    "{} = \"{}\"  # factory {}\n",
                    toml_key(&position_name(slot)),
                    state.action(mode, layer, slot),
                    action_in(factory_matrix, mode, layer, slot)
                ));
            }
        }
        match state.macros(mode) {
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
            .map(|s| TravelSpec::of(&state.travel(mode, s)))
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
    use crate::he::plan::tests::{SAMPLE, blank_state};
    use crate::he::travel::Trigger;

    #[test]
    fn parses_config() {
        let c = parse_config(SAMPLE).unwrap();
        assert_eq!(c.actions[&(0, 0, 28)], Action([0xf0, 0xff, 2]));
        assert_eq!(c.actions[&(0, 0, 58)], Action([0x10, 0x01, 0x06]));
        assert_eq!(c.actions[&(0, 2, 22)].to_string(), "UP");
        assert_eq!(c.travel.len(), 61);
        let w = c.travel[&(0, 16)];
        assert_eq!(w.lengths[0], Some(500));
        assert_eq!(w.trigger, Some(Trigger::Rapid));
        assert_eq!(w.lengths[1], Some(100));
        assert_eq!(c.travel[&(0, 0)].lengths[0], Some(1500));
        assert_eq!(c.travel[&(0, 0)].trigger, None);
        assert_eq!(c.macros[&(0, 0)], Macro::chord("LCTL+Z").unwrap());
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
        assert!(check("[m1.travel]\nW = { press = 0.105 }").unwrap()[0].contains("finer"));
        assert!(check("[m1.travel]\nW = { actuation = 3.5 }").is_err());
        assert!(parse_config("[m1.travel]\nW = { press = 0.0013 }").is_err());
        assert!(check("[m1.travel]\nW = { press = 0.003 }").unwrap()[0].contains("finer"));
    }

    #[test]
    fn export_round_trips() {
        let factory = blank_state();
        let mut state = plan_changes(&parse_config(SAMPLE).unwrap(), &factory)
            .unwrap()
            .want;
        let mut odd = state.travel(0, 1);
        odd.set_trigger(Trigger::Raw(3));
        state.set_travel(0, 1, odd);
        state.set_action(0, 3, 0, Action([0xf0, 0xff, 12]));
        state.set_action(0, 3, 1, Action([0x70, 40, 0xff]));

        let text = export_config(
            &[0],
            &state,
            factory.region(crate::he::region::Region::Matrix),
        );
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

        let config = parse_config(&text).unwrap();
        assert!(plan_changes(&config, &state).unwrap().is_empty());
    }

    #[test]
    fn export_notes_an_undecodable_macro_region() {
        let mut state = blank_state();
        let mut list = vec![Macro::default(); MACRO_COUNT];
        list[0] = Macro::chord("LCTL+C").unwrap();
        state.set_macros(1, &list).unwrap();
        let mut bytes = state.region(crate::he::region::Region::Macros).to_vec();
        bytes[0] = 3;
        let state = State::new([
            state.region(crate::he::region::Region::Matrix).to_vec(),
            state.region(crate::he::region::Region::Travel).to_vec(),
            bytes,
        ])
        .unwrap();
        let text = export_config(
            &[0, 1],
            &state,
            state.region(crate::he::region::Region::Matrix),
        );
        assert!(text.contains("# [m1.macros] left out"), "{text}");
        assert!(text.contains("[m2.macros]\n1 = \"LCTL+C\""), "{text}");
    }
}
