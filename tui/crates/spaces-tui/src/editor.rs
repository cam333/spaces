//! A small text editor for the Files tab.
//!
//! For quick fixes to config and notes on a remote machine, not a
//! replacement for an editor: no undo, no search, no selection. It keeps
//! what the file had — CRLF or LF line endings, a final newline or not, tabs
//! or spaces — so saving an untouched line never rewrites it.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Columns a tab takes on screen.
pub const TAB_WIDTH: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Redraw,
    /// Write the text back (see [`Editor::text`]).
    Save,
    /// Leave the editor.
    Close,
}

#[derive(Debug, Clone)]
pub struct Editor {
    pub path: String,
    pub name: String,
    /// The file's modification time when it was opened or last saved: a
    /// save is refused if the server's copy has changed since.
    pub mtime: f64,
    pub lines: Vec<String>,
    /// Per line: does it end in CRLF? Kept per line, so a file mixing
    /// endings saves back exactly as it was.
    endings: Vec<bool>,
    final_newline: bool,
    indent_with_tabs: bool,
    /// Cursor line, and character (not byte, not column) within it.
    pub row: usize,
    pub col: usize,
    /// First line on screen.
    pub scroll: usize,
    pub dirty: bool,
    /// Esc was pressed with unsaved changes; another Esc discards them.
    pub confirm_discard: bool,
    /// The last save was refused as a conflict; the next one overwrites.
    pub force: bool,
}

impl Editor {
    /// An editor for `bytes`, or why the file cannot be edited here.
    pub fn open(path: &str, mtime: f64, bytes: &[u8]) -> Result<Self, String> {
        if bytes.iter().take(8192).any(|b| *b == 0) {
            return Err("binary files cannot be edited".into());
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| "only UTF-8 text files can be edited".to_string())?;
        let final_newline = text.ends_with('\n');
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines: Vec<String> = Vec::new();
        let mut endings: Vec<bool> = Vec::new();
        for l in body.split('\n') {
            endings.push(l.ends_with('\r'));
            lines.push(l.strip_suffix('\r').unwrap_or(l).to_string());
        }
        if lines.is_empty() {
            lines.push(String::new());
            endings.push(false);
        }
        // The last line only has an ending when the file ends in one; give
        // it the kind the file's other lines use, for when lines follow it.
        if !final_newline {
            let last = endings.len() - 1;
            endings[last] = text.contains("\r\n");
        }
        let indent_with_tabs = lines.iter().any(|l| l.starts_with('\t'));
        Ok(Self {
            path: path.to_string(),
            name: path.rsplit(['/', '\\']).next().unwrap_or(path).to_string(),
            mtime,
            lines,
            endings,
            final_newline,
            indent_with_tabs,
            row: 0,
            col: 0,
            scroll: 0,
            dirty: false,
            confirm_discard: false,
            force: false,
        })
    }

    /// The text to save, each line with its own ending.
    pub fn text(&self) -> String {
        let mut out = String::new();
        let last = self.lines.len() - 1;
        for (i, line) in self.lines.iter().enumerate() {
            out.push_str(line);
            if i < last || self.final_newline {
                out.push_str(if self.endings[i] { "\r\n" } else { "\n" });
            }
        }
        out
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |l| l.chars().count())
    }

    fn byte_at(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    /// The cursor's screen column, tabs expanded.
    pub fn cursor_column(&self) -> usize {
        display_width(self.lines[self.row].chars().take(self.col))
    }

    fn keep_visible(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.row < self.scroll {
            self.scroll = self.row;
        } else if self.row >= self.scroll + rows {
            self.scroll = self.row + 1 - rows;
        }
    }

    fn edited(&mut self) {
        self.dirty = true;
    }

    pub fn handle_key(&mut self, key: KeyEvent, rows: usize) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S')) {
            self.confirm_discard = false;
            return Outcome::Save;
        }
        if key.code == KeyCode::Esc {
            if self.dirty && !self.confirm_discard {
                self.confirm_discard = true;
                return Outcome::Redraw;
            }
            return Outcome::Close;
        }
        self.confirm_discard = false;
        if key.modifiers.contains(KeyModifiers::ALT) && !crate::files::is_altgr_char(&key) {
            return Outcome::None;
        }
        let page = rows.saturating_sub(1).max(1);
        match key.code {
            KeyCode::Up => {
                self.row = self.row.saturating_sub(1);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::Down => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::PageUp => {
                self.row = self.row.saturating_sub(page);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::PageDown => {
                self.row = (self.row + page).min(self.lines.len() - 1);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                }
            }
            KeyCode::Right => {
                if self.col < self.line_len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Home if ctrl => {
                self.row = 0;
                self.col = 0;
            }
            KeyCode::End if ctrl => {
                self.row = self.lines.len() - 1;
                self.col = self.line_len(self.row);
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(self.row),
            KeyCode::Enter => {
                let at = self.byte_at(self.row, self.col);
                let rest = self.lines[self.row].split_off(at);
                // Carry the line's indentation onto the new one.
                let indent: String = self.lines[self.row]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                self.col = indent.chars().count();
                self.lines.insert(self.row + 1, indent + &rest);
                self.endings.insert(self.row + 1, self.endings[self.row]);
                self.row += 1;
                self.edited();
            }
            KeyCode::Backspace => {
                if self.col > 0 {
                    let at = self.byte_at(self.row, self.col - 1);
                    self.lines[self.row].remove(at);
                    self.col -= 1;
                    self.edited();
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    // The joined line ends where the second one did.
                    let ending = self.endings.remove(self.row);
                    self.row -= 1;
                    self.endings[self.row] = ending;
                    self.col = self.line_len(self.row);
                    self.lines[self.row].push_str(&line);
                    self.edited();
                }
            }
            KeyCode::Delete => {
                if self.col < self.line_len(self.row) {
                    let at = self.byte_at(self.row, self.col);
                    self.lines[self.row].remove(at);
                    self.edited();
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    let ending = self.endings.remove(self.row + 1);
                    self.endings[self.row] = ending;
                    self.lines[self.row].push_str(&next);
                    self.edited();
                }
            }
            KeyCode::Tab => {
                let insert = if self.indent_with_tabs {
                    "\t".to_string()
                } else {
                    " ".repeat(TAB_WIDTH)
                };
                let at = self.byte_at(self.row, self.col);
                self.lines[self.row].insert_str(at, &insert);
                self.col += insert.chars().count();
                self.edited();
            }
            KeyCode::Char(c) if !ctrl || crate::files::is_altgr_char(&key) => {
                let at = self.byte_at(self.row, self.col);
                self.lines[self.row].insert(at, c);
                self.col += 1;
                self.edited();
            }
            _ => return Outcome::None,
        }
        self.keep_visible(rows);
        Outcome::Redraw
    }

    /// The wheel: moves the view, and the cursor with it if it would leave.
    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.lines.len().saturating_sub(1);
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
        let rows = rows.max(1);
        if self.row < self.scroll {
            self.row = self.scroll;
        } else if self.row >= self.scroll + rows {
            self.row = self.scroll + rows - 1;
        }
        self.row = self.row.min(max);
        self.col = self.col.min(self.line_len(self.row));
    }
}

/// `line` as drawn: tabs expanded to the next stop, the same way
/// [`display_width`] counts them so the cursor lands on its character, and
/// anything unsafe for a terminal replaced (see `files::sanitize`).
pub fn display_line(line: &str) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in line.chars() {
        if c == '\t' {
            let n = TAB_WIDTH - width % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', n));
            width += n;
        } else {
            out.push_str(&crate::files::sanitize(c.encode_utf8(&mut [0; 4])));
            width += 1;
        }
    }
    out
}

/// Screen width of `chars`, tabs expanded to the next stop.
pub fn display_width(chars: impl Iterator<Item = char>) -> usize {
    chars.fold(0, |w, c| {
        if c == '\t' {
            w + TAB_WIDTH - w % TAB_WIDTH
        } else {
            w + 1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..key(KeyCode::Char(c))
        }
    }

    fn typed(e: &mut Editor, s: &str) {
        for c in s.chars() {
            e.handle_key(key(KeyCode::Char(c)), 20);
        }
    }

    #[test]
    fn an_untouched_file_saves_byte_for_byte() {
        for text in [
            "a\nb\n",
            "a\r\nb\r\n",
            "no newline",
            "",
            "\n\n",
            "tab\tin\n",
        ] {
            let e = Editor::open("/x", 0.0, text.as_bytes()).unwrap();
            assert_eq!(e.text(), text, "{text:?}");
        }
    }

    #[test]
    fn typing_inserts_and_keeps_crlf() {
        let mut e = Editor::open("/x", 0.0, b"hello\r\nworld\r\n").unwrap();
        e.handle_key(key(KeyCode::End), 20);
        typed(&mut e, "!");
        e.handle_key(key(KeyCode::Enter), 20);
        typed(&mut e, "new");
        assert_eq!(e.text(), "hello!\r\nnew\r\nworld\r\n");
        assert!(e.dirty);
    }

    #[test]
    fn enter_keeps_indentation_and_backspace_joins_lines() {
        let mut e = Editor::open("/x", 0.0, b"    fn a() {\n").unwrap();
        e.handle_key(key(KeyCode::End), 20);
        e.handle_key(key(KeyCode::Enter), 20);
        typed(&mut e, "x");
        assert_eq!(e.text(), "    fn a() {\n    x\n");
        for _ in 0..6 {
            e.handle_key(key(KeyCode::Backspace), 20);
        }
        assert_eq!(e.text(), "    fn a() {\n");
    }

    #[test]
    fn delete_joins_with_the_next_line() {
        let mut e = Editor::open("/x", 0.0, b"ab\ncd\n").unwrap();
        e.handle_key(key(KeyCode::End), 20);
        e.handle_key(key(KeyCode::Delete), 20);
        assert_eq!(e.text(), "abcd\n");
    }

    #[test]
    fn tab_follows_the_files_own_indentation() {
        let mut spaces = Editor::open("/x", 0.0, b"a\n").unwrap();
        spaces.handle_key(key(KeyCode::Tab), 20);
        assert_eq!(spaces.text(), "    a\n");
        let mut tabs = Editor::open("/x", 0.0, b"\tb\n").unwrap();
        tabs.handle_key(key(KeyCode::Tab), 20);
        assert_eq!(tabs.text(), "\t\tb\n");
    }

    #[test]
    fn multibyte_characters_edit_by_character() {
        let mut e = Editor::open("/x", 0.0, "héllo".as_bytes()).unwrap();
        e.handle_key(key(KeyCode::Right), 20);
        e.handle_key(key(KeyCode::Right), 20);
        e.handle_key(key(KeyCode::Backspace), 20);
        assert_eq!(e.text(), "hllo");
    }

    #[test]
    fn ctrl_s_saves_and_esc_guards_unsaved_changes() {
        let mut e = Editor::open("/x", 0.0, b"a\n").unwrap();
        assert_eq!(
            e.handle_key(key(KeyCode::Esc), 20),
            Outcome::Close,
            "nothing to lose"
        );
        typed(&mut e, "z");
        assert_eq!(e.handle_key(key(KeyCode::Esc), 20), Outcome::Redraw);
        assert!(e.confirm_discard);
        assert_eq!(e.handle_key(ctrl('s'), 20), Outcome::Save);
        assert!(!e.confirm_discard);
        e.handle_key(key(KeyCode::Esc), 20);
        assert_eq!(e.handle_key(key(KeyCode::Esc), 20), Outcome::Close);
    }

    #[test]
    fn binary_and_non_utf8_files_are_refused() {
        assert!(Editor::open("/x", 0.0, b"a\0b").is_err());
        assert!(Editor::open("/x", 0.0, &[0xff, 0xfe, 0x41]).is_err());
    }

    #[test]
    fn the_cursor_stays_on_screen() {
        let text: String = (0..100).map(|i| format!("{i}\n")).collect();
        let mut e = Editor::open("/x", 0.0, text.as_bytes()).unwrap();
        for _ in 0..30 {
            e.handle_key(key(KeyCode::Down), 10);
        }
        assert_eq!(e.row, 30);
        assert!(e.row >= e.scroll && e.row < e.scroll + 10);
    }

    #[test]
    fn tabs_count_to_the_next_stop() {
        assert_eq!(display_width("a\tb".chars()), 5);
        assert_eq!(display_width("\t".chars()), 4);
    }

    #[test]
    fn mixed_line_endings_save_back_exactly() {
        for text in ["a\r\nb\nc\r\n", "a\nb\r\nc", "one\r\ntwo"] {
            let e = Editor::open("/x", 0.0, text.as_bytes()).unwrap();
            assert_eq!(e.text(), text, "{text:?}");
        }
        let mut e = Editor::open("/x", 0.0, b"a\r\nb\n").unwrap();
        e.handle_key(key(KeyCode::End), 20);
        e.handle_key(key(KeyCode::Enter), 20);
        typed(&mut e, "new");
        assert_eq!(e.text(), "a\r\nnew\r\nb\n", "a split line keeps its ending");
        e.handle_key(key(KeyCode::Home), 20);
        e.handle_key(key(KeyCode::Backspace), 20);
        assert_eq!(e.text(), "anew\r\nb\n");
    }

    #[test]
    fn altgr_characters_are_typed() {
        let mut e = Editor::open("/x", 0.0, b"").unwrap();
        for c in ['\\', '@', '{'] {
            e.handle_key(
                KeyEvent {
                    modifiers: KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ..key(KeyCode::Char(c))
                },
                20,
            );
        }
        assert_eq!(e.text(), "\\@{");
    }

    #[test]
    fn a_line_is_drawn_with_the_tab_stops_the_cursor_counts() {
        assert_eq!(display_line("ab\tc"), "ab  c");
        assert_eq!(display_width("ab\t".chars()), 4, "c sits at column 4");
        assert_eq!(display_line("\u{1b}x"), "\u{FFFD}x");
    }
}
