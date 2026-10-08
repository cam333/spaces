//! Top-level frame composition.
//!
//! [`render`] lays out the terminal into three regions: a sidebar (see
//! [`super::sidebar`]) as wide as [`crate::model::SidebarState::columns`]
//! says, a pane area that fills whatever width remains, and a one-line
//! status bar across the bottom. The command
//! palette (see [`super::palette::render_palette`]) is drawn as an overlay
//! *on top of* the pane area rather than as a region of its own.
//!
//! `f.area()` ultimately comes from the *terminal's own reported size*, not
//! anything the runtime sent — a user can resize their terminal to anything,
//! including narrower than the sidebar itself (a 4x2 frame with a 28-column
//! sidebar is a real case). Every area this module hands to a sub-renderer is
//! checked for zero width/height first, and the status-bar arithmetic below
//! uses checked operations so a pathologically small frame degrades to "draws
//! less" instead of panicking.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Frame;

use crate::model::Model;
use crate::term::PaneTerm;

use super::menu::render_menu;
use super::palette::{render_palette, Palette};
use super::sidebar::{content_area, edge, render_sidebar};
use super::tabs::{render_tab_bar, tab_bar, HEIGHT as TAB_BAR_HEIGHT};
use super::toast::render_toasts;

/// Height of the status bar, in terminal rows.
const STATUS_BAR_HEIGHT: u16 = 1;

/// Right-hand hint shown on the status bar.
const PALETTE_HINT: &str = "ctrl+k palette";

/// Literal text shown on the status bar while [`Model::stale`] is set.
const RECONNECTING: &str = "RECONNECTING";

/// Literal text shown on the status bar when `model.focused` names a pane but
/// [`Model::detached_pane`] says there is no live socket attached to it.
/// Without a visible signal here, a keystroke aimed at a pane with no live
/// socket looks exactly like a keystroke aimed at a pane that's simply quiet,
/// which the binding constraint ("input is refused... rather than silently
/// dropped") rules out. Not shown while [`Model::stale`] — that already covers
/// "the whole client is disconnected", a broader and more urgent fact than
/// "this one pane's socket needs reattaching".
const PANE_DISCONNECTED: &str = "PANE DISCONNECTED";

/// Shown while [`Model::mouse_captured`] is off: the terminal owns the mouse,
/// so click-drag selects text and nothing in this client is clickable.
///
/// Drawn on the **right**, replacing [`PALETTE_HINT`], rather than folded in
/// with the notices on the left. It is not a problem report — it is the mode
/// you are in, it persists until you leave it, and it must not be outranked
/// by `RECONNECTING` or a pane error the way a notice would be. Carrying the
/// way out with it is the point: this is the state in which the palette
/// cannot be reached by clicking.
const SELECT_MODE_HINT: &str = "SELECT MODE \u{b7} ctrl+space m";

/// Splits `area` into (sidebar, tab bar, pane, status) regions, the sidebar
/// `sidebar_width` columns wide — the one
/// place this layout is computed, shared by [`render`], [`pane_area`],
/// [`sidebar_area`] and [`tab_bar_area`] so `main.rs`'s event loop can size
/// a pane's `PaneTerm`/`PaneSocket`, and hit-test a mouse click against the
/// sidebar or the tab bar, to match exactly what's about to be drawn —
/// without a second, possibly-diverging copy of this math. Degenerate (zero
/// width/height) input maps to four zero-sized regions rather than panicking
/// inside `Layout`'s own constraint solver.
///
/// The tab bar takes its row out of the *pane column only*, not the full
/// width: it belongs to the panes it labels, and running it across the
/// sidebar would put a `+ new pane` control above the `spaces` header, which
/// is the one place it does not mean anything. On a terminal too narrow to
/// leave any pane column at all, there is no tab bar either.
///
/// `sidebar_width` is a plain parameter, not read from a model, so this stays
/// a pure function of its inputs: callers pass
/// [`crate::model::SidebarState::columns`] for the terminal's current width,
/// which is where the user's dragged width is clamped to fit.
fn regions(area: Rect, sidebar_width: u16) -> (Rect, Rect, Rect, Rect) {
    if area.width == 0 || area.height == 0 {
        let empty = Rect::new(area.x, area.y, 0, 0);
        return (empty, empty, empty, empty);
    }

    let [main_area, status_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(STATUS_BAR_HEIGHT)]).areas(area);
    let [sidebar_area, right] =
        Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)])
            .areas(main_area);
    if right.width == 0 || right.height == 0 {
        let empty = Rect::new(right.x, right.y, 0, 0);
        return (sidebar_area, empty, empty, status_area);
    }
    let [tab_area, pane_area] =
        Layout::vertical([Constraint::Length(TAB_BAR_HEIGHT), Constraint::Min(0)]).areas(right);
    (sidebar_area, tab_area, pane_area, status_area)
}

/// The screen area available for the focused pane's terminal grid, i.e.
/// `area` minus the sidebar column, the tab bar row, and the status bar row.
/// `main.rs`'s event loop calls this (independently of [`render`]) to size a
/// newly-focused or just-resized pane's `PaneTerm`/`PaneSocket` before the
/// next redraw actually happens.
pub fn pane_area(area: Rect, sidebar_width: u16) -> Rect {
    regions(area, sidebar_width).2
}

/// The screen area occupied by the sidebar column — [`regions`]' first
/// element, exposed the same way [`pane_area`] already exposes its third.
/// `main.rs`'s event loop calls this at click time (`sidebar_area(terminal.size()?)`,
/// not a rect cached from the last render) so a mouse click is always
/// hit-tested against exactly the rect the *next* redraw will draw the
/// sidebar into — a resize landing between render and click would otherwise
/// let a cached rect silently disagree with what's actually on screen,
/// targeting the wrong row.
///
/// The whole column, its draggable edge included — split it with
/// [`super::sidebar::content_area`] and [`super::sidebar::edge`].
pub fn sidebar_area(area: Rect, sidebar_width: u16) -> Rect {
    regions(area, sidebar_width).0
}

/// The one-row strip above the panes. Same contract as [`sidebar_area`]:
/// recomputed at click time from the terminal's current size, never cached.
pub fn tab_bar_area(area: Rect, sidebar_width: u16) -> Rect {
    regions(area, sidebar_width).1
}

/// The region the command-palette overlay is centered in — the pane area
/// normally, or the whole screen on a terminal too narrow to have one.
///
/// The `else` branch is not a nicety: `regions` gives the pane column zero
/// width below the sidebar's width, and an overlay scoped strictly to it would
/// draw nothing, putting `ctrl+k` right back to being an invisible keystroke
/// sink on exactly the terminals where the user has least idea what is going
/// on. Exposed so `main.rs`'s mouse arm can hit-test a click against the same
/// rect [`render`] drew the overlay into — see
/// [`super::palette::palette_layout`].
pub fn palette_area(area: Rect, sidebar_width: u16) -> Rect {
    let (_, _, pane, _) = regions(area, sidebar_width);
    if pane.width > 0 && pane.height > 0 {
        pane
    } else {
        area
    }
}

/// Divides a pane area (i.e. [`pane_area`]'s output) into the one or two
/// regions the visible panes are drawn into, **left first**.
///
/// Pure geometry: this knows nothing about which pane goes where. `split ==
/// false` hands back `area` unchanged and `None` — the ordinary, one-pane
/// case. `split == true` divides the *width* in two: no columns are lost to
/// a divider (there is none — the two grids abut, and each pane's own
/// content is what separates them visually), and an odd width gives the
/// extra column to the right-hand region.
///
/// [`visible_pane_regions`] is what maps these onto the focused and split
/// panes; that mapping is a `Model` question, not a geometric one.
///
/// Computed with plain arithmetic rather than a [`Layout`] because the
/// degenerate cases here are the interesting ones and they must not reach
/// `Layout`'s constraint solver at all: an `area` too small to hold two
/// non-empty columns (fewer than two columns wide, or no rows at all — both
/// genuinely reachable, see this module's own doc comment on terminals
/// narrower than the sidebar) yields `None` for the second region rather
/// than a zero-width rect for a caller to accidentally attach and resize a
/// real PTY to.
pub fn pane_regions(area: Rect, split: bool) -> (Rect, Option<Rect>) {
    if !split || area.width < 2 || area.height == 0 {
        return (area, None);
    }
    let left_width = area.width / 2;
    let right_width = area.width - left_width;
    let left = Rect::new(area.x, area.y, left_width, area.height);
    // `area.x + left_width` cannot overflow: `left_width <= area.width` and
    // a `Rect`'s own invariant is that `x + width` fits in a `u16` (ratatui
    // clamps that in `Rect::new`). Written saturating anyway — `area` is a
    // parameter, and this function's contract is "never panics".
    let right = Rect::new(
        area.x.saturating_add(left_width),
        area.y,
        right_width,
        area.height,
    );
    (left, Some(right))
}

/// The regions [`Model::focused`]'s and [`Model::split`]'s panes occupy in
/// `area`, **in that order** — the single rule the renderer, `main.rs`'s
/// `slot_sizes`, and `main.rs`'s mouse hit-testing all consume, so no two of
/// them can disagree about which pane is which size or which half a click
/// landed in.
///
/// [`pane_regions`] supplies the geometry (left, right); [`Model::split_draws_left`]
/// decides which pane gets which, from `panes` order rather than from which
/// field an id happens to sit in. That is what keeps a focus change from
/// relocating both panes' contents — see that method's doc comment for the
/// full reasoning.
///
/// Whether the area is divided at all still depends on `model.split.is_some()`
/// alone, not on whether that pane has a grid yet: a split whose second pane
/// is still attaching must not reflow the first pane to full width for a
/// frame and back again, which would resize a remote PTY twice for nothing.
///
/// [`Model::focused`]: crate::model::Model::focused
/// [`Model::split`]: crate::model::Model::split
/// [`Model::split_draws_left`]: crate::model::Model::split_draws_left
pub fn visible_pane_regions(area: Rect, model: &Model) -> (Rect, Option<Rect>) {
    // The Files tab takes the area (its terminals keep their size, hidden),
    // or its left half with the focused terminal on the right.
    match model.shown_files().map(|(_, layout)| layout) {
        Some(crate::files::FilesLayout::Full) => return (area, None),
        Some(crate::files::FilesLayout::Split) => {
            let (left, right) = pane_regions(area, true);
            return (right.unwrap_or(left), None);
        }
        None => {}
    }
    let (left, right) = pane_regions(area, model.split.is_some());
    match right {
        // Unsplit: one region, and it is the focused pane's.
        None => (left, None),
        Some(right) if model.split_draws_left() => (right, Some(left)),
        Some(right) => (left, Some(right)),
    }
}

/// Composes the whole screen: sidebar, pane area (with the command-palette
/// overlay on top of it when open), and status bar.
///
/// The pane area shows the terminal grid of `model.focused`'s pane, looked
/// up in `terms`. If nothing is focused, or the focused pane has no entry
/// in `terms` (e.g. it hasn't been fed any output yet), the pane area is
/// simply left blank — never a panic or a lookup that silently substitutes
/// the wrong pane.
///
/// `palette` is drawn *last over the pane area* (see
/// [`super::palette::render_palette`]) rather than as a fourth layout
/// region: the palette is transient chrome, and reflowing the pane grid
/// every time it opens would resize the remote PTY on a keystroke. Taking
/// it as a parameter here matters — an earlier version of this function
/// had no `palette` argument at all, so `ctrl+k` opened a palette that
/// captured every keystroke while drawing nothing.
///
/// This is also the one place a terminal cursor is placed. ratatui hides
/// the cursor for any frame that doesn't call `set_cursor_position`, so
/// without this the user cannot see where they are typing — in the focused
/// pane, or in the palette's query line. Precedence: the palette's query
/// line while it's open (every keystroke goes there), otherwise the focused
/// pane's own cursor, and nothing at all when the client is `stale` or the
/// pane's socket is detached — in both of those cases keystrokes are
/// refused (`main.rs`'s `decide`), so advertising an input point would be a
/// lie.
/// Where the active space's Files tab is drawn, while it is shown.
pub fn files_region(area: Rect, model: &Model) -> Option<Rect> {
    match model.shown_files()?.1 {
        crate::files::FilesLayout::Full => Some(area),
        crate::files::FilesLayout::Split => Some(pane_regions(area, true).0),
    }
}

pub fn render(model: &Model, terms: &HashMap<String, PaneTerm>, palette: &Palette, f: &mut Frame) {
    let area = f.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let sidebar_width = model.sidebar.columns(area.width);
    let (sidebar_area, tab_area, pane_area, status_area) = regions(area, sidebar_width);

    render_sidebar(model, content_area(sidebar_area), f.buffer_mut());
    render_edge(edge(sidebar_area), f.buffer_mut());

    // Built here and drawn here from the same call — `main.rs`'s mouse arm
    // builds its own from the same `tab_bar_area(terminal.size())`, so the
    // two agree without either caching the other's rects.
    let bar = tab_bar(tab_area, model);
    render_tab_bar(model, &bar, tab_area, f.buffer_mut());

    let mut cursor = None;
    let has_pane_area = pane_area.width > 0 && pane_area.height > 0;
    if has_pane_area {
        // Which pane lands in which half is `visible_pane_regions`' call,
        // derived from `panes` order — so focusing the other pane swaps the
        // two model fields (and their sockets) without either pane's
        // *content* moving on screen.
        let (focused_area, split_area) = visible_pane_regions(pane_area, model);
        let files = model
            .shown_files()
            .and_then(|(space, layout)| Some((model.files.get(space)?, layout)));
        if let (Some((view, _)), Some(region)) = (files, files_region(pane_area, model)) {
            super::files::render_files(view, region, model.files_keys, f.buffer_mut());
        }
        let terminal_shown = !matches!(files, Some((_, crate::files::FilesLayout::Full)));
        if let Some(pane_term) = model
            .focused
            .as_deref()
            .and_then(|id| terms.get(id))
            .filter(|_| terminal_shown)
        {
            pane_term.render_into(focused_area, f.buffer_mut());
            if !model.local().stale && !model.detached_pane && !model.files_have_keys() {
                cursor = pane_cursor(focused_area, pane_term);
            }
        }
        // The split pane is drawn but never given the cursor: keystrokes go
        // to `model.focused` only (`main.rs`'s key arm), so advertising an
        // input point in the other half would be a lie — the same reasoning
        // that hides the cursor entirely while `stale`/`detached_pane`.
        if let Some(split_area) = split_area {
            if let Some(pane_term) = model.split.as_deref().and_then(|id| terms.get(id)) {
                pane_term.render_into(split_area, f.buffer_mut());
            }
        }
        // After both grids, so the highlight lands on top of whatever they
        // drew rather than being overwritten by the other half.
        if let Some(sel) = palette.selection.as_ref() {
            render_selection(sel, f.buffer_mut());
        }
        // Over the panes and their selection, under the palette and the
        // menu: a toast is news about another pane, not a modal.
        render_toasts(model, pane_area, f.buffer_mut());
    }

    // A terminal narrower than the sidebar leaves *no* pane area (see
    // `regions`), and the palette still has to appear somewhere — drawing
    // it only over the pane region would put `ctrl+k` right back to being
    // an invisible keystroke sink on exactly the terminals where the user
    // has the least idea what is going on.
    // `palette_area(area)`, not a local re-derivation: `main.rs`'s mouse arm
    // calls that same function to hit-test a click on a candidate row, and a
    // second copy of this choice here is exactly how the two would drift.
    let overlay_area = palette_area(area, sidebar_width);
    debug_assert_eq!(overlay_area, if has_pane_area { pane_area } else { area });
    let overlay_cursor = render_palette(model, palette, overlay_area, f.buffer_mut());
    if palette.open {
        cursor = overlay_cursor;
    }

    // Last of the overlays, and over the whole screen rather than the pane
    // area: the menu is anchored to a *tab*, and a tab near the sidebar edge
    // would otherwise have its menu clipped away. `menu_layout` pushes it back
    // inside whatever area it is given, so passing the full frame is what lets
    // it always land somewhere visible.
    if let Some(menu) = palette.tab_menu.as_ref() {
        render_menu(model, menu, area, f.buffer_mut());
    }

    render_status_bar(model, status_area, f.buffer_mut());

    if let Some(position) = cursor {
        f.set_cursor_position(position);
    }
}

/// Draws the sidebar's draggable edge: a dim `│` down the whole column. The
/// visible line is the affordance — a handle you cannot see is one nobody
/// drags.
fn render_edge(area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    if area.width == 0 {
        return;
    }
    for row in area.y..area.y.saturating_add(area.height) {
        buf[(area.x, row)]
            .set_symbol("\u{2502}")
            .set_style(Style::new().fg(Color::DarkGray));
    }
}

/// Inverts every cell the selection covers.
///
/// Reversed video rather than a background colour: the pane underneath is
/// arbitrary agent output in arbitrary colours, and a fixed highlight colour
/// would be invisible against some of it. Reversing whatever is already there
/// is legible over all of it, and is what terminals do for their own
/// selection.
///
/// Clipped to `buf` as well as to the selection's own region — the region was
/// captured when the drag began and the terminal may have been resized since,
/// so it is not safe to index the buffer with on its own.
fn render_selection(sel: &super::selection::Selection, buf: &mut Buffer) {
    let area = sel.region.intersection(buf.area);
    for row in area.y..area.y.saturating_add(area.height) {
        for column in area.x..area.x.saturating_add(area.width) {
            if sel.contains(column, row) {
                buf[(column, row)].set_style(Style::new().add_modifier(Modifier::REVERSED));
            }
        }
    }
}

/// Translates `pane_term`'s own cursor position into `area`'s coordinates,
/// or `None` if the pane hides its cursor or reports one outside the
/// visible area (`render_into` clips the grid to `area`, so a cursor past
/// that edge has no cell on screen to sit on).
fn pane_cursor(area: Rect, pane_term: &PaneTerm) -> Option<Position> {
    let (col, row) = pane_term.cursor_position()?;
    if col >= area.width || row >= area.height {
        return None;
    }
    Some(Position {
        x: area.x.checked_add(col)?,
        y: area.y.checked_add(row)?,
    })
}

/// Draws the one-line status bar: pane-state tallies on the left, the
/// palette hint right-aligned, and one notice folded into the left side.
///
/// Exactly one notice is ever shown, most-informative-first:
///
/// 1. `RECONNECTING` while [`Model::stale`] — the whole client is
///    disconnected, which subsumes anything a single pane could be saying.
/// 2. [`Model::notice`] — most recently either pane socket's own error
///    frame (`unauthorized`/`not_found`/`conflict`), a refused split attach,
///    or a rejected `workspace.create` (see that field's own doc comment for
///    the shared-slot tradeoff) — strictly more useful than
///    `PANE_DISCONNECTED`, which only restates the *consequence* of the
///    pane-error case. Read through [`Model::notice_text`]: exactly one
///    notice is ever shown and it is drawn the same way whichever domain
///    wrote it, so this is the one reader that deliberately ignores the
///    slot's [`crate::model::NoticeOrigin`].
/// 3. `PANE_DISCONNECTED` when a focused pane has no live socket and the
///    server never said why.
fn render_status_bar(model: &Model, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let (blocked, working, done) = model.counts();
    let tally = format!("{blocked} blocked \u{b7} {working} working \u{b7} {done} done");
    let left = if model.local().stale {
        format!("{RECONNECTING} \u{b7} {tally}")
    } else if let Some(notice) = model.notice_text() {
        format!("{notice} \u{b7} {tally}")
    } else if model.focused.is_some() && model.detached_pane {
        format!("{PANE_DISCONNECTED} \u{b7} {tally}")
    } else {
        tally
    };
    // What is running, and where: see `brand`. Then the tally after a gap.
    let (badge, used) = super::brand::badge(&model.local().node_id, area.width / 2);
    let text_x = if used > 0 { used + 1 } else { 0 };
    ratatui::text::Line::from(badge).render(Rect::new(area.x, area.y, used, area.height), buf);
    let text_area = Rect::new(
        area.x + text_x.min(area.width),
        area.y,
        area.width.saturating_sub(text_x),
        area.height,
    );
    Paragraph::new(left).render(text_area, buf);

    let hint = if model.mouse_captured {
        PALETTE_HINT
    } else {
        SELECT_MODE_HINT
    };
    let hint_width = hint.chars().count() as u16;
    if let Some(hint_x) = area
        .width
        .checked_sub(hint_width)
        .and_then(|offset| area.x.checked_add(offset))
    {
        let hint_area = Rect::new(hint_x, area.y, hint_width, area.height);
        Paragraph::new(hint).right_aligned().render(hint_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NoticeOrigin;
    use crate::ui::sidebar::WIDTH as SIDEBAR_WIDTH;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use spaces_proto::{AgentState, PaneInfo, WorkspaceInfo};

    /// `Model::new`, not `Model::default`: a real session has mouse reporting
    /// on, and the status bar's right-hand hint depends on it.
    fn model_with(panes: Vec<PaneInfo>, workspaces: Vec<WorkspaceInfo>) -> Model {
        let mut model = Model::new();
        model.local_mut().panes = panes;
        model.local_mut().workspaces = workspaces;
        model
    }

    fn pane_named(
        id: &str,
        workspace: &str,
        agent_type: &str,
        state: AgentState,
        seen: bool,
    ) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(workspace.into()),
            agent_type: agent_type.into(),
            agent_state: state,
            seen,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        }
    }

    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rendered(model: &Model, terms: &HashMap<String, PaneTerm>) -> String {
        rendered_with(model, terms, &Palette::default())
    }

    fn rendered_with(
        model: &Model,
        terms: &HashMap<String, PaneTerm>,
        palette: &Palette,
    ) -> String {
        let backend = TestBackend::new(80, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(model, terms, palette, f)).unwrap();
        buffer_text(term.backend().buffer())
    }

    /// Where the backend's cursor ended up after one frame. `TestBackend`
    /// keeps its visibility flag private, so "the frame hid the cursor" is
    /// not directly observable here — it shows up instead as the position
    /// simply not moving (ratatui only calls `set_cursor_position` for a
    /// frame that asked for one). The hidden cases are pinned against
    /// [`pane_cursor`] and `PaneTerm::cursor_position` directly, which is
    /// where the decision actually lives.
    fn cursor_after_render(
        model: &Model,
        terms: &HashMap<String, PaneTerm>,
        palette: &Palette,
    ) -> (u16, u16) {
        let backend = TestBackend::new(80, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(model, terms, palette, f)).unwrap();
        let position = term.get_cursor_position().expect("test backend position");
        (position.x, position.y)
    }

    #[test]
    fn the_status_bar_tallies_states() {
        let m = model_with(
            vec![
                pane_named("a", "w1", "claude", AgentState::Blocked, true),
                pane_named("b", "w1", "codex", AgentState::Working, true),
                pane_named("c", "w1", "aider", AgentState::Idle, false),
            ],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "w".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        let out = rendered(&m, &HashMap::new());
        assert!(out.contains("1 blocked"), "{out}");
        assert!(out.contains("1 working"), "{out}");
        assert!(out.contains("1 done"), "{out}");
    }

    #[test]
    fn a_stale_model_advertises_reconnecting() {
        let mut m = model_with(vec![], vec![]);
        m.local_mut().stale = true;
        assert!(rendered(&m, &HashMap::new()).contains("RECONNECTING"));
    }

    #[test]
    fn a_detached_focused_pane_advertises_pane_disconnected() {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        m.detached_pane = true;
        assert!(rendered(&m, &HashMap::new()).contains("PANE DISCONNECTED"));
    }

    #[test]
    fn detached_pane_with_nothing_focused_shows_no_notice() {
        // `detached_pane` is meaningless without a `focused` pane to be
        // detached from — must not show a notice for state that can't
        // actually arise from `main.rs`'s own logic, but is still
        // representable here.
        let mut m = model_with(vec![], vec![]);
        m.detached_pane = true;
        assert!(!rendered(&m, &HashMap::new()).contains("PANE DISCONNECTED"));
    }

    #[test]
    fn stale_takes_priority_over_pane_disconnected() {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        m.detached_pane = true;
        m.local_mut().stale = true;
        let out = rendered(&m, &HashMap::new());
        assert!(out.contains("RECONNECTING"), "{out}");
        assert!(!out.contains("PANE DISCONNECTED"), "{out}");
    }

    #[test]
    fn a_pane_error_notice_is_shown_instead_of_the_generic_disconnected_text() {
        // `unauthorized`/`not_found`/`conflict` arrive only on the pane socket
        // and have no control-channel counterpart, so this line is the only
        // place the user can learn why the pane never comes back.
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        m.detached_pane = true;
        m.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (unauthorized): token expired".into(),
        );
        let out = rendered(&m, &HashMap::new());
        assert!(out.contains("PANE ERROR (unauthorized)"), "{out}");
        assert!(
            !out.contains(PANE_DISCONNECTED),
            "the specific reason replaces the generic notice: {out}"
        );
        assert!(out.contains("0 blocked"), "the tally is still there: {out}");
    }

    #[test]
    fn reconnecting_still_outranks_a_pane_error_notice() {
        let mut m = model_with(vec![], vec![]);
        m.local_mut().stale = true;
        m.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (conflict): taken over".into(),
        );
        let out = rendered(&m, &HashMap::new());
        assert!(out.contains(RECONNECTING), "{out}");
        assert!(!out.contains("PANE ERROR"), "{out}");
    }

    // --- The tab bar's row, and what it costs the pane area. ---

    #[test]
    fn the_four_regions_tile_the_screen_without_overlapping() {
        let area = Rect::new(0, 0, 80, 24);
        let (sidebar, tabs, panes, status) = regions(area, SIDEBAR_WIDTH);
        assert_eq!(sidebar.x, 0);
        assert_eq!(sidebar.width, SIDEBAR_WIDTH);
        assert_eq!(tabs.x, SIDEBAR_WIDTH, "the bar belongs to the pane column");
        assert_eq!(tabs.width, area.width - SIDEBAR_WIDTH);
        assert_eq!(tabs.height, TAB_BAR_HEIGHT);
        assert_eq!(panes.x, tabs.x, "and the panes sit directly under it");
        assert_eq!(panes.y, tabs.y + TAB_BAR_HEIGHT);
        assert_eq!(panes.y + panes.height, status.y);
        assert_eq!(status.y + status.height, area.height);
        assert_eq!(
            sidebar.height,
            panes.height + TAB_BAR_HEIGHT,
            "the sidebar keeps the row the tab bar took from the panes"
        );
    }

    #[test]
    fn the_regions_follow_the_width_they_are_given() {
        let area = Rect::new(0, 0, 100, 24);
        let (sidebar, tabs, panes, _) = regions(area, 41);
        assert_eq!(sidebar.width, 41);
        assert_eq!(tabs.x, 41);
        assert_eq!(panes.width, 59);
        let (sidebar, tabs, _, _) = regions(area, 0);
        assert_eq!(sidebar.width, 0, "no sidebar at all");
        assert_eq!(tabs.x, 0);
        assert_eq!(tabs.width, 100);
    }

    #[test]
    fn the_sidebar_draws_a_draggable_edge_down_its_last_column() {
        let mut m = model_with(vec![], vec![]);
        m.sidebar.width = 34;
        let out = rendered(&m, &HashMap::new());
        for (row, line) in out.lines().take(11).enumerate() {
            assert_eq!(
                line.chars().nth(33),
                Some('\u{2502}'),
                "row {row}: {line:?}"
            );
        }
        assert_eq!(pane_area(Rect::new(0, 0, 80, 12), 34).x, 34);
    }

    #[test]
    fn the_tab_bar_lists_the_active_spaces_panes() {
        let m = model_with(
            vec![
                pane_named("p1", "w1", "claude", AgentState::Working, true),
                pane_named("p2", "w1", "codex", AgentState::Idle, true),
            ],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "api".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        let out = rendered(&m, &HashMap::new());
        let bar = out.lines().next().unwrap_or_default();
        assert!(bar.contains("claude"), "{out}");
        assert!(bar.contains("codex"), "{out}");
        assert!(bar.contains('+'), "and the button that adds another: {out}");
    }

    #[test]
    fn a_terminal_too_narrow_for_a_pane_column_has_no_tab_bar_either() {
        let (_, tabs, panes, _) = regions(Rect::new(0, 0, SIDEBAR_WIDTH, 10), SIDEBAR_WIDTH);
        assert_eq!(tabs.width, 0);
        assert_eq!(panes.width, 0);
    }

    #[test]
    fn a_narrow_terminal_hides_the_sidebar_and_keeps_the_tab_bar() {
        let mut m = model_with(
            vec![pane_named("a", "w1", "claude", AgentState::Working, true)],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "api".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        m.focused = Some("a".into());
        m.sidebar.note_screen_width(60);
        let backend = TestBackend::new(60, 12);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(&m, &HashMap::new(), &Palette::default(), f))
            .unwrap();
        let out = buffer_text(term.backend().buffer());
        // Above the status bar, whose badge names the client.
        let lines: Vec<&str> = out.lines().collect();
        let body = lines[..lines.len() - 1].join(
            "
",
        );
        assert!(!body.contains("spaces"), "no sidebar: {out}");
        let first = out.lines().next().unwrap_or_default();
        assert!(
            first.contains("claude"),
            "the tab bar is still there: {out}"
        );
        assert!(!first.contains('\u{2502}'), "and so is no edge: {out}");
    }

    #[test]
    fn the_status_bar_shows_the_palette_hint() {
        let m = model_with(vec![], vec![]);
        assert!(rendered(&m, &HashMap::new()).contains("ctrl+k palette"));
    }

    #[test]
    fn select_mode_replaces_the_palette_hint_and_says_how_to_leave() {
        // Not folded in with the notices on the left: this is the mode you are
        // in, not a problem report, and it must not be outranked by
        // `RECONNECTING` — which is exactly when a user wants to copy the
        // error text out.
        let mut m = model_with(vec![], vec![]);
        m.mouse_captured = false;
        m.local_mut().stale = true;
        let out = rendered(&m, &HashMap::new());
        assert!(out.contains("SELECT MODE"), "{out}");
        assert!(out.contains("ctrl+space m"), "the way back: {out}");
        assert!(
            !out.contains("ctrl+k palette"),
            "the palette is not clickable in this mode: {out}"
        );
        assert!(
            out.contains(RECONNECTING),
            "the tally side still works: {out}"
        );
    }

    #[test]
    fn rendering_a_tiny_terminal_does_not_panic() {
        let m = model_with(
            vec![pane_named("p", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        let backend = TestBackend::new(4, 2);
        let mut term = Terminal::new(backend).unwrap();
        let terms = HashMap::new();
        term.draw(|f| render(&m, &terms, &Palette::default(), f))
            .unwrap();
    }

    #[test]
    fn the_focused_panes_terminal_content_fills_the_pane_area() {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "frontend".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        m.focused = Some("p1".into());
        let mut t = PaneTerm::new(40, 10);
        t.feed(b"distinctive-pane-output");
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), t);
        let out = rendered(&m, &terms);
        assert!(out.contains("distinctive-pane-output"), "{out}");
    }

    #[test]
    fn no_focused_pane_leaves_the_pane_area_blank_without_panicking() {
        let m = model_with(vec![], vec![]);
        let mut t = PaneTerm::new(40, 10);
        t.feed(b"should-not-appear");
        let mut terms = HashMap::new();
        terms.insert("unfocused".to_string(), t);
        let out = rendered(&m, &terms);
        assert!(!out.contains("should-not-appear"), "{out}");
    }

    // --- The command-palette overlay (an earlier `render` took no
    // `Palette` at all and nothing anywhere drew one, so `ctrl+k` silently
    // swallowed every keystroke while showing nothing). ---

    fn open_palette(query: &str, selected: usize) -> Palette {
        Palette {
            open: true,
            query: query.to_string(),
            selected,
            ..Palette::default()
        }
    }

    #[test]
    fn an_open_palette_renders_its_candidates() {
        let m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "frontend".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        // Tall enough for the whole list: an 80x12 terminal leaves a 10-row
        // pane area, which after the borders and the query line draws only
        // seven candidates — so asserting on the *last* entry there would
        // fail the next time the list grows, for a reason that has nothing to
        // do with whether candidates render.
        let terms = HashMap::new();
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(&m, &terms, &open_palette("", 0), f))
            .unwrap();
        let out = buffer_text(term.backend().buffer());
        assert!(out.contains("Focus claude"), "{out}");
        assert!(out.contains("New pane in frontend"), "{out}");
        assert!(out.contains("Detach"), "{out}");
    }

    #[test]
    fn an_open_palette_marks_the_selection() {
        let m = model_with(vec![], vec![]);
        // candidates for an empty model are [Next pane, Previous pane,
        // Detach]; index 1 is "Previous pane".
        let out = rendered_with(&m, &HashMap::new(), &open_palette("", 1));
        let marked: Vec<&str> = out
            .lines()
            .filter(|line| line.contains('\u{25b8}'))
            .collect();
        assert_eq!(marked.len(), 1, "exactly one row is marked: {out}");
        assert!(marked[0].contains("Previous pane"), "{out}");
    }

    #[test]
    fn an_open_palette_shows_the_query_being_typed() {
        let m = model_with(vec![], vec![]);
        let out = rendered_with(&m, &HashMap::new(), &open_palette("det", 0));
        assert!(out.contains("> det"), "{out}");
        assert!(out.contains("Detach"), "{out}");
        assert!(
            !out.contains("Next pane"),
            "the query must filter the drawn list too: {out}"
        );
    }

    #[test]
    fn a_closed_palette_draws_nothing_over_the_pane_area() {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        let mut t = PaneTerm::new(40, 10);
        t.feed(b"pane-output-must-stay-visible");
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), t);
        let out = rendered_with(&m, &terms, &Palette::default());
        assert!(out.contains("pane-output-must-stay-visible"), "{out}");
        assert!(!out.contains("Detach"), "{out}");
        // `\u{25b8}` (▸) used to double as the sidebar's own focus
        // indicator, which made a whole-output search for the glyph an
        // invalid proxy for "the palette selection marker leaked" — with
        // `p1` focused it would legitimately mark that pane's row too. That
        // was resolved by moving sidebar focus to styling (bold) instead of
        // a glyph (see `ui/sidebar.rs`'s module docs), so `▸` is once again
        // exclusively the palette's own marker and a plain whole-output
        // check is valid again.
        assert!(
            !out.contains('\u{25b8}'),
            "no palette selection marker should be drawn while the palette is closed: {out}"
        );
    }

    #[test]
    fn the_open_palette_overlay_covers_the_pane_output_underneath_it() {
        // The overlay is opaque (Clear): a busy pane must not bleed through
        // the candidate list.
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        let mut t = PaneTerm::new(60, 10);
        for _ in 0..10 {
            t.feed(b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n");
        }
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), t);
        let out = rendered_with(&m, &terms, &open_palette("", 0));
        assert!(out.contains("Detach"), "{out}");
        let overlay_line = out
            .lines()
            .find(|line| line.contains("Detach"))
            .unwrap_or_default();
        assert!(
            !overlay_line.contains("xxxx"),
            "pane output bled through the overlay: {out}"
        );
    }

    #[test]
    fn a_short_terminal_never_marks_a_candidate_it_clipped_off_the_screen() {
        // At the size the bug was reported at: 13 terminal rows. The
        // status bar takes one, leaving a 12-row pane area, so
        // the box has nine candidate rows — one fewer than the ten
        // `MAX_VISIBLE` used to assume. `enter` resolves against the same
        // `candidates()` list this draws from, so a marker the user cannot
        // see is an action they did not choose.
        let m = model_with(
            (0..8)
                .map(|i| {
                    pane_named(
                        &format!("p{i}"),
                        "w1",
                        &format!("agent{i}"),
                        AgentState::Working,
                        true,
                    )
                })
                .collect(),
            vec![],
        );
        let terms = HashMap::new();
        let backend = TestBackend::new(80, 13);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(&m, &terms, &open_palette("", 9), f))
            .unwrap();
        let out = buffer_text(term.backend().buffer());
        assert_eq!(
            out.lines().filter(|l| l.contains('\u{25b8}')).count(),
            1,
            "the selected candidate must be on screen: {out}"
        );
    }

    #[test]
    fn an_open_palette_is_still_visible_when_the_terminal_is_narrower_than_the_sidebar() {
        // At this width `regions` gives the pane area zero columns, so an
        // overlay scoped strictly to the pane area would draw nothing —
        // and ctrl+k would once again swallow keystrokes invisibly.
        let m = model_with(vec![], vec![]);
        let terms = HashMap::new();
        let backend = TestBackend::new(SIDEBAR_WIDTH - 8, 10);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(&m, &terms, &open_palette("det", 0), f))
            .unwrap();
        assert!(buffer_text(term.backend().buffer()).contains("Detach"));
    }

    #[test]
    fn an_open_palette_on_a_tiny_terminal_does_not_panic() {
        let m = model_with(
            vec![pane_named("p", "w1", "claude", AgentState::Working, true)],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "w".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        let terms = HashMap::new();
        let palette = open_palette("cla", usize::MAX);
        for (w, h) in [(4, 2), (1, 1), (30, 3), (29, 2), (80, 1)] {
            let backend = TestBackend::new(w, h);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| render(&m, &terms, &palette, f)).unwrap();
        }
    }

    // --- The pane cursor (once, nothing ever called `set_cursor_position`,
    // so ratatui hid the cursor for the whole session — in a client whose
    // purpose is typing at an agent). ---

    #[test]
    fn the_focused_panes_cursor_is_placed_inside_the_pane_area() {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        let mut t = PaneTerm::new(40, 10);
        t.feed(b"abc");
        let (col, row) = t.cursor_position().expect("cursor visible");
        assert_eq!((col, row), (3, 0), "three characters in, still on row 0");
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), t);
        // The pane area starts one sidebar-width in and one tab-bar row
        // down, so the reported cursor must be offset by both — proving the
        // translation, not just that *some* position was set.
        assert_eq!(
            cursor_after_render(&m, &terms, &Palette::default()),
            (SIDEBAR_WIDTH + 3, TAB_BAR_HEIGHT)
        );
    }

    #[test]
    fn an_open_palette_puts_the_cursor_on_its_query_line() {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![],
        );
        m.focused = Some("p1".into());
        let mut t = PaneTerm::new(40, 10);
        t.feed(b"abc");
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), t);
        let (x, y) = cursor_after_render(&m, &terms, &open_palette("de", 0));
        // Not the pane's own cursor row/column: the overlay is centered and
        // its query line sits inside a border.
        assert_ne!((x, y), (SIDEBAR_WIDTH + 3, 0));
        let out = rendered_with(&m, &terms, &open_palette("de", 0));
        let query_row = out
            .lines()
            .position(|line| line.contains("> de"))
            .expect("query line drawn");
        assert_eq!(usize::from(y), query_row);
    }

    // --- Split panes: the pure geometry. ---

    #[test]
    fn an_unsplit_pane_area_is_undivided() {
        let (a, b) = pane_regions(Rect::new(0, 0, 80, 20), false);
        assert_eq!(a, Rect::new(0, 0, 80, 20));
        assert!(b.is_none());
    }

    #[test]
    fn a_split_divides_the_width_in_two() {
        let (a, b) = pane_regions(Rect::new(0, 0, 80, 20), true);
        let b = b.expect("a split must yield a second region");
        assert_eq!(a.width + b.width, 80, "no columns lost");
        assert!(a.width.abs_diff(b.width) <= 1, "roughly even: {a:?} {b:?}");
        assert_eq!(a.y, b.y);
    }

    #[test]
    fn a_degenerate_area_does_not_panic_or_produce_a_bogus_second_region() {
        let (a, b) = pane_regions(Rect::new(0, 0, 0, 0), true);
        assert_eq!(a.width, 0);
        assert!(b.is_none_or(|r| r.width == 0));
        let (_, b1) = pane_regions(Rect::new(0, 0, 1, 1), true);
        assert!(
            b1.is_none_or(|r| r.width == 0),
            "1 column cannot split into two"
        );
    }

    #[test]
    fn the_two_split_regions_abut_inside_the_area_they_were_given() {
        // Not anchored at the origin: the real pane area starts one
        // sidebar-width in, and an off-by-one in the second region's `x`
        // would draw the right-hand pane over the left one's last column.
        let area = Rect::new(SIDEBAR_WIDTH, 0, 51, 20);
        let (a, b) = pane_regions(area, true);
        let b = b.expect("a split must yield a second region");
        assert_eq!(a.x, area.x);
        assert_eq!(b.x, a.x + a.width, "no gap and no overlap between them");
        assert_eq!(a.width + b.width, area.width, "no columns lost");
        assert_eq!((a.height, b.height), (area.height, area.height));
        assert_eq!(b.x + b.width, area.x + area.width, "nothing past the edge");
    }

    #[test]
    fn an_odd_width_gives_the_extra_column_to_one_side_and_loses_none() {
        let (a, b) = pane_regions(Rect::new(0, 0, 51, 4), true);
        let b = b.expect("a split must yield a second region");
        assert_eq!(a.width + b.width, 51);
        assert!(a.width.abs_diff(b.width) <= 1, "{a:?} {b:?}");
    }

    #[test]
    fn an_area_with_no_rows_yields_no_second_region() {
        // `regions` hands back a zero-height pane area on a terminal that is
        // all sidebar and status bar; splitting nothing in two is nothing.
        let (a, b) = pane_regions(Rect::new(0, 0, 80, 0), true);
        assert_eq!(a.height, 0);
        assert!(b.is_none());
    }

    #[test]
    fn a_two_column_area_is_the_narrowest_that_still_splits() {
        let (a, b) = pane_regions(Rect::new(0, 0, 2, 4), true);
        let b = b.expect("two columns is one each");
        assert_eq!((a.width, b.width), (1, 1));
    }

    // --- Split panes: what gets drawn. ---

    /// A model with `focused` and `split` both set, and a `terms` entry for
    /// each carrying text unique to that pane.
    fn split_model_and_terms() -> (Model, HashMap<String, PaneTerm>) {
        let mut m = model_with(
            vec![
                pane_named("p1", "w1", "claude", AgentState::Working, true),
                pane_named("p2", "w1", "codex", AgentState::Working, true),
            ],
            vec![],
        );
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let mut left = PaneTerm::new(20, 8);
        left.feed(b"LEFTPANE");
        let mut right = PaneTerm::new(20, 8);
        right.feed(b"RIGHTPANE");
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), left);
        terms.insert("p2".to_string(), right);
        (m, terms)
    }

    #[test]
    fn a_split_draws_both_panes_on_the_same_rows() {
        let (m, terms) = split_model_and_terms();
        let out = rendered(&m, &terms);
        let row = out
            .lines()
            .find(|line| line.contains("LEFTPANE"))
            .unwrap_or_default();
        assert!(
            row.contains("RIGHTPANE"),
            "both panes must be visible at once, on the same row: {out}"
        );
        let left_at = row.find("LEFTPANE").unwrap_or_default();
        let right_at = row.find("RIGHTPANE").unwrap_or_default();
        assert!(
            left_at < right_at,
            "the focused pane occupies the left half: {row:?}"
        );
        assert!(
            right_at >= usize::from(SIDEBAR_WIDTH) + row.len() / 4,
            "the second pane starts in the right half, not next to the first: {row:?}"
        );
    }

    // --- A focus change swaps the two model fields (and their sockets)
    // without either pane's content moving. ---

    #[test]
    fn focusing_the_other_pane_does_not_move_either_panes_content() {
        // The defect this fix exists for: clicking the right-hand pane used
        // to relocate both panes' full contents in one frame.
        let (before, terms) = split_model_and_terms();
        let before = rendered(&before, &terms);

        let (mut after, _) = split_model_and_terms();
        after.focused = Some("p2".into());
        after.split = Some("p1".into());
        let after = rendered(&after, &terms);

        let row_of = |out: &str, needle: &str| {
            out.lines()
                .find(|line| line.contains(needle))
                .unwrap_or_default()
                .find(needle)
        };
        assert_eq!(
            row_of(&before, "LEFTPANE"),
            row_of(&after, "LEFTPANE"),
            "p1 must stay in the same columns after p2 is focused"
        );
        assert_eq!(
            row_of(&before, "RIGHTPANE"),
            row_of(&after, "RIGHTPANE"),
            "and so must p2"
        );
    }

    #[test]
    fn the_cursor_follows_focus_to_the_right_hand_pane() {
        // The other half of the same behaviour: the panes hold still, and
        // the *indicator* moves — which is what every other multiplexer
        // does, and what this client's cursor and sidebar glyph are for.
        let (mut m, terms) = split_model_and_terms();
        m.focused = Some("p2".into());
        m.split = Some("p1".into());
        let (x, _) = cursor_after_render(&m, &terms, &Palette::default());
        let (left, right) = pane_regions(pane_area(Rect::new(0, 0, 80, 12), SIDEBAR_WIDTH), true);
        let right = right.expect("split");
        assert!(
            x >= right.x && x < right.x + right.width,
            "the cursor must be in the right-hand region ({right:?}), not at {x}"
        );
        assert!(x >= left.x + left.width);
    }

    #[test]
    fn visible_regions_hand_back_the_focused_panes_region_first() {
        let area = Rect::new(SIDEBAR_WIDTH, 0, 52, 11);
        let (m, _) = split_model_and_terms();
        let (focused, split) = visible_pane_regions(area, &m);
        assert_eq!(
            focused.x, area.x,
            "p1 is earlier, so focused is on the left"
        );
        assert_eq!(split.expect("split region").x, area.x + focused.width);

        let mut swapped = m.clone();
        swapped.focused = Some("p2".into());
        swapped.split = Some("p1".into());
        let (focused, split) = visible_pane_regions(area, &swapped);
        assert_eq!(
            split.expect("split region").x,
            area.x,
            "p1 is still on the left, it is just the split slot now"
        );
        assert_eq!(focused.x, area.x + area.width - focused.width);
    }

    #[test]
    fn visible_regions_on_an_unsplit_model_are_the_whole_area_and_none() {
        let area = Rect::new(SIDEBAR_WIDTH, 0, 52, 11);
        let (mut m, _) = split_model_and_terms();
        m.split = None;
        assert_eq!(visible_pane_regions(area, &m), (area, None));
    }

    #[test]
    fn an_unsplit_model_draws_only_the_focused_pane() {
        let (mut m, terms) = split_model_and_terms();
        m.split = None;
        let out = rendered(&m, &terms);
        assert!(out.contains("LEFTPANE"), "{out}");
        assert!(
            !out.contains("RIGHTPANE"),
            "with no split there is nowhere to draw a second pane: {out}"
        );
    }

    #[test]
    fn a_split_naming_a_pane_with_no_grid_yet_still_draws_the_focused_one() {
        let (mut m, mut terms) = split_model_and_terms();
        terms.remove("p2");
        m.split = Some("p2".into());
        let out = rendered(&m, &terms);
        assert!(out.contains("LEFTPANE"), "{out}");
    }

    #[test]
    fn the_cursor_stays_in_the_focused_panes_own_half_while_split() {
        let (m, terms) = split_model_and_terms();
        let (x, y) = cursor_after_render(&m, &terms, &Palette::default());
        // "LEFTPANE" is eight characters, so the focused pane's cursor sits
        // at column 8 of its own region — which starts at the sidebar edge,
        // one row below the tab bar.
        assert_eq!((x, y), (SIDEBAR_WIDTH + 8, TAB_BAR_HEIGHT));
    }

    #[test]
    fn a_split_on_a_tiny_terminal_does_not_panic() {
        let (m, terms) = split_model_and_terms();
        for (w, h) in [(4, 2), (1, 1), (29, 3), (30, 2), (80, 1), (0, 0)] {
            let backend = TestBackend::new(w.max(1), h.max(1));
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| render(&m, &terms, &Palette::default(), f))
                .unwrap();
        }
    }

    #[test]
    fn an_open_palette_still_covers_both_halves_of_a_split() {
        // The overlay is centered over the *whole* pane area, not one half —
        // a candidate list bleeding pane output through it is the same
        // defect whether one pane or two are underneath.
        let (m, terms) = split_model_and_terms();
        let out = rendered_with(&m, &terms, &open_palette("", 0));
        let overlay_line = out
            .lines()
            .find(|line| line.contains("Detach"))
            .unwrap_or_default();
        assert!(!overlay_line.contains("LEFTPANE"), "{out}");
        assert!(!overlay_line.contains("RIGHTPANE"), "{out}");
    }

    #[test]
    fn pane_cursor_is_none_outside_the_visible_area() {
        let mut t = PaneTerm::new(40, 10);
        t.feed(b"abcdefghij");
        // A pane area narrower than the cursor's column has no cell to put
        // it on — better no cursor than one drawn over the sidebar.
        assert_eq!(pane_cursor(Rect::new(28, 0, 4, 4), &t), None);
        assert_eq!(
            pane_cursor(Rect::new(28, 0, 40, 4), &t),
            Some(Position { x: 38, y: 0 })
        );
    }

    #[test]
    fn a_hidden_pane_cursor_places_nothing() {
        let mut t = PaneTerm::new(40, 10);
        // DECTCEM off — full-screen programs do this while redrawing.
        t.feed(b"hi\x1b[?25l");
        assert_eq!(t.cursor_position(), None);
        assert_eq!(pane_cursor(Rect::new(28, 0, 40, 10), &t), None);
    }

    // --- Clicking what was drawn. Both overlays are laid out by one function
    // and hit-tested by another, and for a while those two disagreed: the
    // renderer placed the context menu against the whole frame while the
    // router clamped it into the pane column, so every click on a space menu
    // missed and the menu merely closed. Unit tests on either side passed —
    // they each used a self-consistent rect. This renders a real frame, finds
    // a row *in the drawn buffer*, and clicks that screen cell through the
    // production router, which is the only thing that could have caught it. ---

    /// Renders `model`/`palette` into an 80x20 frame and returns the buffer as
    /// text plus the screen rect it was drawn into.
    fn drawn_frame(model: &Model, palette: &Palette) -> (String, Rect) {
        let screen = Rect::new(0, 0, 80, 20);
        let terms = HashMap::new();
        let backend = TestBackend::new(screen.width, screen.height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render(model, &terms, palette, f)).unwrap();
        (buffer_text(term.backend().buffer()), screen)
    }

    /// Where `needle` starts in `drawn`, as a screen `(column, row)`.
    ///
    /// Counted in **characters**, not bytes. A rendered row is full of
    /// multi-byte glyphs — box drawing, the state icons — so `str::find`'s
    /// byte offset is not a terminal column, and using it reported column 92
    /// on an 80-wide frame. One cell is one character here because every
    /// glyph this client draws is single-width; a wide CJK glyph would need
    /// display width instead, and nothing draws one yet.
    fn cell_of(drawn: &str, needle: &str) -> (u16, u16) {
        let row = drawn
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} was never drawn:\n{drawn}"));
        let line = drawn.lines().nth(row).unwrap();
        let byte = line.find(needle).unwrap();
        let column = line[..byte].chars().count();
        (column as u16, row as u16)
    }

    fn menu_model() -> Model {
        let mut m = model_with(
            vec![pane_named("p1", "w1", "claude", AgentState::Working, true)],
            vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "frontend".into(),
                color: None,
                pane_ids: vec![],
            }],
        );
        m.focused = Some("p1".into());
        m
    }

    fn space_menu_open() -> Palette {
        Palette {
            tab_menu: Some(crate::ui::menu::TabMenu {
                target: crate::ui::menu::MenuTarget::Space("w1".into()),
                // In the sidebar, which is the whole point: an overlay rect
                // clamped to the pane column puts this menu somewhere else.
                column: 3,
                row: 1,
            }),
            ..Palette::default()
        }
    }

    /// Clicks the screen cell `needle` was drawn at, through the production
    /// router with the same argument `main.rs` passes it.
    fn click_drawn(model: &mut Model, palette: &mut Palette, needle: &str) -> crate::input::Action {
        let (drawn, screen) = drawn_frame(model, palette);
        let (column, row) = cell_of(&drawn, needle);
        let columns = model.sidebar.columns(screen.width);
        let bar = crate::ui::tabs::tab_bar(crate::ui::tab_bar_area(screen, columns), model);
        let event = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        crate::input::route_mouse(event, screen, &bar, &[], palette, model)
    }

    #[test]
    fn clicking_a_drawn_space_menu_row_fires_that_row() {
        let mut m = menu_model();

        let mut p = space_menu_open();
        let action = click_drawn(&mut m, &mut p, "rename");
        assert!(matches!(action, crate::input::Action::Redraw), "{action:?}");
        assert_eq!(p.renaming_space.as_deref(), Some("w1"));
        assert_eq!(p.query, "frontend", "pre-filled with the current name");

        let mut p = space_menu_open();
        let action = click_drawn(&mut m, &mut p, "color");
        assert!(matches!(action, crate::input::Action::Redraw), "{action:?}");
        assert_eq!(p.picking_color.as_deref(), Some("w1"));

        let mut p = space_menu_open();
        let action = click_drawn(&mut m, &mut p, "close space");
        assert!(
            matches!(&action, crate::input::Action::Command(cmd)
                if matches!(cmd, crate::ui::Command::CloseSpace(id) if id == "w1")),
            "{action:?}"
        );

        let mut p = space_menu_open();
        let action = click_drawn(&mut m, &mut p, "new pane");
        assert!(matches!(action, crate::input::Action::Redraw), "{action:?}");
        assert_eq!(p.picking_agent.as_deref(), Some("w1"));
    }

    #[test]
    fn clicking_a_drawn_pane_menu_row_fires_that_row() {
        // Pane rename and color arrived with the runtime's `pane.update`, and
        // with a rule that a user-set title outranks the shell's own OSC
        // title — without that second half the shell renames it straight back.
        let mut m = menu_model();
        let pane_menu = || Palette {
            tab_menu: Some(crate::ui::menu::TabMenu {
                target: crate::ui::menu::MenuTarget::Pane("p1".into()),
                column: 32,
                row: 0,
            }),
            ..Palette::default()
        };

        let mut p = pane_menu();
        let action = click_drawn(&mut m, &mut p, "rename");
        assert!(matches!(action, crate::input::Action::Redraw), "{action:?}");
        assert_eq!(p.renaming_pane.as_deref(), Some("p1"));

        let mut p = pane_menu();
        let action = click_drawn(&mut m, &mut p, "color");
        assert!(matches!(action, crate::input::Action::Redraw), "{action:?}");
        assert_eq!(p.picking_pane_color.as_deref(), Some("p1"));
    }

    #[test]
    fn clicking_a_drawn_palette_row_fires_that_row() {
        // The same guard for the other overlay, which is centred in the pane
        // area rather than anchored — so it exercises a different derivation.
        let mut m = menu_model();
        let mut p = Palette {
            open: true,
            ..Palette::default()
        };
        let action = click_drawn(&mut m, &mut p, "New pane in frontend");
        assert!(matches!(action, crate::input::Action::Redraw), "{action:?}");
        assert_eq!(p.picking_agent.as_deref(), Some("w1"));
    }
}
