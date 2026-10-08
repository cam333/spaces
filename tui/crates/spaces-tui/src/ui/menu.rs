//! The tab context menu: what a right-click on a tab offers.
//!
//! ```text
//!   ◐ claude ]  ○ codex     +
//!             ┌────────────────┐
//!             │ close pane     │
//!             │ move to api    │
//!             │ move to web    │
//!             └────────────────┘
//! ```
//!
//! Mouse-first multiplexers hang one of these off their tabs; this is the
//! same idea, restricted to what the protocol can actually do to a pane. Every entry resolves to a
//! [`Command`] the palette could also produce, so there is exactly one
//! implementation of closing or moving a pane however the user asked.
//!
//! # Only what the protocol supports
//!
//! `pane.close` and `pane.move` are the two methods in the runtime's dispatch
//! table that act on an existing pane's *identity*. Renaming a pane has no
//! method — `pane.title` arrives as an event but nothing accepts one — so it
//! is deliberately absent rather than present and inert.
//!
//! # Geometry
//!
//! [`menu_layout`] is the single source of truth for where the box and its
//! rows sit, read by both [`render_menu`] and the mouse router — the same
//! discipline [`super::sidebar::sidebar_layout`], [`super::tabs::tab_bar`] and
//! [`super::palette::palette_layout`] follow. The box is anchored just below
//! the click and then *pushed* back inside `area` if it would overflow, so a
//! right-click on the rightmost tab or the bottom row still produces a menu
//! entirely on screen.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::model::Model;

use super::palette::Command;

/// The entry that closes the pane.
pub const CLOSE_LABEL: &str = "close pane";

/// The entry that creates another pane — tab-based multiplexers call this
/// "New Tab"; this runtime has no tabs, and the thing it actually makes is
/// a pane.
pub const NEW_PANE_LABEL: &str = "new pane\u{2026}";

/// The entry that renames a space. Ellipsis because it opens a prompt.
pub const RENAME_LABEL: &str = "rename\u{2026}";

/// The entry that recolors a space.
pub const COLOR_LABEL: &str = "color\u{2026}";

/// Widest the menu is allowed to grow, in cells.
const MAX_WIDTH: u16 = 32;

/// Which pane a right-click opened the menu for, and where it was clicked.
///
/// Lives on [`super::palette::Palette`] alongside the palette's own modes —
/// see that struct's docs for why it is the home of every piece of chrome
/// state the router owns across calls, palette or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabMenu {
    /// What the menu acts on. A pane (right-clicked on its tab) or a space
    /// (right-clicked on its sidebar row) — the two offer different things,
    /// but the box, the anchoring and the click routing are identical, so
    /// they share one overlay rather than growing a second.
    pub target: MenuTarget,
    /// Where the click landed. The box is anchored one row below this, so the
    /// menu does not cover the row it belongs to.
    pub column: u16,
    pub row: u16,
}

/// The thing a context menu was opened on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuTarget {
    Pane(String),
    Space(String),
    /// A space's Files tab.
    Files(String),
}

/// Where the menu's parts sit for one frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuLayout {
    /// The whole box, borders included.
    pub outer: Rect,
    /// `(row rect, index into [`items`])`, top to bottom.
    pub rows: Vec<(Rect, usize)>,
}

/// What the menu offers for `menu`'s pane: close it, then move it to each
/// *other* space.
///
/// The pane's own space is skipped — "move to where it already is" is not an
/// action. A pane whose `workspace_id` resolves to no known space (the
/// "unassigned" case) is offered every space, which is the one situation
/// where moving it somewhere is the whole point.
///
/// Empty only when the model has no record of the pane at all, which
/// [`menu_layout`] treats as "draw nothing" — a menu with no entries is worse
/// than no menu.
pub fn items(model: &Model, menu: &TabMenu) -> Vec<(String, Command)> {
    match &menu.target {
        MenuTarget::Pane(pane_id) => pane_items(model, pane_id),
        MenuTarget::Space(space_id) => space_items(model, space_id),
        MenuTarget::Files(space_id) => {
            if model.files.contains_key(space_id) {
                vec![(
                    "close files".to_string(),
                    Command::CloseFiles(space_id.clone()),
                )]
            } else {
                Vec::new()
            }
        }
    }
}

/// What a right-click on a *tab* offers: a new pane beside it, closing it, and
/// moving it to each other space.
///
/// No `rename` — this runtime has no method that sets a pane
/// title — `state.js`'s `setTitle` is called only from the PTY's own OSC title
/// sequence, so a name set here would be overwritten by the shell within
/// milliseconds. Absent rather than present and inert.
fn pane_items(model: &Model, pane_id: &str) -> Vec<(String, Command)> {
    let Some(pane) = model.local_panes().iter().find(|p| p.id == pane_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // Only when the pane's space is known: a new pane needs somewhere to go.
    if let Some(space) = pane.workspace_id.clone() {
        out.push((
            NEW_PANE_LABEL.to_string(),
            Command::PickAgent {
                workspace_id: space,
            },
        ));
    }
    // Rename and color are `pane.update`, added to the runtime alongside a
    // rule that a user-set title outranks the PTY's own OSC title — without
    // that second half the shell renames the pane back within milliseconds,
    // which is why these were absent until the runtime grew both.
    out.push((
        RENAME_LABEL.to_string(),
        Command::RenamePane {
            pane_id: pane.id.clone(),
        },
    ));
    out.push((
        COLOR_LABEL.to_string(),
        Command::PickPaneColor {
            pane_id: pane.id.clone(),
        },
    ));
    out.push((CLOSE_LABEL.to_string(), Command::ClosePane(pane.id.clone())));
    for ws in model.local_workspaces() {
        if pane.workspace_id.as_deref() == Some(ws.id.as_str()) {
            continue;
        }
        out.push((
            format!("move to {}", ws.name),
            Command::MovePane {
                pane_id: pane.id.clone(),
                workspace_id: ws.id.clone(),
            },
        ));
    }
    out
}

/// What a right-click on a *space* row offers: a new pane in it, renaming it,
/// recoloring it, and closing it.
///
/// Rename and color both resolve to `workspace.update`, which takes `name`
/// and `color` — so unlike a pane, both are things the protocol can actually
/// do. Closing is `workspace.close`, which kills every pane in the space, so
/// it is last and labelled with the space's own name rather than a bare
/// "close": this is the most destructive thing in the client, and the menu
/// appears under the pointer.
///
/// No worktree entries either. Nothing in this protocol models a worktree —
/// a space has an id, a name, a
/// color and its pane ids, and no path at all — so those are absent rather
/// than faked.
fn space_items(model: &Model, space_id: &str) -> Vec<(String, Command)> {
    let Some(ws) = model.local_workspaces().iter().find(|w| w.id == space_id) else {
        return Vec::new();
    };
    vec![
        (
            NEW_PANE_LABEL.to_string(),
            Command::PickAgent {
                workspace_id: ws.id.clone(),
            },
        ),
        (
            RENAME_LABEL.to_string(),
            Command::RenameSpace {
                workspace_id: ws.id.clone(),
            },
        ),
        (
            COLOR_LABEL.to_string(),
            Command::PickSpaceColor {
                workspace_id: ws.id.clone(),
            },
        ),
        (
            format!("close space \"{}\"", ws.name),
            Command::CloseSpace(ws.id.clone()),
        ),
    ]
}

/// What the menu's title calls its target: a pane's title (or agent type when
/// it has none), or a space's name. Empty when the model no longer knows it,
/// which only happens on a menu [`menu_layout`] is about to decline to draw.
fn target_name(model: &Model, target: &MenuTarget) -> String {
    match target {
        MenuTarget::Pane(id) => model
            .local_panes()
            .iter()
            .find(|p| &p.id == id)
            .map(|p| {
                if p.title.is_empty() {
                    p.agent_type.clone()
                } else {
                    p.title.clone()
                }
            })
            .unwrap_or_default(),
        MenuTarget::Space(id) => model
            .local_workspaces()
            .iter()
            .find(|w| &w.id == id)
            .map(|w| w.name.clone())
            .unwrap_or_default(),
        MenuTarget::Files(_) => "files".to_string(),
    }
}

/// Lays the menu out inside `area` (the whole screen), or `None` when there
/// is nothing to show or no room to show it.
///
/// Anchored one row below the click so the box never covers the tab it
/// belongs to, then pushed left/up as needed to keep it inside `area`.
/// Saturating throughout; every dimension is checked.
pub fn menu_layout(model: &Model, menu: &TabMenu, area: Rect) -> Option<MenuLayout> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let entries = items(model, menu);
    if entries.is_empty() {
        return None;
    }

    let widest = entries
        .iter()
        .map(|(label, _)| label.chars().count() as u16)
        .max()
        .unwrap_or(0);
    // Two border columns plus a space of padding each side.
    let width = widest.saturating_add(4).min(MAX_WIDTH).min(area.width);
    let height = (entries.len() as u16).saturating_add(2).min(area.height);
    if width < 3 || height < 3 {
        // No room for a border plus one row. Nothing useful to draw, and a
        // borderless menu would be indistinguishable from pane output.
        return None;
    }

    // Anchor below the click, then pull back inside `area`.
    let right_edge = area.x.saturating_add(area.width);
    let bottom_edge = area.y.saturating_add(area.height);
    let x = menu
        .column
        .min(right_edge.saturating_sub(width))
        .max(area.x);
    let y = menu
        .row
        .saturating_add(1)
        .min(bottom_edge.saturating_sub(height))
        .max(area.y);
    let outer = Rect::new(x, y, width, height);
    let inner = Block::default().borders(Borders::ALL).inner(outer);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    let rows = (0..entries.len())
        .take(usize::from(inner.height))
        .map(|index| {
            let row_y = inner.y.saturating_add(index as u16);
            (Rect::new(inner.x, row_y, inner.width, 1), index)
        })
        .collect();
    Some(MenuLayout { outer, rows })
}

/// The item index a click at `(column, row)` landed on, if any.
pub fn item_at(layout: &MenuLayout, column: u16, row: u16) -> Option<usize> {
    layout
        .rows
        .iter()
        .find(|(rect, _)| hit(*rect, column, row))
        .map(|(_, index)| *index)
}

fn hit(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Draws the menu over whatever is underneath it.
///
/// Opaque, like the palette: [`Clear`] wipes the pane output first, or the
/// entries would be unreadable over a busy agent. Titled with the pane's own
/// name so a menu that appears under the pointer says what it will act on —
/// the one question a context menu must answer.
pub fn render_menu(model: &Model, menu: &TabMenu, area: Rect, buf: &mut Buffer) {
    let area = area.intersection(buf.area);
    let Some(layout) = menu_layout(model, menu, area) else {
        return;
    };
    let entries = items(model, menu);
    let title = target_name(model, &menu.target);

    Clear.render(layout.outer, buf);
    Block::default()
        .borders(Borders::ALL)
        .title(format!(" {title} "))
        .render(layout.outer, buf);
    for (rect, index) in &layout.rows {
        let Some((label, _)) = entries.get(*index) else {
            continue;
        };
        // The destructive entries are coloured, and keyed on the *command*
        // rather than on a row index — which is what this used to do, and
        // which silently started colouring "new pane" the moment that became
        // the first row. Closing a space kills every pane in it, so it earns
        // the same red as closing one. The text still says "close", which is
        // what survives a styling-stripped terminal; the color only
        // reinforces it.
        let destructive = matches!(
            entries.get(*index).map(|(_, cmd)| cmd),
            Some(Command::ClosePane(_)) | Some(Command::CloseSpace(_))
        );
        let style = if destructive {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        Paragraph::new(Line::styled(format!(" {label}"), style)).render(*rect, buf);
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use spaces_proto::{AgentState, PaneInfo, WorkspaceInfo};

    /// The whole frame, as `ui::render` passes it.
    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
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

    fn pane(id: &str, space: &str) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(space.into()),
            agent_type: "claude".into(),
            agent_state: AgentState::Working,
            seen: true,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        }
    }

    fn three_spaces() -> Model {
        let mut m = Model::new();
        m.local_mut().workspaces = vec![ws("w1", "api"), ws("w2", "web"), ws("w3", "docs")];
        m.local_mut().panes = vec![pane("p1", "w1")];
        m
    }

    fn menu_for(pane_id: &str) -> TabMenu {
        TabMenu {
            target: MenuTarget::Pane(pane_id.into()),
            column: 30,
            row: 0,
        }
    }

    fn text(model: &Model, menu: &TabMenu) -> String {
        let mut buf = Buffer::empty(SCREEN);
        render_menu(model, menu, SCREEN, &mut buf);
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // --- The space menu. Rename and color are
    // `workspace.update`; close is `workspace.close`; worktrees have no
    // protocol at all. ---

    fn space_menu(space_id: &str) -> TabMenu {
        TabMenu {
            target: MenuTarget::Space(space_id.into()),
            column: 3,
            row: 2,
        }
    }

    #[test]
    fn a_space_offers_a_new_pane_rename_colour_and_close() {
        let m = three_spaces();
        let entries = items(&m, &space_menu("w1"));
        let labels: Vec<&str> = entries.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                NEW_PANE_LABEL,
                RENAME_LABEL,
                COLOR_LABEL,
                "close space \"api\""
            ]
        );
        assert!(
            matches!(&entries[1].1, Command::RenameSpace { workspace_id } if workspace_id == "w1")
        );
        assert!(
            matches!(&entries[2].1, Command::PickSpaceColor { workspace_id } if workspace_id == "w1")
        );
        assert!(matches!(&entries[3].1, Command::CloseSpace(id) if id == "w1"));
    }

    #[test]
    fn closing_a_space_names_it_because_it_kills_every_pane_in_it() {
        // The most destructive thing in the client, on a menu that appears
        // under the pointer — so it says which space, and it is last.
        let m = three_spaces();
        let entries = items(&m, &space_menu("w2"));
        let (label, cmd) = entries.last().expect("close is last");
        assert!(label.contains("web"), "{label:?}");
        assert!(matches!(cmd, Command::CloseSpace(_)));
    }

    #[test]
    fn a_space_the_model_does_not_know_produces_no_menu() {
        let m = three_spaces();
        assert!(items(&m, &space_menu("nope")).is_empty());
        assert!(menu_layout(&m, &space_menu("nope"), SCREEN).is_none());
    }

    #[test]
    fn the_space_menu_names_the_space_it_will_act_on() {
        let m = three_spaces();
        let mut buf = Buffer::empty(SCREEN);
        render_menu(&m, &space_menu("w3"), SCREEN, &mut buf);
        let out = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(out.contains("docs"), "{out}");
        assert!(out.contains(RENAME_LABEL), "{out}");
        assert!(out.contains("close space"), "{out}");
    }

    #[test]
    fn a_pane_menu_offers_a_new_pane_in_that_panes_own_space() {
        // A tab bar's "New Tab", except this runtime has no tabs — the thing
        // it makes is a pane.
        let m = three_spaces();
        let entries = items(&m, &menu_for("p1"));
        assert_eq!(entries[0].0, NEW_PANE_LABEL);
        assert!(
            matches!(&entries[0].1, Command::PickAgent { workspace_id } if workspace_id == "w1"),
            "{:?}",
            entries[0].1
        );
    }

    #[test]
    fn an_orphaned_pane_is_offered_no_new_pane_because_there_is_nowhere_to_put_it() {
        let mut m = three_spaces();
        m.local_mut().panes[0].workspace_id = None;
        let entries = items(&m, &menu_for("p1"));
        assert!(
            !entries.iter().any(|(l, _)| l == NEW_PANE_LABEL),
            "{entries:?}"
        );
        assert_eq!(
            entries[0].0, RENAME_LABEL,
            "rename leads once there is nowhere to put a new pane: {entries:?}"
        );
    }

    #[test]
    fn only_the_destructive_entries_are_coloured() {
        // Keyed on the command, not on a row index — which is what this used
        // to be, and which silently started colouring "new pane" the moment
        // that became row zero.
        let m = three_spaces();
        for menu in [space_menu("w1"), menu_for("p1")] {
            let layout = menu_layout(&m, &menu, SCREEN).expect("drawn");
            let entries = items(&m, &menu);
            let mut buf = Buffer::empty(SCREEN);
            render_menu(&m, &menu, SCREEN, &mut buf);
            for (rect, index) in &layout.rows {
                let destructive = matches!(
                    entries[*index].1,
                    Command::ClosePane(_) | Command::CloseSpace(_)
                );
                let fg = buf[(rect.x + 1, rect.y)].fg;
                assert_eq!(
                    fg == Color::Red,
                    destructive,
                    "{:?} at row {index}",
                    entries[*index].0
                );
            }
        }
    }

    #[test]
    fn a_pane_offers_a_new_pane_close_then_every_other_space() {
        let m = three_spaces();
        let entries = items(&m, &menu_for("p1"));
        let labels: Vec<&str> = entries.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                NEW_PANE_LABEL,
                RENAME_LABEL,
                COLOR_LABEL,
                CLOSE_LABEL,
                "move to web",
                "move to docs"
            ]
        );
        assert!(matches!(&entries[1].1, Command::RenamePane { pane_id } if pane_id == "p1"));
        assert!(matches!(&entries[2].1, Command::PickPaneColor { pane_id } if pane_id == "p1"));
        assert!(matches!(&entries[3].1, Command::ClosePane(id) if id == "p1"));
        assert!(
            matches!(&entries[4].1, Command::MovePane { pane_id, workspace_id }
                if pane_id == "p1" && workspace_id == "w2"),
            "{:?}",
            entries[2].1
        );
    }

    #[test]
    fn the_panes_own_space_is_not_offered_as_a_destination() {
        // "Move to where it already is" is not an action.
        let m = three_spaces();
        let entries = items(&m, &menu_for("p1"));
        assert!(
            !entries.iter().any(|(label, _)| label == "move to api"),
            "{entries:?}"
        );
    }

    #[test]
    fn an_orphaned_pane_can_be_moved_anywhere() {
        // The one case where every space is a valid destination — and the
        // case where moving it somewhere is the whole point.
        let mut m = three_spaces();
        m.local_mut().panes[0].workspace_id = None;
        let entries = items(&m, &menu_for("p1"));
        assert_eq!(
            entries.len(),
            6,
            "rename, color, close, plus all three spaces: {entries:?}"
        );
    }

    #[test]
    fn a_pane_the_model_does_not_know_produces_no_menu() {
        // Better no menu than an empty box under the pointer.
        let m = three_spaces();
        assert!(items(&m, &menu_for("ghost")).is_empty());
        assert!(menu_layout(&m, &menu_for("ghost"), SCREEN).is_none());
        assert_eq!(text(&m, &menu_for("ghost")).trim(), "");
    }

    #[test]
    fn the_menu_names_the_pane_it_will_act_on() {
        // The one question a context menu appearing under the pointer has to
        // answer.
        let mut m = three_spaces();
        m.local_mut().panes[0].title = "the parser".into();
        let out = text(&m, &menu_for("p1"));
        assert!(out.contains("the parser"), "{out}");
        assert!(out.contains(CLOSE_LABEL), "{out}");
        assert!(out.contains("move to web"), "{out}");
    }

    #[test]
    fn an_untitled_pane_is_named_by_its_agent() {
        let m = three_spaces();
        assert!(text(&m, &menu_for("p1")).contains("claude"));
    }

    #[test]
    fn the_menu_opens_below_the_click_not_over_the_tab() {
        let m = three_spaces();
        let menu = TabMenu {
            target: MenuTarget::Pane("p1".into()),
            column: 30,
            row: 0,
        };
        let layout = menu_layout(&m, &menu, SCREEN).expect("drawn");
        assert_eq!(layout.outer.x, 30);
        assert_eq!(layout.outer.y, 1, "one row below the tab it belongs to");
    }

    #[test]
    fn a_menu_near_an_edge_is_pushed_back_inside_the_screen() {
        // A right-click on the rightmost tab, or on the bottom row, must
        // still produce a menu that is entirely on screen.
        // Offset from the origin so the near-edge assertions mean something:
        // `x >= 0` is vacuously true for a `u16`, and a menu clamped only at
        // the far edges would pass an origin-anchored version of this test
        // while still escaping upward or leftward.
        let area = Rect::new(4, 2, 60, 16);
        let m = three_spaces();
        for (column, row) in [
            (area.x + area.width - 1, area.y),
            (0, 0),
            (area.x, area.y + area.height - 1),
            (u16::MAX, u16::MAX),
        ] {
            let menu = TabMenu {
                target: MenuTarget::Pane("p1".into()),
                column,
                row,
            };
            let layout = menu_layout(&m, &menu, area).expect("drawn");
            for rect in
                std::iter::once(layout.outer).chain(layout.rows.iter().map(|(rect, _)| *rect))
            {
                assert!(rect.x >= area.x, "({column},{row}) -> {rect:?}");
                assert!(rect.y >= area.y, "({column},{row}) -> {rect:?}");
                assert!(
                    rect.x + rect.width <= area.x + area.width,
                    "({column},{row}) -> {rect:?}"
                );
                assert!(
                    rect.y + rect.height <= area.y + area.height,
                    "({column},{row}) -> {rect:?}"
                );
            }
        }
    }

    #[test]
    fn every_row_is_clickable_and_resolves_to_its_own_entry() {
        let m = three_spaces();
        let menu = menu_for("p1");
        let layout = menu_layout(&m, &menu, SCREEN).expect("drawn");
        assert_eq!(layout.rows.len(), items(&m, &menu).len());
        for (rect, index) in &layout.rows {
            assert_eq!(item_at(&layout, rect.x, rect.y), Some(*index));
            assert_eq!(
                item_at(&layout, rect.x + rect.width - 1, rect.y),
                Some(*index)
            );
        }
        // The border is inside the box but on no row, so it resolves to
        // nothing — and `route_within_tab_menu` treats "no row" the same
        // whether the click was inside the box or well outside it: a context
        // menu is modal and any miss dismisses it.
        assert_eq!(item_at(&layout, layout.outer.x, layout.outer.y), None);
        assert_eq!(item_at(&layout, 0, 0), None);
    }

    #[test]
    fn a_long_space_name_cannot_push_the_menu_past_its_maximum_width() {
        let mut m = three_spaces();
        m.local_mut().workspaces[1].name = "a-space-with-an-extremely-long-name-indeed".into();
        let layout = menu_layout(&m, &menu_for("p1"), SCREEN).expect("drawn");
        assert!(layout.outer.width <= MAX_WIDTH, "{layout:?}");
    }

    #[test]
    fn degenerate_screens_neither_panic_nor_produce_targets() {
        let m = three_spaces();
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 80, 0),
            Rect::new(0, 0, 2, 2),
            Rect::new(0, 0, 4, 3),
            Rect::new(0, 0, 3, 24),
        ] {
            let menu = menu_for("p1");
            render_menu(&m, &menu, area, &mut buf);
            if let Some(layout) = menu_layout(&m, &menu, area) {
                assert!(layout.outer.x + layout.outer.width <= area.x + area.width);
                assert!(layout.outer.y + layout.outer.height <= area.y + area.height);
            }
        }
    }

    #[test]
    fn a_space_with_no_other_spaces_still_offers_close() {
        let mut m = Model::new();
        m.local_mut().workspaces = vec![ws("w1", "only")];
        m.local_mut().panes = vec![pane("p1", "w1")];
        let entries = items(&m, &menu_for("p1"));
        assert_eq!(
            entries.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>(),
            vec![NEW_PANE_LABEL, RENAME_LABEL, COLOR_LABEL, CLOSE_LABEL],
            "no destinations, but everything that acts on the pane itself"
        );
        assert!(menu_layout(&m, &menu_for("p1"), SCREEN).is_some());
    }
}
