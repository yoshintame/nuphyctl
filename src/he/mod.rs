//! NuPhy Hall-effect boards (Air60 HE and siblings): a separate protocol
//! family from the Air75 V3 one in `nuphy_protocol`.

/// What a note says about a value this tool does not write.
pub const ONLY_IF_HELD: &str = "apply passes only if the keyboard already holds it";

pub mod actions;
pub mod cmd;
pub mod config;
pub mod layout;
pub mod macros;
pub mod names;
pub mod pending;
pub mod plan;
pub mod protocol;
pub mod region;
pub mod snapshot;
pub mod state;
#[cfg(test)]
pub mod testutil;
pub mod travel;
