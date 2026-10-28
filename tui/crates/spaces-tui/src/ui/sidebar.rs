//! The sidebar: a `spaces` list with a `new`/`menu` footer on top, and an
//! `agents` panel with a `grouped`/`priority` toggle below it.
//!
//! # Shape
//!
//! ```text
//!  spaces                        <- header (lowercase, bold)
//! ◐ frontend                     <- one row per space, state icon first…
//!   main ↑2 *                    <- …plus a dim git line when it has one
//! ✓ api
//!  new                    menu   <- footer, pinned to the section's bottom
//! ──────────────────────────────  <- divider
//!  agents               grouped  <- header + the sort toggle, right-aligned
//! ❯◐ frontend                    <- two rows per agent…
//!    claude — fixing the parser   <- …the second one dim
//! ```
//!
//! Deliberately kept to the sidebar layout agent multiplexers have settled
//! on, down to the lowercase headers, the two sections, the footer's two
//! buttons and the toggle's two words (`grouped`/`priority`) — see
//! [`crate::model::AgentSort`]. The point of this client is that someone
//! who already lives in one can drive it without relearning where anything
//! is.
//!
//! The two sections scroll independently ([`crate::model::Model::sidebar_scroll`]
//! and [`crate::model::Model::agents_scroll`]) because their chrome is
//! *pinned*: the footer sits on the spaces section's last row and the agents
//! header on its own, so neither can scroll out of reach. That is the whole
//! reason this module computes a [`SidebarLayout`] up front rather than
//! flowing one list of rows: a `new` button you have to scroll to find is a
//! button nobody finds.
//!
//! # One geometry, two readers
//!
//! [`sidebar_layout`] is the single source of truth for where every part of
//! the sidebar sits, and [`hit_test`] answers "what did the user click"
//! against that same function — never against rectangles cached from the
//! last render. [`space_row_text`] and [`agent_row_text`] are likewise the
//! single place a row's *text* is decided; the renderer and the width tests
//! both go through them, so neither can disagree about what is on screen.
//!
//! # Why glyphs and not just colour
//!
//! The common default for status indicators is five coloured dots
//! (`● ● ● ○ ·`), which carry four of their five meanings in color alone.
//! This client uses distinct glyphs instead — `× ◐ ✓ ○ ·` —
//! because a signal that survives `TERM=dumb`, `tmux capture-pane -p`, a
//! plain-text log or a screen reader has to be in the text. The colors are
//! still applied on top, the conventional five: red, yellow, teal,
//! green, grey. Same reasoning as the focused pane's `❯`, which is part of
//! [`agent_row_text`]'s output rather than something [`render_sidebar`] adds.
//!
//! Live panes are grouped by [`PaneInfo::workspace_id`], **not**
//! `WorkspaceInfo::pane_ids` — the latter is only accurate right after a
//! snapshot and is not incrementally maintained by `Model::apply_event` (see
//! `model.rs`'s module docs), so it can point at panes that no longer exist
//! or omit ones that do. `workspace_id` is kept current by every pane event
//! this client folds, and is the source of truth here. An agent whose
//! `workspace_id` resolves to no known space is listed under
//! [`UNASSIGNED`] rather than dropped: silently hiding a running pane is
//! worse than a row that admits it doesn't know where the pane belongs.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use spaces_proto::WorkspaceInfoGit;

use crate::model::{display_state, AgentSort, DisplayState, Model, NodeId};

/// Default width of the sidebar column, in terminal cells, its draggable
/// edge included. The live width is [`crate::model::SidebarState::width`],
/// which the user can drag between [`MIN_WIDTH`] and [`MAX_WIDTH`].
pub const WIDTH: u16 = 28;

/// Narrowest a dragged sidebar goes — enough for a state icon, a short space
/// name and the agents header's `priority` toggle.
pub const MIN_WIDTH: u16 = 20;

/// Widest a dragged sidebar goes. Past this it stops being a sidebar.
pub const MAX_WIDTH: u16 = 60;

/// Terminals narrower than this hide the sidebar automatically, leaving the
/// whole width to the tab bar and the panes. Overridden by an explicit
/// `ctrl+space b` until the next resize across it — see
/// [`crate::model::SidebarState::note_screen_width`].
pub const COLLAPSE_BELOW: u16 = 80;

/// The width the sidebar column actually takes: `requested` clamped to
/// [`MIN_WIDTH`]..=[`MAX_WIDTH`], then to half of `screen_width` — the pane
/// area is what this client is for, and a sidebar that takes most of the
/// screen leaves an agent nothing to draw in. The half-screen limit wins
/// over the minimum, so on a very narrow terminal the sidebar shrinks below
/// `MIN_WIDTH` rather than squeezing the panes out.
pub fn clamp_width(requested: u16, screen_width: u16) -> u16 {
    requested.clamp(MIN_WIDTH, MAX_WIDTH).min(screen_width / 2)
}

/// The part of the sidebar column the sections draw into: all of it but the
/// last cell, which is [`edge`].
pub fn content_area(column: Rect) -> Rect {
    Rect::new(
        column.x,
        column.y,
        column.width.saturating_sub(1),
        column.height,
    )
}

/// The sidebar column's last cell, top to bottom: a dim `│` that is the
/// handle for dragging the sidebar wider or narrower.
///
/// A column of its own rather than the content's right-most cell, because
/// that cell is already a button on two rows (`menu` and the sort toggle are
/// right-aligned), and a press there has to mean one thing. Zero-sized for a
/// zero-width column, so [`hit_test`]-style containment can never match it.
pub fn edge(column: Rect) -> Rect {
    if column.width == 0 {
        return Rect::new(column.x, column.y, 0, 0);
    }
    Rect::new(
        column.x.saturating_add(column.width - 1),
        column.y,
        1,
        column.height,
    )
}

/// Section headers. Lowercase and leading-space-indented — see this
/// module's docs for why the familiar look is the point.
pub const SPACES_HEADER: &str = " spaces";
pub const AGENTS_HEADER: &str = " agents";

/// Footer buttons. `new` creates a space (the footer belongs to the spaces
/// section); creating a *pane* is the tab bar's `+`, over the panes it would
/// join — see [`super::tabs`].
pub const NEW_LABEL: &str = " new";
pub const MENU_LABEL: &str = "menu";

/// Click width of the two footer buttons, in cells — a little wider than
/// their text so the target isn't pixel-hunting.
const NEW_BUTTON_WIDTH: u16 = 5;
const MENU_BUTTON_WIDTH: u16 = 6;

/// Shown in place of a space name for an agent whose `workspace_id` resolves
/// to no space this client knows about.
pub const UNASSIGNED: &str = "unassigned";

/// Rows each agent entry occupies: the space line and the agent line.
pub const AGENT_ENTRY_ROWS: u16 = 2;

/// The least height at which the agents section is worth showing at all:
/// its divider, its header, and one whole entry.
const AGENTS_MIN_HEIGHT: u16 = 2 + AGENT_ENTRY_ROWS;

/// The least height the spaces section is ever squeezed to before the agents
/// section is dropped entirely: its header, one space row, and its footer.
const SPACES_MIN_HEIGHT: u16 = 3;

/// How tall the spaces section is, given `total` rows to divide and
/// `space_rows` rows of entries to list — a row count, not an entry count,
/// because a space on a git branch takes two (see [`space_entry_rows`]).
///
/// Content-driven rather than a fixed share of the height: with three spaces
/// on a 40-row terminal, a fixed split would strand the `new`/`menu` footer
/// a dozen blank rows below the last space, reading as a rendering bug
/// rather than as a section boundary. The section takes exactly what it
/// needs — header, every space's rows, footer — and everything left over goes
/// to the agents panel, which is the list that actually grows.
///
/// Bounded at both ends. It never shrinks below [`SPACES_MIN_HEIGHT`] (the
/// list is how you reach anything at all), and never grows past whatever
/// leaves the agents panel [`AGENTS_MIN_HEIGHT`]; past that the spaces list
/// scrolls. Below `SPACES_MIN_HEIGHT + AGENTS_MIN_HEIGHT` rows in total, the
/// agents panel is dropped outright and the spaces section takes everything
/// — see [`sidebar_layout`].
///
/// `requested` is the user's dragged split
/// ([`crate::model::SidebarState::spaces_height`]); when present it replaces
/// the content-driven height, through the same bounds — see
/// [`clamp_spaces_height`].
fn spaces_height(total: u16, space_rows: usize, requested: Option<u16>) -> u16 {
    let needed = requested.unwrap_or_else(|| {
        u16::try_from(space_rows)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
    });
    clamp_spaces_height(needed, total)
}

/// Bounds a spaces-section height to what a `total`-row sidebar can give it:
/// at least [`SPACES_MIN_HEIGHT`], and never so much that the agents panel
/// loses its [`AGENTS_MIN_HEIGHT`]. Below the two minimums together there is
/// only one section, and it takes everything.
///
/// Public so the divider drag can store an already-sensible value, rather
/// than a raw pointer row that would only make sense on the terminal height
/// it was dragged at.
pub fn clamp_spaces_height(requested: u16, total: u16) -> u16 {
    if total < SPACES_MIN_HEIGHT.saturating_add(AGENTS_MIN_HEIGHT) {
        return total;
    }
    requested
        .max(SPACES_MIN_HEIGHT)
        .min(total.saturating_sub(AGENTS_MIN_HEIGHT))
}

/// Where every part of the sidebar sits for one frame. Every rect is either
/// a real region inside `area` or zero-sized; nothing here ever escapes
/// `area`, however small the terminal is.
///
/// Computed by [`sidebar_layout`] and consumed by both [`render_sidebar`]
/// and [`hit_test`] — that shared derivation is what keeps a click and the
/// frame it landed on from disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SidebarLayout {
    pub spaces_header: Rect,
    /// The scrolling list of spaces — one row each, two for a space with a
    /// git line (see [`space_entry_rows`]).
    pub spaces_body: Rect,
    /// The `new` button, at the footer's left edge.
    pub new_button: Rect,
    /// The `menu` button, at the footer's right edge.
    pub menu_button: Rect,
    /// The horizontal rule between the two sections — also the handle for
    /// dragging the split between them.
    pub divider: Rect,
    pub agents_header: Rect,
    /// The `grouped`/`priority` label, right-aligned on the agents header
    /// row. Sized to whichever word is currently shown, so the click target
    /// is exactly the text.
    pub sort_toggle: Rect,
    /// The scrolling list of agents, [`AGENT_ENTRY_ROWS`] rows each.
    pub agents_body: Rect,
}

/// Divides `area` into the regions above. Saturating throughout and never
/// panics: `area` comes from the terminal's own reported size, which a user
/// can make arbitrarily small.
///
/// Degrades by dropping whole parts rather than by shrinking them into
/// uselessness — the agents section goes first (below
/// `SPACES_MIN_HEIGHT + AGENTS_MIN_HEIGHT` rows), then the footer, then the
/// space list itself, leaving just the header. A zero-sized rect is how a
/// part reports "not on screen this frame", and [`hit_test`] can never match
/// one.
///
/// Takes the whole `model`, not just a size, for two reasons: the spaces
/// section's height follows how many spaces there are (see
/// [`spaces_height`]), and the sort toggle's width follows which of
/// `grouped`/`priority` is showing — the two labels differ in length, and a
/// click target sized for the other mode would be off by two cells.
pub fn sidebar_layout(area: Rect, model: &Model) -> SidebarLayout {
    let mut layout = SidebarLayout::default();
    if area.width == 0 || area.height == 0 {
        return layout;
    }

    let space_rows: usize = sidebar_rows(model)
        .iter()
        .map(|row| usize::from(space_entry_rows(row)))
        .sum();
    let spaces_h = spaces_height(area.height, space_rows, model.sidebar.spaces_height);
    let agents_h = area.height.saturating_sub(spaces_h);
    let sort: AgentSort = model.agent_sort;
    let row = |y: u16| Rect::new(area.x, y, area.width, 1);

    // --- spaces section ---
    layout.spaces_header = row(area.y);
    if spaces_h >= 2 {
        let footer_y = area.y.saturating_add(spaces_h).saturating_sub(1);
        let new_w = NEW_BUTTON_WIDTH.min(area.width);
        layout.new_button = Rect::new(area.x, footer_y, new_w, 1);
        let menu_w = MENU_BUTTON_WIDTH.min(area.width);
        layout.menu_button = Rect::new(
            area.x.saturating_add(area.width.saturating_sub(menu_w)),
            footer_y,
            menu_w,
            1,
        );
        layout.spaces_body = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            spaces_h.saturating_sub(2),
        );
    }

    // --- agents section ---
    if agents_h == 0 {
        return layout;
    }
    let agents_y = area.y.saturating_add(spaces_h);
    layout.divider = row(agents_y);
    let header_y = agents_y.saturating_add(1);
    layout.agents_header = row(header_y);
    let toggle_w = (sort.label().chars().count() as u16).min(area.width);
    layout.sort_toggle = Rect::new(
        area.x.saturating_add(area.width.saturating_sub(toggle_w)),
        header_y,
        toggle_w,
        1,
    );
    layout.agents_body = Rect::new(
        area.x,
        agents_y.saturating_add(2),
        area.width,
        agents_h.saturating_sub(2),
    );
    layout
}

/// One drawable row of the sidebar's upper list.
///
/// A single-variant enum today. It stays an enum because the renderer and the
/// hit-tester both match on it, and a row type that can only be one thing is
/// what a spaces list is *now* rather than what it must always be.
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarRow {
    Space(SpaceRow),
}

/// One space in the sidebar's upper list.
///
/// `PartialEq` only: [`WorkspaceInfoGit`] is generated from the protocol
/// schema without `Eq`, and nothing here needs more than `==`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceRow {
    pub id: String,
    /// The node this space lives on.
    pub node: NodeId,
    pub name: String,
    /// The space's rollup across its live panes — see [`Model::rollup`].
    pub state: DisplayState,
    /// Parsed `WorkspaceInfo::color`; see [`accent`].
    pub accent: Option<Color>,
    /// True for the space the tab bar is currently showing.
    pub active: bool,
    /// The owning node's name, drawn dimmed on the row's right edge. `None`
    /// in by-node view (the header above already says it) and whenever there
    /// is only one node — a lone tag on every row would spend 6 of the
    /// sidebar's 28 cells saying something the user already knows.
    pub node_tag: Option<String>,
    /// The space's git status, as folded from snapshots and
    /// `workspace.updated`. `Some` gives the entry its second, dim line —
    /// see [`space_git_text`]; `None` (not a repository, or the runtime has
    /// not looked yet) keeps it to one row.
    pub git: Option<WorkspaceInfoGit>,
}

/// One agent in the sidebar's lower panel. `focused`/`split` mirror
/// [`Model::focused`]/[`Model::split`] and are mutually exclusive, so one
/// prefix column carries both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub pane_id: String,
    /// The space's name, or [`UNASSIGNED`].
    pub space: String,
    pub agent_type: String,
    /// The pane's own title; empty when it has none.
    pub title: String,
    pub state: DisplayState,
    pub focused: bool,
    pub split: bool,
    /// The pane's own accent, parsed from `PaneInfo::color`. Colors its name
    /// on the entry's second line, the way a space's accent colors its name —
    /// never the state icon, which has to stay the loudest thing on the row.
    pub accent: Option<Color>,
}

/// The spaces list, in `model.local_workspaces()` order.
pub fn sidebar_rows(model: &Model) -> Vec<SidebarRow> {
    let active = model.active_space_id().map(str::to_string);
    let node = model.local_id_owned();
    model
        .local_workspaces()
        .iter()
        .map(|ws| {
            SidebarRow::Space(SpaceRow {
                id: ws.id.clone(),
                node: node.clone(),
                name: ws.name.clone(),
                state: model.local().rollup(&ws.id),
                accent: accent(ws.color.as_deref()),
                active: active.as_deref() == Some(ws.id.as_str()),
                node_tag: None,
                git: ws.git.clone(),
            })
        })
        .collect()
}

/// The agents panel's entries, ordered by `model.agent_sort` — see
/// [`Model::agents_in_order`] for the two orderings and why the
/// priority one is a *stable* sort.
pub fn agent_rows(model: &Model) -> Vec<AgentRow> {
    model
        .agents_in_order(model.agent_sort)
        .into_iter()
        .map(|pane| AgentRow {
            pane_id: pane.id.clone(),
            space: model.space_name_of(pane).unwrap_or(UNASSIGNED).to_string(),
            agent_type: pane.agent_type.clone(),
            title: pane.title.clone(),
            state: display_state(pane),
            focused: model.focused.as_deref() == Some(pane.id.as_str()),
            split: model.split.as_deref() == Some(pane.id.as_str()),
            accent: accent(pane.color.as_deref()),
        })
        .collect()
}

/// One glyph per [`DisplayState`] — a symbol set chosen over coloured dots
/// because these five are distinguishable without color. See this module's
/// docs.
pub fn state_icon(state: DisplayState) -> char {
    match state {
        DisplayState::Blocked => '\u{d7}',   // ×
        DisplayState::Working => '\u{25d0}', // ◐
        DisplayState::Done => '\u{2713}',    // ✓
        DisplayState::Idle => '\u{25cb}',    // ○
        DisplayState::Unknown => '\u{b7}',   // ·
    }
}

/// The conventional color for each state, from the current theme. In the
/// `terminal` theme these are indexed ANSI colours, which follow whatever
/// palette the user's terminal is themed with.
pub(crate) fn state_color(state: DisplayState) -> Color {
    let t = super::theme::current();
    match state {
        DisplayState::Blocked => t.red,
        DisplayState::Working => t.yellow,
        DisplayState::Done => t.teal,
        DisplayState::Idle => t.green,
        DisplayState::Unknown => t.overlay0,
    }
}

/// The focus prefix column: `❯` for the pane taking keystrokes, `┃` for the
/// pane visible in the split's other half, a space for everything else.
///
/// `focused` and `split` cannot both be true, so one column carries both and
/// every row's icon still lands in the same place. Four distinct glyphs in
/// this client: `▸` is the palette's selection marker, `❯` focus, `┃` split,
/// and `× ◐ ✓ ○ ·` the agent states.
fn focus_glyph(focused: bool, split: bool) -> char {
    if focused {
        '\u{276f}'
    } else if split {
        '\u{2503}'
    } else {
        ' '
    }
}

/// The accent colors a space can be given, in the order the web UI's own
/// picker offers them.
///
/// Copied from `src/components/common/color-picker.tsx`'s `PRESET_COLORS`
/// rather than invented here, and deliberately so: `WorkspaceInfo::color` is
/// one field shared by two clients, and a space coloured in this one should
/// be indistinguishable from a space coloured in the other — including when
/// the user later opens that picker and finds their space's own colour
/// already highlighted in the grid. A palette of this client's own devising
/// would make TUI-set colors permanently "other".
///
/// `#rrggbb`, which is one of the two forms [`accent`] parses, so anything
/// sent from here also renders here. Lives beside `accent` because these are
/// the two halves of one fact: what colors exist, and how to read one.
pub const COLORS: [&str; 16] = [
    "#ef4444", "#f97316", "#f59e0b", "#eab308", "#84cc16", "#22c55e", "#14b8a6", "#06b6d4",
    "#3b82f6", "#6366f1", "#8b5cf6", "#a855f7", "#d946ef", "#ec4899", "#f43f5e", "#78716c",
];

/// Parses a space's accent color — `#rrggbb`, or the 3-digit `#rgb` short
/// form — into a [`Color::Rgb`]. `color` is `WorkspaceInfo::color`, which
/// ultimately comes from the server (the web UI's own color picker, folded
/// in by `workspace.created`/`workspace.updated` — see `model.rs`), so this
/// is reachable from network input and must never panic on any input:
/// `None`, an empty string, the wrong length, non-hex digits, or whitespace
/// all just return `None` rather than trying to tolerate them. Whitespace is
/// deliberately not trimmed — a color with spaces around it is malformed
/// data, not a formatting variation worth accepting.
///
/// Hex digits are read char-by-char (never by byte-slicing a validated
/// range) so a non-ASCII string — entirely plausible for arbitrary
/// server-supplied data — can never land an index off a UTF-8 char boundary
/// and panic; it simply fails to parse as a hex digit and returns `None`
/// like any other junk.
pub fn accent(color: Option<&str>) -> Option<Color> {
    let hex: Vec<char> = color?.strip_prefix('#')?.chars().collect();
    let digit = |c: char| c.to_digit(16).map(|d| d as u8);
    match hex.len() {
        6 => Some(Color::Rgb(
            digit(hex[0])? * 16 + digit(hex[1])?,
            digit(hex[2])? * 16 + digit(hex[3])?,
            digit(hex[4])? * 16 + digit(hex[5])?,
        )),
        // `#rgb` shorthand: each digit stands for itself doubled (the same
        // rule CSS uses), so `#f80` means the same color as `#ff8800`.
        3 => Some(Color::Rgb(
            digit(hex[0])? * 0x11,
            digit(hex[1])? * 0x11,
            digit(hex[2])? * 0x11,
        )),
        _ => None,
    }
}

/// The exact text drawn for one space row: `"{icon} {name}"`, truncated to
/// `width`. The single place that decides what a space row says.
pub fn space_row_text(row: &SpaceRow, width: u16) -> String {
    truncate(&format!("{} {}", state_icon(row.state), row.name), width)
}

/// The second, dim line of a space on a git branch, truncated to `width`, or
/// `None` for a space with no git status — the single place that decides what
/// that line says.
///
/// ```text
/// ◐ api
///   main ↑2 ↓1 *
/// ```
///
/// Indented two cells so the branch sits under the space's *name* rather
/// than its state icon. The branch comes first (or `detached` when the
/// repository has no branch checked out — a word, because an empty line would
/// read as "not a repository"), then only the tokens that are true: `↑N`
/// commits ahead of upstream, `↓N` behind, `*` for uncommitted changes. A
/// clean, in-sync branch is just its name; the absence of a token is the
/// signal, the same way an idle agent has no `×`.
pub fn space_git_text(row: &SpaceRow, width: u16) -> Option<String> {
    let git = row.git.as_ref()?;
    let mut text = format!("  {}", git.branch.as_deref().unwrap_or("detached"));
    if git.ahead > 0 {
        text.push_str(&format!(" \u{2191}{}", git.ahead));
    }
    if git.behind > 0 {
        text.push_str(&format!(" \u{2193}{}", git.behind));
    }
    if git.dirty {
        text.push_str(" *");
    }
    Some(truncate(&text, width))
}

/// Rows one space entry occupies: two when it has a git line, else one.
///
/// Every reader of the spaces list — the section's height, the scroll clamp,
/// the renderer and the hit-tester — goes through this, so an entry can never
/// be drawn two rows tall and clicked as one.
pub fn space_entry_rows(row: &SidebarRow) -> u16 {
    match row {
        SidebarRow::Space(space) if space.git.is_some() => 2,
        SidebarRow::Space(_) => 1,
    }
}

/// Each spaces-list entry's height, in list order.
pub fn space_heights(rows: &[SidebarRow]) -> Vec<u16> {
    rows.iter().map(space_entry_rows).collect()
}

/// The exact two lines drawn for one agent entry, truncated to `width`:
///
/// ```text
/// ❯◐ frontend
///    claude — fixing the parser
/// ```
///
/// Line 0 identifies *where* the agent is (focus glyph, state icon, space);
/// line 1 identifies *what* it is (agent type, and its title when it has
/// one). Both signals a reader needs are in the text, not in the styling —
/// see this module's docs.
pub fn agent_row_text(row: &AgentRow, width: u16) -> [String; 2] {
    let first = format!(
        "{}{} {}",
        focus_glyph(row.focused, row.split),
        state_icon(row.state),
        row.space
    );
    let second = if row.title.is_empty() {
        format!("   {}", row.agent_type)
    } else {
        format!("   {} \u{2014} {}", row.agent_type, row.title)
    };
    [truncate(&first, width), truncate(&second, width)]
}

/// Truncates `text` to at most `width` **characters** (not bytes — slicing a
/// `String` on a byte index that isn't a char boundary panics, and a pane
/// title or space name can contain multibyte UTF-8), appending `…` in place
/// of whatever was cut. A no-op if `text` already fits.
fn truncate(text: &str, width: u16) -> String {
    let width = usize::from(width);
    if text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    if width > 0 {
        out.push('\u{2026}');
    }
    out
}

/// Clamps a scroll offset measured in *entries* so it can never point past
/// the last full page of `entries` entries, each `rows_per_entry` rows tall,
/// in a viewport `height` cells tall. Saturating throughout and never panics:
/// every input is runtime-derived (the entry count changes as panes come and
/// go; `height` comes from the terminal's own current size).
///
/// Called on **every** render as well as on every click, from the one shared
/// implementation — a scroll offset that was valid a moment ago can be
/// stranded past the end of a list that just got shorter (a pane closing, a
/// space removed), and a click whose row maths used a different offset than
/// the frame it landed on targets the wrong entry.
pub fn clamp_scroll(scroll: usize, entries: usize, height: u16, rows_per_entry: u16) -> usize {
    let per_page = usize::from(height) / usize::from(rows_per_entry.max(1));
    scroll.min(entries.saturating_sub(per_page))
}

/// [`clamp_scroll`] for a list whose entries are not all the same height —
/// the spaces list, where a space on a git branch takes two rows.
///
/// The offset is still in *entries*. It may go no further than the earliest
/// entry from which the rest of the list fits in `height` rows, which is
/// precisely the "last full page" the fixed-height clamp computes — the tests
/// pin the two to agree whenever the heights are uniform. If even the final
/// entry alone is taller than the viewport, the final entry is as far as it
/// goes, so the list never scrolls to blank. Saturating, never panics.
pub fn clamp_scroll_rows(scroll: usize, heights: &[u16], height: u16) -> usize {
    let mut fits = 0u32;
    let mut first = heights.len();
    for (index, rows) in heights.iter().enumerate().rev() {
        fits = fits.saturating_add(u32::from(*rows));
        if fits > u32::from(height) {
            break;
        }
        first = index;
    }
    scroll.min(first.min(heights.len().saturating_sub(1)))
}

/// The entries a variable-height list shows in a `height`-row viewport,
/// starting from entry `scroll`: `(index, row offset, rows shown)` each. The
/// last one may be cut short by the bottom edge, in which case only its
/// first `rows shown` rows are drawn — and only those are clickable, because
/// [`hit_test`] walks this same list.
fn visible_entries(heights: &[u16], scroll: usize, height: u16) -> Vec<(usize, u16, u16)> {
    let mut out = Vec::new();
    let mut y = 0u16;
    for (index, rows) in heights.iter().enumerate().skip(scroll) {
        if y >= height {
            break;
        }
        out.push((index, y, (*rows).min(height - y)));
        y = y.saturating_add(*rows);
    }
    out
}

/// What a click inside the sidebar landed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarHit {
    /// A space row — carries the machine it lives on and that space's id.
    ///
    /// The machine travels with the id because a space id is only unique
    /// *within* a machine: two can each have a `w1`, and re-deriving the owner
    /// from the id alone silently picks the wrong one.
    Space(NodeId, String),
    /// An agent entry — carries that pane's id.
    Agent(String),
    /// The footer's `new` button.
    New,
    /// The footer's `menu` button.
    Menu,
    /// The agents panel's `grouped`/`priority` label.
    SortToggle,
    /// The rule between the two sections, which a press starts dragging.
    Divider,
    /// Inside the spaces list, but not on a row — the wheel still scrolls
    /// here, which is why this is distinct from [`SidebarHit::None`].
    SpacesBody,
    /// Inside the agents list, but not on an entry.
    AgentsBody,
    /// Somewhere inert (a header, or outside the sidebar).
    None,
}

/// Resolves `(column, row)` against the sidebar `model` would draw into
/// `area` right now. Derived from [`sidebar_layout`] and the two `_rows`
/// builders — the same functions [`render_sidebar`] calls — never from
/// rectangles cached by a previous frame, which a resize between render and
/// click would silently invalidate.
///
/// `spaces_scroll`/`agents_scroll` are the callers' already-clamped offsets;
/// both are re-clamped here anyway, against this call's own entry counts, so
/// an offset stranded by a pane that closed since the last render can only
/// mis-target within the list rather than index past it. Every lookup is
/// `.get`, never `[]`.
pub fn hit_test(
    area: Rect,
    model: &Model,
    spaces_scroll: usize,
    agents_scroll: usize,
    column: u16,
    row: u16,
) -> SidebarHit {
    let layout = sidebar_layout(area, model);
    // Buttons before bodies: `sort_toggle` overlaps `agents_header`, and the
    // footer's two buttons sit on a row of their own that is not part of
    // either body, so the order here is about precedence within a row, not
    // between regions.
    if contains(layout.sort_toggle, column, row) {
        return SidebarHit::SortToggle;
    }
    if contains(layout.new_button, column, row) {
        return SidebarHit::New;
    }
    if contains(layout.menu_button, column, row) {
        return SidebarHit::Menu;
    }
    if contains(layout.divider, column, row) {
        return SidebarHit::Divider;
    }
    if contains(layout.spaces_body, column, row) {
        let rows = sidebar_rows(model);
        let heights = space_heights(&rows);
        let body = layout.spaces_body;
        let scroll = clamp_scroll_rows(spaces_scroll, &heights, body.height);
        let offset = row.saturating_sub(body.y);
        let index = visible_entries(&heights, scroll, body.height)
            .into_iter()
            .find(|(_, y, shown)| offset >= *y && offset < y.saturating_add(*shown))
            .map(|(index, _, _)| index);
        return match index.and_then(|i| rows.get(i)) {
            Some(SidebarRow::Space(space)) => {
                SidebarHit::Space(space.node.clone(), space.id.clone())
            }
            None => SidebarHit::SpacesBody,
        };
    }
    if contains(layout.agents_body, column, row) {
        let rows = agent_rows(model);
        let scroll = clamp_scroll(
            agents_scroll,
            rows.len(),
            layout.agents_body.height,
            AGENT_ENTRY_ROWS,
        );
        let offset = usize::from(row.saturating_sub(layout.agents_body.y));
        let index = offset / usize::from(AGENT_ENTRY_ROWS) + scroll;
        return match rows.get(index) {
            Some(agent) => SidebarHit::Agent(agent.pane_id.clone()),
            None => SidebarHit::AgentsBody,
        };
    }
    SidebarHit::None
}

/// `true` iff `(column, row)` falls inside `rect`, with saturating
/// arithmetic so a rect near `u16::MAX` can't overflow computing its far
/// edge. A zero-sized rect contains nothing, which is what lets
/// [`sidebar_layout`] report "not on screen" by handing one back.
fn contains(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Draws the whole sidebar into `area`.
///
/// `model.sidebar_scroll`/`model.agents_scroll` are re-clamped here via
/// [`clamp_scroll`] against this render's own counts and heights before
/// being used — never trusted as-is — so a stale offset from before some
/// panes closed can never slice past the end of a list (which would panic)
/// or leave a section blank. The clamped values are not written back
/// (`render_sidebar` takes `&Model`); the mouse path clamps identically with
/// the same function.
pub fn render_sidebar(model: &Model, area: Rect, buf: &mut Buffer) {
    let layout = sidebar_layout(area, model);
    let t = super::theme::current();
    if t.sidebar_bg != Color::Reset {
        buf.set_style(area.intersection(buf.area), Style::new().bg(t.sidebar_bg));
    }
    let dim = Style::default().fg(t.overlay0);
    let header = dim.add_modifier(Modifier::BOLD);

    draw(
        buf,
        layout.spaces_header,
        Line::styled(SPACES_HEADER, header),
    );
    render_spaces(model, &layout, buf);

    if layout.new_button.width > 0 {
        draw(buf, layout.new_button, Line::styled(NEW_LABEL, dim));
    }
    if layout.menu_button.width > 0 {
        draw_right(buf, layout.menu_button, Line::styled(MENU_LABEL, dim));
    }

    if layout.divider.width > 0 {
        let rule = "\u{2500}".repeat(usize::from(layout.divider.width));
        draw(buf, layout.divider, Line::styled(rule, dim));
    }
    if layout.agents_header.width > 0 {
        draw(
            buf,
            layout.agents_header,
            Line::styled(AGENTS_HEADER, header),
        );
        draw_right(
            buf,
            layout.sort_toggle,
            Line::styled(model.agent_sort.label(), header),
        );
        render_agents(model, &layout, buf);
    }
}

/// The spaces list. A space's accent colors its *name* only — never its
/// state icon, which has to stay the loudest thing on the row whatever
/// color the user picked for the space.
fn render_spaces(model: &Model, layout: &SidebarLayout, buf: &mut Buffer) {
    let body = layout.spaces_body;
    if body.width == 0 || body.height == 0 {
        return;
    }
    let t = super::theme::current();
    let rows = sidebar_rows(model);
    let heights = space_heights(&rows);
    let scroll = clamp_scroll_rows(model.sidebar_scroll, &heights, body.height);
    for (index, offset, shown) in visible_entries(&heights, scroll, body.height) {
        let Some(row) = rows.get(index) else {
            continue;
        };
        let y = body.y.saturating_add(offset);
        let rect = Rect::new(body.x, y, body.width, 1);
        match row {
            SidebarRow::Space(space) => {
                let text = space_row_text(space, body.width);
                // Split off the icon and its trailing space so only the name
                // can carry the accent.
                let icon: String = text.chars().take(2).collect();
                let name: String = text.chars().skip(2).collect();
                let mut name_style = match space.accent {
                    Some(color) => Style::default().fg(color),
                    None => Style::default().fg(t.text),
                };
                if space.active {
                    name_style = name_style.add_modifier(Modifier::BOLD);
                    // The active space's whole entry sits on the row
                    // background, as herdr marks its active workspace.
                    if t.active_row_bg != Color::Reset {
                        let rows = shown.min(2);
                        buf.set_style(
                            Rect::new(body.x, y, body.width, rows).intersection(buf.area),
                            Style::new().bg(t.active_row_bg),
                        );
                    }
                }
                let line = Line::from(vec![
                    Span::styled(icon, Style::default().fg(state_color(space.state))),
                    Span::styled(name, name_style),
                ]);
                draw(buf, rect, line);
                // The git line only when the viewport has room for it — the
                // entry's first row may be the last one on screen.
                if shown >= 2 {
                    if let Some(git) = space_git_text(space, body.width) {
                        draw(
                            buf,
                            Rect::new(body.x, y.saturating_add(1), body.width, 1),
                            Line::styled(git, Style::default().fg(t.mauve)),
                        );
                    }
                }
            }
        }
    }
}

/// The agents panel. Two rows per entry; the focused agent's first line is
/// bold, additional to (not instead of) the `❯` already in its text.
fn render_agents(model: &Model, layout: &SidebarLayout, buf: &mut Buffer) {
    let body = layout.agents_body;
    if body.width == 0 || body.height == 0 {
        return;
    }
    let t = super::theme::current();
    let rows = agent_rows(model);
    let scroll = clamp_scroll(
        model.agents_scroll,
        rows.len(),
        body.height,
        AGENT_ENTRY_ROWS,
    );
    let fits = usize::from(body.height / AGENT_ENTRY_ROWS);
    for (offset, agent) in rows.iter().skip(scroll).take(fits).enumerate() {
        let [first, second] = agent_row_text(agent, body.width);
        let y = body
            .y
            .saturating_add((offset as u16).saturating_mul(AGENT_ENTRY_ROWS));
        let mut head = Style::default().fg(state_color(agent.state));
        if agent.focused {
            head = head.add_modifier(Modifier::BOLD);
            if t.active_row_bg != Color::Reset {
                buf.set_style(
                    Rect::new(body.x, y, body.width, AGENT_ENTRY_ROWS).intersection(buf.area),
                    Style::new().bg(t.active_row_bg),
                );
            }
        }
        draw(
            buf,
            Rect::new(body.x, y, body.width, 1),
            Line::styled(first, head),
        );
        // The agent line carries the pane's own accent when it has one, and
        // falls back to dim. The *first* line keeps the state color, which
        // must not be overridden — a red `×` is the loudest thing in the
        // sidebar for a reason.
        draw(
            buf,
            Rect::new(body.x, y.saturating_add(1), body.width, 1),
            Line::styled(
                second,
                Style::default().fg(agent.accent.unwrap_or(t.overlay0)),
            ),
        );
    }
}

/// Renders one line into one row, or nothing at all for a zero-sized rect.
fn draw(buf: &mut Buffer, area: Rect, line: Line<'static>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    Paragraph::new(line).render(area, buf);
}

/// The same, right-aligned.
fn draw_right(buf: &mut Buffer, area: Rect, line: Line<'static>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    Paragraph::new(line).right_aligned().render(area, buf);
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use spaces_proto::{AgentState, PaneInfo, WorkspaceInfo, WorkspaceInfoGit};

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: 20,
    };

    // --- the fleet views (federation phase 1) ---

    fn ws(id: &str, name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            git: None,
            id: id.into(),
            name: name.into(),
            color: None,
            pane_ids: vec![],
        }
    }

    fn pane(id: &str, space: &str, agent: &str, title: &str, state: AgentState) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(space.into()),
            agent_type: agent.into(),
            agent_state: state,
            seen: true,
            title: title.into(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        }
    }

    /// [`one_of_each`] in `sort` mode — for the geometry cases, where the
    /// only thing that varies is the toggle's own label width.
    fn sorted(sort: AgentSort) -> Model {
        let mut m = one_of_each();
        m.agent_sort = sort;
        m
    }

    /// One space, one working `claude` pane in it, focused.
    fn one_of_each() -> Model {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "frontend")];
        m.local_mut().panes = vec![pane("p1", "w1", "claude", "", AgentState::Working)];
        m.focused = Some("p1".into());
        m
    }

    fn rendered(model: &Model, area: Rect) -> String {
        let mut buf = Buffer::empty(area);
        render_sidebar(model, area, &mut buf);
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // --- The four things the sidebar has to show at all times. ---

    #[test]
    fn the_sidebar_has_both_sections_and_both_footer_buttons() {
        let out = rendered(&one_of_each(), AREA);
        assert!(out.contains("spaces"), "{out}");
        assert!(out.contains("agents"), "{out}");
        assert!(out.contains("new"), "{out}");
        assert!(out.contains("menu"), "{out}");
        assert!(out.contains("grouped"), "the sort toggle is drawn: {out}");
    }

    #[test]
    fn the_section_headers_are_lowercase() {
        // Not styling: lowercase headers are part of the familiar look,
        // and an all-caps banner is the single most obvious way to lose
        // it.
        let out = rendered(&one_of_each(), AREA);
        assert!(out.contains(" spaces"), "{out}");
        assert!(!out.contains("SPACES"), "{out}");
        assert!(out.contains(" agents"), "{out}");
        assert!(!out.contains("AGENTS"), "{out}");
    }

    #[test]
    fn a_space_and_its_agent_are_both_listed() {
        let out = rendered(&one_of_each(), AREA);
        assert_eq!(
            out.matches("frontend").count(),
            2,
            "once as a space, once beside its agent: {out}"
        );
        assert!(out.contains("claude"), "{out}");
    }

    #[test]
    fn an_empty_client_still_draws_its_chrome() {
        // Nothing to list is exactly when the user needs to see `new`.
        let out = rendered(&Model::default(), AREA);
        assert!(out.contains("spaces"), "{out}");
        assert!(out.contains("new"), "{out}");
        assert!(out.contains("agents"), "{out}");
    }

    // --- Layout geometry. ---

    #[test]
    fn the_footer_is_pinned_to_the_bottom_of_the_spaces_section() {
        // The whole reason this module computes a layout instead of flowing
        // rows: a `new` button that scrolls away is a button nobody finds.
        let layout = sidebar_layout(AREA, &one_of_each());
        assert_eq!(
            layout.new_button.y,
            layout.spaces_body.y + layout.spaces_body.height,
            "immediately below the last space row"
        );
        assert_eq!(layout.menu_button.y, layout.new_button.y, "same row");
        assert_eq!(
            layout.divider.y,
            layout.new_button.y + 1,
            "and the agents section starts right under it"
        );
    }

    #[test]
    fn the_two_footer_buttons_sit_at_opposite_ends_and_do_not_overlap() {
        let layout = sidebar_layout(AREA, &one_of_each());
        assert_eq!(layout.new_button.x, AREA.x);
        assert_eq!(
            layout.menu_button.x + layout.menu_button.width,
            AREA.x + AREA.width
        );
        assert!(
            layout.new_button.x + layout.new_button.width <= layout.menu_button.x,
            "{layout:?}"
        );
    }

    #[test]
    fn the_sort_toggle_is_right_aligned_on_the_agents_header_row() {
        for sort in [AgentSort::Grouped, AgentSort::Priority] {
            let layout = sidebar_layout(AREA, &sorted(sort));
            assert_eq!(layout.sort_toggle.y, layout.agents_header.y);
            assert_eq!(
                layout.sort_toggle.x + layout.sort_toggle.width,
                AREA.x + AREA.width
            );
            assert_eq!(
                usize::from(layout.sort_toggle.width),
                sort.label().len(),
                "the target is exactly the word, so the two modes' targets differ"
            );
        }
    }

    #[test]
    fn a_short_sidebar_drops_the_agents_section_rather_than_squashing_it() {
        // Both sections need real room; below that the spaces list — which
        // is how you reach anything at all — wins outright.
        let short = Rect::new(0, 0, WIDTH, SPACES_MIN_HEIGHT + AGENTS_MIN_HEIGHT - 1);
        let layout = sidebar_layout(short, &one_of_each());
        assert_eq!(layout.agents_body, Rect::default());
        assert_eq!(layout.divider, Rect::default());
        assert!(layout.spaces_body.height > 0, "{layout:?}");
        assert!(
            layout.new_button.width > 0,
            "the footer survives: {layout:?}"
        );
    }

    #[test]
    fn every_region_stays_inside_the_area_at_every_size() {
        for height in 0..40u16 {
            for width in [0u16, 1, 4, WIDTH, 80] {
                let area = Rect::new(3, 7, width, height);
                let layout = sidebar_layout(area, &sorted(AgentSort::Priority));
                for rect in [
                    layout.spaces_header,
                    layout.spaces_body,
                    layout.new_button,
                    layout.menu_button,
                    layout.divider,
                    layout.agents_header,
                    layout.sort_toggle,
                    layout.agents_body,
                ] {
                    if rect.width == 0 || rect.height == 0 {
                        continue;
                    }
                    assert!(rect.x >= area.x, "{rect:?} escapes {area:?}");
                    assert!(rect.x + rect.width <= area.x + area.width, "{rect:?}");
                    assert!(rect.y >= area.y, "{rect:?}");
                    assert!(rect.y + rect.height <= area.y + area.height, "{rect:?}");
                }
            }
        }
    }

    #[test]
    fn rendering_any_size_does_not_panic() {
        let m = one_of_each();
        for (w, h) in [(0, 0), (1, 1), (4, 2), (WIDTH, 3), (WIDTH, 7), (80, 40)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(Rect::new(0, 0, w.max(1), h.max(1)));
            render_sidebar(&m, area, &mut buf);
        }
    }

    // --- Status indicators. ---

    #[test]
    fn each_state_has_its_own_glyph_not_just_its_own_colour() {
        // Five dots separated by colour alone is the common default; this
        // client uses distinct glyphs so the signal survives a terminal
        // with styling stripped.
        let icons: Vec<char> = [
            DisplayState::Blocked,
            DisplayState::Working,
            DisplayState::Done,
            DisplayState::Idle,
            DisplayState::Unknown,
        ]
        .into_iter()
        .map(state_icon)
        .collect();
        let unique: std::collections::HashSet<char> = icons.iter().copied().collect();
        assert_eq!(unique.len(), icons.len(), "{icons:?}");
        // And none of them collides with the three navigation glyphs.
        for nav in ['\u{25b8}', '\u{276f}', '\u{2503}'] {
            assert!(!icons.contains(&nav), "{nav:?} in {icons:?}");
        }
    }

    #[test]
    fn a_blocked_agent_shows_the_blocked_glyph_in_both_sections() {
        let mut m = one_of_each();
        m.local_mut().panes[0].agent_state = AgentState::Blocked;
        let out = rendered(&m, AREA);
        assert_eq!(
            out.matches(state_icon(DisplayState::Blocked)).count(),
            2,
            "the space rollup and the agent itself: {out}"
        );
    }

    #[test]
    fn an_unseen_completion_reads_as_done_not_idle() {
        let mut m = one_of_each();
        m.local_mut().panes[0].agent_state = AgentState::Idle;
        m.local_mut().panes[0].seen = false;
        let out = rendered(&m, AREA);
        assert!(out.contains(state_icon(DisplayState::Done)), "{out}");
        assert!(!out.contains(state_icon(DisplayState::Idle)), "{out}");
    }

    #[test]
    fn the_state_icon_is_never_recolored_by_a_space_accent() {
        let mut m = one_of_each();
        m.local_mut().panes[0].agent_state = AgentState::Blocked;
        m.local_mut().workspaces[0].color = Some("#00ccff".into());
        let mut buf = Buffer::empty(AREA);
        render_sidebar(&m, AREA, &mut buf);
        // Row 1 is the first space row: icon at column 0, name from column 2.
        assert_eq!(buf[(0, 1)].symbol(), "\u{d7}");
        assert_eq!(
            buf[(0, 1)].fg,
            Color::Red,
            "an accent must never camouflage the blocked icon"
        );
        assert_eq!(buf[(2, 1)].fg, Color::Rgb(0, 0xcc, 0xff));
    }

    #[test]
    fn accent_parses_both_hex_forms_and_rejects_junk_without_panicking() {
        assert_eq!(accent(Some("#ff8800")), Some(Color::Rgb(0xff, 0x88, 0x00)));
        assert_eq!(accent(Some("#f80")), Some(Color::Rgb(0xff, 0x88, 0x00)));
        for junk in [
            "",
            "#",
            "#12",
            "#12345",
            "#gggggg",
            "red",
            "#ff88000000",
            "  #ff8800  ",
            "#ff88\u{ff}\u{ff}",
            "#\u{1f980}\u{1f980}\u{1f980}",
        ] {
            assert_eq!(accent(Some(junk)), None, "should reject {junk:?}");
        }
        assert_eq!(accent(None), None);
    }

    // --- Focus and split. ---

    #[test]
    fn focus_and_split_get_distinct_glyphs_that_do_not_widen_the_row() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "api")];
        m.local_mut().panes = vec![
            pane("p1", "w1", "claude", "", AgentState::Idle),
            pane("p2", "w1", "codex", "", AgentState::Idle),
            pane("p3", "w1", "aider", "", AgentState::Idle),
        ];
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let rows = agent_rows(&m);
        let line0 = |i: usize| agent_row_text(&rows[i], WIDTH)[0].clone();
        assert!(line0(0).starts_with('\u{276f}'), "{:?}", line0(0));
        assert!(line0(1).starts_with('\u{2503}'), "{:?}", line0(1));
        assert!(line0(2).starts_with(' '), "{:?}", line0(2));
        let widths: Vec<usize> = (0..3).map(|i| line0(i).chars().count()).collect();
        assert_eq!(widths[0], widths[1]);
        assert_eq!(widths[1], widths[2]);
    }

    #[test]
    fn the_sidebar_never_draws_the_palettes_selection_glyph() {
        let out = rendered(&one_of_each(), AREA);
        assert!(!out.contains('\u{25b8}'), "{out}");
    }

    // --- Agent ordering and the toggle. ---

    #[test]
    fn grouped_order_follows_the_spaces_list_and_priority_order_does_not() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "first"), ws("w2", "second")];
        m.local_mut().panes = vec![
            pane("calm", "w1", "claude", "", AgentState::Idle),
            pane("stuck", "w2", "codex", "", AgentState::Blocked),
        ];

        m.agent_sort = AgentSort::Grouped;
        let ids: Vec<String> = agent_rows(&m).into_iter().map(|r| r.pane_id).collect();
        assert_eq!(ids, vec!["calm", "stuck"], "space order");

        m.agent_sort = AgentSort::Priority;
        let ids: Vec<String> = agent_rows(&m).into_iter().map(|r| r.pane_id).collect();
        assert_eq!(ids, vec!["stuck", "calm"], "the one that needs you first");
    }

    #[test]
    fn priority_order_is_stable_so_equal_agents_never_shuffle() {
        // A list that reorders under the cursor is a list you cannot click.
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "api")];
        m.local_mut().panes = (0..6)
            .map(|i| pane(&format!("p{i}"), "w1", "claude", "", AgentState::Working))
            .collect();
        m.agent_sort = AgentSort::Priority;
        let ids: Vec<String> = agent_rows(&m).into_iter().map(|r| r.pane_id).collect();
        assert_eq!(ids, vec!["p0", "p1", "p2", "p3", "p4", "p5"]);
    }

    #[test]
    fn the_toggle_shows_the_mode_it_is_in_and_switches_to_the_other() {
        assert_eq!(AgentSort::Grouped.label(), "grouped");
        assert_eq!(AgentSort::Priority.label(), "priority");
        assert_eq!(AgentSort::Grouped.toggled(), AgentSort::Priority);
        assert_eq!(AgentSort::Priority.toggled(), AgentSort::Grouped);
        let mut m = one_of_each();
        m.agent_sort = AgentSort::Priority;
        assert!(rendered(&m, AREA).contains("priority"));
    }

    #[test]
    fn an_orphaned_agent_is_listed_rather_than_hidden() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane("p1", "nope", "claude", "", AgentState::Working)];
        let out = rendered(&m, AREA);
        assert!(out.contains(UNASSIGNED), "{out}");
        assert!(out.contains("claude"), "{out}");
    }

    #[test]
    fn dead_panes_are_listed_nowhere() {
        let mut m = one_of_each();
        m.local_mut().panes[0].live = false;
        let out = rendered(&m, AREA);
        assert!(!out.contains("claude"), "{out}");
        assert!(agent_rows(&m).is_empty());
    }

    // --- Row text. ---

    #[test]
    fn no_row_ever_exceeds_the_sidebar_width() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "a-very-long-space-name-indeed-yes-truly")];
        m.local_mut().panes = vec![pane(
            "p1",
            "w1",
            "claude",
            "an extremely long pane title that would otherwise run off the edge",
            AgentState::Working,
        )];
        for row in sidebar_rows(&m) {
            let SidebarRow::Space(space) = row;
            assert!(space_row_text(&space, WIDTH).chars().count() <= usize::from(WIDTH));
        }
        for agent in agent_rows(&m) {
            for line in agent_row_text(&agent, WIDTH) {
                assert!(line.chars().count() <= usize::from(WIDTH), "{line:?}");
            }
        }
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        // 20 ASCII bytes then seven 4-byte code points puts the byte offset
        // `WIDTH - 1` strictly inside a crab rather than on a boundary — a
        // future "simplification" to `&text[..n]` panics here.
        let name = format!("{}{}", "a".repeat(20), "\u{1f980}".repeat(7));
        let row = SpaceRow {
            id: "w".into(),
            node: NodeId::new("local"),
            name,
            state: DisplayState::Idle,
            accent: None,
            active: false,
            node_tag: None,
            git: None,
        };
        assert!(space_row_text(&row, WIDTH).chars().count() <= usize::from(WIDTH));
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abc", 1), "\u{2026}");
    }

    #[test]
    fn an_untitled_agent_shows_no_dangling_separator() {
        let m = one_of_each();
        let [_, second] = agent_row_text(&agent_rows(&m)[0], WIDTH);
        assert!(!second.contains('\u{2014}'), "{second:?}");
        assert!(second.contains("claude"), "{second:?}");
    }

    #[test]
    fn a_titled_agent_shows_its_title_on_the_second_line() {
        let mut m = one_of_each();
        m.local_mut().panes[0].title = "fixing".into();
        let [first, second] = agent_row_text(&agent_rows(&m)[0], WIDTH);
        assert!(first.contains("frontend"), "{first:?}");
        assert!(second.contains("claude"), "{second:?}");
        assert!(second.contains("fixing"), "{second:?}");
    }

    // --- Scrolling. ---

    #[test]
    fn clamp_keeps_the_last_page_in_view_for_both_row_heights() {
        assert_eq!(clamp_scroll(0, 3, 10, 1), 0, "everything fits");
        assert_eq!(clamp_scroll(5, 3, 10, 1), 0, "cannot scroll a short list");
        assert_eq!(clamp_scroll(99, 20, 10, 1), 10, "last page, not past it");
        // Two rows per entry: a 10-row viewport holds five entries.
        assert_eq!(clamp_scroll(99, 20, 10, 2), 15);
        assert_eq!(clamp_scroll(0, 0, 0, 0), 0, "degenerate is not a panic");
        assert_eq!(clamp_scroll(usize::MAX, 20, 10, 1), 10, "saturates");
    }

    #[test]
    fn a_stale_scroll_offset_never_blanks_a_section() {
        let mut m = one_of_each();
        m.sidebar_scroll = 500;
        m.agents_scroll = 500;
        let out = rendered(&m, AREA);
        assert!(out.contains("frontend"), "{out}");
        assert!(out.contains("claude"), "{out}");
    }

    #[test]
    fn the_spaces_list_draws_from_its_scroll_offset() {
        let mut m = Model::default();
        m.local_mut().workspaces = (0..20)
            .map(|i| ws(&format!("w{i}"), &format!("space{i}")))
            .collect();
        let out = rendered(&m, AREA);
        assert!(out.contains("space0"), "{out}");
        m.sidebar_scroll = 3;
        let out = rendered(&m, AREA);
        assert!(!out.contains("space0"), "scrolled out of view: {out}");
        assert!(out.contains("space3"), "{out}");
    }

    // --- Hit testing. ---

    /// The layout every hit-test case below is written against.
    fn layout() -> SidebarLayout {
        sidebar_layout(AREA, &one_of_each())
    }

    fn hit(model: &Model, column: u16, row: u16) -> SidebarHit {
        hit_test(
            AREA,
            model,
            model.sidebar_scroll,
            model.agents_scroll,
            column,
            row,
        )
    }

    #[test]
    fn clicking_a_space_row_resolves_to_that_space() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "first"), ws("w2", "second")];
        let body = layout().spaces_body;
        assert_eq!(
            hit(&m, 3, body.y),
            SidebarHit::Space(m.local_id_owned(), "w1".into())
        );
        assert_eq!(
            hit(&m, 3, body.y + 1),
            SidebarHit::Space(m.local_id_owned(), "w2".into())
        );
    }

    #[test]
    fn clicking_an_agent_resolves_to_that_pane_on_either_of_its_two_rows() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "api")];
        m.local_mut().panes = vec![
            pane("p1", "w1", "claude", "", AgentState::Idle),
            pane("p2", "w1", "codex", "", AgentState::Idle),
        ];
        let body = layout().agents_body;
        assert_eq!(hit(&m, 3, body.y), SidebarHit::Agent("p1".into()));
        assert_eq!(
            hit(&m, 3, body.y + 1),
            SidebarHit::Agent("p1".into()),
            "the agent-type line belongs to the same entry"
        );
        assert_eq!(hit(&m, 3, body.y + 2), SidebarHit::Agent("p2".into()));
    }

    #[test]
    fn the_three_buttons_are_clickable_where_they_are_drawn() {
        let m = one_of_each();
        let l = layout();
        assert_eq!(hit(&m, l.new_button.x, l.new_button.y), SidebarHit::New);
        assert_eq!(hit(&m, l.menu_button.x, l.menu_button.y), SidebarHit::Menu);
        assert_eq!(
            hit(&m, l.sort_toggle.x, l.sort_toggle.y),
            SidebarHit::SortToggle
        );
    }

    #[test]
    fn the_sort_toggle_wins_over_the_header_row_it_sits_on() {
        let m = one_of_each();
        let l = layout();
        // Left end of the same row is the header, which is inert.
        assert_eq!(
            hit(&m, l.agents_header.x, l.agents_header.y),
            SidebarHit::None
        );
        assert_eq!(
            hit(
                &m,
                l.sort_toggle.x + l.sort_toggle.width - 1,
                l.agents_header.y
            ),
            SidebarHit::SortToggle
        );
    }

    #[test]
    fn clicking_past_the_last_entry_hits_the_body_not_an_entry() {
        // The agents panel takes whatever height is left over, so it
        // ordinarily has empty rows below its last entry.
        let m = one_of_each();
        let l = sidebar_layout(AREA, &m);
        assert_eq!(
            hit(&m, 3, l.agents_body.y + l.agents_body.height - 1),
            SidebarHit::AgentsBody
        );

        // The spaces list is sized to its own contents, so its only empty
        // row is the one `SPACES_MIN_HEIGHT` keeps when there are no spaces
        // at all — which is exactly when a stray click there must not
        // resolve to a space that doesn't exist.
        let empty = Model::default();
        let l = sidebar_layout(AREA, &empty);
        assert!(l.spaces_body.height > 0, "{l:?}");
        assert_eq!(hit(&empty, 3, l.spaces_body.y), SidebarHit::SpacesBody);
    }

    #[test]
    fn the_spaces_section_is_only_as_tall_as_its_contents() {
        // A fixed share of the height would strand the `new`/`menu` footer a
        // dozen blank rows below the last space, reading as a rendering bug
        // rather than as a section boundary.
        let m = one_of_each();
        let l = sidebar_layout(AREA, &m);
        assert_eq!(l.spaces_body.height, 1, "one space, one row: {l:?}");
        assert_eq!(l.new_button.y, l.spaces_body.y + 1);

        // And it grows with the list, up to the point where the agents panel
        // would be squeezed out — past that the spaces list scrolls instead.
        let mut many = Model::default();
        many.local_mut().workspaces = (0..100).map(|i| ws(&format!("w{i}"), "s")).collect();
        let l = sidebar_layout(AREA, &many);
        assert_eq!(
            l.spaces_body.height,
            AREA.height - 2 - AGENTS_MIN_HEIGHT,
            "{l:?}"
        );
        assert!(l.agents_body.height > 0, "the panel survives: {l:?}");
    }

    #[test]
    fn clicking_a_header_does_nothing() {
        let m = one_of_each();
        let l = layout();
        assert_eq!(hit(&m, 3, l.spaces_header.y), SidebarHit::None);
    }

    #[test]
    fn the_divider_is_the_handle_between_the_two_sections() {
        let m = one_of_each();
        let l = layout();
        for column in [0, 3, WIDTH - 1] {
            assert_eq!(hit(&m, column, l.divider.y), SidebarHit::Divider);
        }
    }

    // --- Resizing: width and the section split. ---

    #[test]
    fn the_width_is_clamped_to_its_range_and_to_half_the_terminal() {
        assert_eq!(clamp_width(WIDTH, 200), WIDTH, "the default stands");
        assert_eq!(clamp_width(5, 200), MIN_WIDTH);
        assert_eq!(clamp_width(999, 200), MAX_WIDTH);
        assert_eq!(clamp_width(50, 80), 40, "never more than half");
        assert_eq!(
            clamp_width(WIDTH, 30),
            15,
            "half the terminal wins even over the minimum"
        );
        assert_eq!(clamp_width(WIDTH, 0), 0, "a degenerate screen is no panic");
        assert_eq!(clamp_width(u16::MAX, u16::MAX), MAX_WIDTH);
    }

    #[test]
    fn the_sidebar_column_is_its_content_plus_a_one_cell_edge() {
        let column = Rect::new(0, 0, 28, 20);
        assert_eq!(content_area(column), Rect::new(0, 0, 27, 20));
        assert_eq!(edge(column), Rect::new(27, 0, 1, 20));
        let narrow = Rect::new(4, 2, 1, 5);
        assert_eq!(content_area(narrow).width, 0);
        assert_eq!(edge(narrow), Rect::new(4, 2, 1, 5));
        assert_eq!(edge(Rect::new(0, 0, 0, 9)).width, 0);
    }

    #[test]
    fn a_dragged_split_overrides_the_content_height() {
        let mut m = one_of_each();
        m.sidebar.spaces_height = Some(10);
        let l = sidebar_layout(AREA, &m);
        assert_eq!(l.divider.y, AREA.y + 10, "{l:?}");
        assert_eq!(l.new_button.y, AREA.y + 9, "the footer moves with it");

        // Clamped both ways, so neither section can be dragged out of
        // existence.
        m.sidebar.spaces_height = Some(0);
        let l = sidebar_layout(AREA, &m);
        assert_eq!(l.divider.y, AREA.y + SPACES_MIN_HEIGHT);
        m.sidebar.spaces_height = Some(999);
        let l = sidebar_layout(AREA, &m);
        assert_eq!(l.divider.y, AREA.y + AREA.height - AGENTS_MIN_HEIGHT);
        assert!(l.agents_body.height > 0);
    }

    #[test]
    fn the_split_clamp_matches_the_layout() {
        for total in 0..30u16 {
            for requested in 0..40u16 {
                let mut m = one_of_each();
                m.sidebar.spaces_height = Some(clamp_spaces_height(requested, total));
                let area = Rect::new(0, 0, WIDTH, total);
                let l = sidebar_layout(area, &m);
                if l.divider.height > 0 {
                    assert_eq!(l.divider.y, clamp_spaces_height(requested, total));
                }
            }
        }
    }

    #[test]
    fn clicking_outside_the_sidebar_hits_nothing() {
        let m = one_of_each();
        assert_eq!(hit(&m, WIDTH + 5, 2), SidebarHit::None);
        assert_eq!(hit(&m, 3, AREA.height + 5), SidebarHit::None);
    }

    #[test]
    fn a_scrolled_list_hit_tests_against_the_row_under_the_cursor() {
        let mut m = Model::default();
        m.local_mut().workspaces = (0..30)
            .map(|i| ws(&format!("w{i}"), &format!("s{i}")))
            .collect();
        m.sidebar_scroll = 5;
        let body = layout().spaces_body;
        assert_eq!(
            hit(&m, 3, body.y + 2),
            SidebarHit::Space(m.local_id_owned(), "w7".into())
        );
    }

    #[test]
    fn hit_testing_a_degenerate_sidebar_never_matches_anything() {
        let m = one_of_each();
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, WIDTH, 0),
            Rect::new(0, 0, 0, 20),
        ] {
            for row in 0..3 {
                assert_eq!(hit_test(area, &m, 0, 0, 0, row), SidebarHit::None);
            }
        }
    }

    // --- Git status on space rows. ---

    fn git(branch: Option<&str>, ahead: u64, behind: u64, dirty: bool) -> WorkspaceInfoGit {
        WorkspaceInfoGit {
            branch: branch.map(str::to_string),
            ahead,
            behind,
            dirty,
        }
    }

    /// Two spaces, the first on a git branch and the second not.
    fn one_git_space() -> Model {
        let mut m = Model::default();
        let mut tracked = ws("w1", "api");
        tracked.git = Some(git(Some("main"), 2, 1, true));
        m.local_mut().workspaces = vec![tracked, ws("w2", "notes")];
        m
    }

    fn first_space(model: &Model) -> SpaceRow {
        match sidebar_rows(model).remove(0) {
            SidebarRow::Space(space) => space,
        }
    }

    #[test]
    fn a_space_on_a_branch_gets_a_second_line_naming_it_and_its_status() {
        let out = rendered(&one_git_space(), AREA);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].contains("api"), "{out}");
        assert_eq!(
            lines[2].trim_end(),
            "  main \u{2191}2 \u{2193}1 *",
            "indented under the name, branch then tokens: {out}"
        );
        assert!(lines[3].contains("notes"), "the next space follows: {out}");
    }

    #[test]
    fn the_git_line_only_says_what_is_true() {
        let mut row = first_space(&one_git_space());
        let line = |row: &SpaceRow| space_git_text(row, WIDTH);
        row.git = Some(git(Some("main"), 0, 0, false));
        assert_eq!(line(&row).as_deref(), Some("  main"));
        row.git = Some(git(Some("dev"), 3, 0, false));
        assert_eq!(line(&row).as_deref(), Some("  dev \u{2191}3"));
        row.git = Some(git(Some("dev"), 0, 4, true));
        assert_eq!(line(&row).as_deref(), Some("  dev \u{2193}4 *"));
        row.git = Some(git(None, 0, 0, false));
        assert_eq!(
            line(&row).as_deref(),
            Some("  detached"),
            "no branch is a detached HEAD, said in words"
        );
        row.git = None;
        assert_eq!(line(&row), None);
    }

    #[test]
    fn the_git_line_is_drawn_dim_and_truncated_by_characters() {
        let mut m = Model::default();
        let mut tracked = ws("w1", "api");
        tracked.git = Some(git(Some(&"\u{1f980}".repeat(40)), 0, 0, true));
        m.local_mut().workspaces = vec![tracked];
        let line = space_git_text(&first_space(&m), WIDTH).expect("tracked");
        assert_eq!(line.chars().count(), usize::from(WIDTH), "{line:?}");
        assert!(line.ends_with('\u{2026}'), "{line:?}");

        let mut buf = Buffer::empty(AREA);
        render_sidebar(&m, AREA, &mut buf);
        assert_eq!(buf[(2, 2)].fg, Color::DarkGray, "the second line is dim");
    }

    #[test]
    fn a_space_without_git_stays_one_line() {
        let l = sidebar_layout(AREA, &one_of_each());
        assert_eq!(l.spaces_body.height, 1, "{l:?}");
        assert_eq!(space_entry_rows(&sidebar_rows(&one_of_each())[0]), 1);
        assert_eq!(space_entry_rows(&sidebar_rows(&one_git_space())[0]), 2);
    }

    #[test]
    fn the_spaces_section_makes_room_for_both_lines_of_a_git_entry() {
        let m = one_git_space();
        let l = sidebar_layout(AREA, &m);
        assert_eq!(l.spaces_body.height, 3, "two rows for api, one for notes");
        assert_eq!(l.new_button.y, l.spaces_body.y + 3, "footer still below");
    }

    #[test]
    fn both_lines_of_a_git_entry_select_its_space() {
        let m = one_git_space();
        let body = sidebar_layout(AREA, &m).spaces_body;
        let space = |id: &str| SidebarHit::Space(m.local_id_owned(), id.into());
        assert_eq!(hit(&m, 3, body.y), space("w1"));
        assert_eq!(hit(&m, 3, body.y + 1), space("w1"), "the branch line too");
        assert_eq!(hit(&m, 3, body.y + 2), space("w2"));
    }

    #[test]
    fn variable_height_scroll_clamps_to_the_last_page_that_fills_the_viewport() {
        // Heights [2, 1, 2, 1] in a 3-row viewport: starting at entry 2
        // shows 2 + 1 = 3 rows, which is the last full page.
        assert_eq!(clamp_scroll_rows(99, &[2, 1, 2, 1], 3), 2);
        assert_eq!(clamp_scroll_rows(1, &[2, 1, 2, 1], 3), 1, "in range");
        assert_eq!(clamp_scroll_rows(5, &[2, 1], 10), 0, "everything fits");
        assert_eq!(clamp_scroll_rows(9, &[2, 2], 1), 1, "taller than the view");
        assert_eq!(clamp_scroll_rows(usize::MAX, &[], 0), 0);
        // Uniform heights agree with the fixed-height clamp exactly.
        for scroll in 0..30 {
            assert_eq!(
                clamp_scroll_rows(scroll, &[1; 20], 10),
                clamp_scroll(scroll, 20, 10, 1)
            );
            assert_eq!(
                clamp_scroll_rows(scroll, &[2; 20], 10),
                clamp_scroll(scroll, 20, 10, 2)
            );
        }
    }

    #[test]
    fn a_scrolled_list_of_git_spaces_hit_tests_the_entry_under_the_cursor() {
        let mut m = Model::default();
        m.local_mut().workspaces = (0..30)
            .map(|i| {
                let mut w = ws(&format!("w{i}"), &format!("s{i}"));
                if i % 2 == 0 {
                    w.git = Some(git(Some("main"), 0, 0, false));
                }
                w
            })
            .collect();
        m.sidebar_scroll = 4;
        let body = sidebar_layout(AREA, &m).spaces_body;
        let space = |id: &str| SidebarHit::Space(m.local_id_owned(), id.into());
        // w4 takes two rows, w5 one, then w6.
        assert_eq!(hit(&m, 3, body.y + 1), space("w4"));
        assert_eq!(hit(&m, 3, body.y + 2), space("w5"));
        assert_eq!(hit(&m, 3, body.y + 3), space("w6"));
        let out = rendered(&m, AREA);
        assert!(out.contains("s4"), "{out}");
        assert!(!out.contains("s3 "), "{out}");
    }

    #[test]
    fn a_git_entry_cut_off_at_the_bottom_never_draws_over_the_footer() {
        let mut m = Model::default();
        m.local_mut().workspaces = (0..30)
            .map(|i| {
                let mut w = ws(&format!("w{i}"), &format!("s{i}"));
                w.git = Some(git(Some("main"), 0, 0, false));
                w
            })
            .collect();
        for height in 0..30u16 {
            let area = Rect::new(0, 0, WIDTH, height);
            let l = sidebar_layout(area, &m);
            let mut buf = Buffer::empty(Rect::new(0, 0, WIDTH, height.max(1)));
            render_sidebar(&m, area, &mut buf);
            if l.new_button.width > 0 {
                assert_eq!(buf[(1, l.new_button.y)].symbol(), "n", "at {height}");
            }
        }
    }

    #[test]
    fn hit_testing_with_absurd_scroll_offsets_never_panics() {
        let mut m = one_of_each();
        m.sidebar_scroll = usize::MAX;
        m.agents_scroll = usize::MAX;
        for row in 0..AREA.height {
            for column in 0..AREA.width {
                hit(&m, column, row);
            }
        }
    }
}
