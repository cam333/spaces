//! Toasts: a short-lived line in the top-right corner of the pane area when
//! a pane *other than the one you are looking at* blocks or finishes.
//!
//! ```text
//!                               × parser · api  blocked
//!                               ✓ claude · web  done
//! ```
//!
//! The runtime already decides when a state change is worth a human's
//! attention — `pane.notify`, debounced so a blocked-then-resolved flicker
//! never fires — and plays a sound or posts a desktop notification for it.
//! This is the terminal client's version: [`crate::model::Model::push_toast`]
//! folds the event in, and this module lays out and draws what is pending.
//!
//! Newest on top, at most [`crate::model::MAX_TOASTS`], each gone after
//! [`crate::model::TOAST_TTL`] — expired by `main.rs`'s existing redraw tick
//! rather than a timer of its own. Clicking one focuses its pane.
//!
//! Like the sidebar and the tab bar, [`toast_layout`] is the one geometry
//! both [`render_toasts`] and the mouse router read, so a click and the frame
//! it landed on cannot disagree. And like them, the state lives in the text —
//! the icon and the word — with color only reinforcing it.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::model::{DisplayState, Model, Toast};

use super::sidebar::{state_color, state_icon};

/// The exact text of one toast, before it is fitted to a width: the state
/// icon, the pane, the space it is in when that is known, and the state as a
/// word.
pub fn toast_text(toast: &Toast) -> String {
    let word = match toast.state {
        DisplayState::Blocked => "blocked",
        DisplayState::Done => "done",
        other => other.label(),
    };
    let place = match toast.space.as_deref() {
        Some(space) => format!("{} \u{b7} {space}", toast.pane),
        None => toast.pane.clone(),
    };
    format!(" {} {place}  {word} ", state_icon(toast.state))
}

/// Where each pending toast sits inside `pane_area`, newest first: one row
/// each, stacked down from the area's top-right corner and right-aligned to
/// it. A toast wider than the area is clipped to it (by characters, at
/// render time); an area too short for all of them shows the newest that fit.
/// A zero-sized area yields nothing to draw or click.
pub fn toast_layout(pane_area: Rect, model: &Model) -> Vec<(Rect, String)> {
    if pane_area.width == 0 || pane_area.height == 0 {
        return Vec::new();
    }
    shown(model)
        .take(usize::from(pane_area.height))
        .enumerate()
        .map(|(row, toast)| {
            let width = u16::try_from(toast_text(toast).chars().count())
                .unwrap_or(u16::MAX)
                .min(pane_area.width);
            let rect = Rect::new(
                pane_area.x.saturating_add(pane_area.width - width),
                pane_area.y.saturating_add(row as u16),
                width,
                1,
            );
            (rect, toast.pane_id.clone())
        })
        .collect()
}

/// The toasts on screen, newest first: every pending one except a toast for
/// the pane that has since been focused — by a click on its tab, the agents
/// panel, the palette — which has nothing left to tell you.
fn shown(model: &Model) -> impl Iterator<Item = &Toast> {
    model
        .toasts
        .iter()
        .filter(move |t| model.focused.as_deref() != Some(t.pane_id.as_str()))
}

/// The pane whose toast a click at `(column, row)` landed on, if any.
pub fn toast_at(layout: &[(Rect, String)], column: u16, row: u16) -> Option<&str> {
    layout
        .iter()
        .find(|(rect, _)| {
            rect.width > 0
                && column >= rect.x
                && column < rect.x.saturating_add(rect.width)
                && row == rect.y
        })
        .map(|(_, id)| id.as_str())
}

/// Draws every pending toast into `pane_area`. Black on the state's own
/// color, so a toast reads as something laid over the agent's output rather
/// than as more of it.
pub fn render_toasts(model: &Model, pane_area: Rect, buf: &mut Buffer) {
    for ((rect, _), toast) in toast_layout(pane_area, model).iter().zip(shown(model)) {
        let rect = rect.intersection(buf.area);
        if rect.width == 0 {
            continue;
        }
        let text: String = toast_text(toast)
            .chars()
            .take(usize::from(rect.width))
            .collect();
        let style = Style::default()
            .fg(super::theme::current().ink())
            .bg(state_color(toast.state))
            .add_modifier(Modifier::BOLD);
        Paragraph::new(Line::styled(text, style)).render(rect, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use std::time::Instant;

    const AREA: Rect = Rect {
        x: 28,
        y: 1,
        width: 52,
        height: 20,
    };

    fn toast(pane_id: &str, pane: &str, space: Option<&str>, state: DisplayState) -> Toast {
        Toast {
            pane_id: pane_id.into(),
            pane: pane.into(),
            space: space.map(str::to_string),
            state,
            expires_at: Instant::now(),
        }
    }

    fn with_toasts(toasts: Vec<Toast>) -> Model {
        let mut m = Model::default();
        m.toasts = toasts;
        m
    }

    fn row_text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn a_toast_names_the_pane_its_space_and_the_state_in_words() {
        let t = toast("p2", "parser", Some("api"), DisplayState::Blocked);
        assert_eq!(toast_text(&t), " \u{d7} parser \u{b7} api  blocked ");
        let t = toast("p2", "codex", None, DisplayState::Done);
        assert_eq!(toast_text(&t), " \u{2713} codex  done ");
    }

    #[test]
    fn toasts_stack_down_from_the_top_right_newest_first() {
        let m = with_toasts(vec![
            toast("new", "claude", Some("api"), DisplayState::Done),
            toast(
                "old",
                "a much longer pane name",
                None,
                DisplayState::Blocked,
            ),
        ]);
        let layout = toast_layout(AREA, &m);
        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0].1, "new");
        assert_eq!(layout[0].0.y, AREA.y, "newest on top");
        assert_eq!(layout[1].0.y, AREA.y + 1);
        for (rect, _) in &layout {
            assert_eq!(rect.x + rect.width, AREA.x + AREA.width, "right-aligned");
        }
    }

    #[test]
    fn a_toast_is_drawn_where_it_is_laid_out() {
        let m = with_toasts(vec![toast(
            "p2",
            "parser",
            Some("api"),
            DisplayState::Blocked,
        )]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        render_toasts(&m, AREA, &mut buf);
        let row = row_text(&buf, AREA.y);
        assert!(
            row.trim_end().ends_with("parser \u{b7} api  blocked"),
            "{row:?}"
        );
        let (rect, _) = &toast_layout(AREA, &m)[0];
        assert_eq!(
            buf[(rect.x + 1, rect.y)].bg,
            Color::Red,
            "the state's color"
        );
    }

    #[test]
    fn clicking_a_toast_resolves_to_its_pane() {
        let m = with_toasts(vec![
            toast("new", "claude", None, DisplayState::Done),
            toast("old", "codex", None, DisplayState::Blocked),
        ]);
        let layout = toast_layout(AREA, &m);
        let (rect, _) = &layout[1];
        assert_eq!(toast_at(&layout, rect.x, rect.y), Some("old"));
        assert_eq!(
            toast_at(&layout, rect.x + rect.width - 1, rect.y),
            Some("old")
        );
        assert_eq!(toast_at(&layout, rect.x.saturating_sub(1), rect.y), None);
        assert_eq!(toast_at(&layout, rect.x, rect.y + 1), None);
    }

    #[test]
    fn a_toast_for_a_pane_since_focused_is_not_shown() {
        let mut m = with_toasts(vec![
            toast("a", "claude", None, DisplayState::Done),
            toast("b", "codex", None, DisplayState::Blocked),
        ]);
        m.focused = Some("a".into());
        let layout = toast_layout(AREA, &m);
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].1, "b");
        assert_eq!(layout[0].0.y, AREA.y, "and the rest close up");
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        render_toasts(&m, AREA, &mut buf);
        assert!(row_text(&buf, AREA.y).contains("codex"));
        assert!(!row_text(&buf, AREA.y + 1).contains("claude"));
    }

    #[test]
    fn a_narrow_or_short_area_clips_toasts_rather_than_escaping_it() {
        let long = "\u{1f980}".repeat(80);
        let m = with_toasts(vec![
            toast("a", &long, None, DisplayState::Done),
            toast("b", "x", None, DisplayState::Done),
        ]);
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(5, 5, 10, 1),
            Rect::new(5, 5, 1, 3),
            Rect::new(0, 0, 80, 24),
        ] {
            let layout = toast_layout(area, &m);
            assert!(layout.len() <= usize::from(area.height));
            for (rect, _) in &layout {
                assert!(rect.x >= area.x && rect.x + rect.width <= area.x + area.width);
                assert!(rect.y >= area.y && rect.y < area.y + area.height.max(1));
            }
            let mut buf = Buffer::empty(Rect::new(0, 0, 90, 30));
            render_toasts(&m, area, &mut buf);
        }
    }
}
