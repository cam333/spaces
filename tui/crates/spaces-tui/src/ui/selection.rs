//! Drag-to-select over a pane's grid, and copying the result to the system
//! clipboard.
//!
//! # Why the client does this itself
//!
//! A terminal cannot both report mouse events to an application and use them
//! for its own selection — it has to pick one, and it picks the application.
//! So while this client is watching the mouse (which is what makes the
//! sidebar, tabs and menus clickable), the terminal's own click-drag
//! selection is unavailable, and `ctrl+c` is an interrupt for the agent
//! rather than a copy.
//!
//! [`crate::model::Model::mouse_captured`] offers the blunt way out — hand
//! the mouse back and let the terminal do it — but that only works if the
//! terminal cooperates, and it costs every click in the app. Selecting here
//! instead works the same everywhere and keeps the app clickable.
//!
//! # How the text gets to the clipboard
//!
//! [`osc52`] — the terminal escape that asks the *terminal* to set the system
//! clipboard. Chosen over a native clipboard crate for one reason that
//! matters here: it travels down the same connection everything else does, so
//! it works when the client is running over SSH on another machine, which is
//! the case this whole runtime exists to serve. A native clipboard would
//! write to the clipboard of whatever box the client happens to run on.
//!
//! Not every terminal honours OSC 52 (some require it to be enabled), so the
//! status bar reports how much was copied rather than assuming it landed.

use ratatui::layout::Rect;

/// A drag in progress, or a completed one, over one pane's grid.
///
/// Coordinates are **screen** cells, not grid indices, and `region` is the
/// pane's rect at the moment the drag began — everything is clipped to it, so
/// dragging out of the pane selects to its edge rather than into the sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub pane_id: String,
    pub region: Rect,
    /// Where the drag started. May be after [`Selection::head`] on screen —
    /// dragging up and left is as valid as down and right.
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl Selection {
    /// The two ends in reading order, so every consumer below can assume
    /// `start <= end` and none of them has to think about drag direction.
    ///
    /// Ordered by row first, then column: that is what makes a selection
    /// spanning three lines mean "the tail of the first, all of the second,
    /// the head of the third" rather than a rectangle.
    pub fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        let (a, h) = (self.anchor, self.head);
        if (a.1, a.0) <= (h.1, h.0) {
            (a, h)
        } else {
            (h, a)
        }
    }

    /// `true` when the drag never moved — a plain click, which must not
    /// select anything or clobber the clipboard.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// `true` iff the cell at `(column, row)` is inside the selection.
    ///
    /// Flow selection, not a rectangle: the first row runs from the start
    /// column to the end of the pane, whole rows in between are entirely
    /// selected, and the last row runs from the pane's left edge to the end
    /// column. Cells outside `region` are never selected however far the
    /// pointer was dragged.
    pub fn contains(&self, column: u16, row: u16) -> bool {
        if self.is_empty() || !within(self.region, column, row) {
            return false;
        }
        let (start, end) = self.ordered();
        if row < start.1 || row > end.1 {
            return false;
        }
        // The end column is inclusive, so a one-cell drag selects one cell.
        let from = if row == start.1 {
            start.0
        } else {
            self.region.x
        };
        let to = if row == end.1 {
            end.0
        } else {
            self.region
                .x
                .saturating_add(self.region.width)
                .saturating_sub(1)
        };
        column >= from && column <= to
    }

    /// The selected text, read out of `lines` — the pane's visible grid, one
    /// entry per row, as [`crate::term::PaneTerm::visible_lines`] produces it.
    ///
    /// Trailing spaces are trimmed from every line, because a terminal grid is
    /// padded to its full width and copying that padding turns one selected
    /// word into a word plus sixty spaces. Rows are joined with `\n`; a
    /// selection inside a single row yields no newline at all.
    ///
    /// Every index is derived by subtraction from `region` and then bounds
    /// checked with `.get`/`.take`, never by slicing on a byte offset — a
    /// pane's grid is arbitrary UTF-8 from a remote process, and byte-slicing
    /// it off a char boundary panics.
    pub fn text(&self, lines: &[String]) -> String {
        if self.is_empty() {
            return String::new();
        }
        let (start, end) = self.ordered();
        let mut out: Vec<String> = Vec::new();
        for row in start.1..=end.1 {
            let Some(line) = usize::from(row.saturating_sub(self.region.y))
                .checked_add(0)
                .and_then(|index| lines.get(index))
            else {
                continue;
            };
            let from = usize::from(if row == start.1 {
                start.0.saturating_sub(self.region.x)
            } else {
                0
            });
            let to = if row == end.1 {
                usize::from(end.0.saturating_sub(self.region.x)).saturating_add(1)
            } else {
                usize::MAX
            };
            let taken: String = line
                .chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            out.push(taken.trim_end().to_string());
        }
        out.join("\n")
    }
}

/// `true` iff `(column, row)` is inside `rect`. A zero-sized rect contains
/// nothing.
pub fn within(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// The OSC 52 escape sequence that asks the terminal to put `text` on the
/// system clipboard.
///
/// `\x1b]52;c;<base64>\x07` — `c` is the clipboard selection (as opposed to
/// the X11 primary), and BEL terminates it, which more terminals accept than
/// the ST form.
///
/// Returns an empty string for empty input rather than an escape that would
/// *clear* the user's clipboard: a click that selected nothing must leave
/// whatever they copied earlier alone.
pub fn osc52(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    format!("\u{1b}]52;c;{}\u{7}", base64(text.as_bytes()))
}

/// Standard base64, no line breaks — the encoding OSC 52 specifies.
///
/// Hand-written rather than pulled from a crate: it is fifteen lines, this is
/// the only place in the client that needs it, and the alternative is a
/// dependency in a binary that ships per-platform.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let index = ((n >> (18 - 6 * i)) & 0b11_1111) as usize;
                out.push(ALPHABET[index] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION: Rect = Rect {
        x: 28,
        y: 1,
        width: 20,
        height: 4,
    };

    fn selection(anchor: (u16, u16), head: (u16, u16)) -> Selection {
        Selection {
            pane_id: "p1".into(),
            region: REGION,
            anchor,
            head,
        }
    }

    fn grid() -> Vec<String> {
        vec![
            "hello world         ".into(),
            "second line here    ".into(),
            "third               ".into(),
            "fourth line         ".into(),
        ]
    }

    #[test]
    fn a_drag_that_never_moved_selects_nothing() {
        // A plain click must not clobber the clipboard.
        let s = selection((30, 2), (30, 2));
        assert!(s.is_empty());
        assert_eq!(s.text(&grid()), "");
        assert!(!s.contains(30, 2));
        assert_eq!(osc52(""), "", "and nothing is sent to the terminal");
    }

    #[test]
    fn a_selection_within_one_row_takes_just_those_cells() {
        // Columns 28..=32 of row 1 — "hello".
        let s = selection((28, 1), (32, 1));
        assert_eq!(s.text(&grid()), "hello");
        assert!(s.contains(28, 1));
        assert!(s.contains(32, 1));
        assert!(
            !s.contains(33, 1),
            "the end column is inclusive, not beyond"
        );
        assert!(!s.contains(30, 2), "and no other row is touched");
    }

    #[test]
    fn dragging_backwards_selects_the_same_text() {
        let forwards = selection((28, 1), (32, 1)).text(&grid());
        let backwards = selection((32, 1), (28, 1)).text(&grid());
        assert_eq!(forwards, backwards);
        assert_eq!(backwards, "hello");
    }

    #[test]
    fn a_multi_row_selection_flows_rather_than_making_a_rectangle() {
        // From "world" on row 1 through "second" on row 2: the tail of the
        // first line, then the head of the next — not a column block.
        let s = selection((34, 1), (33, 2));
        assert_eq!(s.text(&grid()), "world\nsecond");
        // A middle row is selected end to end.
        let whole = selection((34, 1), (33, 3));
        assert_eq!(whole.text(&grid()), "world\nsecond line here\nthird");
        assert!(
            whole.contains(REGION.x, 2),
            "the middle row starts at the edge"
        );
        assert!(
            whole.contains(REGION.x + REGION.width - 1, 2),
            "and runs to it"
        );
    }

    #[test]
    fn trailing_grid_padding_is_never_copied() {
        // A terminal grid is padded to its full width; copying that turns one
        // word into a word plus sixty spaces.
        let s = selection((28, 3), (47, 3));
        assert_eq!(s.text(&grid()), "third");
    }

    #[test]
    fn dragging_outside_the_pane_selects_to_its_edge_and_no_further() {
        // The pointer left the pane; the selection must not run into the
        // sidebar or past the last row.
        let s = selection((28, 1), (200, 200));
        assert!(!s.contains(27, 2), "not into the sidebar");
        assert!(!s.contains(30, 0), "not above the pane");
        assert!(!s.contains(30, 5), "not below it");
        // And reading it back never indexes past the grid it was given.
        let text = s.text(&grid());
        assert!(text.starts_with("hello world"), "{text:?}");
        assert_eq!(text.lines().count(), 4, "{text:?}");
    }

    #[test]
    fn a_selection_over_a_shorter_grid_than_its_region_does_not_panic() {
        // The pane was resized smaller between the drag and the read.
        let s = selection((28, 1), (40, 4));
        assert_eq!(s.text(&["only one line".to_string()]), "only one line");
        assert_eq!(s.text(&[]), "");
    }

    #[test]
    fn multibyte_content_is_sliced_by_characters_not_bytes() {
        // A pane's grid is arbitrary UTF-8 from a remote process; byte-slicing
        // it off a char boundary panics.
        let lines = vec!["\u{1f980}\u{1f980}\u{1f980}\u{1f980}\u{1f980}".to_string()];
        let s = selection((29, 1), (30, 1));
        assert_eq!(s.text(&lines), "\u{1f980}\u{1f980}");
    }

    #[test]
    fn osc52_carries_the_text_base64_encoded_to_the_system_clipboard() {
        let escape = osc52("hi");
        assert!(escape.starts_with("\u{1b}]52;c;"), "{escape:?}");
        assert!(escape.ends_with('\u{7}'), "{escape:?}");
        assert!(escape.contains("aGk="), "{escape:?}");
    }

    #[test]
    fn base64_matches_the_standard_at_every_padding_length() {
        // The three chunk remainders, which is where hand-written encoders go
        // wrong.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // Bytes above 0x7f, which a pane's UTF-8 is full of.
        assert_eq!(base64("\u{1f980}".as_bytes()), "8J+mgA==");
    }

    #[test]
    fn a_degenerate_region_selects_nothing_and_never_panics() {
        for region in [
            Rect::new(0, 0, 0, 0),
            Rect::new(28, 1, 0, 4),
            Rect::new(28, 1, 20, 0),
        ] {
            let s = Selection {
                pane_id: "p1".into(),
                region,
                anchor: (28, 1),
                head: (40, 3),
            };
            assert!(!s.contains(30, 2), "{region:?}");
            let _ = s.text(&grid());
        }
    }
}
