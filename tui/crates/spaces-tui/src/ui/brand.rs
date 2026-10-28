//! What the client is and which machine it is showing.
//!
//! A full-screen client covers the terminal it runs in, and with
//! `spaces --remote` the same screen can be this machine or another one. The
//! status bar therefore opens with two solid pills — the `◆ spaces` mark in
//! the brand accent, then the machine's name in a colour of its own — and the
//! terminal's title says the same thing, so a tab or a window list
//! identifies it too.
//!
//! A machine's colour is derived from its name alone: stable across runs,
//! the same on every client, and different from its neighbours often enough
//! that `production` reads as `production` before the name is read.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// The brand accent: the web app's default indigo. The mark is drawn in the
/// current theme's accent, which is this in the `terminal` theme.
pub const ACCENT: Color = Color::Rgb(0x63, 0x66, 0xf1);

/// The mark's text, padded inside its pill.
pub const MARK: &str = " \u{25c6} spaces ";

/// Dark text on the pills, legible on every colour below.
const INK: Color = Color::Rgb(0x11, 0x11, 0x1b);

/// Machine colours. Deliberately none of the agent-state colours (red,
/// yellow, teal, green, grey), so a pill never reads as a state.
const HOST_COLORS: [Color; 8] = [
    Color::Rgb(0x89, 0xb4, 0xfa), // blue
    Color::Rgb(0xcb, 0xa6, 0xf7), // mauve
    Color::Rgb(0xf5, 0xc2, 0xe7), // pink
    Color::Rgb(0xfa, 0xb3, 0x87), // peach
    Color::Rgb(0x74, 0xc7, 0xec), // sapphire
    Color::Rgb(0xb4, 0xbe, 0xfe), // lavender
    Color::Rgb(0xf2, 0xcd, 0xcd), // flamingo
    Color::Rgb(0xee, 0x99, 0xa0), // maroon
];

/// Longest machine name shown, in characters.
const MAX_HOST_CHARS: usize = 24;

/// The colour of the machine called `host`. FNV-1a, so it is the same
/// everywhere, not just within one run.
pub fn host_color(host: &str) -> Color {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in host.to_ascii_lowercase().bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    HOST_COLORS[(hash % HOST_COLORS.len() as u32) as usize]
}

/// The badge for `host` within `width` columns, and the columns it takes.
///
/// Shrinks rather than overflows: the machine pill goes first, then the
/// mark, and below that nothing is drawn. An empty `host` (no snapshot yet)
/// shows the mark alone.
pub fn badge(host: &str, width: u16) -> (Vec<Span<'static>>, u16) {
    let mark_width = MARK.chars().count() as u16;
    let mut spans = Vec::new();
    let mut used = 0;
    if width < mark_width {
        return (spans, 0);
    }
    spans.push(Span::styled(
        MARK,
        Style::default()
            .fg(INK)
            .bg(super::theme::current().accent)
            .add_modifier(Modifier::BOLD),
    ));
    used += mark_width;

    let name: String = host.chars().take(MAX_HOST_CHARS).collect();
    if !name.is_empty() {
        let pill = format!(" {name} ");
        let pill_width = pill.chars().count() as u16;
        if used + pill_width <= width {
            spans.push(Span::styled(
                pill,
                Style::default().fg(INK).bg(host_color(host)),
            ));
            used += pill_width;
        }
    }
    (spans, used)
}

/// The terminal title: `spaces · <machine>`, or `spaces` before the
/// machine is known. Control characters are dropped, since this goes
/// straight into an escape sequence and a hostname comes off the wire.
pub fn window_title(host: &str) -> String {
    let name: String = host
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_HOST_CHARS)
        .collect();
    if name.is_empty() {
        "spaces".to_string()
    } else {
        format!("spaces \u{b7} {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn the_badge_names_the_client_and_the_machine() {
        let (spans, used) = badge("production", 80);
        assert_eq!(text(&spans), " \u{25c6} spaces  production ");
        assert_eq!(used, text(&spans).chars().count() as u16);
        assert_eq!(spans[0].style.bg, Some(ACCENT));
        assert_eq!(spans[1].style.bg, Some(host_color("production")));
    }

    #[test]
    fn the_badge_gives_up_the_machine_before_the_mark_and_then_everything() {
        let mark = MARK.chars().count() as u16;
        assert_eq!(text(&badge("production", mark + 3).0), MARK);
        assert_eq!(badge("production", mark - 1), (vec![], 0));
    }

    #[test]
    fn before_a_snapshot_there_is_only_the_mark() {
        assert_eq!(text(&badge("", 80).0), MARK);
    }

    #[test]
    fn a_long_machine_name_is_cut_by_characters() {
        let long = "é".repeat(60);
        let (spans, _) = badge(&long, 200);
        assert_eq!(spans[1].content.chars().count(), MAX_HOST_CHARS + 2);
    }

    #[test]
    fn a_machine_keeps_its_colour_and_neighbours_tend_to_differ() {
        assert_eq!(host_color("production"), host_color("production"));
        assert_eq!(host_color("Production"), host_color("production"));
        let distinct: std::collections::HashSet<_> =
            ["production", "staging", "laptop", "desktop", "build-1"]
                .iter()
                .map(|h| format!("{:?}", host_color(h)))
                .collect();
        assert!(distinct.len() >= 3, "{distinct:?}");
    }

    #[test]
    fn the_title_names_the_machine_and_cannot_carry_escapes() {
        assert_eq!(window_title("production"), "spaces \u{b7} production");
        assert_eq!(window_title(""), "spaces");
        assert_eq!(window_title("evil\x07\x1b]0;x"), "spaces \u{b7} evil]0;x");
    }
}
