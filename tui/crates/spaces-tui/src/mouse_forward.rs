//! Mouse events for the program running in a pane.
//!
//! A terminal reports the mouse to a program only once that program asks
//! (`DECSET 1000`/`1002`/`1003`, usually with `1006` for the SGR encoding).
//! Until this module the client never did: every click was chrome or
//! navigation, so a program that drew something clickable — an agent's side
//! panel with a close button, say — could be shown but never clicked, and
//! a panel with no keyboard binding to dismiss it was stuck on screen.
//!
//! The rule is a terminal's rule: when the focused pane's program has asked
//! for the mouse, events over that pane go to it, encoded the way it asked.
//! Shift is the escape hatch, as in most terminals: with Shift held, a drag
//! selects text in the client instead.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

/// What the program in a pane asked the terminal to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseReporting {
    /// `1002`: motion while a button is held.
    pub drag: bool,
    /// `1003`: all motion, button or not.
    pub motion: bool,
    /// `1006`: SGR encoding. Without it, the legacy X10 bytes.
    pub sgr: bool,
}

/// The bytes to send the program for `event`, or `None` when the event is not
/// over `region` or is not something this mode reports.
///
/// Shift-modified events are never forwarded — see the module docs.
pub fn encode(event: &MouseEvent, region: Rect, mode: MouseReporting) -> Option<Vec<u8>> {
    if event.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    if event.column < region.x
        || event.row < region.y
        || event.column >= region.x.saturating_add(region.width)
        || event.row >= region.y.saturating_add(region.height)
    {
        return None;
    }
    // 1-based, relative to the pane, which is all the program can see.
    let x = u32::from(event.column - region.x) + 1;
    let y = u32::from(event.row - region.y) + 1;

    let (code, release) = match event.kind {
        MouseEventKind::Down(button) => (button_code(button), false),
        MouseEventKind::Up(button) => (button_code(button), true),
        MouseEventKind::Drag(button) if mode.drag || mode.motion => {
            (button_code(button) + 32, false)
        }
        // 3 + 32: "no button", moving.
        MouseEventKind::Moved if mode.motion => (35, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        _ => return None,
    };
    let code = code + modifier_bits(event.modifiers);

    if mode.sgr {
        let end = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes());
    }
    // X10: a release is button 3 and carries no button identity. Each value
    // is offset by 32 into one byte, so coordinates past 223 cannot be sent.
    let code = if release {
        3 + modifier_bits(event.modifiers)
    } else {
        code
    };
    if x > 223 || y > 223 {
        return None;
    }
    Some(vec![
        0x1b,
        b'[',
        b'M',
        (32 + code) as u8,
        (32 + x) as u8,
        (32 + y) as u8,
    ])
}

fn button_code(button: MouseButton) -> u32 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn modifier_bits(modifiers: KeyModifiers) -> u32 {
    let mut bits = 0;
    if modifiers.contains(KeyModifiers::ALT) {
        bits += 8;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        bits += 16;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION: Rect = Rect {
        x: 28,
        y: 1,
        width: 80,
        height: 30,
    };
    const SGR: MouseReporting = MouseReporting {
        drag: false,
        motion: false,
        sgr: true,
    };

    fn at(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn sgr(event: MouseEvent) -> Option<String> {
        encode(&event, REGION, SGR).map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn a_click_is_reported_relative_to_the_pane_and_one_based() {
        let down = at(MouseEventKind::Down(MouseButton::Left), 28, 1);
        assert_eq!(sgr(down).as_deref(), Some("\x1b[<0;1;1M"));
        let up = at(MouseEventKind::Up(MouseButton::Left), 107, 30);
        assert_eq!(sgr(up).as_deref(), Some("\x1b[<0;80;30m"));
    }

    #[test]
    fn nothing_outside_the_pane_is_reported() {
        assert_eq!(
            sgr(at(MouseEventKind::Down(MouseButton::Left), 27, 5)),
            None
        );
        assert_eq!(
            sgr(at(MouseEventKind::Down(MouseButton::Left), 108, 5)),
            None
        );
        assert_eq!(
            sgr(at(MouseEventKind::Down(MouseButton::Left), 40, 0)),
            None
        );
        assert_eq!(
            sgr(at(MouseEventKind::Down(MouseButton::Left), 40, 31)),
            None
        );
    }

    #[test]
    fn buttons_and_the_wheel_have_their_own_codes() {
        assert_eq!(
            sgr(at(MouseEventKind::Down(MouseButton::Right), 30, 2)).as_deref(),
            Some("\x1b[<2;3;2M")
        );
        assert_eq!(
            sgr(at(MouseEventKind::Down(MouseButton::Middle), 30, 2)).as_deref(),
            Some("\x1b[<1;3;2M")
        );
        assert_eq!(
            sgr(at(MouseEventKind::ScrollUp, 30, 2)).as_deref(),
            Some("\x1b[<64;3;2M")
        );
        assert_eq!(
            sgr(at(MouseEventKind::ScrollDown, 30, 2)).as_deref(),
            Some("\x1b[<65;3;2M")
        );
    }

    #[test]
    fn motion_is_reported_only_in_the_modes_that_ask_for_it() {
        let drag = at(MouseEventKind::Drag(MouseButton::Left), 30, 2);
        let moved = at(MouseEventKind::Moved, 30, 2);
        assert_eq!(sgr(drag), None, "1000 alone reports clicks only");
        assert_eq!(sgr(moved), None);
        let drag_mode = MouseReporting { drag: true, ..SGR };
        assert_eq!(
            encode(&drag, REGION, drag_mode),
            Some(b"\x1b[<32;3;2M".to_vec())
        );
        assert_eq!(
            encode(&moved, REGION, drag_mode),
            None,
            "1002 is motion with a button held"
        );
        let any = MouseReporting {
            motion: true,
            ..SGR
        };
        assert_eq!(encode(&moved, REGION, any), Some(b"\x1b[<35;3;2M".to_vec()));
    }

    #[test]
    fn alt_and_ctrl_are_encoded_and_shift_is_kept_for_selecting() {
        let mut e = at(MouseEventKind::Down(MouseButton::Left), 30, 2);
        e.modifiers = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert_eq!(sgr(e).as_deref(), Some("\x1b[<24;3;2M"));
        e.modifiers = KeyModifiers::SHIFT;
        assert_eq!(sgr(e), None);
    }

    #[test]
    fn without_sgr_the_legacy_bytes_are_sent_and_releases_lose_their_button() {
        let x10 = MouseReporting { sgr: false, ..SGR };
        let down = at(MouseEventKind::Down(MouseButton::Left), 28, 1);
        assert_eq!(
            encode(&down, REGION, x10),
            Some(vec![0x1b, b'[', b'M', 32, 33, 33])
        );
        let up = at(MouseEventKind::Up(MouseButton::Right), 28, 1);
        assert_eq!(
            encode(&up, REGION, x10),
            Some(vec![0x1b, b'[', b'M', 35, 33, 33])
        );
    }

    #[test]
    fn legacy_coordinates_past_223_are_dropped_rather_than_wrapped() {
        let wide = Rect {
            x: 0,
            y: 0,
            width: 400,
            height: 10,
        };
        let x10 = MouseReporting { sgr: false, ..SGR };
        let far = at(MouseEventKind::Down(MouseButton::Left), 300, 1);
        assert_eq!(encode(&far, wide, x10), None);
        assert!(encode(&far, wide, SGR).is_some(), "SGR has no such limit");
    }
}
