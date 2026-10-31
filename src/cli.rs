use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "nuphyctl", about = "NuPhy keyboard control CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print all available command paths
    Commands,
    /// List HID devices visible to hidapi
    List,
    /// RGB-related commands
    Rgb(RgbCommand),
    /// Keymap commands (read, remap, back up and restore keys)
    Keymap(KeymapCommand),
    /// Send a raw HID output report
    Raw(RawCommand),
    /// Hall-effect boards (Air60 HE and siblings): keymap and key travel
    He(HeCommand),
}

#[derive(Args, Clone, Debug)]
pub struct DeviceSelector {
    /// USB vendor ID (hex like 0x19f5 or decimal)
    #[arg(long, value_parser = parse_u16_any_base)]
    pub vid: Option<u16>,
    /// USB product ID (hex like 0x3245 or decimal)
    #[arg(long, value_parser = parse_u16_any_base)]
    pub pid: Option<u16>,
    /// hidraw path (for example /dev/hidraw5)
    #[arg(long)]
    pub path: Option<String>,
    /// HID interface number
    #[arg(long)]
    pub iface: Option<i32>,
    /// HID usage page (hex like 0x0001 or decimal)
    #[arg(long, value_parser = parse_u16_any_base)]
    pub usage_page: Option<u16>,
    /// HID usage (hex like 0x0000 or decimal)
    #[arg(long, value_parser = parse_u16_any_base)]
    pub usage: Option<u16>,
}

#[derive(Subcommand, Debug)]
pub enum RgbSubcommand {
    /// Set backlight effect and color using #RRGGBB or RRGGBB
    Set(RgbSetArgs),
    /// Set side-light effect and color
    Side(RgbSideSetArgs),
    /// Set decorative-light effect and color (experimental)
    Decorative(RgbDecorativeSetArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RgbEffect {
    Ray,
    Stair,
    Static,
    Breath,
    Flower,
    Wave,
    Ripple,
    Spout,
    Galaxy,
    Rotation,
    #[value(name = "ripple2", alias = "ripple-2", alias = "ripple-alt")]
    Ripple2,
    Point,
    Grid,
    Time,
    Rain,
    Ribbon,
    Gaming,
    Identify,
    Windmill,
    Diagonal,
}

impl RgbEffect {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Ray => "Ray",
            Self::Stair => "Stair",
            Self::Static => "Static",
            Self::Breath => "Breath",
            Self::Flower => "Flower",
            Self::Wave => "Wave",
            Self::Ripple => "Ripple",
            Self::Spout => "Spout",
            Self::Galaxy => "Galaxy",
            Self::Rotation => "Rotation",
            Self::Ripple2 => "Ripple (2nd)",
            Self::Point => "Point",
            Self::Grid => "Grid",
            Self::Time => "Time",
            Self::Rain => "Rain",
            Self::Ribbon => "Ribbon",
            Self::Gaming => "Gaming",
            Self::Identify => "Identify",
            Self::Windmill => "Windmill",
            Self::Diagonal => "Diagonal",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RgbSideEffect {
    Time,
    Neon,
    Static,
    #[value(alias = "breath")]
    Breathe,
    Rhythm,
}

impl RgbSideEffect {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Time => "Time",
            Self::Neon => "Neon",
            Self::Static => "Static",
            Self::Breathe => "Breathe",
            Self::Rhythm => "Rhythm",
        }
    }

    pub const fn supports_custom_color(self) -> bool {
        matches!(self, Self::Static | Self::Breathe)
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RgbDirection {
    Left,
    Right,
}

impl RgbDirection {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum RgbColorMode {
    Custom,
    Preset,
}

impl RgbColorMode {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Preset => "preset",
        }
    }
}

#[derive(Args, Debug)]
pub struct RgbCommand {
    #[command(subcommand)]
    pub action: RgbSubcommand,
}

#[derive(Args, Debug)]
pub struct RgbSetArgs {
    /// Lighting effect (defaults to static)
    #[arg(long, value_enum, default_value_t = RgbEffect::Static)]
    pub effect: RgbEffect,

    /// Color in #RRGGBB or RRGGBB format
    #[arg(long)]
    pub hex: String,

    /// Brightness in percent (0-100)
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub brightness: u8,

    /// Animation speed gear (0-4)
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=4))]
    pub speed: u8,

    /// Effect direction
    #[arg(long, value_enum, default_value_t = RgbDirection::Right)]
    pub direction: RgbDirection,

    /// Color source (custom RGB or preset/palette)
    #[arg(long, value_enum, default_value_t = RgbColorMode::Custom)]
    pub color_mode: RgbColorMode,

    /// Palette index used when --color-mode preset
    #[arg(long, default_value_t = 0)]
    pub palette_index: u8,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct RgbSideSetArgs {
    /// Side-light effect
    #[arg(long, value_enum, default_value_t = RgbSideEffect::Static)]
    pub effect: RgbSideEffect,

    /// Color in #RRGGBB or RRGGBB format (optional for non-color effects)
    #[arg(long)]
    pub hex: Option<String>,

    /// Brightness in percent (0-100)
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub brightness: u8,

    /// Animation speed gear (0-4)
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=4))]
    pub speed: u8,

    /// Color source (custom RGB or preset/palette)
    #[arg(long, value_enum, default_value_t = RgbColorMode::Custom)]
    pub color_mode: RgbColorMode,

    /// Palette index used when --color-mode preset
    #[arg(long, default_value_t = 0)]
    pub palette_index: u8,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct RgbDecorativeSetArgs {
    /// Decorative-light effect
    #[arg(long, value_enum, default_value_t = RgbSideEffect::Static)]
    pub effect: RgbSideEffect,

    /// Color in #RRGGBB or RRGGBB format (optional for non-color effects)
    #[arg(long)]
    pub hex: Option<String>,

    /// Brightness in percent (0-100)
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u8).range(0..=100))]
    pub brightness: u8,

    /// Animation speed gear (0-4)
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(0..=4))]
    pub speed: u8,

    /// Color source (custom RGB or preset/palette)
    #[arg(long, value_enum, default_value_t = RgbColorMode::Custom)]
    pub color_mode: RgbColorMode,

    /// Palette index used when --color-mode preset
    #[arg(long, default_value_t = 0)]
    pub palette_index: u8,

    /// Decorative-light base offset (model-dependent)
    #[arg(long, value_parser = clap::value_parser!(u16).range(0..=255), default_value_t = 17)]
    pub base_offset: u16,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapCommand {
    #[command(subcommand)]
    pub action: KeymapSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum KeymapSubcommand {
    /// Show firmware version, current mode and keymap geometry
    Info(KeymapInfoArgs),
    /// Print the keymap (named keycodes and positions)
    Dump(KeymapDumpArgs),
    /// Print keys that differ from the factory keymap as an `apply` TOML file
    Export(KeymapExportArgs),
    /// Set one key, then read it back and verify
    Set(KeymapSetArgs),
    /// Apply a declarative TOML keymap file (only listed keys are touched)
    Apply(KeymapApplyArgs),
    /// Reset a mode's layers to the factory keymap
    Reset(KeymapResetArgs),
    /// Write a binary backup back to the keyboard (only differing keys)
    RestoreBackup(KeymapRestoreBackupArgs),
    /// List known keycode names
    Keycodes(KeymapListArgs),
    /// List named key positions (Air75 V3 layout)
    Positions(KeymapListArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum KeymapMode {
    Mac,
    #[value(alias = "win")]
    Windows,
}

impl KeymapMode {
    pub const fn index(self) -> u8 {
        match self {
            Self::Mac => 0,
            Self::Windows => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum KeymapResetMode {
    Mac,
    #[value(alias = "win")]
    Windows,
    All,
}

#[derive(Args, Debug)]
pub struct KeymapInfoArgs {
    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapDumpArgs {
    /// Only this layer (number like 5, or mode.layer like windows.fn)
    #[arg(long, conflicts_with = "mode")]
    pub layer: Option<String>,

    /// Only the layers of this mode
    #[arg(long, value_enum)]
    pub mode: Option<KeymapMode>,

    /// Read the factory keymap (GetDefaultKeys) instead of the active one
    #[arg(long)]
    pub defaults: bool,

    /// Only show keys that differ from the factory keymap
    #[arg(long, conflicts_with = "defaults")]
    pub changed: bool,

    /// Also save the full keymap (all layers) as a binary backup to this file
    #[arg(long, value_name = "FILE")]
    pub raw_out: Option<PathBuf>,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapExportArgs {
    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapSetArgs {
    /// Layer (number like 4, or mode.layer like windows.base)
    #[arg(long)]
    pub layer: String,

    /// Key position: name (F1, ESC, YELLOW, KNOB_CCW), row,col (3,12) or #index
    pub position: String,

    /// Keycode: name (BRID, F20, MISSION_CONTROL, MO(5), TRNS) or hex (0x00be)
    pub code: String,

    /// Show what would be written without writing
    #[arg(long)]
    pub dry_run: bool,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapApplyArgs {
    /// TOML keymap file
    pub file: PathBuf,

    /// Show the diff without writing
    #[arg(long)]
    pub dry_run: bool,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapResetArgs {
    /// Which layers to reset
    #[arg(long, value_enum)]
    pub mode: KeymapResetMode,

    /// Confirm the reset (required)
    #[arg(long)]
    pub yes: bool,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapRestoreBackupArgs {
    /// Binary backup (from `keymap dump --raw-out` or an automatic backup)
    pub file: PathBuf,

    /// Show the diff without writing
    #[arg(long)]
    pub dry_run: bool,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct KeymapListArgs {
    /// Print JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum RawSubcommand {
    /// Send a raw output report (64 bytes)
    Send(RawSendArgs),
}

#[derive(Args, Debug)]
pub struct RawCommand {
    #[command(subcommand)]
    pub action: RawSubcommand,
}

#[derive(Args, Debug)]
pub struct RawSendArgs {
    /// Report payload bytes (space-separated hex or 128 hex chars)
    #[arg(long)]
    pub hex: String,

    /// HID report ID (NuPhy packets use 0)
    #[arg(long, default_value_t = 0)]
    pub report_id: u8,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct HeCommand {
    #[command(subcommand)]
    pub action: HeSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum HeSubcommand {
    /// Show firmware version and the current mode
    Info(HeInfoArgs),
    /// Print key actions per mode and layer
    Dump(HeDumpArgs),
    /// Print per-key travel: actuation point, Rapid Trigger, dead zones (mm)
    Travel(HeReadArgs),
    /// Print changed actions, macros and the travel table as an `apply` TOML file
    Export(HeExportArgs),
    /// Validate a TOML file without a keyboard
    Check(HeCheckArgs),
    /// Apply a declarative TOML file (only listed keys and fields are touched)
    Apply(HeWriteArgs),
    /// Write an `he` backup back to the keyboard (only differing entries)
    RestoreBackup(HeWriteArgs),
}

#[derive(Args, Debug)]
pub struct HeInfoArgs {
    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

/// A mode as NuPhy IO labels it: m1, m2 or m3 (0-based inside).
fn parse_he_mode(input: &str) -> Result<u8, String> {
    crate::he::layout::parse_mode(input).map_err(|e| e.to_string())
}

#[derive(Args, Debug)]
pub struct HeReadArgs {
    /// Only this mode (m1, m2 or m3)
    #[arg(long, value_parser = parse_he_mode)]
    pub mode: Option<u8>,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct HeDumpArgs {
    #[command(flatten)]
    pub read: HeReadArgs,

    /// Only keys that differ from the factory matrix
    #[arg(long)]
    pub changed: bool,
}

#[derive(Args, Debug)]
pub struct HeExportArgs {
    /// Only this mode (m1, m2 or m3)
    #[arg(long, value_parser = parse_he_mode)]
    pub mode: Option<u8>,

    #[command(flatten)]
    pub device: DeviceSelector,
}

#[derive(Args, Debug)]
pub struct HeCheckArgs {
    /// TOML file to validate
    pub file: PathBuf,
}

/// Arguments of the commands that write: `apply` and `restore-backup`.
#[derive(Args, Debug)]
pub struct HeWriteArgs {
    /// TOML config (`apply`) or backup file (`restore-backup`)
    pub file: PathBuf,

    /// Show the changes without writing
    #[arg(long)]
    pub dry_run: bool,

    /// Print JSON
    #[arg(long)]
    pub json: bool,

    #[command(flatten)]
    pub device: DeviceSelector,
}

fn parse_u16_any_base(input: &str) -> Result<u16, String> {
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        input.parse::<u16>().map_err(|e| e.to_string())
    }
}
