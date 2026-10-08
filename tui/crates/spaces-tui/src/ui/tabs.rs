//! The tab bar: one row across the top of the pane area, one tab per live
//! pane in the active space, and a `+` button that adds another.
//!
//! ```text
//! [ ◐ claude ]  ○ codex     ✓ aider      +
//! ```
//!
//! # Tabs are panes here, not a third level
//!
//! The usual nesting elsewhere is *workspace → tab → pane*, with a tab bar
//! to switch tabs. This runtime's protocol has no tab entity at all — a space holds
//! panes directly — so rather than invent one client-side (which could not
//! survive a detach, and which the web UI and the VR client would know
//! nothing about), the bar shows the **panes of the active space** and its
//! `+` creates another pane there. That is the interaction a tab strip
//! actually gives you; only the middle noun is missing. If the protocol
//! grows tabs later, this module is where they land.
//!
//! Which space is "active" is [`Model::active_space_id`] — the sidebar
//! selection if there is one, else the focused pane's space, else the first.
//!
//! # Text carries the state, styling only reinforces it
//!
//! The active tab is drawn reversed *and* bracketed — `[ ◐ claude ]` versus
//! `  ○ codex  ` — because the brackets occupy padding cells the tab already
//! reserved, so marking a tab active never changes its width and never
//! reflows the strip. Same reasoning as the sidebar's `❯`: a signal that
//! only exists in styling is gone on `TERM=dumb`, in `tmux capture-pane -p`,
//! and to a screen reader. Each tab also leads with its agent's own state
//! icon (see [`super::sidebar::state_icon`]), so the bar answers "which of
//! these wants me" without a trip to the sidebar.
//!
//! # Overflow
//!
//! When the tabs do not all fit, the strip scrolls:
//!
//! ```text
//! < ○ codex  [ ◐ claude ]  ✓ aider   +           >
//! ```
//!
//! A `<` button takes the strip's first two cells and a `>` its last two;
//! each click moves the strip one tab ([`Model::tab_scroll`]). Tabs scrolled
//! out of view either side are counted in [`TabBar::hidden`] rather than
//! drawn clipped, and the `+` still follows the last drawn tab.
//!
//! The strip also follows focus. Focus moves from places that cannot see the
//! strip — the agents panel, `ctrl+k`, `ctrl+space` next/previous — so
//! [`tab_bar`] scrolls the focused pane's tab into view by itself, every
//! frame, *unless* the user has scrolled by hand since that pane was focused
//! ([`Model::tab_scroll_pin`]); otherwise a click on `>` that moved the
//! focused tab off the strip would be undone on the very next frame.
//!
//! [`Model::tab_scroll`]: crate::model::Model::tab_scroll
//! [`Model::tab_scroll_pin`]: crate::model::Model::tab_scroll_pin

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::model::{display_state, Model};

use super::sidebar::state_icon;

/// Rows the tab bar occupies. `frame.rs` subtracts this from the pane area.
pub const HEIGHT: u16 = 1;

/// Narrowest a tab is ever drawn, in cells.
const MIN_TAB_WIDTH: u16 = 8;

/// Cells of padding a tab adds around its label: two each side, of which the
/// inner one carries the active brackets.
const TAB_PADDING: u16 = 4;

/// Width of the `+` button — `" + "`.
const NEW_TAB_WIDTH: u16 = 3;
/// `[files]`.
const FILES_TAB_WIDTH: u16 = 7;

/// Width of each scroll button — `"< "` at the strip's left end, `" >"` at
/// its right.
pub const SCROLL_WIDTH: u16 = 2;

/// Which way a scroll button moves the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabScroll {
    /// `<` — towards the first tab.
    Left,
    /// `>` — towards the last.
    Right,
}

/// Where each part of the tab bar sits for one frame.
///
/// Built by [`tab_bar`] and consumed by both [`render_tab_bar`] and the
/// mouse router, so a click and the frame it landed on cannot disagree —
/// the same discipline [`super::sidebar::sidebar_layout`] follows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabBar {
    /// `(rect, pane_id)` per drawn tab, left to right.
    pub tabs: Vec<(Rect, String)>,
    /// The `+` button. Zero-sized when there is no room for it, or no active
    /// space to create a pane in.
    pub new_tab: Rect,
    /// The active space's Files tab, just before `+`. Zero-sized when that
    /// space has none open.
    pub files_tab: Rect,
    /// The space `new_tab` would create a pane in. `None` when there are no
    /// spaces at all — the one case where the bar has nothing to offer.
    pub space_id: Option<String>,
    /// How many of the active space's panes did not fit — scrolled out of
    /// view on either side.
    pub hidden: usize,
    /// The `<` button. Zero-sized unless the tabs overflow (and the bar is
    /// wide enough to spare the cells — see [`tab_bar`]).
    pub scroll_left: Rect,
    /// The `>` button, flush with the bar's right edge. Zero-sized exactly
    /// when `scroll_left` is.
    pub scroll_right: Rect,
    /// Index, into the active space's live panes, of the first tab drawn —
    /// [`Model::tab_scroll`](crate::model::Model::tab_scroll) after the
    /// focus reveal and the clamp below.
    pub first: usize,
    /// The furthest `first` may go: the earliest tab from which every
    /// remaining tab fits. Scrolling past it would only strand empty strip.
    pub max_first: usize,
}

/// Width one tab needs for `label`: its text plus [`TAB_PADDING`], never
/// below [`MIN_TAB_WIDTH`].
fn tab_width(label: &str) -> u16 {
    (label.chars().count() as u16)
        .saturating_add(TAB_PADDING)
        .max(MIN_TAB_WIDTH)
}

/// Longest name a tab shows, in characters. A pane title is free text an
/// agent sets — without a cap, one pane titled with a whole sentence takes
/// the entire strip and every other tab overflows out of reach. Truncated by
/// **characters**, never by byte-slicing a `String`, which panics off a
/// UTF-8 boundary.
const MAX_NAME: usize = 16;

/// The text inside one tab: the pane's state icon, then its title if it has
/// one, else its agent type. A title is the more useful of the two once two
/// `claude` panes are open, which is the case the bar exists for.
fn tab_label(pane: &spaces_proto::PaneInfo) -> String {
    let name = if pane.title.is_empty() {
        pane.agent_type.as_str()
    } else {
        pane.title.as_str()
    };
    let icon = state_icon(display_state(pane));
    if name.chars().count() <= MAX_NAME {
        return format!("{icon} {name}");
    }
    let clipped: String = name.chars().take(MAX_NAME.saturating_sub(1)).collect();
    format!("{icon} {clipped}\u{2026}")
}

/// Lays the bar out inside `area`.
///
/// The `+` button's [`NEW_TAB_WIDTH`] cells are reserved **before** tabs are
/// placed, so the way to add a pane never disappears because there are
/// already too many — the failure that would make the bar useless exactly
/// when you need it. Tabs then fill what is left, left to right, and
/// whatever does not fit is counted in [`TabBar::hidden`] rather than drawn
/// clipped.
///
/// When the tabs overflow, [`SCROLL_WIDTH`] cells at each end are reserved
/// too, for the `<`/`>` buttons, and the strip starts at tab
/// [`TabBar::first`]: the model's [`Model::tab_scroll`], moved just far
/// enough to show the focused pane's tab (unless the user pinned it by
/// scrolling by hand — see the module docs), then clamped to
/// [`TabBar::max_first`]. A bar too narrow to spare the button cells and
/// still show a tab drops the buttons rather than the tabs.
///
/// Saturating throughout; a zero-width or zero-height `area` yields an empty
/// bar rather than a panic.
///
/// [`Model::tab_scroll`]: crate::model::Model::tab_scroll
pub fn tab_bar(area: Rect, model: &Model) -> TabBar {
    let mut bar = TabBar::default();
    if area.width == 0 || area.height == 0 {
        return bar;
    }
    let Some(space_id) = model.active_space_id().map(str::to_string) else {
        return bar;
    };
    let panes = model.live_panes_in(&space_id);
    bar.space_id = Some(space_id.clone());
    let widths: Vec<u16> = panes.iter().map(|p| tab_width(&tab_label(p))).collect();

    // Room for the `+` is reserved out of the strip up front, so it survives
    // however many tabs there are — but it is *placed* immediately after the
    // last one rather than flush right. Marooned at the far edge of a wide
    // terminal it read as unrelated furniture; beside the tabs it reads as
    // "one more of these", which is what it makes.
    // The Files tab sits with `+` in the reserved space on the right, so it
    // never scrolls out of view with the terminal tabs.
    let files_width = if model.files.contains_key(&space_id) {
        FILES_TAB_WIDTH.min(area.width.saturating_sub(NEW_TAB_WIDTH))
    } else {
        0
    };
    let new_width = NEW_TAB_WIDTH.min(area.width) + files_width;
    let total: u32 = widths.iter().map(|w| u32::from(*w)).sum();
    let overflows = total.saturating_add(u32::from(new_width)) > u32::from(area.width);
    let buttons = overflows && area.width >= SCROLL_WIDTH * 2 + new_width + MIN_TAB_WIDTH;
    let (strip_x, strip_width) = if buttons {
        (
            area.x.saturating_add(SCROLL_WIDTH),
            area.width - SCROLL_WIDTH * 2 - new_width,
        )
    } else {
        (area.x, area.width.saturating_sub(new_width))
    };
    let strip_end = strip_x.saturating_add(strip_width);

    bar.max_first = last_page_start(&widths, strip_width);
    let mut first = model.tab_scroll;
    let focused = model.focused.as_deref();
    if focused.is_some() && model.tab_scroll_pin.as_deref() != focused {
        if let Some(index) = panes.iter().position(|p| Some(p.id.as_str()) == focused) {
            first = reveal(&widths, first, index, strip_width);
        }
    }
    bar.first = first.min(bar.max_first);

    let mut x = strip_x;
    for (pane, width) in panes.iter().zip(&widths).skip(bar.first) {
        if x.saturating_add(*width) > strip_end {
            break;
        }
        bar.tabs
            .push((Rect::new(x, area.y, *width, 1), pane.id.clone()));
        x = x.saturating_add(*width);
    }
    bar.hidden = panes.len().saturating_sub(bar.tabs.len());

    // `x` is now just past the last tab, and cannot exceed `strip_end` — the
    // loop stops before a tab would cross it — so the button always fits
    // inside `area` without being clamped, and left of `>` when there is one.
    bar.files_tab = Rect::new(x, area.y, files_width, 1);
    bar.new_tab = Rect::new(x + files_width, area.y, new_width - files_width, 1);
    if buttons {
        bar.scroll_left = Rect::new(area.x, area.y, SCROLL_WIDTH, 1);
        bar.scroll_right = Rect::new(
            area.x.saturating_add(area.width - SCROLL_WIDTH),
            area.y,
            SCROLL_WIDTH,
            1,
        );
    }
    bar
}

/// The earliest index from which every remaining tab fits in `strip` cells:
/// the furthest the strip is ever allowed to scroll. A last tab wider than
/// the whole strip still counts as a page of its own, so the strip can always
/// reach it.
fn last_page_start(widths: &[u16], strip: u16) -> usize {
    let mut used = 0u32;
    let mut first = widths.len();
    for (index, width) in widths.iter().enumerate().rev() {
        used = used.saturating_add(u32::from(*width));
        if used > u32::from(strip) {
            break;
        }
        first = index;
    }
    first.min(widths.len().saturating_sub(1))
}

/// The smallest change to `first` that puts tab `index` wholly on a strip
/// `strip` cells wide: back to it if it is scrolled off the left, forward
/// just far enough if it is off the right, and `first` untouched if it is
/// already visible — so a focus change inside the strip never jolts it.
fn reveal(widths: &[u16], first: usize, index: usize, strip: u16) -> usize {
    if index < first {
        return index;
    }
    let mut start = first;
    while start < index {
        let span: u32 = widths
            .get(start..=index)
            .unwrap_or_default()
            .iter()
            .map(|w| u32::from(*w))
            .sum();
        if span <= u32::from(strip) {
            break;
        }
        start += 1;
    }
    start
}

/// Draws the bar. `bar` must be the one [`tab_bar`] produced for this same
/// `area` and `model` — passing it in rather than recomputing it here is
/// what guarantees the rects the mouse router hit-tests are the rects that
/// were actually drawn.
pub fn render_tab_bar(model: &Model, bar: &TabBar, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let dim = Style::default().fg(Color::DarkGray);

    for (rect, pane_id) in &bar.tabs {
        let Some(pane) = model.local_panes().iter().find(|p| &p.id == pane_id) else {
            continue;
        };
        // A terminal is not the active tab while Files fills the area.
        let active = model.focused.as_deref() == Some(pane_id.as_str())
            && model.shown_files().map(|(_, l)| l) != Some(crate::files::FilesLayout::Full);
        let split = model.split.as_deref() == Some(pane_id.as_str());
        let label = tab_label(pane);
        // Centre the label in the tab, then spend the innermost padding cell
        // on each side marking the active tab — same width either way.
        let pad = usize::from(rect.width).saturating_sub(label.chars().count());
        let left = pad / 2;
        let right = pad - left;
        let (open, close) = if active {
            ('[', ']')
        } else if split {
            ('\u{2503}', '\u{2503}')
        } else {
            (' ', ' ')
        };
        // `left`/`right` are both >= 2 (a tab is at least its label plus
        // `TAB_PADDING`), so spending one cell each on the markers can never
        // underflow or push the label out.
        let text = format!(
            "{}{open}{label}{close}{}",
            " ".repeat(left.saturating_sub(1)),
            " ".repeat(right.saturating_sub(1)),
        );
        // A pane's own accent colors its tab. Reversed video on the active
        // one turns that foreground into the tab's *background*, which is
        // what makes a colored tab read as selected rather than merely
        // brighter — and is why the accent is applied before the reverse
        // rather than instead of it.
        let accent = super::sidebar::accent(pane.color.as_deref());
        let mut style = Style::default().fg(accent.unwrap_or(Color::Gray));
        if active {
            style = Style::default()
                .fg(accent.unwrap_or(Color::White))
                .add_modifier(Modifier::REVERSED | Modifier::BOLD);
        }
        Paragraph::new(Line::from(Span::styled(text, style))).render(*rect, buf);
    }

    if bar.files_tab.width > 0 {
        let shown = model.shown_files().is_some();
        let style = if shown {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::REVERSED | Modifier::BOLD)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let label = if shown { "[files]" } else { " files " };
        Paragraph::new(Line::styled(label, style)).render(bar.files_tab, buf);
    }

    if bar.new_tab.width > 0 && bar.space_id.is_some() {
        Paragraph::new(Line::styled(" + ", dim.add_modifier(Modifier::BOLD)))
            .render(bar.new_tab, buf);
    }

    // A button that can still move the strip is drawn brighter than one that
    // has reached its end — but both are drawn, so the strip's two edges
    // never jump in and out as the user scrolls through it.
    let button = |live: bool| {
        if live {
            Style::default()
                .fg(Color::Gray)
                .add_modifier(Modifier::BOLD)
        } else {
            dim
        }
    };
    if bar.scroll_left.width > 0 {
        Paragraph::new(Line::styled("< ", button(bar.first > 0))).render(bar.scroll_left, buf);
    }
    if bar.scroll_right.width > 0 {
        Paragraph::new(Line::styled(" >", button(bar.first < bar.max_first)))
            .render(bar.scroll_right, buf);
    }
}

/// Which scroll button a click at `(column, row)` landed on, if either.
pub fn scroll_at(bar: &TabBar, column: u16, row: u16) -> Option<TabScroll> {
    if hit(bar.scroll_left, column, row) {
        Some(TabScroll::Left)
    } else if hit(bar.scroll_right, column, row) {
        Some(TabScroll::Right)
    } else {
        None
    }
}

/// The pane a click at `(column, row)` selected, if it landed on a tab.
/// First match wins; [`tab_bar`] never produces overlapping rects, but this
/// must not depend on that — "pick one deterministically" is the only
/// behaviour that cannot surprise.
pub fn tab_at(bar: &TabBar, column: u16, row: u16) -> Option<&str> {
    bar.tabs
        .iter()
        .find(|(rect, _)| hit(*rect, column, row))
        .map(|(_, id)| id.as_str())
}

/// `true` iff the click landed on the `+` button.
pub fn is_files_tab(bar: &TabBar, column: u16, row: u16) -> bool {
    hit(bar.files_tab, column, row)
}

pub fn is_new_tab(bar: &TabBar, column: u16, row: u16) -> bool {
    bar.space_id.is_some() && hit(bar.new_tab, column, row)
}

fn hit(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use spaces_proto::{AgentState, PaneInfo, WorkspaceInfo};

    const AREA: Rect = Rect {
        x: 28,
        y: 0,
        width: 52,
        height: 1,
    };

    fn ws(id: &str, name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            git: None,
            id: id.into(),
            name: name.into(),
            color: None,
            pane_ids: vec![],
        }
    }

    fn pane(id: &str, space: &str, agent: &str, state: AgentState) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(space.into()),
            agent_type: agent.into(),
            agent_state: state,
            seen: true,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        }
    }

    fn two_panes() -> Model {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "api")];
        m.local_mut().panes = vec![
            pane("p1", "w1", "claude", AgentState::Working),
            pane("p2", "w1", "codex", AgentState::Idle),
        ];
        m.focused = Some("p1".into());
        m
    }

    /// The pane ids the bar drew, left to right.
    fn ids_of(bar: &TabBar) -> Vec<String> {
        bar.tabs.iter().map(|(_, id)| id.clone()).collect()
    }

    fn text(model: &Model, bar: &TabBar, area: Rect) -> String {
        let mut buf = Buffer::empty(Rect::new(0, 0, area.x + area.width, area.height));
        render_tab_bar(model, bar, area, &mut buf);
        (0..buf.area.width)
            .map(|x| buf[(x, area.y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn every_live_pane_in_the_active_space_gets_a_tab() {
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        let ids = ids_of(&bar);
        assert_eq!(ids, vec!["p1", "p2"]);
        assert_eq!(bar.space_id.as_deref(), Some("w1"));
        assert_eq!(bar.hidden, 0);
    }

    #[test]
    fn only_the_active_spaces_panes_are_shown() {
        let mut m = two_panes();
        m.local_mut().workspaces.push(ws("w2", "web"));
        m.local_mut()
            .panes
            .push(pane("other", "w2", "aider", AgentState::Idle));
        assert_eq!(
            ids_of(&tab_bar(AREA, &m)),
            vec!["p1", "p2"],
            "focused pane's space"
        );

        // Selecting the other space swaps the whole strip.
        m.selected_space = Some("w2".into());
        assert_eq!(ids_of(&tab_bar(AREA, &m)), vec!["other"]);
    }

    #[test]
    fn dead_panes_get_no_tab() {
        let mut m = two_panes();
        m.local_mut().panes[1].live = false;
        let ids = ids_of(&tab_bar(AREA, &m));
        assert_eq!(ids, vec!["p1"]);
    }

    #[test]
    fn the_tab_bar_shows_each_panes_name_and_state() {
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        let out = text(&m, &bar, AREA);
        assert!(out.contains("claude"), "{out}");
        assert!(out.contains("codex"), "{out}");
        assert!(
            out.contains(state_icon(crate::model::DisplayState::Working)),
            "{out}"
        );
        assert!(
            out.contains(state_icon(crate::model::DisplayState::Idle)),
            "{out}"
        );
    }

    #[test]
    fn a_pane_with_a_title_is_named_by_it() {
        let mut m = two_panes();
        m.local_mut().panes[0].title = "parser".into();
        let bar = tab_bar(AREA, &m);
        let out = text(&m, &bar, AREA);
        assert!(out.contains("parser"), "{out}");
    }

    #[test]
    fn the_active_tab_is_marked_in_the_text_not_only_in_the_styling() {
        // Bold and reverse video vanish on a terminal with styling stripped;
        // the brackets do not.
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        let out = text(&m, &bar, AREA);
        let active_at = out.find('[').expect("the focused tab is bracketed");
        let claude_at = out.find("claude").expect("drawn");
        assert!(active_at < claude_at, "{out:?}");
        assert_eq!(
            out.matches('[').count(),
            1,
            "exactly one active tab: {out:?}"
        );
        assert_eq!(out.matches(']').count(), 1, "{out:?}");
    }

    #[test]
    fn marking_a_tab_active_does_not_change_its_width() {
        // If it did, focusing a pane would reflow the whole strip and every
        // other tab would move out from under the cursor.
        let m = two_panes();
        let before: Vec<Rect> = tab_bar(AREA, &m).tabs.iter().map(|(r, _)| *r).collect();
        let mut after_model = m.clone();
        after_model.focused = Some("p2".into());
        let after: Vec<Rect> = tab_bar(AREA, &after_model)
            .tabs
            .iter()
            .map(|(r, _)| *r)
            .collect();
        assert_eq!(before, after);
    }

    #[test]
    fn the_split_pane_gets_its_own_mark_distinct_from_the_active_one() {
        let mut m = two_panes();
        m.split = Some("p2".into());
        let bar = tab_bar(AREA, &m);
        let out = text(&m, &bar, AREA);
        assert!(out.contains('\u{2503}'), "{out:?}");
        assert_eq!(out.matches('[').count(), 1, "{out:?}");
    }

    #[test]
    fn the_new_tab_button_is_always_reachable_however_many_tabs_there_are() {
        // Its cells are reserved before tabs are placed — the way to add a
        // pane must not vanish because there are already too many.
        let mut m = two_panes();
        m.local_mut().panes = (0..40)
            .map(|i| pane(&format!("p{i}"), "w1", "claude", AgentState::Idle))
            .collect();
        m.focused = Some("p0".into());
        let bar = tab_bar(AREA, &m);
        assert_eq!(bar.new_tab.width, NEW_TAB_WIDTH);
        assert!(bar.hidden > 0, "precondition: the strip overflowed");
        assert!(is_new_tab(&bar, bar.new_tab.x, bar.new_tab.y));
        // Still inside the bar even with the strip full — the reservation is
        // what guarantees that, not the position.
        assert!(
            bar.new_tab.x + bar.new_tab.width <= AREA.x + AREA.width,
            "the button must not run off the edge: {:?}",
            bar.new_tab
        );
    }

    #[test]
    fn the_bar_draws_the_button_beside_the_tabs() {
        // Renders the real bar and reads the row back, rather than trusting
        // rects: the two dead-seam bugs in this crate both passed rect tests
        // while nothing reached the screen.
        use ratatui::buffer::Buffer;
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        let mut buf = Buffer::empty(AREA);
        render_tab_bar(&m, &bar, AREA, &mut buf);
        let row: String = (AREA.x..AREA.x + AREA.width)
            .map(|x| buf[(x, AREA.y)].symbol().to_string())
            .collect::<Vec<_>>()
            .join("");
        let trimmed = row.trim_end();
        assert!(trimmed.contains('+'), "the button is drawn: {trimmed:?}");
        let plus = trimmed.find('+').unwrap();
        // Nothing but the tabs before it, and nothing after it at all.
        assert_eq!(
            plus,
            trimmed.len() - 1,
            "it ends the drawn row: {trimmed:?}"
        );
        assert!(
            plus < 40,
            "and sits near the tabs, not at the far edge: {plus}"
        );
    }

    #[test]
    fn the_new_tab_button_sits_next_to_the_last_tab() {
        // Flush right it was marooned at the far edge of a wide terminal and
        // read as unrelated furniture. Beside the tabs it reads as "one more
        // of these", which is what it makes.
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        let (last, _) = bar.tabs.last().expect("two panes are drawn");
        assert_eq!(bar.new_tab.x, last.x + last.width);
        assert!(
            bar.new_tab.x + bar.new_tab.width < AREA.x + AREA.width,
            "and well short of the edge with only two tabs"
        );
    }

    #[test]
    fn a_space_with_no_panes_still_offers_the_button_first() {
        // Nothing to sit beside, so it leads — which is the only thing on the
        // bar at that point and the only way out of an empty space.
        let mut m = two_panes();
        m.local_mut().panes = vec![];
        let bar = tab_bar(AREA, &m);
        assert!(bar.tabs.is_empty());
        assert_eq!(bar.new_tab.x, AREA.x);
        assert!(is_new_tab(&bar, bar.new_tab.x, bar.new_tab.y));
    }

    #[test]
    fn overflowing_tabs_are_counted_not_drawn_clipped() {
        let mut m = two_panes();
        m.local_mut().panes = (0..40)
            .map(|i| pane(&format!("p{i}"), "w1", "claude", AgentState::Idle))
            .collect();
        m.focused = Some("p0".into());
        let bar = tab_bar(AREA, &m);
        assert_eq!(bar.tabs.len() + bar.hidden, 40);
        let strip_end = AREA.x + AREA.width - NEW_TAB_WIDTH;
        for (rect, _) in &bar.tabs {
            assert!(rect.x + rect.width <= strip_end, "{rect:?}");
        }
    }

    #[test]
    fn clicking_a_tab_resolves_to_its_pane() {
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        let (rect, _) = bar.tabs[1];
        assert_eq!(tab_at(&bar, rect.x, rect.y), Some("p2"));
        assert_eq!(tab_at(&bar, rect.x + rect.width - 1, rect.y), Some("p2"));
        assert_eq!(tab_at(&bar, bar.new_tab.x, bar.new_tab.y), None);
    }

    #[test]
    fn a_client_with_no_spaces_offers_no_tabs_and_no_plus() {
        let m = Model::default();
        let bar = tab_bar(AREA, &m);
        assert!(bar.tabs.is_empty());
        assert_eq!(bar.space_id, None);
        assert!(!is_new_tab(&bar, AREA.x, AREA.y));
        assert_eq!(text(&m, &bar, AREA).trim(), "");
    }

    #[test]
    fn a_space_with_no_panes_still_offers_the_plus_that_makes_one() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "empty")];
        let bar = tab_bar(AREA, &m);
        assert!(bar.tabs.is_empty());
        assert_eq!(bar.space_id.as_deref(), Some("w1"));
        assert!(is_new_tab(&bar, bar.new_tab.x, bar.new_tab.y));
        assert!(text(&m, &bar, AREA).contains('+'));
    }

    #[test]
    fn degenerate_areas_neither_panic_nor_produce_targets() {
        let m = two_panes();
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 52, 0),
            Rect::new(0, 0, 0, 1),
            Rect::new(0, 0, 1, 1),
            Rect::new(0, 0, 3, 1),
        ] {
            let bar = tab_bar(area, &m);
            let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
            render_tab_bar(&m, &bar, area, &mut buf);
            for (rect, _) in &bar.tabs {
                assert!(rect.x + rect.width <= area.x + area.width, "{rect:?}");
            }
        }
    }

    #[test]
    fn a_long_title_cannot_take_the_whole_strip() {
        // A pane title is free text an agent sets; without a cap one
        // sentence-titled pane pushes every other tab out of reach.
        let mut m = two_panes();
        m.local_mut().panes[0].title = "refactoring the entire authentication subsystem".into();
        let bar = tab_bar(AREA, &m);
        assert_eq!(ids_of(&bar), vec!["p1", "p2"], "both still fit");
        assert!(bar.tabs[0].0.width <= MAX_NAME as u16 + 2 + TAB_PADDING);
        assert!(text(&m, &bar, AREA).contains('\u{2026}'), "and it says so");
    }

    #[test]
    fn a_multibyte_title_is_clipped_by_characters_not_bytes() {
        let mut m = two_panes();
        m.local_mut().panes[0].title = "\u{1f980}".repeat(40);
        let bar = tab_bar(AREA, &m);
        assert!(!bar.tabs.is_empty());
        text(&m, &bar, AREA);
    }

    // --- Scrolling an overflowing strip. ---

    /// Forty `claude` panes in one space — far more than [`AREA`] holds.
    fn crowded(focused: &str) -> Model {
        let mut m = two_panes();
        m.local_mut().panes = (0..40)
            .map(|i| pane(&format!("p{i}"), "w1", "claude", AgentState::Idle))
            .collect();
        m.focused = Some(focused.into());
        m
    }

    #[test]
    fn a_strip_that_fits_has_no_scroll_buttons() {
        let m = two_panes();
        let bar = tab_bar(AREA, &m);
        assert_eq!(bar.scroll_left, Rect::default());
        assert_eq!(bar.scroll_right, Rect::default());
        let out = text(&m, &bar, AREA);
        assert!(!out.contains('<') && !out.contains('>'), "{out:?}");
    }

    #[test]
    fn an_overflowing_strip_draws_a_button_at_each_end() {
        let m = crowded("p0");
        let bar = tab_bar(AREA, &m);
        assert_eq!(bar.scroll_left, Rect::new(AREA.x, AREA.y, SCROLL_WIDTH, 1));
        assert_eq!(
            bar.scroll_right,
            Rect::new(AREA.x + AREA.width - SCROLL_WIDTH, AREA.y, SCROLL_WIDTH, 1)
        );
        let out = text(&m, &bar, AREA);
        let strip: String = out.chars().skip(usize::from(AREA.x)).collect();
        assert!(strip.starts_with("< "), "{strip:?}");
        assert!(strip.ends_with(" >"), "{strip:?}");
    }

    #[test]
    fn tabs_and_the_plus_sit_between_the_two_buttons() {
        let m = crowded("p0");
        let bar = tab_bar(AREA, &m);
        let first = bar.tabs.first().expect("some tabs drawn").0;
        assert_eq!(first.x, bar.scroll_left.x + bar.scroll_left.width);
        let (last, _) = bar.tabs.last().cloned().expect("drawn");
        assert_eq!(
            bar.new_tab.x,
            last.x + last.width,
            "the plus follows the tabs"
        );
        assert!(
            bar.new_tab.x + bar.new_tab.width <= bar.scroll_right.x,
            "and never overlaps the right button: {bar:?}"
        );
        assert_eq!(bar.tabs.len() + bar.hidden, 40);
    }

    #[test]
    fn the_strip_starts_from_the_scroll_offset() {
        let mut m = crowded("p0");
        m.tab_scroll = 3;
        m.tab_scroll_pin = Some("p0".into());
        let bar = tab_bar(AREA, &m);
        assert_eq!(bar.first, 3);
        assert_eq!(ids_of(&bar)[0], "p3");
        assert_eq!(bar.tabs.len() + bar.hidden, 40, "hidden counts both sides");
    }

    #[test]
    fn the_focused_tab_is_scrolled_into_view() {
        // Focus moved somewhere off the strip — by the sidebar, `ctrl+k`, or
        // next/previous — so the strip follows it.
        let m = crowded("p30");
        let bar = tab_bar(AREA, &m);
        assert!(
            ids_of(&bar).contains(&"p30".to_string()),
            "{:?}",
            ids_of(&bar)
        );

        // And back the other way, from a strip scrolled past it.
        let mut m = crowded("p1");
        m.tab_scroll = 20;
        let bar = tab_bar(AREA, &m);
        assert_eq!(ids_of(&bar)[0], "p1");
    }

    #[test]
    fn scrolling_by_hand_may_leave_the_focused_tab_behind() {
        // The pin records that the user scrolled while this pane was focused;
        // revealing it again would undo every click on `>`.
        let mut m = crowded("p0");
        m.tab_scroll = 10;
        m.tab_scroll_pin = Some("p0".into());
        let bar = tab_bar(AREA, &m);
        assert_eq!(ids_of(&bar)[0], "p10");

        // Focusing a different pane drops the pin's hold.
        m.focused = Some("p2".into());
        let bar = tab_bar(AREA, &m);
        assert!(ids_of(&bar).contains(&"p2".to_string()));
    }

    #[test]
    fn the_offset_never_scrolls_past_a_full_last_page() {
        let mut m = crowded("p0");
        m.tab_scroll = 999;
        m.tab_scroll_pin = Some("p0".into());
        let bar = tab_bar(AREA, &m);
        assert_eq!(bar.first, bar.max_first);
        assert_eq!(ids_of(&bar).last().map(String::as_str), Some("p39"));
        // One tab further left would not have fit.
        assert!(bar.tabs.len() > 1, "{bar:?}");
    }

    #[test]
    fn the_scroll_buttons_resolve_to_their_direction() {
        let m = crowded("p0");
        let bar = tab_bar(AREA, &m);
        assert_eq!(
            scroll_at(&bar, bar.scroll_left.x + 1, AREA.y),
            Some(TabScroll::Left)
        );
        assert_eq!(
            scroll_at(&bar, bar.scroll_right.x, AREA.y),
            Some(TabScroll::Right)
        );
        let (tab, _) = bar.tabs[0];
        assert_eq!(scroll_at(&bar, tab.x, tab.y), None);
        assert_eq!(tab_at(&bar, bar.scroll_left.x, AREA.y), None);
        assert!(!is_new_tab(&bar, bar.scroll_right.x, AREA.y));
    }

    #[test]
    fn a_strip_too_narrow_for_buttons_falls_back_to_counting() {
        let m = crowded("p0");
        for width in 0..16u16 {
            let area = Rect::new(0, 0, width, 1);
            let bar = tab_bar(area, &m);
            for (rect, _) in &bar.tabs {
                assert!(rect.x + rect.width <= area.x + area.width, "{rect:?}");
            }
            for rect in [bar.scroll_left, bar.scroll_right, bar.new_tab] {
                assert!(rect.x + rect.width <= area.x + area.width, "{rect:?}");
            }
            let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
            render_tab_bar(&m, &bar, area, &mut buf);
        }
    }

    #[test]
    fn a_tab_is_never_narrower_than_the_minimum() {
        let mut m = two_panes();
        m.local_mut().panes[0].agent_type = "x".into();
        let bar = tab_bar(AREA, &m);
        assert!(bar.tabs[0].0.width >= MIN_TAB_WIDTH);
    }
}
