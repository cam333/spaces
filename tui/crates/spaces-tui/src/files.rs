//! The Files tab: a browser and viewer for the filesystem of the machine a
//! space lives on.
//!
//! Pure state and key handling. Nothing here talks to the server: a key that
//! needs data turns into an [`Effect`], `main.rs` performs it with the
//! `fs.*` protocol methods, and the answer comes back through
//! [`FilesView::set_listing`] / [`FilesView::set_preview`]. That keeps every
//! rule below testable without a connection.
//!
//! The tab is not a pane. Panes are server-side terminals addressed by id in
//! `Model::focused`/`split`, and a dozen places assume an id there has a
//! socket behind it. A Files tab lives in `Model::files` instead, keyed by
//! space, and is shown through `Model::files_shown`.

use std::sync::OnceLock;

use base64::Engine;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// How much of a file the viewer reads: one `fs.read` chunk.
pub const PREVIEW_BYTES: u64 = 1024 * 1024;
/// How much the preview reads while the selection moves over a file — the
/// start of it, quickly. Enter reads up to [`PREVIEW_BYTES`].
pub const PEEK_BYTES: u64 = 64 * 1024;
/// How long the selection rests on a file before it is previewed, so holding
/// an arrow key does not send a read per row.
pub const PEEK_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// Highlighting is sequential from the top; past this many lines the rest
/// is shown plain so a huge file cannot stall the loop.
const HIGHLIGHT_MAX_LINES: usize = 5000;
/// Rows a page key moves when the viewport is unknown.
const DEFAULT_PAGE: usize = 20;
/// Columns Left/Right move the viewer sideways.
const HSCROLL_STEP: usize = 8;
/// Rows one wheel notch moves.
const WHEEL_ROWS: usize = 3;

/// Files that open in an app on the user's own machine rather than inline.
const EXTERNAL_EXTS: &[&str] = &[
    // Not SVG: it opens in a browser, which runs any script inside it.
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "pdf", "heic",
];

/// True for the types "open on this machine" may launch: viewable, never
/// executable.
pub fn opens_locally(ext: &str) -> bool {
    EXTERNAL_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// A question the tab is asking in its footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// The local file to upload into the folder on screen.
    Upload(String),
    /// The upload's name is taken there: replace it?
    Overwrite { local: String },
    /// Several dropped at once, `taken` of them already here: replace
    /// those (y), upload only the others (n), or nothing (esc).
    OverwriteMany {
        locals: Vec<String>,
        taken: Vec<String>,
    },
    /// A name for a new folder in the folder on screen.
    NewFolder(String),
    /// A new name (or a path, to move it) for `from`.
    Rename { from: String, text: String },
    /// Delete `path`? `is_dir` says it takes everything inside with it.
    Delete {
        path: String,
        name: String,
        is_dir: bool,
    },
}

/// How the Files tab shares the pane area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesLayout {
    /// The whole pane area; terminals are hidden but stay attached.
    Full,
    /// Files on the left, the focused terminal on the right.
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    LinkDir,
    LinkFile,
    Broken,
    Other,
    /// The `..` row: the folder above. Never a target for d, m, x or e.
    Up,
}

impl EntryKind {
    fn from_wire(s: &str) -> Self {
        match s {
            "dir" => Self::Dir,
            "file" => Self::File,
            "link-dir" => Self::LinkDir,
            "link-file" => Self::LinkFile,
            "link-broken" => Self::Broken,
            _ => Self::Other,
        }
    }

    /// A real folder. Not [`EntryKind::Up`], which only navigates.
    pub fn is_dir(self) -> bool {
        matches!(self, Self::Dir | Self::LinkDir)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: f64,
}

/// One folder, as `fs.list` returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub current: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

impl Listing {
    /// Parses an `fs.list` result; `None` for anything malformed.
    pub fn from_wire(v: &serde_json::Value) -> Option<Self> {
        let text = |v: &serde_json::Value| v.as_str().map(str::to_string);
        Some(Self {
            current: text(v.get("current")?)?,
            parent: v.get("parent").and_then(text),
            truncated: v
                .get("truncated")
                .and_then(|t| t.as_bool())
                .unwrap_or(false),
            entries: v
                .get("entries")?
                .as_array()?
                .iter()
                .filter_map(|e| {
                    Some(Entry {
                        name: text(e.get("name")?)?,
                        path: text(e.get("path")?)?,
                        kind: EntryKind::from_wire(e.get("type")?.as_str()?),
                        size: e.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                        mtime: e.get("mtime").and_then(|m| m.as_f64()).unwrap_or(0.0),
                    })
                })
                .collect(),
        })
    }
}

#[derive(Debug, Clone)]
pub enum PreviewBody {
    Text {
        lines: Vec<Line<'static>>,
        /// Only the first `loaded` bytes of the file are shown.
        truncated: bool,
        loaded: u64,
    },
    Binary,
    /// An image or PDF: shown by opening it on the user's machine.
    External,
}

#[derive(Debug, Clone)]
pub struct Preview {
    /// What `d`, `o` and `e` act on while the viewer has the keys.
    pub path: String,
    pub name: String,
    pub size: u64,
    pub body: PreviewBody,
    pub scroll: usize,
    pub hscroll: usize,
}

/// Which half of the tab the keys move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    List,
    Preview,
}

/// One space's Files tab.
#[derive(Debug, Clone, Default)]
pub struct FilesView {
    /// The machine the space lives on — where every `fs.*` call goes.
    /// `None` for the machine this client is connected to.
    pub node: Option<String>,
    pub listing: Option<Listing>,
    /// Index into [`FilesView::visible`].
    pub selected: usize,
    pub list_scroll: usize,
    pub filter: String,
    /// The filter box has the keys.
    pub filtering: bool,
    pub preview: Option<Preview>,
    pub side: Side,
    /// The last error, shown in the footer until the next action.
    pub status: Option<String>,
    /// The last thing that went right, or a transfer's progress.
    pub note: Option<String>,
    pub prompt: Option<Prompt>,
    /// The file being edited, which takes the whole tab while it is open.
    pub editor: Option<crate::editor::Editor>,
    /// The `..` row for the folder on screen, when it has a parent.
    pub up: Option<Entry>,
    /// The folders the browser may reach, for which breadcrumbs click.
    pub roots: Vec<String>,
    /// A file to preview once the selection has rested on it.
    pub peek: Option<(String, std::time::Instant)>,
}

/// What a key or click needs from outside this module.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    Redraw,
    /// List this folder (`fs.list`).
    List(String),
    /// List the first allowed folder.
    Root,
    /// Read the start of this file for the viewer (`fs.read`).
    Read(String),
    /// Read just the start of a file for the preview beside the list,
    /// leaving the keys with the list.
    Peek(String),
    /// Leave the tab: hand the keys back to the terminal.
    Hide,
    /// Save this file into the local Downloads folder.
    Download {
        path: String,
        size: u64,
    },
    /// Save this whole folder into the local Downloads folder.
    DownloadFolder(String),
    /// Fetch this image or PDF and open it in its default app here.
    OpenLocal {
        path: String,
        size: u64,
    },
    /// Send a local file into the folder `dir`.
    Upload {
        local: String,
        dir: String,
        overwrite: bool,
    },
    /// Send several local files or folders into `dir`, one after another.
    UploadMany {
        locals: Vec<String>,
        dir: String,
        overwrite: bool,
    },
    /// Create this folder (`fs.mkdir`).
    Mkdir(String),
    /// Rename or move (`fs.move`).
    Move {
        from: String,
        to: String,
    },
    /// Delete this file or folder (`fs.delete`).
    Delete(String),
    /// Read this file and open it in the editor.
    Edit(String),
    /// Write the editor's text back. `expect_mtime` refuses the save if the
    /// file changed on the server since it was opened; `None` overwrites.
    Save {
        path: String,
        text: String,
        expect_mtime: Option<f64>,
    },
}

impl FilesView {
    pub fn new(node: Option<String>) -> Self {
        Self {
            node,
            ..Self::default()
        }
    }

    /// The rows of the list: `..` (unless filtering), then the entries the
    /// filter lets through, in listing order.
    pub fn visible(&self) -> Vec<&Entry> {
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        let needle = self.filter.to_lowercase();
        self.up
            .iter()
            .filter(|_| needle.is_empty())
            .chain(
                listing
                    .entries
                    .iter()
                    .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle)),
            )
            .collect()
    }

    /// The selected row, `..` included.
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }

    /// The selected row when it is a real file or folder — what d, m, x and
    /// e act on. Never `..`.
    pub fn selected_real(&self) -> Option<&Entry> {
        self.selected_entry().filter(|e| e.kind != EntryKind::Up)
    }

    /// Shows a new folder. Going up selects the folder just left, so
    /// backing out and back in is two keystrokes.
    pub fn set_listing(&mut self, listing: Listing) {
        let came_from = self.listing.as_ref().map(|l| l.current.clone());
        // The same folder again (a refresh after an upload or a change):
        // keep the filter being typed and the selection, by path. Clearing
        // them mid-typing turned the rest of the typing into commands.
        if came_from.as_deref() == Some(listing.current.as_str()) {
            let selected = self.selected_entry().map(|e| e.path.clone());
            self.listing = Some(listing);
            let count = self.visible().len();
            self.selected = selected
                .and_then(|p| self.visible().iter().position(|e| e.path == p))
                .unwrap_or(self.selected)
                .min(count.saturating_sub(1));
            return;
        }
        self.filter.clear();
        self.filtering = false;
        self.up = listing.parent.clone().map(|parent| Entry {
            name: "..".into(),
            path: parent,
            kind: EntryKind::Up,
            size: 0,
            mtime: 0.0,
        });
        self.list_scroll = 0;
        self.listing = Some(listing);
        // Back up a level: the folder just left. Into a folder: its first
        // entry, not `..`, so enter goes further in rather than back out.
        let first_real = usize::from(self.up.is_some()).min(self.visible().len().saturating_sub(1));
        self.selected = came_from
            // Never `..`: going into a folder, its `..` is the folder just
            // left, and selecting it would make enter go straight back out.
            .and_then(|from| {
                self.visible()
                    .iter()
                    .position(|e| e.path == from && e.kind != EntryKind::Up)
            })
            .unwrap_or(first_real);
        self.status = None;
        self.peek = None;
        self.schedule_peek();
    }

    /// After the selection moves: preview the file under it shortly, show
    /// an image's or PDF's note at once, or clear the viewer for a folder.
    fn schedule_peek(&mut self) {
        self.peek = None;
        let Some(e) = self.selected_entry() else {
            return;
        };
        if !matches!(e.kind, EntryKind::File | EntryKind::LinkFile) {
            if self.side == Side::List {
                self.preview = None;
            }
            return;
        }
        if self.preview.as_ref().is_some_and(|p| p.path == e.path) {
            return;
        }
        let (path, size) = (e.path.clone(), e.size);
        if ext_of(&path).is_some_and(|x| opens_locally(&x)) {
            self.set_preview(&path, size, &[]);
            return;
        }
        self.peek = Some((path, std::time::Instant::now() + PEEK_DELAY));
    }

    /// The file whose preview is due by `now`, taken.
    pub fn due_peek(&mut self, now: std::time::Instant) -> Option<String> {
        match &self.peek {
            Some((_, due)) if *due <= now => self.peek.take().map(|(path, _)| path),
            _ => None,
        }
    }

    /// Where each part of the header path sits, and the folder each opens:
    /// `None` for parts above the reachable folders, which do not click.
    pub fn crumbs(&self, header: Rect) -> Vec<(Rect, String, Option<String>)> {
        let Some(current) = self.listing.as_ref().map(|l| l.current.as_str()) else {
            return Vec::new();
        };
        let sep = if current.contains('\\') && !current.contains('/') {
            '\\'
        } else {
            '/'
        };
        // Each part with the path up to and including it.
        let mut parts: Vec<(String, String)> = Vec::new();
        let mut so_far = String::new();
        for (i, piece) in current.split(sep).enumerate() {
            if piece.is_empty() {
                if i == 0 {
                    so_far.push(sep);
                }
                continue;
            }
            if !so_far.is_empty() && !so_far.ends_with(sep) {
                so_far.push(sep);
            }
            so_far.push_str(piece);
            parts.push((piece.to_string(), so_far.clone()));
        }
        let reachable = |p: &str| {
            self.roots.iter().any(|r| {
                // A root of `/` or `C:\` already ends in the separator.
                p == r
                    || p.strip_prefix(r.as_str())
                        .is_some_and(|rest| r.ends_with(sep) || rest.starts_with(sep))
            })
        };
        // Laid out from the right, so the end of a long path stays visible.
        let start = header.x + 4; // after the folder icon
        let width = header.width.saturating_sub(4);
        // An absolute path starts with its root, drawn as the separator.
        if current.starts_with(sep) {
            parts.insert(0, (String::new(), sep.to_string()));
        }
        let last = parts.len().saturating_sub(1);
        let mut out = Vec::new();
        let mut used: u16 = 0;
        for (i, (label, path)) in parts.iter().enumerate().rev() {
            // The root part is the separator itself; every other part is
            // followed by one, except where you are.
            let text = if label.is_empty() {
                sep.to_string()
            } else if i == last {
                sanitize(label)
            } else {
                format!("{}{sep}", sanitize(label))
            };
            let w = text.chars().count() as u16;
            if used + w > width {
                break;
            }
            used += w;
            let click = reachable(path).then(|| path.clone());
            out.push((Rect::new(0, header.y, w, 1), text, click));
        }
        out.reverse();
        let mut x = start;
        for (rect, _, _) in out.iter_mut() {
            rect.x = x;
            x += rect.width;
        }
        out
    }

    /// Opens a file just read in the editor, or says why it cannot be.
    pub fn start_editing(&mut self, path: &str, size: u64, mtime: f64, bytes: &[u8]) {
        if size > bytes.len() as u64 {
            self.status = Some(format!(
                "only files up to {} can be edited here",
                human_size(PREVIEW_BYTES)
            ));
            return;
        }
        match crate::editor::Editor::open(path, mtime, bytes) {
            Ok(editor) => {
                self.editor = Some(editor);
                self.status = None;
                self.note = None;
            }
            Err(why) => self.status = Some(why),
        }
    }

    /// A save went through: the editor is clean again, and the viewer shows
    /// what was saved.
    pub fn saved(&mut self, size: u64, mtime: f64) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        editor.dirty = false;
        editor.force = false;
        editor.mtime = mtime;
        let (path, text) = (editor.path.clone(), editor.text());
        self.note = Some(format!("saved {}", human_size(size)));
        self.status = None;
        if self.preview.as_ref().is_some_and(|p| p.path == path) {
            let side = self.side;
            self.set_preview(&path, size, text.as_bytes());
            self.side = side;
            self.note = Some(format!("saved {}", human_size(size)));
        }
    }

    /// Shows the first `bytes` of a file. `size` is the whole file's.
    pub fn set_preview(&mut self, path: &str, size: u64, bytes: &[u8]) {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase());
        let body = if ext.as_deref().is_some_and(|e| EXTERNAL_EXTS.contains(&e)) {
            PreviewBody::External
        } else if looks_binary(bytes) {
            PreviewBody::Binary
        } else {
            PreviewBody::Text {
                lines: highlight(&name, &String::from_utf8_lossy(bytes)),
                truncated: size > bytes.len() as u64,
                loaded: bytes.len() as u64,
            }
        };
        self.preview = Some(Preview {
            path: path.to_string(),
            name,
            size,
            body,
            scroll: 0,
            hscroll: 0,
        });
        self.status = None;
    }

    /// Keeps the selection inside the list and on screen.
    fn clamp(&mut self, rows: usize) {
        let count = self.visible().len();
        self.selected = self.selected.min(count.saturating_sub(1));
        let rows = rows.max(1);
        if self.selected < self.list_scroll {
            self.list_scroll = self.selected;
        } else if self.selected >= self.list_scroll + rows {
            self.list_scroll = self.selected + 1 - rows;
        }
    }

    fn move_selection(&mut self, delta: isize, rows: usize) -> Effect {
        let count = self.visible().len();
        if count == 0 {
            return Effect::None;
        }
        self.selected = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        self.clamp(rows);
        self.schedule_peek();
        Effect::Redraw
    }

    /// Enter on the selected entry: into a folder, or open a file.
    fn open_selected(&mut self) -> Effect {
        match self.selected_entry() {
            Some(e) if e.kind.is_dir() || e.kind == EntryKind::Up => Effect::List(e.path.clone()),
            Some(e) if matches!(e.kind, EntryKind::File | EntryKind::LinkFile) => {
                if ext_of(&e.name).is_some_and(|x| opens_locally(&x)) {
                    return Effect::OpenLocal {
                        path: e.path.clone(),
                        size: e.size,
                    };
                }
                let path = e.path.clone();
                self.side = Side::Preview;
                // The full read supersedes any preview still waiting to fire.
                self.peek = None;
                Effect::Read(path)
            }
            Some(_) => {
                self.status = Some("not a file or folder".into());
                Effect::Redraw
            }
            None => Effect::None,
        }
    }

    fn go_up(&self) -> Effect {
        match self.listing.as_ref().and_then(|l| l.parent.clone()) {
            Some(parent) => Effect::List(parent),
            None => Effect::None,
        }
    }

    /// One key, while the tab has the keyboard. `rows` is how many list
    /// rows are on screen, for paging and keeping the selection visible.
    pub fn handle_key(&mut self, key: KeyEvent, rows: usize) -> Effect {
        if let Some(editor) = self.editor.as_mut() {
            return match editor.handle_key(key, rows) {
                crate::editor::Outcome::None => Effect::None,
                crate::editor::Outcome::Redraw => Effect::Redraw,
                crate::editor::Outcome::Close => {
                    self.editor = None;
                    self.status = None;
                    Effect::Redraw
                }
                crate::editor::Outcome::Save => Effect::Save {
                    path: editor.path.clone(),
                    text: editor.text(),
                    expect_mtime: (!editor.force).then_some(editor.mtime),
                },
            };
        }
        let page = if rows > 1 { rows - 1 } else { DEFAULT_PAGE };
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && !is_altgr_char(&key)
        {
            return Effect::None;
        }
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        match key.code {
            KeyCode::Char('d') if !self.filtering => return self.transfer_target(false),
            KeyCode::Char('o') if !self.filtering => return self.transfer_target(true),
            KeyCode::Char('e') if !self.filtering => {
                let target = if self.side == Side::Preview {
                    self.preview
                        .as_ref()
                        .filter(|p| matches!(p.body, PreviewBody::Text { .. }))
                        .map(|p| p.path.clone())
                } else {
                    self.selected_real()
                        .filter(|e| matches!(e.kind, EntryKind::File | EntryKind::LinkFile))
                        .map(|e| e.path.clone())
                };
                return match target {
                    Some(path) => Effect::Edit(path),
                    None => {
                        self.status = Some("select a text file to edit".into());
                        Effect::Redraw
                    }
                };
            }
            KeyCode::Char('u') if !self.filtering => {
                self.prompt = Some(Prompt::Upload(String::new()));
                self.status = None;
                return Effect::Redraw;
            }
            KeyCode::Char('n') if !self.filtering && self.listing.is_some() => {
                self.prompt = Some(Prompt::NewFolder(String::new()));
                self.status = None;
                return Effect::Redraw;
            }
            KeyCode::Char('m') | KeyCode::F(2) if !self.filtering && self.side == Side::List => {
                if let Some(e) = self.selected_real() {
                    self.prompt = Some(Prompt::Rename {
                        from: e.path.clone(),
                        text: e.name.clone(),
                    });
                    self.status = None;
                    return Effect::Redraw;
                }
                return Effect::None;
            }
            KeyCode::Delete | KeyCode::Char('x') if !self.filtering && self.side == Side::List => {
                if let Some(e) = self.selected_real() {
                    self.prompt = Some(Prompt::Delete {
                        path: e.path.clone(),
                        name: e.name.clone(),
                        is_dir: e.kind.is_dir(),
                    });
                    self.status = None;
                    return Effect::Redraw;
                }
                return Effect::None;
            }
            _ => {}
        }
        if self.filtering {
            match key.code {
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected = 0;
                    self.list_scroll = 0;
                }
                KeyCode::Backspace => {
                    if self.filter.pop().is_none() {
                        self.filtering = false;
                    }
                    self.selected = 0;
                    self.list_scroll = 0;
                }
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                }
                KeyCode::Enter => {
                    self.filtering = false;
                    return self.open_selected();
                }
                KeyCode::Up => return self.move_selection(-1, rows),
                KeyCode::Down => return self.move_selection(1, rows),
                _ => return Effect::None,
            }
            return Effect::Redraw;
        }
        if self.side == Side::Preview && self.preview.is_some() {
            return self.preview_key(key, page);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1, rows),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1, rows),
            KeyCode::PageUp => self.move_selection(-(page as isize), rows),
            KeyCode::PageDown => self.move_selection(page as isize, rows),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2, rows),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2, rows),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.go_up(),
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.clear();
                Effect::Redraw
            }
            KeyCode::Tab if self.preview.is_some() => {
                self.side = Side::Preview;
                Effect::Redraw
            }
            KeyCode::Char('r') => match &self.listing {
                Some(l) => Effect::List(l.current.clone()),
                None => Effect::Root,
            },
            KeyCode::Char('~') => Effect::Root,
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    Effect::Redraw
                } else if self.preview.is_some() {
                    self.preview = None;
                    Effect::Redraw
                } else {
                    Effect::Hide
                }
            }
            _ => Effect::None,
        }
    }

    /// The file `d` or `o` acts on: the one in the viewer while it has the
    /// keys, else the selected one.
    fn transfer_target(&mut self, open: bool) -> Effect {
        if self.filtering {
            return Effect::None;
        }
        let target = if self.side == Side::Preview {
            self.preview
                .as_ref()
                .map(|p| (p.path.clone(), p.name.clone(), p.size, false))
        } else {
            self.selected_real()
                .map(|e| (e.path.clone(), e.name.clone(), e.size, e.kind.is_dir()))
        };
        let Some((path, name, size, is_dir)) = target else {
            return Effect::None;
        };
        if is_dir {
            if open {
                self.status = Some("folders cannot be opened here; d downloads it".into());
                return Effect::Redraw;
            }
            self.status = None;
            return Effect::DownloadFolder(path);
        }
        if open && !ext_of(&name).is_some_and(|x| opens_locally(&x)) {
            self.status = Some("only images and PDFs open on this machine; d downloads".into());
            return Effect::Redraw;
        }
        self.status = None;
        if open {
            Effect::OpenLocal { path, size }
        } else {
            Effect::Download { path, size }
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Effect {
        let Some(prompt) = self.prompt.as_mut() else {
            return Effect::None;
        };
        match prompt {
            Prompt::Upload(text) => match key.code {
                KeyCode::Char(c) => text.push(c),
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let local = unquote(text);
                    let Some(dir) = self.listing.as_ref().map(|l| l.current.clone()) else {
                        self.prompt = None;
                        return Effect::Redraw;
                    };
                    if local.is_empty() {
                        self.prompt = None;
                        return Effect::Redraw;
                    }
                    let name = local
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&local)
                        .to_string();
                    let taken = self.listing.as_ref().is_some_and(|l| {
                        l.entries.iter().any(|e| e.name.eq_ignore_ascii_case(&name))
                    });
                    if taken {
                        self.prompt = Some(Prompt::Overwrite { local });
                        return Effect::Redraw;
                    }
                    self.prompt = None;
                    return Effect::Upload {
                        local,
                        dir,
                        overwrite: false,
                    };
                }
                _ => return Effect::None,
            },
            Prompt::OverwriteMany { locals, taken } => {
                let (locals, taken) = (locals.clone(), taken.clone());
                let Some(dir) = self.listing.as_ref().map(|l| l.current.clone()) else {
                    self.prompt = None;
                    return Effect::Redraw;
                };
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') => {
                        self.prompt = None;
                        Effect::UploadMany {
                            locals,
                            dir,
                            overwrite: true,
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        self.prompt = None;
                        let rest: Vec<String> =
                            locals.into_iter().filter(|l| !taken.contains(l)).collect();
                        if rest.is_empty() {
                            Effect::Redraw
                        } else {
                            Effect::UploadMany {
                                locals: rest,
                                dir,
                                overwrite: false,
                            }
                        }
                    }
                    KeyCode::Esc => {
                        self.prompt = None;
                        Effect::Redraw
                    }
                    _ => Effect::None,
                };
            }
            Prompt::Overwrite { local } => {
                let local = local.clone();
                self.prompt = None;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    if let Some(dir) = self.listing.as_ref().map(|l| l.current.clone()) {
                        return Effect::Upload {
                            local,
                            dir,
                            overwrite: true,
                        };
                    }
                }
            }
            Prompt::NewFolder(text) | Prompt::Rename { text, .. } => match key.code {
                KeyCode::Char(c) => text.push(c),
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let typed = text.trim().to_string();
                    let prompt = self.prompt.take();
                    let Some(dir) = self.listing.as_ref().map(|l| l.current.clone()) else {
                        return Effect::Redraw;
                    };
                    if typed.is_empty() {
                        return Effect::Redraw;
                    }
                    let target = resolve_typed(&dir, &typed);
                    return match prompt {
                        Some(Prompt::NewFolder(_)) => Effect::Mkdir(target),
                        Some(Prompt::Rename { from, .. }) if from != target => {
                            Effect::Move { from, to: target }
                        }
                        _ => Effect::Redraw,
                    };
                }
                _ => return Effect::None,
            },
            Prompt::Delete { path, .. } => {
                let path = path.clone();
                self.prompt = None;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    return Effect::Delete(path);
                }
            }
        }
        Effect::Redraw
    }

    /// Selects the entry at `path`, when the listing has it. The list
    /// scrolls to it when drawn (see [`FilesView::scroll_for`]).
    pub fn select_path(&mut self, path: &str) {
        if let Some(i) = self.visible().iter().position(|e| e.path == path) {
            self.selected = i;
        }
    }

    /// The first list row to draw in `rows` rows: the stored scroll, moved
    /// just enough to show the selection. Shared by the renderer and the
    /// mouse, so a click lands on the row that was drawn.
    pub fn scroll_for(&self, rows: usize) -> usize {
        let rows = rows.max(1);
        if self.selected < self.list_scroll {
            self.selected
        } else if self.selected >= self.list_scroll + rows {
            self.selected + 1 - rows
        } else {
            self.list_scroll
        }
    }

    fn preview_key(&mut self, key: KeyEvent, page: usize) -> Effect {
        let Some(p) = self.preview.as_mut() else {
            return Effect::None;
        };
        let last = match &p.body {
            PreviewBody::Text { lines, .. } => lines.len().saturating_sub(1),
            _ => 0,
        };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => p.scroll = p.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.scroll = (p.scroll + 1).min(last),
            KeyCode::PageUp => p.scroll = p.scroll.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => p.scroll = (p.scroll + page).min(last),
            KeyCode::Home | KeyCode::Char('g') => p.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => p.scroll = last,
            KeyCode::Left | KeyCode::Char('h') => {
                p.hscroll = p.hscroll.saturating_sub(HSCROLL_STEP)
            }
            KeyCode::Right | KeyCode::Char('l') => p.hscroll += HSCROLL_STEP,
            KeyCode::Tab | KeyCode::Esc | KeyCode::Backspace => self.side = Side::List,
            _ => return Effect::None,
        }
        Effect::Redraw
    }

    /// A click or wheel notch inside the tab. `area` is the whole tab.
    pub fn handle_mouse(&mut self, event: MouseEvent, area: Rect) -> Effect {
        let layout = layout(self, area);
        let inside = |r: Rect| {
            r.width > 0
                && event.column >= r.x
                && event.column < r.x + r.width
                && event.row >= r.y
                && event.row < r.y + r.height
        };
        let rows = usize::from(layout.list.height);
        if let Some(editor) = self.editor.as_mut() {
            let editor_rows = usize::from(layout.preview.height.saturating_sub(1));
            match event.kind {
                MouseEventKind::ScrollUp => editor.scroll_by(-(WHEEL_ROWS as isize), editor_rows),
                MouseEventKind::ScrollDown => editor.scroll_by(WHEEL_ROWS as isize, editor_rows),
                _ => return Effect::None,
            }
            return Effect::Redraw;
        }
        match event.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = matches!(event.kind, MouseEventKind::ScrollDown);
                if inside(layout.preview) {
                    if let Some(p) = self.preview.as_mut() {
                        let last = match &p.body {
                            PreviewBody::Text { lines, .. } => lines.len().saturating_sub(1),
                            _ => 0,
                        };
                        p.scroll = if down {
                            (p.scroll + WHEEL_ROWS).min(last)
                        } else {
                            p.scroll.saturating_sub(WHEEL_ROWS)
                        };
                    }
                } else {
                    let count = self.visible().len();
                    let max = count.saturating_sub(rows);
                    self.list_scroll = if down {
                        (self.list_scroll + WHEEL_ROWS).min(max)
                    } else {
                        self.list_scroll.saturating_sub(WHEEL_ROWS)
                    };
                }
                Effect::Redraw
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                if inside(layout.header) {
                    let hit = self
                        .crumbs(layout.header)
                        .into_iter()
                        .find(|(r, _, _)| event.column >= r.x && event.column < r.x + r.width);
                    return match hit {
                        Some((_, _, Some(path))) => Effect::List(path),
                        _ => Effect::None,
                    };
                }
                if inside(layout.list) {
                    self.side = Side::List;
                    let index = self.scroll_for(rows) + usize::from(event.row - layout.list.y);
                    if index >= self.visible().len() {
                        return Effect::Redraw;
                    }
                    // One click: into a folder, or the file into the viewer
                    // (the keys stay with the list, to keep browsing).
                    self.selected = index;
                    match self
                        .selected_entry()
                        .map(|e| (e.kind, e.path.clone(), e.size))
                    {
                        Some((kind, path, _)) if kind.is_dir() || kind == EntryKind::Up => {
                            Effect::List(path)
                        }
                        Some((EntryKind::File | EntryKind::LinkFile, path, size)) => {
                            if ext_of(&path).is_some_and(|x| opens_locally(&x)) {
                                self.set_preview(&path, size, &[]);
                                Effect::Redraw
                            } else {
                                self.peek = None;
                                Effect::Read(path)
                            }
                        }
                        _ => Effect::Redraw,
                    }
                } else if inside(layout.preview) && self.preview.is_some() {
                    self.side = Side::Preview;
                    Effect::Redraw
                } else {
                    Effect::Redraw
                }
            }
            _ => Effect::None,
        }
    }
}

/// Where each part of the tab sits. Shared by the renderer and
/// [`FilesView::handle_mouse`] so a click lands on what was drawn there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FilesRects {
    pub header: Rect,
    pub list: Rect,
    pub preview: Rect,
    pub footer: Rect,
}

/// Lays the tab out in `area`: a header row, the list beside the viewer,
/// and a footer row. Narrow areas show one of list or viewer, whichever has
/// the keys.
pub fn layout(view: &FilesView, area: Rect) -> FilesRects {
    if area.height < 3 || area.width == 0 {
        return FilesRects::default();
    }
    let header = Rect::new(area.x, area.y, area.width, 1);
    let footer = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 2);
    let (list, preview) = if view.editor.is_some() {
        (Rect::default(), body)
    } else if area.width >= 60 {
        let list_w = (area.width * 2 / 5).clamp(24, 48);
        (
            Rect::new(body.x, body.y, list_w, body.height),
            Rect::new(
                body.x + list_w + 1,
                body.y,
                area.width - list_w - 1,
                body.height,
            ),
        )
    } else if view.side == Side::Preview && view.preview.is_some() {
        (Rect::default(), body)
    } else {
        (body, Rect::default())
    };
    FilesRects {
        header,
        list,
        preview,
        footer,
    }
}

/// How many list rows a tab of height `height` shows.
pub fn list_rows(height: u16) -> usize {
    usize::from(height.saturating_sub(2))
}

/// Decodes an `fs.read` result into `(path, size, mtime, bytes)`.
pub fn decode_read(v: &serde_json::Value) -> Option<(String, u64, f64, Vec<u8>)> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(v.get("data")?.as_str()?)
        .ok()?;
    Some((
        v.get("path")?.as_str()?.to_string(),
        v.get("size")?.as_u64()?,
        v.get("mtime").and_then(|m| m.as_f64()).unwrap_or(0.0),
        data,
    ))
}

/// What a typed name or path means, seen from the folder `dir`: a bare
/// name lives in `dir`, an absolute path is taken as is, and anything else
/// is relative to `dir`. The server resolves `..` and checks the result.
fn resolve_typed(dir: &str, typed: &str) -> String {
    let absolute =
        typed.starts_with('/') || typed.starts_with('\\') || typed.as_bytes().get(1) == Some(&b':');
    if absolute {
        typed.to_string()
    } else {
        crate::transfer::remote_join(dir, typed)
    }
}

/// `/x`, `\x`, `C:\x` or `C:/x` — a path that does not depend on where the
/// client was started.
fn is_absolute_path(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with('\\')
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
}

/// AltGr on Windows arrives as CONTROL+ALT with the character it typed
/// (`\`, `@`, `{` on many layouts): text, not a chord.
pub fn is_altgr_char(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && key
            .modifiers
            .contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// A name's extension, lowercased.
fn ext_of(name: &str) -> Option<String> {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())
}

impl FilesView {
    /// True while keys mean commands here: no prompt, editor or filter is
    /// taking text. Pasted text is only special in this state.
    pub fn takes_commands(&self) -> bool {
        self.prompt.is_none() && self.editor.is_none() && !self.filtering
    }

    /// Text dropped or pasted onto the list: local files and folders are
    /// uploaded into the folder on screen. Never read as commands — a path
    /// typed out as keys would otherwise run them (`x` then `y` deletes).
    pub fn handle_paste(&mut self, text: &str, exists: impl Fn(&str) -> bool) -> Effect {
        let Some(dir) = self.listing.as_ref().map(|l| l.current.clone()) else {
            return Effect::None;
        };
        let locals = dropped_paths(text, &exists);
        if locals.is_empty() {
            self.status = Some(format!(
                "drop or paste a file or folder from this machine to upload it \u{2014} nothing at {}",
                text.trim().chars().take(60).collect::<String>()
            ));
            return Effect::Redraw;
        }
        let names: Vec<String> = self
            .listing
            .as_ref()
            .map(|l| l.entries.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default();
        let taken: Vec<String> = locals
            .iter()
            .filter(|l| names.iter().any(|n| n.eq_ignore_ascii_case(base_name(l))))
            .cloned()
            .collect();
        self.status = None;
        if locals.len() == 1 {
            let local = locals.into_iter().next().unwrap_or_default();
            if taken.is_empty() {
                return Effect::Upload {
                    local,
                    dir,
                    overwrite: false,
                };
            }
            self.prompt = Some(Prompt::Overwrite { local });
            return Effect::Redraw;
        }
        if taken.is_empty() {
            return Effect::UploadMany {
                locals,
                dir,
                overwrite: false,
            };
        }
        self.prompt = Some(Prompt::OverwriteMany { locals, taken });
        Effect::Redraw
    }
}

/// The last part of a local path.
fn base_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
}

/// The local paths in dropped or pasted text: the whole text when it names
/// something (a path with spaces, unquoted), else each quoted or
/// space-separated piece that does — the forms terminals use when several
/// files are dropped at once.
pub fn dropped_paths(text: &str, exists: impl Fn(&str) -> bool) -> Vec<String> {
    // A drop is always a full path. Accepting relative ones would let a
    // burst of ordinary typing name a file next to wherever the client runs.
    let exists = |p: &str| is_absolute_path(p) && exists(p);
    let whole = unquote(text);
    if !whole.is_empty() && exists(&whole) {
        return vec![whole];
    }
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut piece = String::new();
        if c == '"' || c == '\'' {
            chars.next();
            for d in chars.by_ref() {
                if d == c {
                    break;
                }
                piece.push(d);
            }
        } else {
            while let Some(&d) = chars.peek() {
                if d.is_whitespace() {
                    break;
                }
                piece.push(d);
                chars.next();
            }
        }
        if !piece.is_empty() && exists(&piece) && !out.contains(&piece) {
            out.push(piece);
        }
    }
    out
}

/// A path as typed or pasted: trimmed, and without the quotes a terminal
/// adds when a file is dragged onto it.
fn unquote(s: &str) -> String {
    let t = s.trim();
    let t = t
        .strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .or_else(|| t.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
        .unwrap_or(t);
    t.trim().to_string()
}

/// A file with a NUL byte near the start is not text.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|b| *b == 0)
}

/// True when [`sanitize`] would change `s` beyond expanding tabs.
pub fn needs_sanitizing(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
    })
}

/// Text safe to put in a terminal cell: tabs expanded, and every control
/// character — which would otherwise reach the user's terminal as an escape
/// sequence from a file anyone could have written — and every bidi override
/// replaced.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\t' => out.push_str("    "),
            '\n' | '\r' => {}
            c if c.is_control() => out.push('\u{FFFD}'),
            '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' => {
                out.push('\u{FFFD}')
            }
            c => out.push(c),
        }
    }
    out
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let mut set = ThemeSet::load_defaults();
        set.themes.remove("base16-ocean.dark").unwrap_or_default()
    })
}

/// `text` as styled lines, highlighted by the syntax its name implies.
pub fn highlight(name: &str, text: &str) -> Vec<Line<'static>> {
    let set = syntaxes();
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or(name);
    let syntax = set
        .find_syntax_by_extension(ext)
        .or_else(|| set.find_syntax_by_extension(name))
        .or_else(|| {
            text.lines()
                .next()
                .and_then(|l| set.find_syntax_by_first_line(l))
        });
    let mut out = Vec::new();
    let mut highlighter = syntax.map(|s| HighlightLines::new(s, theme()));
    for (i, line) in LinesWithEndings::from(text).enumerate() {
        let styled = match highlighter.as_mut() {
            Some(h) if i < HIGHLIGHT_MAX_LINES => h.highlight_line(line, set).ok(),
            _ => None,
        };
        out.push(match styled {
            Some(ranges) => Line::from(
                ranges
                    .into_iter()
                    .map(|(style, piece)| {
                        let fg = style.foreground;
                        let mut s = Style::default().fg(Color::Rgb(fg.r, fg.g, fg.b));
                        if style
                            .font_style
                            .contains(syntect::highlighting::FontStyle::BOLD)
                        {
                            s = s.add_modifier(Modifier::BOLD);
                        }
                        Span::styled(sanitize(piece), s)
                    })
                    .collect::<Vec<_>>(),
            ),
            None => Line::raw(sanitize(line)),
        });
    }
    out
}

/// A size in the units a person reads.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, MouseButton};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            name: name.into(),
            path: format!("/dev/app/{name}"),
            kind,
            size: 10,
            mtime: 0.0,
        }
    }

    fn view() -> FilesView {
        let mut v = FilesView::new(None);
        v.set_listing(Listing {
            current: "/dev/app".into(),
            parent: Some("/dev".into()),
            entries: vec![
                entry("src", EntryKind::Dir),
                entry("Cargo.toml", EntryKind::File),
                entry("README.md", EntryKind::File),
            ],
            truncated: false,
        });
        v
    }

    #[test]
    fn enter_opens_a_folder_or_reads_a_file() {
        let mut v = view();
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::List("/dev/app/src".into())
        );
        v.handle_key(key(KeyCode::Down), 10);
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::Read("/dev/app/Cargo.toml".into())
        );
        assert_eq!(v.side, Side::Preview, "the viewer takes the keys");
    }

    #[test]
    fn backspace_goes_up_and_selects_the_folder_left() {
        let mut v = view();
        assert_eq!(
            v.handle_key(key(KeyCode::Backspace), 10),
            Effect::List("/dev".into())
        );
        v.set_listing(Listing {
            current: "/dev".into(),
            parent: None,
            entries: vec![
                entry("other", EntryKind::Dir),
                Entry {
                    path: "/dev/app".into(),
                    ..entry("app", EntryKind::Dir)
                },
            ],
            truncated: false,
        });
        assert_eq!(v.selected_entry().map(|e| e.name.as_str()), Some("app"));
        assert_eq!(
            v.handle_key(key(KeyCode::Backspace), 10),
            Effect::None,
            "no parent at a root"
        );
    }

    #[test]
    fn slash_filters_and_enter_opens_the_match() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('/')), 10);
        for c in "read".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(v.visible().len(), 1);
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::Read("/dev/app/README.md".into())
        );
    }

    #[test]
    fn letters_are_commands_until_slash() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('j')), 10);
        assert_eq!(v.selected_entry().unwrap().name, "Cargo.toml");
        assert!(v.filter.is_empty());
    }

    #[test]
    fn selection_stays_on_screen() {
        let mut v = FilesView::new(None);
        v.set_listing(Listing {
            current: "/d".into(),
            parent: None,
            entries: (0..50)
                .map(|i| entry(&format!("f{i:02}"), EntryKind::File))
                .collect(),
            truncated: false,
        });
        v.handle_key(key(KeyCode::PageDown), 10);
        v.handle_key(key(KeyCode::PageDown), 10);
        assert_eq!(v.selected, 18);
        assert!(v.selected >= v.list_scroll && v.selected < v.list_scroll + 10);
        v.handle_key(key(KeyCode::End), 10);
        assert_eq!(v.selected, 49);
        assert_eq!(v.list_scroll, 40);
    }

    #[test]
    fn esc_backs_out_one_layer_at_a_time() {
        let mut v = view();
        v.set_preview("/dev/app/README.md", 4, b"# hi");
        v.side = Side::List;
        assert_eq!(v.handle_key(key(KeyCode::Esc), 10), Effect::Redraw);
        assert!(v.preview.is_none());
        assert_eq!(v.handle_key(key(KeyCode::Esc), 10), Effect::Hide);
    }

    #[test]
    fn the_viewer_scrolls_and_tab_returns_to_the_list() {
        let mut v = view();
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        v.set_preview("/dev/app/notes.txt", text.len() as u64, text.as_bytes());
        v.side = Side::Preview;
        v.handle_key(key(KeyCode::PageDown), 11);
        assert_eq!(v.preview.as_ref().unwrap().scroll, 10);
        v.handle_key(key(KeyCode::End), 11);
        assert_eq!(v.preview.as_ref().unwrap().scroll, 99);
        v.handle_key(key(KeyCode::Tab), 11);
        assert_eq!(v.side, Side::List);
    }

    #[test]
    fn images_and_pdfs_open_elsewhere_and_binaries_are_not_shown() {
        let mut v = view();
        v.set_preview("/d/photo.PNG", 3, b"\x89PN");
        assert!(matches!(
            v.preview.as_ref().unwrap().body,
            PreviewBody::External
        ));
        v.set_preview("/d/blob.bin", 3, b"a\0b");
        assert!(matches!(
            v.preview.as_ref().unwrap().body,
            PreviewBody::Binary
        ));
        v.set_preview("/d/big.log", 10_000_000, b"x\n");
        assert!(matches!(
            v.preview.as_ref().unwrap().body,
            PreviewBody::Text {
                truncated: true,
                ..
            }
        ));
    }

    #[test]
    fn file_contents_cannot_reach_the_terminal_as_escapes() {
        let lines = highlight("evil.txt", "a\x1b]52;c;cHduZWQ=\x07b\u{202E}c\u{9b}d\n");
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!text.chars().any(|c| c.is_control()), "{text:?}");
        assert!(!text.contains('\u{202E}'));
        assert!(text.starts_with('a') && text.ends_with('d'));
    }

    #[test]
    fn code_is_highlighted() {
        let lines = highlight("main.rs", "fn main() {}\n");
        assert!(lines[0].spans.len() > 1, "split into styled pieces");
        assert!(lines[0]
            .spans
            .iter()
            .any(|s| matches!(s.style.fg, Some(Color::Rgb(..)))));
    }

    #[test]
    fn one_click_opens_a_folder_or_previews_a_file_and_dot_dot_goes_up() {
        let mut v = view();
        let area = Rect::new(0, 0, 100, 20);
        let list = layout(&v, area).list;
        let click = |row: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: list.x + 1,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // Rows: .., src, Cargo.toml, README.md
        assert_eq!(
            v.handle_mouse(click(list.y), area),
            Effect::List("/dev".into())
        );
        assert_eq!(
            v.handle_mouse(click(list.y + 1), area),
            Effect::List("/dev/app/src".into())
        );
        assert_eq!(
            v.handle_mouse(click(list.y + 3), area),
            Effect::Read("/dev/app/README.md".into())
        );
        assert_eq!(v.side, Side::List, "the keys stay with the list");
    }

    #[test]
    fn going_into_a_folder_never_lands_on_dot_dot() {
        // The folder you came from is that folder's `..` — the one entry
        // that must not be re-selected on the way in.
        let mut v = FilesView::new(None);
        v.set_listing(Listing {
            current: "/dev".into(),
            parent: None,
            entries: vec![entry("app", EntryKind::Dir)],
            truncated: false,
        });
        v.set_listing(Listing {
            current: "/dev/app".into(),
            parent: Some("/dev".into()),
            entries: vec![entry("src", EntryKind::Dir)],
            truncated: false,
        });
        assert_eq!(v.selected_entry().unwrap().name, "src");
    }

    #[test]
    fn dot_dot_leads_the_list_and_entering_a_folder_selects_its_first_entry() {
        let v = view();
        assert_eq!(v.visible()[0].name, "..");
        assert_eq!(v.visible()[0].kind, EntryKind::Up);
        assert_eq!(v.selected_entry().unwrap().name, "src", "not ..");
        let mut top = FilesView::new(None);
        top.set_listing(Listing {
            current: "/dev".into(),
            parent: None,
            entries: vec![entry("a", EntryKind::Dir)],
            truncated: false,
        });
        assert_eq!(top.visible().len(), 1, "no .. at a root");
    }

    #[test]
    fn enter_on_dot_dot_goes_up_and_nothing_else_acts_on_it() {
        let mut v = view();
        v.handle_key(key(KeyCode::Up), 10);
        assert_eq!(v.selected_entry().unwrap().kind, EntryKind::Up);
        for c in ['d', 'm', 'x', 'e'] {
            v.handle_key(key(KeyCode::Char(c)), 10);
            assert!(v.prompt.is_none(), "{c} prompted on ..");
        }
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::List("/dev".into())
        );
    }

    #[test]
    fn the_preview_follows_the_selection_once_it_rests() {
        let mut v = view();
        v.handle_key(key(KeyCode::Down), 10); // Cargo.toml
        let now = std::time::Instant::now();
        assert_eq!(v.due_peek(now), None, "not straight away");
        assert_eq!(
            v.due_peek(now + PEEK_DELAY * 2),
            Some("/dev/app/Cargo.toml".into())
        );
        // Moving on before it is due replaces it.
        v.handle_key(key(KeyCode::Down), 10);
        v.handle_key(key(KeyCode::Up), 10);
        v.handle_key(key(KeyCode::Down), 10); // README.md
        assert_eq!(
            v.due_peek(now + PEEK_DELAY * 2),
            Some("/dev/app/README.md".into())
        );
        // A folder clears the viewer instead.
        v.set_preview("/dev/app/README.md", 2, b"hi");
        v.handle_key(key(KeyCode::Home), 10);
        v.handle_key(key(KeyCode::Down), 10); // src
        assert!(v.preview.is_none());
    }

    #[test]
    fn breadcrumbs_open_folders_within_reach_only() {
        let mut v = view();
        v.roots = vec!["/dev".into()];
        let header = Rect::new(0, 0, 80, 1);
        let crumbs = v.crumbs(header);
        let texts: Vec<&str> = crumbs.iter().map(|(_, t, _)| t.as_str()).collect();
        assert_eq!(texts, vec!["/", "dev/", "app"]);
        assert_eq!(crumbs[0].2, None, "/ is out of reach");
        assert_eq!(crumbs[1].2.as_deref(), Some("/dev"));
        assert_eq!(crumbs[2].2.as_deref(), Some("/dev/app"));
        // A click lands on the part drawn there.
        let at = crumbs[1].0;
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: at.x,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            v.handle_mouse(event, Rect::new(0, 0, 80, 20)),
            Effect::List("/dev".into())
        );
    }

    #[test]
    fn listings_parse_from_the_wire() {
        let l = Listing::from_wire(&serde_json::json!({
            "current": "/d", "parent": null, "truncated": false,
            "entries": [{ "name": "a", "path": "/d/a", "type": "link-dir", "size": 0, "mtime": 1.5 }],
        }))
        .unwrap();
        assert_eq!(l.parent, None);
        assert_eq!(l.entries[0].kind, EntryKind::LinkDir);
        assert!(l.entries[0].kind.is_dir());
    }

    #[test]
    fn d_downloads_the_selected_file_or_folder() {
        let mut v = view();
        assert_eq!(
            v.handle_key(key(KeyCode::Char('d')), 10),
            Effect::DownloadFolder("/dev/app/src".into())
        );
        assert_eq!(v.handle_key(key(KeyCode::Char('o')), 10), Effect::Redraw);
        assert!(v.status.as_deref().unwrap().contains("cannot be opened"));
        v.handle_key(key(KeyCode::Down), 10);
        assert_eq!(
            v.handle_key(key(KeyCode::Char('d')), 10),
            Effect::Download {
                path: "/dev/app/Cargo.toml".into(),
                size: 10
            }
        );
    }

    #[test]
    fn o_opens_only_images_and_pdfs_and_enter_on_one_opens_it() {
        let mut v = view();
        v.handle_key(key(KeyCode::Down), 10);
        assert_eq!(v.handle_key(key(KeyCode::Char('o')), 10), Effect::Redraw);
        assert!(v.status.as_deref().unwrap().contains("only images"));
        v.listing
            .as_mut()
            .unwrap()
            .entries
            .push(entry("chart.PDF", EntryKind::File));
        v.select_path("/dev/app/chart.PDF");
        let open = Effect::OpenLocal {
            path: "/dev/app/chart.PDF".into(),
            size: 10,
        };
        assert_eq!(v.handle_key(key(KeyCode::Char('o')), 10), open);
        assert_eq!(v.handle_key(key(KeyCode::Enter), 10), open);
    }

    #[test]
    fn u_asks_for_a_local_file_and_strips_drag_and_drop_quotes() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('u')), 10);
        for c in "\"C:\\tmp\\notes.txt\"".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::Upload {
                local: "C:\\tmp\\notes.txt".into(),
                dir: "/dev/app".into(),
                overwrite: false
            }
        );
        assert!(v.prompt.is_none());
    }

    #[test]
    fn uploading_over_an_existing_name_asks_first() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('u')), 10);
        for c in "/tmp/README.md".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(v.handle_key(key(KeyCode::Enter), 10), Effect::Redraw);
        assert!(matches!(v.prompt, Some(Prompt::Overwrite { .. })));
        assert!(matches!(
            v.handle_key(key(KeyCode::Char('y')), 10),
            Effect::Upload {
                overwrite: true,
                ..
            }
        ));
        // Anything but y cancels.
        v.handle_key(key(KeyCode::Char('u')), 10);
        for c in "/tmp/README.md".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        v.handle_key(key(KeyCode::Enter), 10);
        assert_eq!(v.handle_key(key(KeyCode::Char('n')), 10), Effect::Redraw);
        assert!(v.prompt.is_none());
    }

    #[test]
    fn typing_in_a_prompt_never_triggers_commands() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('u')), 10);
        for c in "dou/".chars() {
            assert_eq!(v.handle_key(key(KeyCode::Char(c)), 10), Effect::Redraw);
        }
        assert_eq!(v.prompt, Some(Prompt::Upload("dou/".into())));
    }

    #[test]
    fn n_makes_a_folder_in_the_folder_on_screen() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('n')), 10);
        for c in "lib".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::Mkdir("/dev/app/lib".into())
        );
        assert!(v.prompt.is_none());
    }

    #[test]
    fn m_renames_in_place_or_moves_by_path() {
        let mut v = view();
        v.handle_key(key(KeyCode::Down), 10);
        v.handle_key(key(KeyCode::Char('m')), 10);
        assert_eq!(
            v.prompt,
            Some(Prompt::Rename {
                from: "/dev/app/Cargo.toml".into(),
                text: "Cargo.toml".into()
            }),
            "pre-filled with the current name"
        );
        // Unchanged: nothing to do.
        assert_eq!(v.handle_key(key(KeyCode::Enter), 10), Effect::Redraw);

        v.handle_key(key(KeyCode::F(2)), 10);
        for _ in 0.."Cargo.toml".len() {
            v.handle_key(key(KeyCode::Backspace), 10);
        }
        for c in "src/Cargo.toml".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(
            v.handle_key(key(KeyCode::Enter), 10),
            Effect::Move {
                from: "/dev/app/Cargo.toml".into(),
                to: "/dev/app/src/Cargo.toml".into()
            }
        );
    }

    #[test]
    fn delete_asks_and_only_y_deletes() {
        let mut v = view();
        v.handle_key(key(KeyCode::Delete), 10);
        assert!(matches!(
            v.prompt,
            Some(Prompt::Delete { is_dir: true, .. })
        ));
        assert_eq!(v.handle_key(key(KeyCode::Char('n')), 10), Effect::Redraw);
        assert!(v.prompt.is_none());
        v.handle_key(key(KeyCode::Char('x')), 10);
        assert_eq!(
            v.handle_key(key(KeyCode::Char('y')), 10),
            Effect::Delete("/dev/app/src".into())
        );
    }

    #[test]
    fn e_edits_the_selected_text_file_and_ctrl_s_saves_with_its_mtime() {
        let mut v = view();
        v.handle_key(key(KeyCode::Down), 10);
        assert_eq!(
            v.handle_key(key(KeyCode::Char('e')), 10),
            Effect::Edit("/dev/app/Cargo.toml".into())
        );
        v.start_editing("/dev/app/Cargo.toml", 4, 123.0, b"[a]\n");
        assert!(v.editor.is_some());
        // Every key is the editor's now, letters included.
        v.handle_key(key(KeyCode::Char('d')), 10);
        let save = KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..key(KeyCode::Char('s'))
        };
        assert_eq!(
            v.handle_key(save, 10),
            Effect::Save {
                path: "/dev/app/Cargo.toml".into(),
                text: "d[a]\n".into(),
                expect_mtime: Some(123.0)
            }
        );
        v.saved(5, 456.0);
        assert!(!v.editor.as_ref().unwrap().dirty);
        assert_eq!(v.editor.as_ref().unwrap().mtime, 456.0);
    }

    #[test]
    fn a_refused_save_lets_the_next_one_overwrite() {
        let mut v = view();
        v.start_editing("/f", 1, 1.0, b"a");
        v.editor.as_mut().unwrap().force = true;
        let save = KeyEvent {
            modifiers: KeyModifiers::CONTROL,
            ..key(KeyCode::Char('s'))
        };
        assert!(matches!(
            v.handle_key(save, 10),
            Effect::Save {
                expect_mtime: None,
                ..
            }
        ));
    }

    #[test]
    fn files_too_big_or_not_text_are_not_edited() {
        let mut v = view();
        v.start_editing("/big", PREVIEW_BYTES + 1, 0.0, b"partial");
        assert!(v.editor.is_none());
        assert!(v.status.as_deref().unwrap().contains("up to"));
        v.start_editing("/bin", 3, 0.0, b"a\0b");
        assert!(v.editor.is_none());
    }

    #[test]
    fn a_dropped_path_uploads_and_is_never_read_as_commands() {
        let mut v = view();
        let exists = |p: &str| p == "C:\\Users\\me\\xyz.txt";
        assert_eq!(
            v.handle_paste("\"C:\\Users\\me\\xyz.txt\" ", exists),
            Effect::Upload {
                local: "C:\\Users\\me\\xyz.txt".into(),
                dir: "/dev/app".into(),
                overwrite: false
            }
        );
        assert!(
            v.prompt.is_none(),
            "no delete prompt from the x and y in it"
        );
    }

    #[test]
    fn several_dropped_files_upload_together_and_ask_once_about_clashes() {
        let mut v = view();
        let exists = |p: &str| ["/tmp/a b.txt", "/tmp/README.md", "/tmp/new.txt"].contains(&p);
        assert_eq!(
            v.handle_paste("'/tmp/a b.txt' /tmp/new.txt", exists),
            Effect::UploadMany {
                locals: vec!["/tmp/a b.txt".into(), "/tmp/new.txt".into()],
                dir: "/dev/app".into(),
                overwrite: false
            }
        );
        assert_eq!(
            v.handle_paste("/tmp/README.md /tmp/new.txt", exists),
            Effect::Redraw
        );
        assert!(
            matches!(&v.prompt, Some(Prompt::OverwriteMany { taken, .. }) if taken == &vec!["/tmp/README.md".to_string()])
        );
        assert_eq!(
            v.handle_key(key(KeyCode::Char('n')), 10),
            Effect::UploadMany {
                locals: vec!["/tmp/new.txt".into()],
                dir: "/dev/app".into(),
                overwrite: false
            },
            "n uploads only the ones not already here"
        );
    }

    #[test]
    fn a_whole_unquoted_path_with_spaces_is_one_path() {
        let exists = |p: &str| p == "/tmp/my notes.txt";
        assert_eq!(
            dropped_paths("/tmp/my notes.txt", exists),
            vec!["/tmp/my notes.txt"]
        );
    }

    #[test]
    fn pasted_text_that_names_nothing_says_so() {
        let mut v = view();
        assert_eq!(v.handle_paste("hello there", |_| false), Effect::Redraw);
        assert!(v
            .status
            .as_deref()
            .unwrap()
            .contains("nothing at hello there"));
    }

    #[test]
    fn a_refresh_of_the_same_folder_keeps_the_filter_being_typed() {
        // An upload finishing mid-filter re-lists the folder; clearing the
        // filter then turned the rest of the typing into commands.
        let mut v = view();
        v.handle_key(key(KeyCode::Char('/')), 10);
        v.handle_key(key(KeyCode::Char('r')), 10);
        let again = v.listing.clone().unwrap();
        v.set_listing(again);
        assert!(v.filtering);
        assert_eq!(v.filter, "r");
        v.handle_key(key(KeyCode::Char('x')), 10);
        assert!(v.prompt.is_none(), "x is filter text, not delete");
        assert_eq!(v.filter, "rx");
    }

    #[test]
    fn d_and_o_are_filter_text_while_filtering() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('/')), 10);
        for c in "readme".chars() {
            v.handle_key(key(KeyCode::Char(c)), 10);
        }
        assert_eq!(v.filter, "readme");
    }

    #[test]
    fn opening_a_file_cancels_the_pending_preview() {
        let mut v = view();
        v.handle_key(key(KeyCode::Down), 10); // Cargo.toml: a preview is pending
        assert!(v.peek.is_some());
        v.handle_key(key(KeyCode::Enter), 10);
        assert!(
            v.peek.is_none(),
            "the full read is not overwritten by a late preview"
        );
    }

    #[test]
    fn altgr_characters_type_into_prompts() {
        let mut v = view();
        v.handle_key(key(KeyCode::Char('u')), 10);
        v.handle_key(
            KeyEvent {
                modifiers: KeyModifiers::CONTROL | KeyModifiers::ALT,
                ..key(KeyCode::Char('\\'))
            },
            10,
        );
        assert_eq!(v.prompt, Some(Prompt::Upload("\\".into())));
    }

    #[test]
    fn only_absolute_paths_count_as_a_drop() {
        let exists = |_: &str| true;
        assert!(dropped_paths("notes.txt", exists).is_empty());
        assert!(dropped_paths("src/main.rs", exists).is_empty());
        assert_eq!(dropped_paths("/tmp/a.txt", exists), vec!["/tmp/a.txt"]);
        assert_eq!(dropped_paths("C:\\x\\a.txt", exists), vec!["C:\\x\\a.txt"]);
    }

    #[test]
    fn breadcrumbs_click_under_a_root_of_slash() {
        let mut v = view();
        v.roots = vec!["/".into()];
        let crumbs = v.crumbs(Rect::new(0, 0, 80, 1));
        assert!(
            crumbs.iter().all(|(_, _, click)| click.is_some()),
            "{crumbs:?}"
        );
    }

    #[test]
    fn an_upload_clash_is_found_whatever_the_case() {
        let mut v = view();
        assert_eq!(v.handle_paste("/tmp/readme.MD", |_| true), Effect::Redraw);
        assert!(matches!(v.prompt, Some(Prompt::Overwrite { .. })));
    }

    #[test]
    fn sizes_read_like_sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(4300), "4.2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}
