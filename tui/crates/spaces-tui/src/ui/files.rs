//! Draws a [`FilesView`]: the folder list beside the viewer, a header naming
//! the folder and a footer with the keys or the last error.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::files::{human_size, layout, sanitize, EntryKind, FilesView, PreviewBody, Prompt, Side};

const LIST_KEYS: &str = "drop a file here to upload \u{b7} enter/click open \u{b7} \u{2190} or .. up \u{b7} / filter \u{b7} e edit \u{b7} d download \u{b7} u upload \u{b7} n new folder \u{b7} m rename \u{b7} x delete \u{b7} esc back";
const PREVIEW_KEYS: &str =
    "\u{2191}\u{2193} scroll \u{b7} \u{2190}\u{2192} sideways \u{b7} d download \u{b7} tab list";
const FILTER_KEYS: &str = "type to filter \u{b7} enter open \u{b7} esc clear";

/// Draws `view` into `area`. `has_keys` brightens whichever half the keys
/// move, so it is clear where typing goes.
pub fn render_files(view: &FilesView, area: Rect, has_keys: bool, buf: &mut Buffer) {
    let rects = layout(view, area);
    if rects.header.width == 0 {
        return;
    }
    let dim = Style::default().fg(Color::DarkGray);

    // Header: the folder, and the filter while there is one.
    let mut header = vec![Span::styled(
        " \u{1F4C1} ".to_string(),
        Style::default().fg(Color::Yellow),
    )];
    // The path as breadcrumbs: each part opens that folder, the last one
    // (where you are) bold, parts out of reach dimmed.
    let crumbs = view.crumbs(rects.header);
    if crumbs.is_empty() {
        header.push(Span::styled("loading\u{2026}", dim));
    }
    let last = crumbs.len().saturating_sub(1);
    for (i, (_, text, click)) in crumbs.into_iter().enumerate() {
        let style = if i == last {
            Style::default().add_modifier(Modifier::BOLD)
        } else if click.is_some() {
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED)
        } else {
            dim
        };
        header.push(Span::styled(text, style));
    }
    if view.filtering || !view.filter.is_empty() {
        header.push(Span::styled(
            format!("   filter: {}", sanitize(&view.filter)),
            Style::default().fg(Color::Cyan),
        ));
        if view.filtering {
            header.push(Span::styled("\u{2588}", Style::default().fg(Color::Cyan)));
        }
    }
    Paragraph::new(Line::from(header)).render(rects.header, buf);

    if let Some(editor) = &view.editor {
        render_editor(editor, rects.preview, has_keys, buf);
        let footer = match (&view.status, editor.confirm_discard) {
            (Some(status), _) => Span::styled(
                format!(" {}", sanitize(status)),
                Style::default().fg(Color::Red),
            ),
            (None, true) => Span::styled(
                " unsaved changes \u{2014} esc again to discard them, ctrl+s to save",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            (None, false) => match &view.note {
                Some(note) => Span::styled(
                    format!(" {}", sanitize(note)),
                    Style::default().fg(Color::Green),
                ),
                None => Span::styled(" ctrl+s save \u{b7} esc close", dim),
            },
        };
        Paragraph::new(Line::from(footer)).render(rects.footer, buf);
        return;
    }

    render_list(view, rects.list, has_keys && view.side == Side::List, buf);
    if rects.list.width > 0 && rects.preview.width > 0 {
        // A one-column gutter between the halves.
        let x = rects.list.x + rects.list.width;
        for y in rects.list.y..rects.list.y + rects.list.height {
            buf[(x, y)].set_char('\u{2502}').set_style(dim);
        }
    }
    render_preview(
        view,
        rects.preview,
        has_keys && view.side == Side::Preview,
        buf,
    );

    // Footer: a question, else the last error, else the last note, else the
    // keys for whatever has them.
    let footer = match (&view.prompt, &view.status, &view.note) {
        (Some(Prompt::Upload(text)), _, _) => Span::styled(
            format!(
                " upload from (a path on this machine, or drag a file here): {}\u{2588}",
                sanitize(text)
            ),
            Style::default().fg(Color::Cyan),
        ),
        (Some(Prompt::OverwriteMany { locals, taken }), _, _) => Span::styled(
            format!(
                " {} of the {} dropped already exist here \u{2014} y replaces them \u{b7} n uploads only the others \u{b7} esc cancels",
                taken.len(),
                locals.len()
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        (Some(Prompt::Overwrite { local }), _, _) => Span::styled(
            format!(
                " {} already exists here \u{2014} replace it (a folder is merged into it)? y/n",
                sanitize(local.rsplit(['/', '\\']).next().unwrap_or(local))
            ),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        (Some(Prompt::NewFolder(text)), _, _) => Span::styled(
            format!(" new folder: {}\u{2588}", sanitize(text)),
            Style::default().fg(Color::Cyan),
        ),
        (Some(Prompt::Rename { text, .. }), _, _) => Span::styled(
            format!(
                " rename to (or a path to move it): {}\u{2588}",
                sanitize(text)
            ),
            Style::default().fg(Color::Cyan),
        ),
        (Some(Prompt::Delete { name, is_dir, .. }), _, _) => Span::styled(
            if *is_dir {
                format!(
                    " delete the folder {} and everything in it? y/n",
                    sanitize(name)
                )
            } else {
                format!(" delete {}? y/n", sanitize(name))
            },
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        (None, Some(status), _) => Span::styled(
            format!(" {}", sanitize(status)),
            Style::default().fg(Color::Red),
        ),
        (None, None, Some(note)) => Span::styled(
            format!(" {}", sanitize(note)),
            Style::default().fg(Color::Green),
        ),
        (None, None, None) => {
            let keys = if view.filtering {
                FILTER_KEYS
            } else if view.side == Side::Preview && view.preview.is_some() {
                PREVIEW_KEYS
            } else {
                LIST_KEYS
            };
            Span::styled(format!(" {keys}"), dim)
        }
    };
    Paragraph::new(Line::from(footer)).render(rects.footer, buf);
}

fn render_list(view: &FilesView, area: Rect, active: bool, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let visible = view.visible();
    if visible.is_empty() {
        let note = if view.listing.is_none() {
            ""
        } else if view.filter.is_empty() {
            " (empty folder)"
        } else {
            " (nothing matches)"
        };
        Paragraph::new(Line::styled(note, Style::default().fg(Color::DarkGray))).render(area, buf);
        return;
    }
    let width = usize::from(area.width);
    for (row, (index, entry)) in visible
        .iter()
        .enumerate()
        .skip(view.scroll_for(usize::from(area.height)))
        .take(usize::from(area.height))
        .enumerate()
    {
        let y = area.y + row as u16;
        let (icon, color) = match entry.kind {
            EntryKind::Dir => ("\u{25B8} ", Color::Blue),
            EntryKind::LinkDir => ("\u{21AA} ", Color::Cyan),
            EntryKind::LinkFile => ("\u{21AA} ", Color::Gray),
            EntryKind::Broken => ("\u{2717} ", Color::Red),
            EntryKind::Up => ("\u{2191} ", Color::Blue),
            _ => ("  ", Color::Gray),
        };
        let suffix = if entry.kind.is_dir() { "/" } else { "" };
        let size = if entry.kind.is_dir() || entry.kind == EntryKind::Up {
            String::new()
        } else {
            human_size(entry.size)
        };
        let name = sanitize(&entry.name);
        // Name truncated so the size column always fits.
        let room = width.saturating_sub(2 + size.chars().count() + 2);
        let mut shown: String = format!("{name}{suffix}").chars().take(room).collect();
        if format!("{name}{suffix}").chars().count() > room && room > 0 {
            shown.pop();
            shown.push('\u{2026}');
        }
        let pad = width.saturating_sub(2 + shown.chars().count() + size.chars().count() + 1);
        let mut style = Style::default().fg(color);
        if index == view.selected {
            style = if active {
                style.add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                style.add_modifier(Modifier::UNDERLINED)
            };
        }
        let line = Line::from(vec![
            Span::styled(format!("{icon}{shown}{}", " ".repeat(pad)), style),
            Span::styled(format!("{size} "), style.fg(Color::DarkGray)),
        ]);
        Paragraph::new(line).render(Rect::new(area.x, y, area.width, 1), buf);
    }
}

fn render_preview(view: &FilesView, area: Rect, active: bool, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let dim = Style::default().fg(Color::DarkGray);
    let Some(p) = &view.preview else {
        Paragraph::new(Line::styled(" select a file to preview it", dim)).render(area, buf);
        return;
    };
    let title_style = if active {
        Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let title = Line::from(vec![
        Span::styled(format!(" {} ", sanitize(&p.name)), title_style),
        Span::styled(format!("  {}", human_size(p.size)), dim),
    ]);
    Paragraph::new(title).render(Rect::new(area.x, area.y, area.width, 1), buf);
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    match &p.body {
        PreviewBody::Binary => {
            Paragraph::new(Line::styled(" binary file \u{2014} not shown", dim)).render(body, buf);
        }
        PreviewBody::External => {
            Paragraph::new(Line::styled(
                " image or PDF \u{2014} press o to open it on this machine",
                dim,
            ))
            .render(body, buf);
        }
        PreviewBody::Text {
            lines,
            truncated,
            loaded,
        } => {
            let gutter = lines.len().to_string().len();
            let rows = usize::from(body.height);
            let mut out: Vec<Line> = lines
                .iter()
                .enumerate()
                .skip(p.scroll)
                .take(rows)
                .map(|(n, line)| {
                    let mut spans = vec![Span::styled(format!("{:>gutter$} ", n + 1), dim)];
                    spans.extend(skip_columns(line, p.hscroll));
                    Line::from(spans)
                })
                .collect();
            if *truncated && p.scroll + rows > lines.len() && out.len() < rows {
                out.push(Line::styled(
                    format!(
                        " \u{2026} only the first {} shown{}",
                        human_size(*loaded),
                        if *loaded < crate::files::PREVIEW_BYTES {
                            " \u{2014} enter shows more"
                        } else {
                            ""
                        }
                    ),
                    dim,
                ));
            }
            Paragraph::new(out).render(body, buf);
        }
    }
}

/// The editor: a title, the lines with numbers, and the cursor as a
/// reversed cell. Scrolls sideways just enough to keep the cursor in view.
fn render_editor(editor: &crate::editor::Editor, area: Rect, active: bool, buf: &mut Buffer) {
    if area.width == 0 || area.height < 2 {
        return;
    }
    let dim = Style::default().fg(Color::DarkGray);
    let mut title = vec![Span::styled(
        format!(" editing {} ", sanitize(&editor.name)),
        Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
    )];
    if editor.dirty {
        title.push(Span::styled(
            " \u{25cf} unsaved",
            Style::default().fg(Color::Yellow),
        ));
    }
    Paragraph::new(Line::from(title)).render(Rect::new(area.x, area.y, area.width, 1), buf);

    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let gutter = editor.lines.len().to_string().len() + 1;
    let text_width = usize::from(body.width).saturating_sub(gutter).max(1);
    let cursor_col = editor.cursor_column();
    let offset = cursor_col.saturating_sub(text_width - 1);
    for (screen_row, (n, line)) in editor
        .lines
        .iter()
        .enumerate()
        .skip(editor.scroll)
        .take(usize::from(body.height))
        .enumerate()
    {
        let y = body.y + screen_row as u16;
        let shown: String = crate::editor::display_line(line)
            .chars()
            .skip(offset)
            .take(text_width)
            .collect();
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{:>w$} ", n + 1, w = gutter - 1), dim),
            Span::raw(shown),
        ]))
        .render(Rect::new(body.x, y, body.width, 1), buf);
        if n == editor.row && active {
            let x = body.x + (gutter + cursor_col - offset) as u16;
            if x < body.x + body.width {
                let cell = &mut buf[(x, y)];
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
                if cell.symbol().trim().is_empty() {
                    cell.set_char(' ');
                }
            }
        }
    }
}

/// `line` with its first `n` columns dropped, for sideways scrolling.
fn skip_columns(line: &Line<'static>, n: usize) -> Vec<Span<'static>> {
    let mut left = n;
    let mut out = Vec::new();
    for span in &line.spans {
        let count = span.content.chars().count();
        if left >= count {
            left -= count;
            continue;
        }
        let rest: String = span.content.chars().skip(left).collect();
        left = 0;
        out.push(Span::styled(rest, span.style));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::{Entry, Listing};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn drawn(view: &FilesView, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render_files(view, f.area(), true, f.buffer_mut()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample() -> FilesView {
        let mut v = FilesView::new(None);
        v.set_listing(Listing {
            current: "/home/me/dev/app".into(),
            parent: Some("/home/me/dev".into()),
            entries: vec![
                Entry {
                    name: "src".into(),
                    path: "/x/src".into(),
                    kind: EntryKind::Dir,
                    size: 0,
                    mtime: 0.0,
                },
                Entry {
                    name: "main.rs".into(),
                    path: "/x/main.rs".into(),
                    kind: EntryKind::File,
                    size: 4300,
                    mtime: 0.0,
                },
            ],
            truncated: false,
        });
        v
    }

    #[test]
    fn draws_the_folder_its_entries_and_the_keys() {
        let text = drawn(&sample(), 80, 8);
        assert!(text.contains("/home/me/dev/app"), "{text}");
        assert!(text.contains("src/"), "{text}");
        assert!(text.contains("main.rs"), "{text}");
        assert!(text.contains("4.2 KB"), "{text}");
        assert!(text.contains("click open"), "{text}");
    }

    #[test]
    fn draws_a_file_with_line_numbers() {
        let mut v = sample();
        v.set_preview("/x/main.rs", 26, b"fn main() {\n    run();\n}\n");
        let text = drawn(&v, 80, 8);
        assert!(text.contains("1 fn main() {"), "{text}");
        assert!(text.contains("2     run();"), "{text}");
    }

    #[test]
    fn a_hostile_file_name_is_drawn_inert() {
        let mut v = sample();
        v.listing.as_mut().unwrap().entries[0].name = "a\x1b[2Jb".into();
        let text = drawn(&v, 80, 8);
        assert!(!text.contains('\x1b'));
        assert!(text.contains("a\u{FFFD}[2Jb"), "{text}");
    }

    #[test]
    fn an_error_takes_the_footer() {
        let mut v = sample();
        v.status = Some("/etc is outside the folders the file browser may use".into());
        assert!(drawn(&v, 80, 8).contains("outside the folders"));
    }
}
