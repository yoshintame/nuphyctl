mod app;
mod backup;
mod cli;
mod color;
mod he;
mod hid_transport;
mod keycodes;
mod keymap;
mod keymap_cmd;
mod nuphy_protocol;
mod output;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use hidapi::HidApi;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut api = HidApi::new().context("failed to initialize hidapi")?;
    app::run(cli, &mut api)
}
