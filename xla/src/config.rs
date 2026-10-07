//! Configuration: defaults, COSMIC's own settings, `config.yaml`, then flags.
//!
//! Four layers, each overriding the one before it:
//!
//! 1. the built-in defaults below,
//! 2. COSMIC's settings, so xla follows the desktop's icon theme, interface
//!    font and dark/light mode without being told to,
//! 3. `~/.config/xla/config.yaml`,
//! 4. command line flags.
//!
//! A broken config file is reported on stderr and then ignored rather than
//! being fatal. xla normally runs from a keybinding with no terminal attached,
//! so refusing to start would present as "the launcher stopped working" with
//! nothing to explain it; carrying on with the earlier layers' values at least
//! keeps it usable. `xla --dump-config` prints what was actually resolved.
//!
//! This mirrors xsw, the window switcher this launcher is styled after: the
//! `layout` and `colors` blocks describe the same panel drawn the same way.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where COSMIC keeps the toolkit settings we read.
const TK_CONFIG: &str = "cosmic/com.system76.CosmicTk/v1";
/// Where COSMIC records whether the dark palette is in use.
const MODE_CONFIG: &str = "cosmic/com.system76.CosmicTheme.Mode/v1";

/// Fully resolved configuration.
#[derive(Debug, Clone, Serialize)]
pub struct Config {
    /// Icon theme to search first, e.g. "Cosmic".
    pub icon_theme: String,
    /// Interface font family, or `None` to let fontconfig pick a sans-serif.
    ///
    /// Serialised under the file's name for it, so `--dump-config` output can
    /// be used as a config file unchanged.
    #[serde(rename = "font")]
    pub font_family: Option<String>,
    /// Whether to draw the dark palette. Written out as `theme:` for the same
    /// reason, since that is the key the file uses.
    #[serde(rename = "theme", serialize_with = "serialize_theme")]
    pub dark: bool,
    /// Overall width of the launcher in logical pixels.
    pub width: u32,
    /// Largest number of result rows shown before the list scrolls.
    pub max_rows: usize,
    /// Which display the launcher appears on.
    pub display: Display,
    /// Order equally good matches by how recently they were launched.
    pub history: bool,
    /// Show the application's description under its name.
    pub show_descriptions: bool,
    /// Greyed-out text shown in the query row while it is empty.
    pub placeholder: String,
    /// Command prefix for `Terminal=true` entries, e.g. "alacritty -e". The
    /// application's own command is appended to it.
    ///
    /// `None` means "work it out at launch time from what is installed", which
    /// is what the default does; see [`crate::apps::default_terminal`].
    pub terminal: Option<String>,
    /// Whether pressing the binding again while the launcher is up closes it.
    pub toggle: bool,
    /// How long the launcher may hold an exclusive keyboard grab.
    #[serde(serialize_with = "serialize_secs", rename = "max_lifetime_secs")]
    pub max_lifetime: Duration,
    pub layout: Layout,
    pub colors: Colors,
    /// Per-application overrides, first match wins.
    pub app_rules: Vec<AppRule>,

    /// Query to start with. Set by `--query`, never by the file, because it
    /// describes one invocation rather than a preference.
    #[serde(skip)]
    pub query: String,
    /// The file this was read from, so `--dump-config` can name the right one
    /// even when `--config` pointed somewhere else.
    #[serde(skip)]
    pub config_path: PathBuf,
}

/// Pixel metrics, in logical pixels, scaled by the output factor at draw time.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Layout {
    pub row_height: u32,
    pub padding: u32,
    /// Gap between the icon and the text column.
    pub icon_gap: u32,
    pub icon_size: u32,
    /// Height of the query row above the results.
    pub input_height: u32,
    /// Corner radius of the panel.
    pub corner_radius: f32,
    /// Corner radius of the selected row's highlight.
    pub row_corner_radius: f32,
    /// Font size of the application name.
    pub name_size: f32,
    /// Font size of the description under the name.
    pub description_size: f32,
    /// Font size of the typed query.
    pub input_size: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            row_height: 46,
            padding: 10,
            icon_gap: 10,
            icon_size: 30,
            input_height: 44,
            corner_radius: 4.0,
            row_corner_radius: 4.0,
            name_size: 14.5,
            description_size: 12.0,
            input_size: 15.5,
        }
    }
}

impl Layout {
    /// Logical height needed for the query row plus `rows` result rows.
    pub fn height_for(&self, rows: usize) -> u32 {
        self.padding * 2 + self.input_height + self.row_height * rows as u32
    }

    /// Top of the result list, below the query row.
    pub fn rows_top(&self) -> u32 {
        self.padding + self.input_height
    }

    /// Width available for the text column, given the panel width.
    ///
    /// Shared with the renderer rather than duplicated, so the width check and
    /// the actual layout cannot drift apart.
    pub fn text_width(&self, width: u32) -> u32 {
        width.saturating_sub(self.text_x() + self.padding * 2)
    }

    /// Left edge of the text column, shared by the query and the names below
    /// it so they line up.
    pub fn text_x(&self) -> u32 {
        self.icon_x() + self.icon_size + self.icon_gap
    }

    /// Left edge of the icon column.
    pub fn icon_x(&self) -> u32 {
        self.padding + self.padding / 2
    }

    /// Height of one row's text block, used to centre it vertically.
    pub fn text_block_height(&self) -> f32 {
        if self.description_size > 0.0 {
            self.name_size + self.description_size + 4.0
        } else {
            self.name_size
        }
    }

    /// Height of one line of text at `size`, matching the renderer's metrics.
    pub fn line_height(size: f32) -> f32 {
        size * 1.25
    }
}

/// The colours the launcher draws with.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Colors {
    pub background: Rgba,
    pub selection: Rgba,
    pub name: Rgba,
    pub description: Rgba,
    pub name_selected: Rgba,
    pub description_selected: Rgba,
    /// The typed query, and the caret after it.
    pub query: Rgba,
    /// The placeholder shown while the query is empty, and the "no matches"
    /// line.
    pub placeholder: Rgba,
    /// Hairline between the query row and the results.
    pub divider: Rgba,
}

impl Colors {
    /// The palette for a mode, before any per-colour overrides.
    ///
    /// The shared entries are xsw's, so the two tools draw the same panel.
    fn for_mode(dark: bool) -> Self {
        if dark {
            Self {
                background: Rgba::new(20, 20, 24, 242),
                selection: Rgba::new(90, 130, 220, 235),
                name: Rgba::new(238, 238, 242, 255),
                description: Rgba::new(150, 150, 158, 255),
                name_selected: Rgba::new(255, 255, 255, 255),
                description_selected: Rgba::new(226, 232, 248, 255),
                query: Rgba::new(238, 238, 242, 255),
                placeholder: Rgba::new(110, 110, 120, 255),
                divider: Rgba::new(255, 255, 255, 26),
            }
        } else {
            Self {
                background: Rgba::new(250, 250, 252, 245),
                selection: Rgba::new(70, 115, 215, 235),
                name: Rgba::new(24, 24, 28, 255),
                description: Rgba::new(110, 110, 120, 255),
                name_selected: Rgba::new(255, 255, 255, 255),
                description_selected: Rgba::new(230, 236, 250, 255),
                query: Rgba::new(24, 24, 28, 255),
                placeholder: Rgba::new(154, 154, 164, 255),
                divider: Rgba::new(0, 0, 0, 26),
            }
        }
    }
}

/// Which output the launcher is shown on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Display {
    /// Let the compositor choose, which gives the output holding the focused
    /// window. This is what layer-shell does when handed no output.
    Active,
    /// The output COSMIC marks as primary, the same one Settings and
    /// `cosmic-randr list` call the Xwayland primary.
    #[default]
    Primary,
    /// A specific output by name, e.g. "HDMI-A-1".
    Named(String),
}

impl std::str::FromStr for Display {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim() {
            "" => Err("display must be \"active\", \"primary\", or an output name".to_string()),
            "active" => Ok(Self::Active),
            "primary" | "main" => Ok(Self::Primary),
            // Anything else is taken as an output name rather than rejected,
            // which is what makes naming a display work without a second key.
            name => Ok(Self::Named(name.to_string())),
        }
    }
}

impl std::fmt::Display for Display {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => f.write_str("active"),
            Self::Primary => f.write_str("primary"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

impl Serialize for Display {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Display {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// A colour written as `#rrggbb` or `#rrggbbaa`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

impl std::str::FromStr for Rgba {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("not a hex colour: {raw}"));
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
        match hex.len() {
            6 => Ok(Self::new(byte(0), byte(2), byte(4), 255)),
            8 => Ok(Self::new(byte(0), byte(2), byte(4), byte(6))),
            _ => Err(format!("colour must be #rrggbb or #rrggbbaa, got {raw}")),
        }
    }
}

impl std::fmt::Display for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }
}

impl Serialize for Rgba {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Overrides what one application looks like in the list, or hides it.
///
/// Distributions ship plenty of desktop entries with names that are hard to
/// search for and a few that nobody wants to launch at all, and neither is
/// something to fix by editing files under `/usr/share`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRule {
    /// Desktop entry id, without the `.desktop` suffix. `xla --list` prints
    /// the id of every application in the first column.
    pub id: String,
    /// Name to show, and to search, instead of the entry's own.
    #[serde(default)]
    pub name: Option<String>,
    /// Description to show instead of the entry's own.
    #[serde(default)]
    pub description: Option<String>,
    /// Icon name to show instead of the entry's own.
    #[serde(default)]
    pub icon: Option<String>,
    /// Drop this application from the list entirely.
    #[serde(default)]
    pub hide: bool,
}

impl AppRule {
    /// Whether this rule applies to a desktop entry id.
    ///
    /// Case-insensitive, because ids are file names and people type them from
    /// memory rather than copying them.
    fn matches(&self, id: &str) -> bool {
        self.id.eq_ignore_ascii_case(id)
    }
}

/// What xla was asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Show the launcher.
    #[default]
    Show,
    /// Print the application list to stdout and exit, for debugging.
    List,
    /// Print the resolved configuration and exit.
    DumpConfig,
    /// Print usage and exit.
    Help,
    /// Print the version and exit.
    Version,
}

/// The file's shape: every field optional, so an absent one falls through to
/// whatever the earlier layer decided.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    width: Option<u32>,
    max_rows: Option<usize>,
    display: Option<Display>,
    theme: Option<Theme>,
    font: Option<String>,
    icon_theme: Option<String>,
    history: Option<bool>,
    show_descriptions: Option<bool>,
    placeholder: Option<String>,
    terminal: Option<String>,
    toggle: Option<bool>,
    max_lifetime_secs: Option<u64>,
    layout: Option<Layout>,
    colors: Option<FileColors>,
    app_rules: Option<Vec<AppRule>>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Theme {
    /// Follow COSMIC's own dark/light setting.
    System,
    Dark,
    Light,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileColors {
    background: Option<Rgba>,
    selection: Option<Rgba>,
    name: Option<Rgba>,
    description: Option<Rgba>,
    name_selected: Option<Rgba>,
    description_selected: Option<Rgba>,
    query: Option<Rgba>,
    placeholder: Option<Rgba>,
    divider: Option<Rgba>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            icon_theme: "Cosmic".to_string(),
            font_family: None,
            dark: true,
            width: 560,
            max_rows: 8,
            display: Display::Primary,
            history: true,
            show_descriptions: true,
            placeholder: "Search applications…".to_string(),
            terminal: None,
            toggle: true,
            max_lifetime: Duration::from_secs(120),
            layout: Layout::default(),
            colors: Colors::for_mode(true),
            app_rules: Vec::new(),
            query: String::new(),
            config_path: default_config_path(),
        }
    }
}

impl Config {
    /// Resolves all four layers.
    ///
    /// Returns the mode alongside the config, plus any non-fatal warnings the
    /// caller should print.
    pub fn load(args: impl Iterator<Item = String>) -> Result<(Self, Mode, Vec<String>), String> {
        let mut config = Self::default();
        let mut warnings = Vec::new();

        // Layer 2: the desktop's own settings.
        config.apply_cosmic();

        // Parsed up front because `--config` decides which file to read, but
        // applied last so flags win.
        let flags = Flags::parse(args)?;

        // Layer 3: the file. `theme` and colour overrides are carried aside
        // because the palette can only be built once dark/light is final.
        let mut theme = None;
        let mut color_overrides = FileColors::default();
        let path = flags.config_path.clone().unwrap_or_else(default_config_path);
        config.config_path = path.clone();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_yaml::from_str::<Option<FileConfig>>(&text) {
                // A file that is empty or all comments parses as `None`.
                Ok(None) => {}
                Ok(Some(file)) => {
                    // Applied to a copy first: a value the file gets wrong is
                    // reported and dropped rather than being fatal, exactly as
                    // an unknown key is. Otherwise editing the file to
                    // something invalid would present as the launcher key
                    // silently doing nothing, with no terminal to show why.
                    let mut candidate = config.clone();
                    let mut candidate_theme = theme;
                    let mut candidate_colors = FileColors::default();
                    candidate.apply_file(file, &mut candidate_theme, &mut candidate_colors);
                    match candidate.check_with(candidate_theme, &candidate_colors) {
                        Ok(()) => {
                            config = candidate;
                            theme = candidate_theme;
                            color_overrides = candidate_colors;
                        }
                        Err(err) => warnings.push(format!("ignoring {}: {err}", path.display())),
                    }
                }
                Err(err) => warnings.push(format!("ignoring {}: {err}", path.display())),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => warnings.push(format!("cannot read {}: {err}", path.display())),
        }

        // Layer 4: flags.
        config.apply_flags(&flags, &mut theme);

        match theme {
            Some(Theme::Dark) => config.dark = true,
            Some(Theme::Light) => config.dark = false,
            // `system` and "unset" both mean whatever COSMIC said.
            Some(Theme::System) | None => {}
        }
        config.colors = Colors::for_mode(config.dark);
        color_overrides.apply(&mut config.colors);

        config.validate()?;
        Ok((config, flags.mode, warnings))
    }

    /// Reads COSMIC's settings, keeping the default for anything unreadable.
    ///
    /// COSMIC stores each setting as its own small file holding a RON value, so
    /// `.../CosmicTk/v1/icon_theme` holds the eight bytes `"Cosmic"`. Pulling
    /// three scalars out of three files by hand is smaller and faster than
    /// taking on a RON parser.
    fn apply_cosmic(&mut self) {
        if let Some(raw) = read_cosmic(TK_CONFIG, "icon_theme") {
            let name = raw.trim().trim_matches('"');
            if !name.is_empty() {
                self.icon_theme = name.to_string();
            }
        }
        if let Some(raw) = read_cosmic(TK_CONFIG, "interface_font") {
            self.font_family = ron_field(&raw, "family");
        }
        if let Some(raw) = read_cosmic(MODE_CONFIG, "is_dark") {
            self.dark = raw.trim() == "true";
        }
    }

    fn apply_file(&mut self, file: FileConfig, theme: &mut Option<Theme>, colors: &mut FileColors) {
        if let Some(width) = file.width {
            self.width = width;
        }
        if let Some(rows) = file.max_rows {
            self.max_rows = rows;
        }
        if let Some(display) = file.display {
            self.display = display;
        }
        if let Some(font) = file.font {
            self.font_family = Some(font);
        }
        if let Some(icons) = file.icon_theme {
            self.icon_theme = icons;
        }
        if let Some(history) = file.history {
            self.history = history;
        }
        if let Some(show) = file.show_descriptions {
            self.show_descriptions = show;
        }
        if let Some(text) = file.placeholder {
            self.placeholder = text;
        }
        if let Some(terminal) = file.terminal {
            self.terminal = Some(terminal);
        }
        if let Some(toggle) = file.toggle {
            self.toggle = toggle;
        }
        if let Some(secs) = file.max_lifetime_secs {
            self.max_lifetime = Duration::from_secs(secs);
        }
        if let Some(layout) = file.layout {
            self.layout = layout;
        }
        if let Some(rules) = file.app_rules {
            self.app_rules = rules;
        }
        if let Some(from_file) = file.theme {
            *theme = Some(from_file);
        }
        if let Some(from_file) = file.colors {
            *colors = from_file;
        }
    }

    fn apply_flags(&mut self, flags: &Flags, theme: &mut Option<Theme>) {
        if let Some(width) = flags.width {
            self.width = width;
        }
        if let Some(rows) = flags.max_rows {
            self.max_rows = rows;
        }
        if let Some(font) = &flags.font {
            self.font_family = Some(font.clone());
        }
        if let Some(icons) = &flags.icon_theme {
            self.icon_theme = icons.clone();
        }
        if let Some(display) = &flags.display {
            self.display = display.clone();
        }
        if let Some(terminal) = &flags.terminal {
            self.terminal = Some(terminal.clone());
        }
        if let Some(dark) = flags.dark {
            *theme = Some(if dark { Theme::Dark } else { Theme::Light });
        }
        if let Some(query) = &flags.query {
            self.query = query.clone();
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_rows == 0 {
            return Err("max_rows must be at least 1".to_string());
        }
        if self.layout.icon_size == 0 {
            return Err("layout.icon_size must be greater than 0".to_string());
        }
        if self.layout.name_size <= 0.0 {
            return Err("layout.name_size must be greater than 0".to_string());
        }
        if self.layout.input_size <= 0.0 {
            return Err("layout.input_size must be greater than 0".to_string());
        }
        // A row has to be tall enough for the icon and the text it holds, or
        // consecutive rows would overlap.
        let needed = self.layout.icon_size.max(self.layout.text_block_height().ceil() as u32);
        if self.layout.row_height < needed {
            return Err(format!(
                "layout.row_height ({}) is too small: {needed} needed for icon_size {} and the text",
                self.layout.row_height, self.layout.icon_size
            ));
        }
        // Likewise for the query row, which holds one line of larger text.
        let needed = Layout::line_height(self.layout.input_size).ceil() as u32;
        if self.layout.input_height < needed {
            return Err(format!(
                "layout.input_height ({}) is too small: {needed} needed for input_size {}",
                self.layout.input_height, self.layout.input_size
            ));
        }
        if self.max_lifetime < Duration::from_secs(1) {
            return Err("max_lifetime_secs must be at least 1".to_string());
        }
        // The only real constraint on width is that something is left for the
        // text after the icon column; there is no arbitrary minimum beyond it.
        const MIN_TEXT: u32 = 32;
        if self.layout.text_width(self.width) < MIN_TEXT {
            return Err(format!(
                "width ({}) leaves only {}px for text after the icon column; \
                 needs at least {} for these layout values",
                self.width,
                self.layout.text_width(self.width),
                self.layout.text_x() + self.layout.padding * 2 + MIN_TEXT,
            ));
        }
        Ok(())
    }

    /// Validates as if this config were fully resolved with `theme` and
    /// `colors` applied, without mutating anything.
    fn check_with(&self, theme: Option<Theme>, colors: &FileColors) -> Result<(), String> {
        let mut probe = self.clone();
        match theme {
            Some(Theme::Dark) => probe.dark = true,
            Some(Theme::Light) => probe.dark = false,
            Some(Theme::System) | None => {}
        }
        probe.colors = Colors::for_mode(probe.dark);
        colors.apply(&mut probe.colors);
        probe.validate()
    }

    /// The first rule that applies to a desktop entry id, if any.
    pub fn app_rule(&self, id: &str) -> Option<&AppRule> {
        self.app_rules.iter().find(|rule| rule.matches(id))
    }

    /// Serialises the resolved config, for `--dump-config`.
    pub fn to_yaml(&self) -> String {
        serde_yaml::to_string(self)
            .unwrap_or_else(|err| format!("# could not serialise config: {err}\n"))
    }
}

impl FileColors {
    fn apply(&self, colors: &mut Colors) {
        if let Some(c) = self.background {
            colors.background = c;
        }
        if let Some(c) = self.selection {
            colors.selection = c;
        }
        if let Some(c) = self.name {
            colors.name = c;
        }
        if let Some(c) = self.description {
            colors.description = c;
        }
        if let Some(c) = self.name_selected {
            colors.name_selected = c;
        }
        if let Some(c) = self.description_selected {
            colors.description_selected = c;
        }
        if let Some(c) = self.query {
            colors.query = c;
        }
        if let Some(c) = self.placeholder {
            colors.placeholder = c;
        }
        if let Some(c) = self.divider {
            colors.divider = c;
        }
    }
}

/// Raw command line flags, before being folded into a [`Config`].
#[derive(Debug, Default)]
struct Flags {
    mode: Mode,
    config_path: Option<PathBuf>,
    width: Option<u32>,
    max_rows: Option<usize>,
    display: Option<Display>,
    font: Option<String>,
    icon_theme: Option<String>,
    terminal: Option<String>,
    dark: Option<bool>,
    query: Option<String>,
}

impl Flags {
    /// Unknown flags are an error: a keybinding is the only place these are
    /// ever typed, so silently ignoring a typo would be worse than refusing.
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut flags = Self::default();
        let mut args = args.peekable();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => flags.mode = Mode::Help,
                "-V" | "--version" => flags.mode = Mode::Version,
                "--list" => flags.mode = Mode::List,
                "--dump-config" => flags.mode = Mode::DumpConfig,
                "--light" => flags.dark = Some(false),
                "--dark" => flags.dark = Some(true),
                "--width" => flags.width = Some(parse_next(&mut args, "--width")?),
                "--max-rows" => flags.max_rows = Some(parse_next(&mut args, "--max-rows")?),
                "--query" => {
                    flags.query =
                        Some(args.next().ok_or_else(|| "--query needs a value".to_string())?);
                }
                "--display" => {
                    let raw = args.next().ok_or_else(|| "--display needs a value".to_string())?;
                    flags.display = Some(raw.parse()?);
                }
                "--config" => {
                    flags.config_path = Some(PathBuf::from(
                        args.next().ok_or_else(|| "--config needs a path".to_string())?,
                    ));
                }
                "--icon-theme" => {
                    flags.icon_theme =
                        Some(args.next().ok_or_else(|| "--icon-theme needs a value".to_string())?);
                }
                "--terminal" => {
                    flags.terminal =
                        Some(args.next().ok_or_else(|| "--terminal needs a value".to_string())?);
                }
                "--font" => {
                    flags.font =
                        Some(args.next().ok_or_else(|| "--font needs a value".to_string())?);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }

        Ok(flags)
    }
}

fn parse_next<T: std::str::FromStr>(
    args: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String> {
    let raw = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
    raw.trim().parse().map_err(|_| format!("{flag}: not a number: {raw}"))
}

fn serialize_secs<S: serde::Serializer>(value: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_u64(value.as_secs())
}

fn serialize_theme<S: serde::Serializer>(dark: &bool, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(if *dark { "dark" } else { "light" })
}

/// `~/.config/xla/config.yaml`, honouring `XDG_CONFIG_HOME`.
pub fn default_config_path() -> PathBuf {
    config_home().join("xla/config.yaml")
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn read_cosmic(dir: &str, key: &str) -> Option<String> {
    std::fs::read_to_string(config_home().join(dir).join(key)).ok()
}

/// Extracts `field: "value"` out of a RON struct without a RON parser.
fn ron_field(raw: &str, field: &str) -> Option<String> {
    let after = raw.split_once(&format!("{field}:"))?.1;
    let opened = after.split_once('"')?.1;
    let (value, _) = opened.split_once('"')?;
    (!value.is_empty()).then(|| value.to_string())
}

pub const USAGE: &str = "\
xla - COSMIC application launcher

Usage: xla [options]

Shows a centered list of installed applications with a query row above it.
Type to filter, Up/Down to pick, Enter to launch, Escape to dismiss. Bind it
to a key combination in COSMIC Settings > Keyboard > Shortcuts.

Configuration is read from ~/.config/xla/config.yaml; these flags override it.

Options:
      --query <text>     start with this query already typed
      --list             print the application list to stdout and exit
      --dump-config      print the resolved configuration and exit
      --config <path>    read this config file instead of the default
      --width <px>       width of the launcher
      --max-rows <n>     result rows shown before scrolling
      --display <d>      active, primary, or an output name like HDMI-A-1
      --icon-theme <s>   icon theme to search
      --font <family>    font family
      --terminal <cmd>   command prefix for Terminal=true entries
      --dark, --light    force a palette
  -h, --help             print this help
  -V, --version          print the version
";

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a config from YAML text, exercising the same merge the file
    /// layer uses but without touching the filesystem.
    fn from_yaml(text: &str) -> Result<Config, String> {
        let file: FileConfig = serde_yaml::from_str(text).map_err(|err| err.to_string())?;
        let mut config = Config::default();
        let mut theme = None;
        let mut colors = FileColors::default();
        config.apply_file(file, &mut theme, &mut colors);
        if let Some(Theme::Light) = theme {
            config.dark = false;
        }
        config.colors = Colors::for_mode(config.dark);
        colors.apply(&mut config.colors);
        config.validate()?;
        Ok(config)
    }

    fn flags(args: &[&str]) -> Result<Flags, String> {
        Flags::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn ron_field_extracts_quoted_value() {
        let raw = "(\n    family: \"Hurmit Nerd Font Propo\",\n    weight: Normal,\n)";
        assert_eq!(ron_field(raw, "family").as_deref(), Some("Hurmit Nerd Font Propo"));
        assert_eq!(ron_field(raw, "missing"), None);
        assert_eq!(ron_field("( family: \"\" )", "family"), None);
    }

    #[test]
    fn unknown_flag_is_an_error() {
        assert!(flags(&["--nope"]).is_err());
    }

    #[test]
    fn flags_are_parsed() {
        assert_eq!(flags(&["--list"]).unwrap().mode, Mode::List);
        assert_eq!(flags(&["--dump-config"]).unwrap().mode, Mode::DumpConfig);
        assert_eq!(flags(&["--width", "800"]).unwrap().width, Some(800));
        assert_eq!(flags(&["--query", "term"]).unwrap().query.as_deref(), Some("term"));
        assert_eq!(flags(&["--dark"]).unwrap().dark, Some(true));
        assert_eq!(flags(&["--light"]).unwrap().dark, Some(false));
        assert!(flags(&["--width", "abc"]).is_err());
        assert!(flags(&["--width"]).is_err());
        assert!(flags(&["--query"]).is_err());
        assert!(flags(&["--config"]).is_err());
    }

    #[test]
    fn empty_config_is_valid() {
        assert_eq!(from_yaml("{}").unwrap().width, Config::default().width);
    }

    #[test]
    fn unknown_config_key_is_rejected() {
        // Catching a typo matters more than tolerating it: a silently ignored
        // key looks exactly like a feature that does not work.
        assert!(from_yaml("wdith: 700").is_err());
        assert!(from_yaml("layout:\n  row_hight: 60").is_err());
        assert!(from_yaml("app_rules:\n  - id: a\n    nope: 1").is_err());
        // xsw's keys are not xla's; a copied config should say so rather than
        // quietly dropping half of itself.
        assert!(from_yaml("show_titles: false").is_err());
    }

    #[test]
    fn scalars_override_defaults() {
        let c = from_yaml(
            "width: 700\nmax_rows: 5\nhistory: false\nshow_descriptions: false\ntoggle: false",
        )
        .unwrap();
        assert_eq!(c.width, 700);
        assert_eq!(c.max_rows, 5);
        assert!(!c.history);
        assert!(!c.show_descriptions);
        assert!(!c.toggle);
    }

    #[test]
    fn layout_uses_defaults_for_absent_fields() {
        let c = from_yaml("layout:\n  row_height: 72").unwrap();
        assert_eq!(c.layout.row_height, 72);
        assert_eq!(c.layout.icon_size, Layout::default().icon_size, "kept default");
    }

    #[test]
    fn invalid_dimensions_are_rejected() {
        assert!(from_yaml("width: 10").is_err());
        assert!(from_yaml("max_rows: 0").is_err());
        // Too short for a 30px icon plus two lines of text.
        assert!(from_yaml("layout:\n  row_height: 20").is_err());
        // The query row has to fit one line of its own text.
        assert!(from_yaml("layout:\n  input_height: 8").is_err());
        // A huge icon leaves no width for the text column.
        assert!(from_yaml("layout:\n  icon_size: 600\n  row_height: 620").is_err());
    }

    #[test]
    fn height_accounts_for_the_query_row() {
        let layout = Layout::default();
        // padding * 2 + input_height + row_height * rows.
        assert_eq!(layout.height_for(0), 64);
        assert_eq!(layout.height_for(1), 110);
        assert_eq!(layout.height_for(8), 432);
        assert_eq!(layout.rows_top(), 54, "padding + input_height");
    }

    #[test]
    fn text_width_matches_the_drawn_geometry() {
        // Guards the shared definition: if this drifts from render.rs the
        // width check would pass while text overflowed the panel.
        let layout = Layout::default();
        assert_eq!(layout.icon_x(), 15, "padding + padding / 2");
        assert_eq!(layout.text_x(), 55, "icon_x + icon_size + icon_gap");
        assert_eq!(layout.text_width(560), 485, "width - text_x - padding * 2");
        assert_eq!(layout.text_width(50), 0, "saturates rather than underflowing");
    }

    #[test]
    fn a_bad_value_in_the_file_is_a_warning_not_a_failure() {
        // The whole point: editing the config to something invalid must leave
        // a working launcher, since it runs from a keybinding with no terminal.
        let base = Config::default();
        let file: FileConfig = serde_yaml::from_str("width: 20").unwrap();
        let mut candidate = base.clone();
        let mut theme = None;
        let mut colors = FileColors::default();
        candidate.apply_file(file, &mut theme, &mut colors);
        assert!(candidate.check_with(theme, &colors).is_err(), "rejected");
        // The caller keeps `base`, which is still valid.
        assert!(base.validate().is_ok());
    }

    #[test]
    fn theme_light_switches_the_palette() {
        let dark = from_yaml("theme: dark").unwrap();
        let light = from_yaml("theme: light").unwrap();
        assert_ne!(dark.colors.background, light.colors.background);
    }

    #[test]
    fn colors_parse_and_override() {
        let c =
            from_yaml("colors:\n  background: \"#102030\"\n  divider: \"#01020304\"").unwrap();
        assert_eq!(c.colors.background, Rgba::new(0x10, 0x20, 0x30, 255));
        assert_eq!(c.colors.divider, Rgba::new(1, 2, 3, 4));
        // Untouched entries keep the palette default.
        assert_eq!(c.colors.name, Colors::for_mode(true).name);
    }

    #[test]
    fn bad_colors_are_rejected() {
        assert!(from_yaml("colors:\n  background: \"#12345\"").is_err());
        assert!(from_yaml("colors:\n  background: \"nope\"").is_err());
    }

    #[test]
    fn rgba_roundtrips_through_its_text_form() {
        let colour: Rgba = "#0a0b0c0d".parse().unwrap();
        assert_eq!(colour.to_string(), "#0a0b0c0d");
        assert_eq!("#aabbcc".parse::<Rgba>().unwrap().a, 255, "alpha defaults to opaque");
    }

    #[test]
    fn display_parses_its_three_forms() {
        assert_eq!(from_yaml("display: active").unwrap().display, Display::Active);
        assert_eq!(from_yaml("display: primary").unwrap().display, Display::Primary);
        // "main" is accepted as a synonym, since that is what people call it.
        assert_eq!(from_yaml("display: main").unwrap().display, Display::Primary);
        assert_eq!(
            from_yaml("display: HDMI-A-1").unwrap().display,
            Display::Named("HDMI-A-1".to_string()),
            "anything unrecognised is an output name, not an error"
        );
        assert_eq!(Config::default().display, Display::Primary);
        assert_eq!(Display::default(), Config::default().display, "enum and config agree");
    }

    #[test]
    fn display_rejects_an_empty_value() {
        assert!(from_yaml("display: \"\"").is_err());
    }

    #[test]
    fn display_round_trips_through_its_text_form() {
        for value in ["active", "primary", "DP-2"] {
            let parsed: Display = value.parse().unwrap();
            assert_eq!(parsed.to_string(), value);
        }
        // The dump has to be re-readable, so a named display must survive it.
        let config = Config { display: Display::Named("DP-1".to_string()), ..Config::default() };
        let reparsed: FileConfig = serde_yaml::from_str(&config.to_yaml()).unwrap();
        assert_eq!(reparsed.display, Some(Display::Named("DP-1".to_string())));
    }

    #[test]
    fn app_rules_match_by_id_case_insensitively() {
        let c = from_yaml(
            "app_rules:\n  - id: Chromium\n    name: Browser\n  - id: htop\n    hide: true\n",
        )
        .unwrap();
        assert_eq!(c.app_rule("chromium").unwrap().name.as_deref(), Some("Browser"));
        assert!(c.app_rule("htop").unwrap().hide);
        assert!(c.app_rule("firefox").is_none());
    }

    #[test]
    fn first_matching_app_rule_wins() {
        let c = from_yaml("app_rules:\n  - id: a\n    name: First\n  - id: a\n    name: Second")
            .unwrap();
        assert_eq!(c.app_rule("a").unwrap().name.as_deref(), Some("First"));
    }

    #[test]
    fn dump_config_is_valid_yaml_that_parses_back() {
        // --dump-config output should be usable as a starting config, so every
        // key it emits has to be one the file layer accepts.
        let dumped = Config::default().to_yaml();
        assert!(dumped.contains("max_lifetime_secs:"), "durations use their file names");
        let reparsed: FileConfig =
            serde_yaml::from_str(&dumped).expect("dumped config must round-trip");
        assert_eq!(reparsed.width, Some(Config::default().width));
        assert!(reparsed.theme.is_some(), "theme survives the round trip");
    }
}
