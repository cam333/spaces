//! Themes: every colour the client's own chrome is drawn in, in one place.
//!
//! The palette's roles and the built-in themes are herdr's
//! (`src/app/state.rs` and `src/config/theme.rs` in
//! <https://github.com/herdrdev/herdr>, Apache-2.0): the same names, the
//! same aliases, the same values. One deliberate difference is `terminal`. In
//! herdr it is a 16-colour theme of its own; here it is exactly what this
//! client drew before it had themes (indexed ANSI colours that follow the
//! terminal's own palette, reversed video for selection, no backgrounds), so
//! choosing it gives back the old look unchanged.
//!
//! Pane output is never themed: an agent's colours are its own. A theme
//! colours the sidebar, the tab bar, the status bar and the overlays.
//!
//! The choice belongs to the machine the client runs on, like tips: it is
//! saved as `"theme"` in `~/.spaces/tui.json` from the menu (`ctrl+k`, then
//! "theme"). With nothing saved the client starts in [`DEFAULT_THEME`].

use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders};

/// Every colour role the chrome uses. Field names and meanings are herdr's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Primary accent: the brand mark, the active tab, matched letters.
    pub accent: Color,
    /// Background of the tab bar, the status bar and the overlays.
    pub panel_bg: Color,
    /// Sidebar background. `Reset` keeps the terminal's own.
    pub sidebar_bg: Color,
    /// Background of the active space's row and the focused agent's rows.
    pub active_row_bg: Color,
    /// Background of the selected row in a list.
    pub selection_bg: Color,
    /// Subtle surface.
    pub surface0: Color,
    /// Rules and edges: the sidebar's edge, overlay borders.
    pub surface1: Color,
    /// Very dim surface.
    pub surface_dim: Color,
    /// Muted text: headers, hints, secondary lines.
    pub overlay0: Color,
    /// Slightly brighter muted text.
    pub overlay1: Color,
    /// Main text.
    pub text: Color,
    /// Subdued text: inactive tabs, menu entries.
    pub subtext0: Color,
    /// Git branches and other special labels.
    pub mauve: Color,
    /// Idle agents; success.
    pub green: Color,
    /// Working agents; the Files tab.
    pub yellow: Color,
    /// Blocked agents; destructive entries; errors.
    pub red: Color,
    /// Folders.
    pub blue: Color,
    /// Finished agents; links.
    pub teal: Color,
    /// Warnings.
    pub peach: Color,
}

impl Theme {
    /// Whether this is the theme that leaves the terminal's own colours
    /// alone. Decided by the panel background, which every other theme sets.
    pub fn is_terminal(&self) -> bool {
        self.panel_bg == Color::Reset
    }

    /// Text drawn on a solid colour: a pill, a toast, the active tab.
    pub fn ink(&self) -> Color {
        if self.is_terminal() {
            Color::Black
        } else {
            self.panel_bg
        }
    }

    /// The selected row of a list. Reversed video in `terminal`, where no
    /// background colour can be known to contrast with the text; the theme's
    /// selection background everywhere else.
    pub fn selected(&self) -> Style {
        if self.selection_bg == Color::Reset {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().fg(self.text).bg(self.selection_bg)
        }
    }

    /// A solid chip in `color`: the active tab, the open Files tab.
    pub fn chip(&self, color: Color) -> Style {
        if self.is_terminal() {
            Style::new()
                .fg(color)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::new()
                .fg(self.ink())
                .bg(color)
                .add_modifier(Modifier::BOLD)
        }
    }

    /// The colour of the active tab when its pane has no accent of its own.
    pub fn active_tab(&self) -> Color {
        if self.is_terminal() {
            self.overlay1
        } else {
            self.accent
        }
    }

    /// Letters a search matched.
    pub fn highlight(&self) -> Color {
        if self.is_terminal() {
            self.teal
        } else {
            self.accent
        }
    }

    /// The bordered box of an overlay (the menu, the right-click menu): the
    /// panel background, muted borders and an accented title. Plain borders in
    /// `terminal`.
    pub fn overlay_block(&self) -> Block<'static> {
        let block = Block::default().borders(Borders::ALL);
        if self.is_terminal() {
            return block;
        }
        block
            .style(Style::new().fg(self.text).bg(self.panel_bg))
            .border_style(Style::new().fg(self.surface1))
            .title_style(Style::new().fg(self.accent).add_modifier(Modifier::BOLD))
    }
}

/// Starting theme when this machine has never chosen one: herdr's default.
pub const DEFAULT_THEME: &str = "catppuccin";

/// Every built-in theme, in the order the picker lists them (herdr's order).
pub const THEMES: &[(&str, Theme)] = &[
    ("catppuccin", CATPPUCCIN),
    ("catppuccin-latte", CATPPUCCIN_LATTE),
    ("terminal", TERMINAL),
    ("tokyo-night", TOKYO_NIGHT),
    ("tokyo-night-day", TOKYO_NIGHT_DAY),
    ("dracula", DRACULA),
    ("nord", NORD),
    ("gruvbox", GRUVBOX),
    ("gruvbox-light", GRUVBOX_LIGHT),
    ("one-dark", ONE_DARK),
    ("one-light", ONE_LIGHT),
    ("solarized", SOLARIZED),
    ("solarized-light", SOLARIZED_LIGHT),
    ("kanagawa", KANAGAWA),
    ("kanagawa-lotus", KANAGAWA_LOTUS),
    ("rose-pine", ROSE_PINE),
    ("rose-pine-dawn", ROSE_PINE_DAWN),
    ("vesper", VESPER),
];

/// The built-in name `name` refers to, accepting herdr's aliases, any case,
/// and spaces or underscores for dashes. `None` for an unknown name.
pub fn canonical_name(name: &str) -> Option<&'static str> {
    match name.trim().to_lowercase().replace([' ', '_'], "-").as_str() {
        "catppuccin" | "catppuccin-mocha" => Some("catppuccin"),
        "catppuccin-latte" | "latte" | "light" => Some("catppuccin-latte"),
        "terminal" => Some("terminal"),
        "tokyo-night" | "tokyonight" => Some("tokyo-night"),
        "tokyo-night-day" | "tokyo-day" | "tokyonight-day" => Some("tokyo-night-day"),
        "dracula" => Some("dracula"),
        "nord" => Some("nord"),
        "gruvbox" | "gruvbox-dark" => Some("gruvbox"),
        "gruvbox-light" => Some("gruvbox-light"),
        "one-dark" | "onedark" => Some("one-dark"),
        "one-light" | "onelight" => Some("one-light"),
        "solarized" | "solarized-dark" => Some("solarized"),
        "solarized-light" => Some("solarized-light"),
        "kanagawa" => Some("kanagawa"),
        "kanagawa-lotus" | "lotus" => Some("kanagawa-lotus"),
        "rose-pine" | "rosepine" => Some("rose-pine"),
        "rose-pine-dawn" | "rosepine-dawn" | "dawn" => Some("rose-pine-dawn"),
        "vesper" => Some("vesper"),
        _ => None,
    }
}

#[cfg(test)]
/// The theme called `name` (or one of its aliases), or `None`.
pub fn by_name(name: &str) -> Option<Theme> {
    index_of(name).map(|i| THEMES[i].1)
}

fn index_of(name: &str) -> Option<usize> {
    let canonical = canonical_name(name)?;
    THEMES.iter().position(|(n, _)| *n == canonical)
}

/// No preview is showing.
const NO_PREVIEW: usize = usize::MAX;

/// The chosen theme, as an index into [`THEMES`]. Starts at `terminal` so
/// that anything rendering before [`choose`] (the test suite, chiefly) draws
/// exactly what the client drew before it had themes.
static CHOSEN: AtomicUsize = AtomicUsize::new(2);
/// The theme the picker is previewing, or [`NO_PREVIEW`].
static PREVIEW: AtomicUsize = AtomicUsize::new(NO_PREVIEW);

const _: () = assert!(
    matches!(THEMES[2].1.panel_bg, Color::Reset),
    "THEMES[2] must be terminal"
);

/// The theme to draw with now: the one being previewed, else the chosen one.
pub fn current() -> Theme {
    let preview = PREVIEW.load(Ordering::Relaxed);
    let index = if preview < THEMES.len() {
        preview
    } else {
        CHOSEN.load(Ordering::Relaxed)
    };
    THEMES.get(index).map_or(TERMINAL, |(_, t)| *t)
}

/// The chosen theme's name (not the previewed one).
pub fn current_name() -> &'static str {
    THEMES
        .get(CHOSEN.load(Ordering::Relaxed))
        .map_or("terminal", |(n, _)| n)
}

/// Makes `name` the chosen theme. Returns its canonical name, or `None` (and
/// changes nothing) when no theme has that name.
pub fn choose(name: &str) -> Option<&'static str> {
    let index = index_of(name)?;
    CHOSEN.store(index, Ordering::Relaxed);
    Some(THEMES[index].0)
}

/// Shows `name` in place of the chosen theme until the next call; `None`
/// (or an unknown name) goes back to the chosen one.
pub fn preview(name: Option<&str>) {
    let index = name.and_then(index_of).unwrap_or(NO_PREVIEW);
    PREVIEW.store(index, Ordering::Relaxed);
}

/// The theme this machine saved, or [`DEFAULT_THEME`]. An unknown saved name
/// falls back too, rather than leaving the client without colours.
pub fn saved() -> &'static str {
    crate::tips::read_pref("theme")
        .and_then(|v| v.as_str().and_then(canonical_name))
        .unwrap_or(DEFAULT_THEME)
}

/// Saves `name` as this machine's theme. Best effort, like tips.
pub fn save(name: &str) {
    crate::tips::save_pref("theme", serde_json::Value::String(name.to_string()));
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// What this client drew before it had themes.
pub const TERMINAL: Theme = Theme {
    accent: super::brand::ACCENT, // the brand indigo the mark always had
    panel_bg: Color::Reset,
    sidebar_bg: Color::Reset,
    active_row_bg: Color::Reset,
    selection_bg: Color::Reset,
    surface0: Color::Reset,
    surface1: Color::DarkGray,
    surface_dim: Color::DarkGray,
    overlay0: Color::DarkGray,
    overlay1: Color::White,
    text: Color::Reset,
    subtext0: Color::Gray,
    mauve: Color::DarkGray,
    green: Color::Green,
    yellow: Color::Yellow,
    red: Color::Red,
    blue: Color::Blue,
    teal: Color::Cyan,
    peach: Color::Yellow,
};

/// Catppuccin Mocha.
pub const CATPPUCCIN: Theme = Theme {
    accent: rgb(137, 180, 250),
    panel_bg: rgb(24, 24, 37),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(30, 30, 46),
    selection_bg: rgb(49, 50, 68),
    surface0: rgb(49, 50, 68),
    surface1: rgb(69, 71, 90),
    surface_dim: rgb(30, 30, 46),
    overlay0: rgb(108, 112, 134),
    overlay1: rgb(127, 132, 156),
    text: rgb(205, 214, 244),
    subtext0: rgb(166, 173, 200),
    mauve: rgb(203, 166, 247),
    green: rgb(166, 227, 161),
    yellow: rgb(249, 226, 175),
    red: rgb(243, 139, 168),
    blue: rgb(137, 180, 250),
    teal: rgb(148, 226, 213),
    peach: rgb(250, 179, 135),
};

/// Catppuccin Latte.
pub const CATPPUCCIN_LATTE: Theme = Theme {
    accent: rgb(30, 102, 245),
    panel_bg: rgb(239, 241, 245),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(230, 233, 239),
    selection_bg: rgb(189, 208, 245),
    surface0: rgb(204, 208, 218),
    surface1: rgb(188, 192, 204),
    surface_dim: rgb(230, 233, 239),
    overlay0: rgb(156, 160, 176),
    overlay1: rgb(140, 143, 161),
    text: rgb(76, 79, 105),
    subtext0: rgb(108, 111, 133),
    mauve: rgb(136, 57, 239),
    green: rgb(64, 160, 43),
    yellow: rgb(223, 142, 29),
    red: rgb(210, 15, 57),
    blue: rgb(30, 102, 245),
    teal: rgb(23, 146, 153),
    peach: rgb(254, 100, 11),
};

/// Tokyo Night.
pub const TOKYO_NIGHT: Theme = Theme {
    accent: rgb(122, 162, 247),
    panel_bg: rgb(26, 27, 38),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(35, 38, 54),
    selection_bg: rgb(45, 54, 80),
    surface0: rgb(36, 40, 59),
    surface1: rgb(65, 72, 104),
    surface_dim: rgb(26, 27, 38),
    overlay0: rgb(86, 95, 137),
    overlay1: rgb(105, 113, 150),
    text: rgb(192, 202, 245),
    subtext0: rgb(169, 177, 214),
    mauve: rgb(187, 154, 247),
    green: rgb(158, 206, 106),
    yellow: rgb(224, 175, 104),
    red: rgb(247, 118, 142),
    blue: rgb(122, 162, 247),
    teal: rgb(125, 207, 255),
    peach: rgb(255, 158, 100),
};

/// Tokyo Night Day.
pub const TOKYO_NIGHT_DAY: Theme = Theme {
    accent: rgb(46, 125, 233),
    panel_bg: rgb(225, 226, 231),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(210, 211, 218),
    selection_bg: rgb(182, 202, 231),
    surface0: rgb(196, 200, 218),
    surface1: rgb(168, 174, 203),
    surface_dim: rgb(210, 211, 218),
    overlay0: rgb(137, 144, 179),
    overlay1: rgb(104, 112, 154),
    text: rgb(55, 96, 191),
    subtext0: rgb(97, 114, 176),
    mauve: rgb(120, 71, 189),
    green: rgb(88, 117, 57),
    yellow: rgb(140, 108, 62),
    red: rgb(245, 42, 101),
    blue: rgb(46, 125, 233),
    teal: rgb(17, 140, 116),
    peach: rgb(177, 92, 0),
};

/// Dracula.
pub const DRACULA: Theme = Theme {
    accent: rgb(189, 147, 249),
    panel_bg: rgb(40, 42, 54),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(55, 60, 82),
    selection_bg: rgb(70, 63, 93),
    surface0: rgb(68, 71, 90),
    surface1: rgb(98, 114, 164),
    surface_dim: rgb(40, 42, 54),
    overlay0: rgb(98, 114, 164),
    overlay1: rgb(130, 140, 180),
    text: rgb(248, 248, 242),
    subtext0: rgb(210, 210, 220),
    mauve: rgb(255, 121, 198),
    green: rgb(80, 250, 123),
    yellow: rgb(241, 250, 140),
    red: rgb(255, 85, 85),
    blue: rgb(139, 233, 253),
    teal: rgb(139, 233, 253),
    peach: rgb(255, 184, 108),
};

/// Nord.
pub const NORD: Theme = Theme {
    accent: rgb(136, 192, 208),
    panel_bg: rgb(46, 52, 64),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(67, 76, 94),
    selection_bg: rgb(64, 80, 93),
    surface0: rgb(59, 66, 82),
    surface1: rgb(67, 76, 94),
    surface_dim: rgb(46, 52, 64),
    overlay0: rgb(76, 86, 106),
    overlay1: rgb(100, 110, 130),
    text: rgb(236, 239, 244),
    subtext0: rgb(216, 222, 233),
    mauve: rgb(180, 142, 173),
    green: rgb(163, 190, 140),
    yellow: rgb(235, 203, 139),
    red: rgb(191, 97, 106),
    blue: rgb(129, 161, 193),
    teal: rgb(143, 188, 187),
    peach: rgb(208, 135, 112),
};

/// Gruvbox Dark.
pub const GRUVBOX: Theme = Theme {
    accent: rgb(215, 153, 33),
    panel_bg: rgb(40, 40, 40),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(50, 49, 48),
    selection_bg: rgb(75, 63, 39),
    surface0: rgb(60, 56, 54),
    surface1: rgb(80, 73, 69),
    surface_dim: rgb(40, 40, 40),
    overlay0: rgb(146, 131, 116),
    overlay1: rgb(168, 153, 132),
    text: rgb(235, 219, 178),
    subtext0: rgb(213, 196, 161),
    mauve: rgb(211, 134, 155),
    green: rgb(184, 187, 38),
    yellow: rgb(250, 189, 47),
    red: rgb(251, 73, 52),
    blue: rgb(131, 165, 152),
    teal: rgb(142, 192, 124),
    peach: rgb(254, 128, 25),
};

/// Gruvbox Light.
pub const GRUVBOX_LIGHT: Theme = Theme {
    accent: rgb(7, 102, 120),
    panel_bg: rgb(251, 241, 199),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(242, 229, 188),
    selection_bg: rgb(235, 219, 178),
    surface0: rgb(235, 219, 178),
    surface1: rgb(213, 196, 161),
    surface_dim: rgb(242, 229, 188),
    overlay0: rgb(146, 131, 116),
    overlay1: rgb(124, 111, 100),
    text: rgb(60, 56, 54),
    subtext0: rgb(80, 73, 69),
    mauve: rgb(143, 63, 113),
    green: rgb(121, 116, 14),
    yellow: rgb(181, 118, 20),
    red: rgb(157, 0, 6),
    blue: rgb(7, 102, 120),
    teal: rgb(66, 123, 88),
    peach: rgb(175, 58, 3),
};

/// One Dark.
pub const ONE_DARK: Theme = Theme {
    accent: rgb(97, 175, 239),
    panel_bg: rgb(40, 44, 52),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(49, 54, 64),
    selection_bg: rgb(51, 70, 89),
    surface0: rgb(44, 49, 58),
    surface1: rgb(62, 68, 81),
    surface_dim: rgb(40, 44, 52),
    overlay0: rgb(92, 99, 112),
    overlay1: rgb(115, 122, 135),
    text: rgb(171, 178, 191),
    subtext0: rgb(150, 156, 168),
    mauve: rgb(198, 120, 221),
    green: rgb(152, 195, 121),
    yellow: rgb(229, 192, 123),
    red: rgb(224, 108, 117),
    blue: rgb(97, 175, 239),
    teal: rgb(86, 182, 194),
    peach: rgb(209, 154, 102),
};

/// One Light.
pub const ONE_LIGHT: Theme = Theme {
    accent: rgb(64, 120, 242),
    panel_bg: rgb(250, 250, 250),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(216, 219, 226),
    selection_bg: rgb(205, 219, 248),
    surface0: rgb(240, 240, 241),
    surface1: rgb(229, 229, 230),
    surface_dim: rgb(245, 245, 246),
    overlay0: rgb(160, 161, 167),
    overlay1: rgb(104, 107, 119),
    text: rgb(56, 58, 66),
    subtext0: rgb(104, 107, 119),
    mauve: rgb(166, 38, 164),
    green: rgb(80, 161, 79),
    yellow: rgb(193, 132, 1),
    red: rgb(228, 86, 73),
    blue: rgb(64, 120, 242),
    teal: rgb(1, 132, 188),
    peach: rgb(152, 104, 1),
};

/// Solarized Dark.
pub const SOLARIZED: Theme = Theme {
    accent: rgb(38, 139, 210),
    panel_bg: rgb(0, 43, 54),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(22, 75, 87),
    selection_bg: rgb(8, 62, 85),
    surface0: rgb(7, 54, 66),
    surface1: rgb(88, 110, 117),
    surface_dim: rgb(0, 43, 54),
    overlay0: rgb(88, 110, 117),
    overlay1: rgb(101, 123, 131),
    text: rgb(147, 161, 161),
    subtext0: rgb(131, 148, 150),
    mauve: rgb(211, 54, 130),
    green: rgb(133, 153, 0),
    yellow: rgb(181, 137, 0),
    red: rgb(220, 50, 47),
    blue: rgb(38, 139, 210),
    teal: rgb(42, 161, 152),
    peach: rgb(203, 75, 22),
};

/// Solarized Light.
pub const SOLARIZED_LIGHT: Theme = Theme {
    accent: rgb(38, 139, 210),
    panel_bg: rgb(253, 246, 227),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(238, 232, 213),
    selection_bg: rgb(201, 220, 223),
    surface0: rgb(238, 232, 213),
    surface1: rgb(147, 161, 161),
    surface_dim: rgb(238, 232, 213),
    overlay0: rgb(147, 161, 161),
    overlay1: rgb(88, 110, 117),
    text: rgb(101, 123, 131),
    subtext0: rgb(131, 148, 150),
    mauve: rgb(211, 54, 130),
    green: rgb(133, 153, 0),
    yellow: rgb(181, 137, 0),
    red: rgb(220, 50, 47),
    blue: rgb(38, 139, 210),
    teal: rgb(42, 161, 152),
    peach: rgb(203, 75, 22),
};

/// Kanagawa.
pub const KANAGAWA: Theme = Theme {
    accent: rgb(126, 156, 216),
    panel_bg: rgb(31, 31, 40),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(54, 54, 70),
    selection_bg: rgb(50, 56, 75),
    surface0: rgb(42, 42, 55),
    surface1: rgb(54, 54, 70),
    surface_dim: rgb(31, 31, 40),
    overlay0: rgb(114, 113, 105),
    overlay1: rgb(135, 134, 125),
    text: rgb(220, 215, 186),
    subtext0: rgb(200, 195, 170),
    mauve: rgb(149, 127, 184),
    green: rgb(118, 148, 106),
    yellow: rgb(192, 163, 110),
    red: rgb(195, 64, 67),
    blue: rgb(126, 156, 216),
    teal: rgb(127, 180, 202),
    peach: rgb(255, 160, 102),
};

/// Kanagawa Lotus.
pub const KANAGAWA_LOTUS: Theme = Theme {
    accent: rgb(77, 105, 155),
    panel_bg: rgb(242, 236, 188),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(213, 206, 163),
    selection_bg: rgb(220, 213, 172),
    surface0: rgb(220, 213, 172),
    surface1: rgb(201, 203, 209),
    surface_dim: rgb(213, 206, 163),
    overlay0: rgb(160, 156, 172),
    overlay1: rgb(138, 137, 128),
    text: rgb(84, 84, 100),
    subtext0: rgb(67, 67, 108),
    mauve: rgb(98, 76, 131),
    green: rgb(111, 137, 78),
    yellow: rgb(119, 113, 63),
    red: rgb(200, 64, 83),
    blue: rgb(77, 105, 155),
    teal: rgb(78, 140, 162),
    peach: rgb(204, 109, 0),
};

/// Rosé Pine.
pub const ROSE_PINE: Theme = Theme {
    accent: rgb(196, 167, 231),
    panel_bg: rgb(25, 23, 36),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(38, 35, 58),
    selection_bg: rgb(59, 52, 75),
    surface0: rgb(31, 29, 46),
    surface1: rgb(38, 35, 58),
    surface_dim: rgb(38, 35, 58),
    overlay0: rgb(110, 106, 134),
    overlay1: rgb(144, 140, 170),
    text: rgb(224, 222, 244),
    subtext0: rgb(200, 197, 220),
    mauve: rgb(196, 167, 231),
    green: rgb(49, 116, 143),
    yellow: rgb(246, 193, 119),
    red: rgb(235, 111, 146),
    blue: rgb(49, 116, 143),
    teal: rgb(156, 207, 216),
    peach: rgb(234, 154, 151),
};

/// Rosé Pine Dawn.
pub const ROSE_PINE_DAWN: Theme = Theme {
    accent: rgb(144, 122, 169),
    panel_bg: rgb(250, 244, 237),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(227, 217, 207),
    selection_bg: rgb(242, 233, 225),
    surface0: rgb(242, 233, 225),
    surface1: rgb(255, 250, 243),
    surface_dim: rgb(242, 233, 225),
    overlay0: rgb(152, 147, 165),
    overlay1: rgb(121, 117, 147),
    text: rgb(70, 66, 97),
    subtext0: rgb(121, 117, 147),
    mauve: rgb(144, 122, 169),
    green: rgb(40, 105, 131),
    yellow: rgb(234, 157, 52),
    red: rgb(180, 99, 122),
    blue: rgb(40, 105, 131),
    teal: rgb(86, 148, 159),
    peach: rgb(215, 130, 126),
};

/// Vesper.
pub const VESPER: Theme = Theme {
    accent: rgb(255, 199, 153),
    panel_bg: rgb(26, 26, 26),
    sidebar_bg: Color::Reset,
    active_row_bg: rgb(16, 16, 16),
    selection_bg: rgb(35, 35, 35),
    surface0: rgb(35, 35, 35),
    surface1: rgb(40, 40, 40),
    surface_dim: rgb(16, 16, 16),
    overlay0: rgb(92, 92, 92),
    overlay1: rgb(126, 126, 126),
    text: rgb(255, 255, 255),
    subtext0: rgb(160, 160, 160),
    mauve: rgb(255, 209, 168),
    green: rgb(153, 255, 228),
    yellow: rgb(255, 199, 153),
    red: rgb(255, 128, 128),
    blue: rgb(176, 176, 176),
    teal: rgb(102, 221, 204),
    peach: rgb(255, 199, 153),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_theme_resolves_by_its_own_name() {
        for (name, theme) in THEMES {
            assert_eq!(canonical_name(name), Some(*name));
            assert_eq!(by_name(name), Some(*theme), "{name}");
        }
    }

    #[test]
    fn herdr_aliases_resolve() {
        for (alias, want) in [
            ("catppuccin-mocha", "catppuccin"),
            ("Tokyo Night", "tokyo-night"),
            ("gruvbox_dark", "gruvbox"),
            ("dawn", "rose-pine-dawn"),
            ("light", "catppuccin-latte"),
        ] {
            assert_eq!(canonical_name(alias), Some(want), "{alias}");
        }
        assert_eq!(canonical_name("catppucin"), None);
    }

    #[test]
    fn the_default_is_a_listed_theme() {
        assert!(by_name(DEFAULT_THEME).is_some());
    }

    #[test]
    fn only_terminal_leaves_the_terminal_colours_alone() {
        for (name, theme) in THEMES {
            assert_eq!(theme.is_terminal(), *name == "terminal", "{name}");
        }
    }

    #[test]
    fn terminal_draws_what_the_client_drew_before_themes() {
        let t = TERMINAL;
        assert_eq!(t.selected(), Style::new().add_modifier(Modifier::REVERSED));
        assert_eq!(
            t.chip(Color::Yellow),
            Style::new()
                .fg(Color::Yellow)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        );
        assert_eq!(t.active_tab(), Color::White);
        assert_eq!(t.highlight(), Color::Cyan);
        assert_eq!(t.ink(), Color::Black);
        assert_eq!(t.overlay_block(), Block::default().borders(Borders::ALL));
    }

    #[test]
    fn a_coloured_theme_fills_its_chips_and_selection() {
        let t = CATPPUCCIN;
        assert_eq!(t.chip(t.accent).bg, Some(t.accent));
        assert_eq!(t.chip(t.accent).fg, Some(t.panel_bg));
        assert_eq!(t.selected().bg, Some(t.selection_bg));
        assert!(!t.selected().add_modifier.contains(Modifier::REVERSED));
    }
}
