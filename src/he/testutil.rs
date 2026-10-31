//! Fixtures shared by the HE tests.

use crate::he::actions::Action;
use crate::he::layout::{KeyRef, TravelRef, key_count};
use crate::he::macros::Macro;
use crate::he::region::{LAYERS_PER_MODE, MACRO_COUNT, MODES, Region};
use crate::he::state::State;
use crate::he::travel::TravelRecord;

/// Every key empty, every travel record a stock 1.50 mm, no macros.
pub fn blank_state() -> State {
    let mut state = State::from_regions(Region::ALL.map(|r| vec![0u8; r.len()])).unwrap();
    for mode in 0..MODES {
        for layer in 0..LAYERS_PER_MODE {
            for slot in 0..key_count() {
                state
                    .matrix
                    .set_action(KeyRef { mode, layer, slot }, Action::NONE);
            }
        }
        for slot in 0..key_count() {
            let record = TravelRecord([0x02, 0, 0x95, 0, 0, 0, 0, 0]);
            state.travel.set_record(TravelRef { mode, slot }, record);
        }
        state
            .macros
            .set_macros(mode, &vec![Macro::default(); MACRO_COUNT])
            .unwrap();
    }
    state
}

pub const SAMPLE: &str = r##"
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
