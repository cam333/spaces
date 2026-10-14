//! Per-pane terminal emulator.
//!
//! [`PaneTerm`] wraps an `alacritty_terminal::Term` fed by raw PTY bytes
//! read off a pane's data socket (see `spaces-proto`'s pane client). It owns
//! no I/O itself: [`PaneTerm::feed`] is handed whatever bytes arrived on the
//! wire and turns them into grid state; [`PaneTerm::visible_lines`] and
//! [`PaneTerm::render_into`] turn that grid state back into something the
//! rest of the TUI can display.
//!
//! This module renders bytes — it does not interpret them. Agent state
//! (blocked/working/done/...) is computed server-side and arrives over the
//! control channel as `pane.state` events (see `model.rs`); nothing here
//! inspects PTY output to infer it.
//!
//! `feed`'s input is untrusted network data by construction (raw bytes off
//! a pane socket), so no byte sequence may panic the process — malformed or
//! truncated UTF-8, escape sequences, and oversized/zero dimensions are all
//! within the range of "well, the pane said so" and must degrade instead of
//! aborting. `render_into`'s `area` is caller-supplied and may not fit the
//! target buffer; cells outside the buffer are skipped via
//! [`ratatui::buffer::Buffer::cell_mut`] rather than indexed (indexing
//! panics out of range).

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Line;
use alacritty_terminal::term::cell::Flags as CellFlags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color as AnsiColor, NamedColor};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color as RColor, Modifier};

/// Smallest grid `alacritty_terminal` is asked to allocate. A pane report of
/// `0x0` (or a resize down to it) would otherwise reach `Grid::new`/`Row::new`,
/// which `debug_assert!` on `columns >= 1` — clamping here keeps a
/// degenerate size report from ever reaching that assertion.
const MIN_COLS: u16 = 1;
const MIN_ROWS: u16 = 1;

/// Dimensions passed to `Term::new`/`Term::resize`. `alacritty_terminal`
/// ships an equivalent `term::test::TermSize`, but that lives in a module
/// documented as "terminal test helpers" — this crate isn't a test, so it
/// defines its own rather than reaching into that module from production
/// code.
struct Size {
    cols: usize,
    lines: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Largest single scroll [`PaneTerm::scroll`] passes through, in lines.
///
/// Comfortably past the 10,000 lines `Config::default()` keeps, so it never
/// truncates a real scroll, and far enough from `i32`'s edge that
/// `alacritty_terminal`'s unchecked target arithmetic cannot overflow — see
/// that function.
const MAX_SCROLL_DELTA: i32 = 100_000;

/// A per-pane terminal emulator: raw PTY bytes in, a rendered grid out.
pub struct PaneTerm {
    term: Term<VoidListener>,
    parser: ansi::Processor,
}

impl PaneTerm {
    /// Builds a new terminal grid of `cols` x `rows` cells. Dimensions
    /// below [`MIN_COLS`]/[`MIN_ROWS`] are clamped rather than rejected —
    /// this is fed straight from a pane's reported PTY size, which this
    /// module has no basis to treat as an error.
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = Size {
            cols: cols.max(MIN_COLS) as usize,
            lines: rows.max(MIN_ROWS) as usize,
        };
        let term = Term::new(Config::default(), &size, VoidListener);
        Self {
            term,
            parser: ansi::Processor::new(),
        }
    }

    /// Feeds raw PTY bytes into the terminal's VTE parser. This is network
    /// input by definition (pane socket bytes, unparsed) — every byte value
    /// and every escape sequence, valid or not, is handled by the parser's
    /// own state machine and can't panic this call.
    pub fn feed(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.parser.advance(&mut self.term, *byte);
        }
    }

    /// Scrolls the view `delta` lines back into the scrollback buffer
    /// (positive) or forward towards the live output (negative), stopping at
    /// either end.
    ///
    /// The buffer itself has been there all along — `Config::default()` keeps
    /// 10,000 lines of history — but nothing ever moved the display offset, so
    /// output that scrolled off the top was unreachable.
    ///
    /// `delta` is clamped to [`MAX_SCROLL_DELTA`] first, because
    /// `alacritty_terminal`'s own clamping is **not** total: it saturates at
    /// the oldest line and at the live edge, but computes the target with
    /// unchecked arithmetic before doing so, and `i32::MAX` panics with
    /// `attempt to add with overflow` inside `grid/mod.rs` on the way. Found
    /// by this module's own boundary test, not in the wild — today's only
    /// caller passes ±3 — but this is a `pub fn` on a type driven by network
    /// output, and "no path panics" holds regardless of who calls it now.
    pub fn scroll(&mut self, delta: i32) {
        let delta = delta.clamp(-MAX_SCROLL_DELTA, MAX_SCROLL_DELTA);
        self.term.scroll_display(Scroll::Delta(delta));
    }

    /// Jumps back to the live output. Called whenever the user types into the
    /// pane: reading history is one mode, and typing is the unambiguous signal
    /// that they are done with it — a keystroke whose echo appears somewhere
    /// off-screen is the worst of both.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// How many lines back the view currently sits; `0` at the live edge.
    ///
    /// Read by the renderer to translate display rows into grid lines, and by
    /// the status bar, which has to say that what is on screen is history —
    /// otherwise a scrolled-back pane looks exactly like an agent that has
    /// stopped producing output.
    pub fn scrollback_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// What the program in this pane has asked the terminal to report about
    /// the mouse, or `None` when it has not asked — see `mouse_forward`.
    pub fn mouse_reporting(&self) -> Option<crate::mouse_forward::MouseReporting> {
        let mode = self.term.mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return None;
        }
        Some(crate::mouse_forward::MouseReporting {
            drag: mode.contains(TermMode::MOUSE_DRAG),
            motion: mode.contains(TermMode::MOUSE_MOTION),
            sgr: mode.contains(TermMode::SGR_MOUSE),
        })
    }

    /// Whether the program in this pane has asked for bracketed paste
    /// (`?2004h`): pasted text wrapped in markers, so it can tell a paste
    /// from typing and keep its line breaks as text rather than as Enter.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Resizes the grid to `cols` x `rows`, same clamping as [`PaneTerm::new`].
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let size = Size {
            cols: cols.max(MIN_COLS) as usize,
            lines: rows.max(MIN_ROWS) as usize,
        };
        self.term.resize(size);
    }

    /// The visible grid as plain text, one `String` per row, in display order
    /// top to bottom — which means *scrolled* order: a pane scrolled back into
    /// its history reads out the history, not the live edge.
    ///
    /// Dead code in the production build until drag-select shipped;
    /// `ui::selection::Selection::text` now reads the copied text out through
    /// it, which is also what makes the scroll translation below load-bearing
    /// rather than merely tidy — copying from a scrolled-back pane has to
    /// yield what is on screen.
    pub fn visible_lines(&self) -> Vec<String> {
        let grid = self.term.grid();
        let cols = grid.columns();
        // `Index<Line>` is screen-relative and ignores the display offset, so
        // a scrolled-back view has to be translated here — the same
        // arithmetic alacritty's own `display_iter` does.
        let offset = grid.display_offset() as i32;
        (0..grid.screen_lines())
            .map(|row| {
                let mut line = String::with_capacity(cols);
                for cell in &grid[Line(row as i32 - offset)] {
                    line.push(cell.c);
                }
                line
            })
            .collect()
    }

    /// Where the cursor currently sits in the visible grid, as
    /// `(column, row)` zero-based offsets from the grid's top-left — the
    /// coordinates a caller adds to its own pane-area origin to place a
    /// real terminal cursor (`ui::frame::render` does exactly that).
    ///
    /// `None` when the pane has hidden its cursor (`DECTCEM`, i.e. the
    /// `TermMode::SHOW_CURSOR` flag off — full-screen programs do this
    /// constantly while redrawing) or when the reported position isn't
    /// inside the visible grid at all. Every conversion below is checked:
    /// the cursor point comes from the VTE parser's own state, which is
    /// driven by untrusted pane bytes, so a negative or oversized
    /// coordinate degrades to "no cursor" rather than panicking on an
    /// `as`-cast or an out-of-range index.
    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        if !self.term.mode().contains(TermMode::SHOW_CURSOR) {
            return None;
        }
        // Scrolled back into history: the cursor belongs to the live edge,
        // which is not what is on screen. Drawing it over a history row would
        // advertise an input point that is not there — the same reasoning that
        // hides it while `detached_pane`.
        if self.scrollback_offset() > 0 {
            return None;
        }
        let grid = self.term.grid();
        let point = grid.cursor.point;
        let row = u16::try_from(point.line.0).ok()?;
        let col = u16::try_from(point.column.0).ok()?;
        if usize::from(row) >= grid.screen_lines() || usize::from(col) >= grid.columns() {
            return None;
        }
        Some((col, row))
    }

    /// Renders the visible grid into `buf` within `area`, mapping each
    /// cell's foreground/background to a [`ratatui::style::Color`]. `area`
    /// is caller-supplied and may extend beyond both the grid and `buf`'s
    /// own area in either dimension — cells that fall outside either are
    /// skipped via [`Buffer::cell_mut`] rather than indexed, since indexing
    /// panics on an out-of-range position.
    pub fn render_into(&self, area: Rect, buf: &mut Buffer) {
        let grid = self.term.grid();
        let rows = grid.screen_lines().min(area.height as usize);
        let cols = grid.columns().min(area.width as usize);
        // Same translation `visible_lines` makes, and for the same reason:
        // without it the view never moves however far the offset is scrolled.
        let offset = grid.display_offset() as i32;

        for row in 0..rows {
            let Some(y) = area.y.checked_add(row as u16) else {
                continue;
            };
            for (col, cell) in (&grid[Line(row as i32 - offset)])
                .into_iter()
                .take(cols)
                .enumerate()
            {
                let Some(x) = area.x.checked_add(col as u16) else {
                    continue;
                };
                let Some(buf_cell) = buf.cell_mut(Position { x, y }) else {
                    continue;
                };
                buf_cell.set_char(cell.c);
                buf_cell.set_fg(to_ratatui_color(cell.fg));
                buf_cell.set_bg(to_ratatui_color(cell.bg));
                // Assigned, not merged: the cell is the sole authority on its
                // attributes, and whatever was drawn in this buffer region
                // before the pane must not bleed through.
                buf_cell.modifier = to_ratatui_modifier(cell.flags);
            }
        }
    }
}

#[cfg(test)]
mod scrollback_tests {
    use super::*;

    /// A 20x4 grid with ten numbered lines fed through it, so six have
    /// scrolled off the top.
    fn scrolled_past() -> PaneTerm {
        let mut t = PaneTerm::new(20, 4);
        for n in 0..10 {
            t.feed(format!("line{n}\r\n").as_bytes());
        }
        t
    }

    fn visible(t: &PaneTerm) -> String {
        t.visible_lines().join("\n")
    }

    #[test]
    fn output_that_scrolled_off_the_top_can_be_scrolled_back_to() {
        // The buffer has always held 10,000 lines; nothing could reach it.
        let mut t = scrolled_past();
        assert!(
            !visible(&t).contains("line0"),
            "precondition: {}",
            visible(&t)
        );
        assert_eq!(t.scrollback_offset(), 0, "starts at the live edge");

        t.scroll(6);
        assert_eq!(t.scrollback_offset(), 6);
        assert!(visible(&t).contains("line1"), "{}", visible(&t));

        // All the way back reaches the very first line written. Not `scroll(7)`
        // — the exact offset depends on the trailing newline leaving an empty
        // eleventh line, which is arithmetic about the fixture rather than
        // about scrolling.
        t.scroll(i32::MAX);
        assert!(visible(&t).contains("line0"), "{}", visible(&t));
    }

    #[test]
    fn scrolling_forward_returns_to_the_live_output() {
        let mut t = scrolled_past();
        t.scroll(6);
        t.scroll(-6);
        assert_eq!(t.scrollback_offset(), 0);
        assert!(visible(&t).contains("line9"), "{}", visible(&t));
    }

    #[test]
    fn scrolling_stops_at_both_ends_rather_than_running_off() {
        let mut t = scrolled_past();
        t.scroll(i32::MAX);
        let oldest = t.scrollback_offset();
        assert!(oldest > 0);
        t.scroll(i32::MAX);
        assert_eq!(
            t.scrollback_offset(),
            oldest,
            "cannot go past the oldest line"
        );

        t.scroll(i32::MIN);
        assert_eq!(t.scrollback_offset(), 0);
        t.scroll(i32::MIN);
        assert_eq!(t.scrollback_offset(), 0, "nor past the live edge");
    }

    #[test]
    fn typing_snaps_the_view_back_to_the_live_edge() {
        // A keystroke whose echo lands off-screen is the worst of both modes.
        let mut t = scrolled_past();
        t.scroll(6);
        assert!(t.scrollback_offset() > 0);
        t.scroll_to_bottom();
        assert_eq!(t.scrollback_offset(), 0);
        assert!(visible(&t).contains("line9"), "{}", visible(&t));
    }

    #[test]
    fn a_scrolled_back_pane_shows_no_cursor() {
        // The cursor belongs to the live edge, which is not what is on screen;
        // drawing it over a history row advertises an input point that is not
        // there.
        let mut t = scrolled_past();
        assert!(t.cursor_position().is_some(), "precondition");
        t.scroll(3);
        assert_eq!(t.cursor_position(), None);
        t.scroll_to_bottom();
        assert!(t.cursor_position().is_some(), "and it comes back");
    }

    #[test]
    fn what_is_rendered_matches_what_is_read_out_while_scrolled() {
        // `render_into` and `visible_lines` translate the display offset
        // independently; copying from a scrolled-back pane has to yield what
        // is on screen, so the two must agree.
        let mut t = scrolled_past();
        t.scroll(4);
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        t.render_into(area, &mut buf);
        let drawn: Vec<String> = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(drawn, t.visible_lines());
    }

    #[test]
    fn scrolling_a_pane_with_no_history_yet_does_nothing_and_never_panics() {
        let mut t = PaneTerm::new(20, 4);
        t.feed(b"just one line");
        for delta in [1, -1, i32::MAX, i32::MIN, 0] {
            t.scroll(delta);
            assert_eq!(t.scrollback_offset(), 0, "delta {delta}");
        }
        assert!(visible(&t).contains("just one line"));
    }
}

/// Maps an `alacritty_terminal` cell's SGR flags to `ratatui` modifiers.
/// Layout-bookkeeping flags (`WRAPLINE`, the wide-char markers) have no
/// visual meaning and are ignored; every underline variant folds onto
/// `UNDERLINED`, the only kind `ratatui` can express.
fn to_ratatui_modifier(flags: CellFlags) -> Modifier {
    let mut modifier = Modifier::empty();
    if flags.contains(CellFlags::BOLD) {
        modifier |= Modifier::BOLD;
    }
    if flags.contains(CellFlags::DIM) {
        modifier |= Modifier::DIM;
    }
    if flags.contains(CellFlags::ITALIC) {
        modifier |= Modifier::ITALIC;
    }
    if flags.intersects(CellFlags::ALL_UNDERLINES) {
        modifier |= Modifier::UNDERLINED;
    }
    if flags.contains(CellFlags::INVERSE) {
        modifier |= Modifier::REVERSED;
    }
    if flags.contains(CellFlags::HIDDEN) {
        modifier |= Modifier::HIDDEN;
    }
    if flags.contains(CellFlags::STRIKEOUT) {
        modifier |= Modifier::CROSSED_OUT;
    }
    modifier
}

/// Maps an `alacritty_terminal` cell color to its `ratatui` equivalent.
/// `NamedColor` carries alacritty's dim/bright variants that `ratatui`
/// doesn't distinguish (e.g. `DimRed`/`BrightRed` both being shades of
/// red) — those fold onto the closest plain ANSI color rather than
/// growing `ratatui::style::Color` a case it doesn't have.
fn to_ratatui_color(color: AnsiColor) -> RColor {
    match color {
        AnsiColor::Spec(rgb) => RColor::Rgb(rgb.r, rgb.g, rgb.b),
        AnsiColor::Indexed(i) => RColor::Indexed(i),
        AnsiColor::Named(named) => match named {
            NamedColor::Black | NamedColor::DimBlack => RColor::Black,
            NamedColor::Red | NamedColor::DimRed => RColor::Red,
            NamedColor::Green | NamedColor::DimGreen => RColor::Green,
            NamedColor::Yellow | NamedColor::DimYellow => RColor::Yellow,
            NamedColor::Blue | NamedColor::DimBlue => RColor::Blue,
            NamedColor::Magenta | NamedColor::DimMagenta => RColor::Magenta,
            NamedColor::Cyan | NamedColor::DimCyan => RColor::Cyan,
            NamedColor::White | NamedColor::DimWhite => RColor::Gray,
            NamedColor::BrightBlack => RColor::DarkGray,
            NamedColor::BrightRed => RColor::LightRed,
            NamedColor::BrightGreen => RColor::LightGreen,
            NamedColor::BrightYellow => RColor::LightYellow,
            NamedColor::BrightBlue => RColor::LightBlue,
            NamedColor::BrightMagenta => RColor::LightMagenta,
            NamedColor::BrightCyan => RColor::LightCyan,
            NamedColor::BrightWhite => RColor::White,
            NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => {
                RColor::Reset
            }
            NamedColor::Background => RColor::Reset,
            NamedColor::Cursor => RColor::Reset,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_reporting_follows_what_the_program_asked_for() {
        let mut t = PaneTerm::new(80, 24);
        assert_eq!(t.mouse_reporting(), None, "nothing until a program asks");
        t.feed(b"\x1b[?1000h\x1b[?1006h");
        let mode = t.mouse_reporting().expect("1000 turns reporting on");
        assert!(mode.sgr && !mode.drag && !mode.motion);
        t.feed(b"\x1b[?1002h");
        assert!(t.mouse_reporting().unwrap().drag);
        t.feed(b"\x1b[?1002l\x1b[?1000l");
        assert_eq!(t.mouse_reporting(), None, "and off again when it stops");
    }

    #[test]
    fn bracketed_paste_follows_what_the_program_asked_for() {
        let mut t = PaneTerm::new(80, 24);
        assert!(!t.bracketed_paste(), "nothing until a program asks");
        t.feed(b"\x1b[?2004h");
        assert!(t.bracketed_paste());
        t.feed(b"\x1b[?2004l");
        assert!(!t.bracketed_paste());
    }

    #[test]
    fn plain_text_lands_in_the_grid() {
        let mut t = PaneTerm::new(20, 4);
        t.feed(b"hello world");
        assert!(t.visible_lines()[0].starts_with("hello world"));
    }

    #[test]
    fn newlines_advance_rows() {
        let mut t = PaneTerm::new(20, 4);
        t.feed(b"one\r\ntwo");
        let lines = t.visible_lines();
        assert!(lines[0].starts_with("one"));
        assert!(lines[1].starts_with("two"));
    }

    #[test]
    fn ansi_colour_sequences_are_consumed_not_printed() {
        let mut t = PaneTerm::new(20, 2);
        t.feed(b"\x1b[31mred\x1b[0m");
        let l = &t.visible_lines()[0];
        assert!(l.starts_with("red"), "got {l:?}");
        assert!(!l.contains('\x1b'), "escape leaked into the text: {l:?}");
    }

    #[test]
    fn cursor_movement_is_honoured() {
        let mut t = PaneTerm::new(20, 4);
        t.feed(b"abc\x1b[1;1Hz");
        assert!(t.visible_lines()[0].starts_with('z'));
    }

    #[test]
    fn output_beyond_the_last_row_scrolls() {
        let mut t = PaneTerm::new(20, 3);
        for i in 1..=10 {
            t.feed(format!("line{i}\r\n").as_bytes());
        }
        let joined = t.visible_lines().join("\n");
        assert!(joined.contains("line10"), "got {joined:?}");
        assert!(
            !joined.contains("line1\n"),
            "old rows should have scrolled off"
        );
    }

    #[test]
    fn resize_changes_the_reported_width() {
        let mut t = PaneTerm::new(20, 3);
        t.resize(60, 10);
        assert_eq!(t.visible_lines().len(), 10);
    }

    #[test]
    fn invalid_utf8_does_not_panic() {
        let mut t = PaneTerm::new(20, 2);
        t.feed(&[0xff, 0xfe, b'o', b'k']);
        let _ = t.visible_lines();
    }

    // --- Additional coverage ---

    #[test]
    fn zero_sized_dimensions_are_clamped_not_panicking() {
        let mut t = PaneTerm::new(0, 0);
        // Must not panic on construction, feed, or read-back.
        t.feed(b"hi");
        let lines = t.visible_lines();
        assert_eq!(lines.len(), MIN_ROWS as usize);
    }

    #[test]
    fn resize_to_zero_is_clamped_not_panicking() {
        let mut t = PaneTerm::new(20, 5);
        t.resize(0, 0);
        let lines = t.visible_lines();
        assert_eq!(lines.len(), MIN_ROWS as usize);
        assert_eq!(lines[0].chars().count(), MIN_COLS as usize);
    }

    #[test]
    fn render_into_with_area_larger_than_grid_does_not_panic() {
        let mut t = PaneTerm::new(5, 2);
        t.feed(b"hi");
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        // area bigger than both the grid and the buffer's own bounds.
        t.render_into(Rect::new(0, 0, 100, 100), &mut buf);
        let cell = buf.cell((0, 0)).expect("cell in bounds");
        assert_eq!(cell.symbol(), "h");
    }

    #[test]
    fn render_into_with_area_smaller_than_grid_clips() {
        let mut t = PaneTerm::new(10, 5);
        t.feed(b"hello");
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 1));
        // Only 3 columns and 1 row are writable; must not panic or write
        // outside that area.
        t.render_into(Rect::new(0, 0, 3, 1), &mut buf);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "h");
        assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "e");
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "l");
    }

    #[test]
    fn render_into_maps_named_colours() {
        let mut t = PaneTerm::new(10, 1);
        t.feed(b"\x1b[31mr\x1b[0m");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        t.render_into(Rect::new(0, 0, 10, 1), &mut buf);
        let cell = buf.cell((0, 0)).expect("cell in bounds");
        assert_eq!(cell.symbol(), "r");
        assert_eq!(cell.fg, RColor::Red);
    }

    #[test]
    fn render_into_maps_dim_text_to_the_dim_modifier() {
        // Claude Code's prompt-suggestion placeholder is SGR 2 (faint) over
        // the default foreground — if the flag is dropped, the hint renders
        // exactly like text the user already typed.
        let mut t = PaneTerm::new(10, 1);
        t.feed(b"\x1b[2mh\x1b[0mx");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        t.render_into(Rect::new(0, 0, 10, 1), &mut buf);
        let hint = buf.cell((0, 0)).expect("cell in bounds");
        assert_eq!(hint.symbol(), "h");
        assert!(hint.modifier.contains(Modifier::DIM), "{:?}", hint.modifier);
        let typed = buf.cell((1, 0)).expect("cell in bounds");
        assert_eq!(typed.symbol(), "x");
        assert_eq!(typed.modifier, Modifier::empty(), "reset must clear it");
    }

    #[test]
    fn render_into_maps_the_other_text_attributes_too() {
        let mut t = PaneTerm::new(10, 1);
        t.feed(b"\x1b[1mb\x1b[0m\x1b[3mi\x1b[0m\x1b[4mu\x1b[0m\x1b[7mr\x1b[0m\x1b[9ms\x1b[0m");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        t.render_into(Rect::new(0, 0, 10, 1), &mut buf);
        let modifier_at = |x: u16| buf.cell((x, 0)).expect("cell in bounds").modifier;
        assert!(modifier_at(0).contains(Modifier::BOLD));
        assert!(modifier_at(1).contains(Modifier::ITALIC));
        assert!(modifier_at(2).contains(Modifier::UNDERLINED));
        assert!(modifier_at(3).contains(Modifier::REVERSED));
        assert!(modifier_at(4).contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn render_into_overwrites_a_stale_modifier_rather_than_accumulating() {
        // The buffer area a pane draws into may hold styling from whatever
        // was rendered there before; a plain cell must clear it, not merge.
        let mut t = PaneTerm::new(10, 1);
        t.feed(b"p");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        buf.cell_mut(Position { x: 0, y: 0 })
            .expect("cell in bounds")
            .modifier
            .insert(Modifier::BOLD);
        t.render_into(Rect::new(0, 0, 10, 1), &mut buf);
        let cell = buf.cell((0, 0)).expect("cell in bounds");
        assert_eq!(cell.symbol(), "p");
        assert_eq!(cell.modifier, Modifier::empty());
    }

    #[test]
    fn render_into_area_offset_from_buffer_origin_is_honoured() {
        let mut t = PaneTerm::new(5, 1);
        t.feed(b"x");
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        t.render_into(Rect::new(2, 3, 5, 1), &mut buf);
        assert_eq!(buf.cell((2, 3)).unwrap().symbol(), "x");
    }
}
