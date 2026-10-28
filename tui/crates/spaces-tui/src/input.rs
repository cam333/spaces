//! Keystroke routing: the single place that decides whether a key event is
//! chrome (palette) or pane input, and how pane input is byte-encoded.
//!
//! [`route`] is fed directly by terminal input (real keystrokes, once Task
//! 9 wires up the event loop) — exactly the kind of untrusted-by-origin
//! input this workspace's no-panic rule covers, same as `term.rs::feed` for
//! network bytes. No path through `route` or [`encode_pane_input`] calls
//! `unwrap`/`expect`/`panic!`, and `Palette::selected`'s arithmetic uses
//! saturating operations, further clamped against the live candidate count
//! where one is available (see "Enter in an open palette" below), so moving
//! the selection can never overflow, underflow, or point past the list a
//! caller would look it up against.
//!
//! # `KeyEventKind::Release` is rejected, not filtered on `Press`
//!
//! On Windows, crossterm 0.28.1 reports a key event for both key-down and
//! key-up (`event/sys/windows/parse.rs:283-291` sets `kind =
//! KeyEventKind::Press`/`Release` from the raw `key_down` flag, and nothing
//! downstream filters `Release` back out — verified against
//! `event/source/windows.rs`). Left unfiltered, one physical keystroke
//! would route twice: a character doubled in the pane, `ctrl+k` toggling
//! the palette open and immediately shut on the same press, a `ctrl+space
//! d` chord's `d`-release leaking a stray `d` into the pane. `route`'s
//! first statement rejects `KeyEventKind::Release`.
//!
//! It rejects `Release` specifically rather than requiring `== Press`,
//! because the common `if key.kind == KeyEventKind::Press` idiom also drops
//! `KeyEventKind::Repeat` — the Kitty keyboard protocol's held-key
//! auto-repeat event — which must still reach the pane so a held arrow key
//! keeps moving.
//!
//! # The `ctrl+space` chords
//!
//! Detach is a two-key sequence, but `route`'s fixed signature has no parameter
//! to carry "the previous key was `ctrl+space`" across calls.
//! [`Palette::awaiting_chord_key`] is where that state lives instead; see its
//! doc comment for why. A second completion was added on the same prefix — `s`
//! toggles the side-by-side split — so the set lives in [`chord_completion`]
//! rather than inline in `route`. It has since grown `m` (mouse capture), `o`
//! (the agents panel's sort order) and `b` (hide or show the sidebar).
//!
//! Trade-off, not a decision this module gets to make differently:
//! intercepting `ctrl+space` costs the user the NUL byte (0x00) it
//! otherwise produces, which some terminal programs bind to something else
//! (`emacs set-mark`, for one). Recorded here as a known limitation; not
//! addressed by this implementation.
//!
//! `ctrl+c` deliberately reaches the pane instead of being treated as a
//! kill switch — interrupting an agent is the single most common thing a
//! user does with this client, and a TUI that quit on `ctrl+c` would be
//! unusable. [`Action::Quit`] is consequently never constructed by `route`
//! at all; it exists in the `Action` enum for the main event loop to
//! return once its own shutdown path (clean detach, a fatal transport
//! error, ...) decides the process should exit. Don't read its absence
//! here as an oversight.
//!
//! # CapsLock and Shift on `ctrl+k` / the chord's `d` and `s`
//!
//! Windows resolves the *character* for a `ctrl`-chorded key from the
//! active keyboard layout independently of the `ctrl` bit, and flips its
//! case whenever Shift is physically held *or* CapsLock is on (an XOR —
//! `event/sys/windows/parse.rs`'s `get_char_for_key`), while only Shift
//! also sets `KeyModifiers::SHIFT` (`ControlKeyState`'s `From` impl reads
//! the literal Shift key, not CapsLock). So `ctrl+k` can legitimately
//! arrive as `KeyCode::Char('K')` with modifiers `CONTROL` (CapsLock on) or
//! `CONTROL | SHIFT` (Shift physically held) as well as the "obvious"
//! `Char('k')` + `CONTROL`. `is_ctrl_k`/`is_ctrl_space` match their letter
//! case-insensitively via [`ctrl_and_maybe_shift`], and the chord's second
//! key accepts `d`/`D` the same way via [`is_unmodified_or_shifted`].
//!
//! # Enter in an open palette
//!
//! Resolving "the selected `Command`" needs the live candidate list, so
//! `route` takes `model: &Model` and resolves it itself via
//! `super::ui::palette::visible_candidates(model, palette).get(palette.selected)`
//! — never indexed, since the query may have changed since `selected` was
//! last moved. An empty or out-of-range selection just closes the palette
//! and returns [`Action::Redraw`] with no `Command`.
//!
//! `visible_candidates`, not `candidates`: the palette has a second mode
//! (the space-naming prompt the sidebar's `+ new workspace` button
//! opens), and `Enter`, `Down` and the renderer must all resolve against the
//! *same* list or `enter` fires something the user is not looking at. An
//! empty naming query yields an empty list, which is exactly the
//! "out-of-range selection closes without a `Command`" case above.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::model::{Model, SidebarDrag};
use crate::ui::menu;
use crate::ui::palette::{self, visible_candidates, Command, Palette};
use crate::ui::selection;
use crate::ui::sidebar::{self, SidebarHit};
use crate::ui::tabs::{self, TabBar};

/// Rows one wheel notch moves a sidebar list.
const SCROLL_ROWS: usize = 1;

/// Lines one wheel notch moves a pane''s scrollback. Three, the near-universal
/// terminal default — one line per notch makes reading back through an agent''s
/// output interminable, and a whole page overshoots.
const PANE_SCROLL_LINES: i32 = 3;

/// What a routed keystroke resolved to.
#[derive(Debug)]
pub enum Action {
    /// A palette command was selected/confirmed.
    Command(Command),
    /// Bytes to write to the focused pane's PTY.
    PaneInput(Vec<u8>),
    /// Chrome state changed (palette opened/closed, query edited, selection
    /// moved) but no `Command` and no pane input resulted.
    Redraw,
    /// Never produced by `route` — see the module docs for why this is
    /// deliberate, not missing.
    Quit,
    /// A drag selection was released with text in it. `main.rs` holds the pane
    /// grids, so it is the one that reads the text out and sends it to the
    /// clipboard; the selection itself stays on [`Palette::selection`].
    CopySelection,
    /// Move one pane''s view `delta` lines back into its scrollback (positive)
    /// or forward towards the live output (negative). Applied by `main.rs`
    /// for the same reason [`Action::CopySelection`] is: the grids live there.
    ScrollPane { pane_id: String, delta: i32 },
    /// A key for the Files tab, which has the keyboard.
    FilesKey(KeyEvent),
    /// The keystroke had no effect: it was a `KeyEventKind::Release`, no
    /// pane is focused and it wasn't a chrome binding, it was consumed as
    /// an in-progress `ctrl+space` chord with nothing to show for it yet,
    /// or (while the palette is open) it simply isn't bound to anything.
    None,
}

/// `true` iff `modifiers` is exactly `CONTROL`, or `CONTROL` plus `SHIFT`.
/// Covers the CapsLock-on and physical-Shift variants of a `ctrl+<letter>`
/// chord (see the module docs) without also matching `alt`/`super`/...
/// combinations, which are a different chord entirely.
fn ctrl_and_maybe_shift(modifiers: KeyModifiers) -> bool {
    modifiers == KeyModifiers::CONTROL || modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT)
}

/// `true` iff `modifiers` is empty or exactly `SHIFT` — no `ctrl`/`alt`/...
fn is_unmodified_or_shifted(modifiers: KeyModifiers) -> bool {
    modifiers.is_empty() || modifiers == KeyModifiers::SHIFT
}

/// `true` iff `key` is `ctrl+space` (case has no meaning for a space, but
/// see [`ctrl_and_maybe_shift`] for why the modifier check still tolerates
/// an extra `SHIFT` bit) — crossterm 0.28 parses the raw NUL byte a
/// terminal sends for that chord (Unix) and the corresponding
/// virtual-key/control-state pair (Windows) identically, as
/// `KeyCode::Char(' ')` — verified against crossterm's own
/// `event/sys/unix/parse.rs` (`b'\0' => KeyEvent::new(KeyCode::Char(' '),
/// KeyModifiers::CONTROL)`) and `event/sys/windows/parse.rs`.
fn is_ctrl_space(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char(' ') && ctrl_and_maybe_shift(key.modifiers)
}

/// `true` iff `key` is `ctrl+k`, the palette toggle — matched
/// case-insensitively; see the module docs' CapsLock/Shift section.
fn is_ctrl_k(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'k'))
        && ctrl_and_maybe_shift(key.modifiers)
}

/// The [`Command`] `key` completes an already-armed `ctrl+space` prefix
/// with, or `None` if it completes nothing — in which case [`route`] lets
/// it fall through to ordinary routing rather than swallowing it.
///
/// Both letters are matched case-insensitively and tolerate a `SHIFT`
/// modifier for exactly the reason the palette toggle does — see the module
/// docs' CapsLock/Shift section: on Windows the character a key produces is
/// case-flipped by CapsLock without `KeyModifiers::SHIFT` ever being set, so
/// a strict `Char('d')`-with-no-modifiers match would silently stop
/// detaching for any user with CapsLock on.
///
/// Pulled out of `route`'s chord arm as its own function so the growing set
/// of completions is one table rather than a chain of `if`s inside the
/// hottest branch in the router.
fn chord_completion(key: &KeyEvent) -> Option<Command> {
    if !is_unmodified_or_shifted(key.modifiers) {
        return None;
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if c.eq_ignore_ascii_case(&'d') {
        Some(Command::Detach)
    } else if c.eq_ignore_ascii_case(&'s') {
        Some(Command::ToggleSplit)
    } else if c.eq_ignore_ascii_case(&'m') {
        // `m` for mouse. Reachable from the keyboard specifically because the
        // thing it turns on is *the mouse* — a user who has just turned
        // clicking off cannot click their way back.
        Some(Command::ToggleMouse)
    } else if c.eq_ignore_ascii_case(&'b') {
        // `b` for bar: hide the sidebar, or bring it back — including on a
        // narrow terminal that hid it automatically.
        Some(Command::ToggleSidebar)
    } else if c.eq_ignore_ascii_case(&'f') {
        // `f` for files: the active space's Files tab.
        Some(Command::OpenFiles)
    } else if c.eq_ignore_ascii_case(&'o') {
        // `o` for order: the agents panel's `grouped`/`priority` sort, which
        // was otherwise reachable only by clicking that panel's header.
        Some(Command::ToggleAgentSort)
    } else {
        None
    }
}

/// The single place that decides whether a keystroke is chrome (the
/// palette) or pane input, per these bindings:
/// `ctrl+k` toggles the palette; while open, printable keys edit the query,
/// `up`/`down` move the selection, `enter` emits the selected `Command`,
/// `esc` closes it; while closed, `ctrl+space d` detaches and everything
/// else is pane input.
pub fn route(key: KeyEvent, palette: &mut Palette, focused: Option<&str>, model: &Model) -> Action {
    // See the module docs' `KeyEventKind::Release` section: reject release
    // events outright so one physical keystroke never routes twice.
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }

    // Second half of a `ctrl+space` chord. This check runs before anything
    // else so a cancelling key still falls through to whatever it would
    // ordinarily have done (open the palette, reach the pane, ...) rather
    // than being swallowed silently.
    if palette.awaiting_chord_key {
        palette.awaiting_chord_key = false;
        if let Some(cmd) = chord_completion(&key) {
            return Action::Command(cmd);
        }
        // Any other key (including a second `ctrl+space`, which re-arms
        // the prefix below, or `esc`, which reaches the pane as 0x1b like
        // any other cancelling key) falls through to ordinary routing.
    }

    // `ctrl+space` only starts the detach chord while the palette is
    // closed — the bindings scope it to "while closed". While open
    // it is simply an unhandled key inside the palette (falls through to
    // the `palette.open` branch below, which never produces pane input).
    if is_ctrl_space(&key) && !palette.open {
        palette.awaiting_chord_key = true;
        return Action::None;
    }

    if is_ctrl_k(&key) {
        if palette.open {
            close_palette(palette);
        } else {
            palette.open = true;
            palette.tab_menu = None;
        }
        return Action::Redraw;
    }

    if palette.open {
        return route_within_palette(key, palette, model);
    }

    // The Files tab, when it has the keys, gets every key the chord and the
    // palette did not take.
    if model.files_have_keys() {
        return Action::FilesKey(key);
    }

    route_as_pane_input(&key, focused)
}

/// The single place that decides what a mouse event does: focus a pane from
/// the sidebar, a tab, or the split's own half of the screen; create a pane
/// or a space; scroll either sidebar list; flip the agents panel's sort;
/// open the palette; dismiss it. A sibling of [`route`], not a replacement —
/// the two are never called for the same event, since `main.rs`'s event loop
/// dispatches on `crossterm::event::Event`'s own `Key`/`Mouse` variants
/// before either is reached.
///
/// Only three [`MouseEventKind`]s do anything at all — a left
/// [`MouseEventKind::Down`], [`MouseEventKind::ScrollUp`] and
/// [`MouseEventKind::ScrollDown`] — checked first so nothing below can react
/// to mouse movement, a right-click, or a horizontal (touchpad) scroll,
/// which has no analogue here.
///
/// # Why this takes `&mut Model`
///
/// Three of the sidebar's controls change *only* local presentation: which
/// space is selected, which way the agents panel is sorted, and where each
/// of its two lists is scrolled to. None of them is a request to the server,
/// so routing them through `Action::Command` — which `decide` refuses while
/// the model is `stale`, and which `handle_command` can only run with a live
/// connection — would make the sidebar stop responding to the mouse exactly
/// when a user is most likely to be poking at it. Taking `&mut Model` lets
/// this function apply them directly and answer [`Action::Redraw`].
///
/// It also removes the duplicate scroll bookkeeping the caller used to do:
/// the offsets are clamped here with [`sidebar::clamp_scroll`], the same
/// function `render_sidebar` clamps with on every frame, so a click's row
/// maths and the frame it landed on can never disagree.
///
/// # Precedence
///
/// While the palette is open, a left click closes it (mirroring `route`'s
/// own `Esc`/`ctrl+k` handling — see [`close_palette`]) and returns
/// [`Action::Redraw`], wherever on screen it landed: this function is never
/// given the overlay's own rect, so every click while it is open counts as
/// outside. A wheel event is ignored outright instead ([`Action::None`],
/// nothing touched) rather than scrolling a list hidden underneath the
/// overlay — a state change the user cannot see happen, only notice later as
/// the list having silently jumped.
///
/// Otherwise, in order: the sidebar (via [`sidebar::hit_test`], which is
/// what knows about buttons, rows and the two bodies), then the tab bar
/// (`tabs`), then `panes` — the `(region, pane_id)` pairs
/// `ui::visible_pane_regions` produced for this same frame. The sidebar wins
/// over anything that overlaps it; a click that misses everything is
/// [`Action::None`].
///
/// A wheel event only ever moves a scroll offset, and only over one of the
/// sidebar's two list bodies. Over the tab bar, a pane, or the status bar it
/// does nothing at all: a remote PTY is not something this client scrolls
/// back through, and falling through to the sidebar would scroll a list the
/// pointer is nowhere near.
///
/// `tabs` and `panes` are passed in rather than recomputed here for the same
/// reason `sidebar` is a `Rect` parameter: they must be the geometry the
/// frame was actually drawn with.
///
/// **Never** returns `Action::PaneInput` — a mouse event is chrome or
/// navigation, never bytes for the pane's PTY.
pub fn route_mouse(
    event: MouseEvent,
    screen: Rect,
    tabs: &TabBar,
    panes: &[(Rect, String)],
    palette: &mut Palette,
    model: &mut Model,
) -> Action {
    // Derived here, not passed in. Each overlay has to be hit-tested against
    // the rect *its own renderer* used — the sidebar's column, the palette's
    // centred pane area, the menu's whole frame — and every one of those is a
    // pure function of the frame. Taking them as parameters meant a caller
    // could hand over a rect that disagreed with what was drawn, and one did:
    // `main.rs` passed the palette's overlay where the menu's frame belonged,
    // so a space menu drawn at column 3 was hit-tested against a box clamped
    // into column 28 and every click on it missed. The compiler cannot catch
    // a wrong `Rect`; not having the parameter can.
    let columns = model.sidebar.columns(screen.width);
    let sidebar_column = crate::ui::sidebar_area(screen, columns);
    let sidebar = sidebar::content_area(sidebar_column);
    let is_wheel = matches!(
        event.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    );
    let is_right = event.kind == MouseEventKind::Down(MouseButton::Right);
    let is_drag = event.kind == MouseEventKind::Drag(MouseButton::Left);
    let is_up = event.kind == MouseEventKind::Up(MouseButton::Left);
    if !is_wheel
        && !is_right
        && !is_drag
        && !is_up
        && event.kind != MouseEventKind::Down(MouseButton::Left)
    {
        return Action::None;
    }

    // Drag and release belong to a selection in progress and to nothing else,
    // so they are resolved before every overlay: a drag that began in a pane
    // must keep extending even as the pointer crosses the sidebar, and the
    // release that ends it must not be mistaken for a click on whatever is
    // under the pointer by then.
    //
    // A drag that began on the sidebar's edge or divider is resolved first of
    // all: it was armed by a press on chrome, never in a pane, so there is no
    // selection it could be extending.
    if is_drag || is_up {
        if let Some(drag) = model.sidebar.drag {
            return route_sidebar_drag(event, is_up, drag, sidebar, screen.width, model);
        }
        return route_drag(event, is_up, palette);
    }
    // Every press or notch starts a new gesture. A sidebar drag whose release
    // never arrived — let go outside the terminal window, say — must not keep
    // resizing the sidebar on the next unrelated drag.
    model.sidebar.drag = None;

    // The context menu is the topmost overlay, so it is resolved before
    // anything underneath it — including the palette, which cannot be open at
    // the same time (opening either closes the other) but must not be
    // *assumed* closed by an ordering that a later change could break.
    if palette.tab_menu.is_some() {
        return route_within_tab_menu(event, is_wheel, is_right, screen, palette, model);
    }

    if is_right {
        // Right-click means one thing: a context menu. Anywhere it does not
        // open one it is inert, never "the same as a left click" — a stray
        // right-click must not focus a pane or create anything.
        //
        // A tab opens the pane menu; a space row in the sidebar opens the
        // space menu. Tabs are checked first only because they cannot overlap
        // the sidebar, so the order is arbitrary and does not need defending.
        let target = tabs::tab_at(tabs, event.column, event.row)
            .map(|id| menu::MenuTarget::Pane(id.to_string()))
            .or_else(|| {
                tabs::is_files_tab(tabs, event.column, event.row)
                    .then(|| tabs.space_id.clone())
                    .flatten()
                    .map(menu::MenuTarget::Files)
            })
            .or_else(|| {
                match sidebar::hit_test(
                    sidebar,
                    model,
                    model.sidebar_scroll,
                    model.agents_scroll,
                    event.column,
                    event.row,
                ) {
                    // An agent row is a pane too, so it gets the pane menu —
                    // the sidebar is where you reach a pane whose tab has
                    // scrolled out of the strip.
                    sidebar::SidebarHit::Agent(pane_id) => Some(menu::MenuTarget::Pane(pane_id)),
                    sidebar::SidebarHit::Space(_, space_id) => {
                        Some(menu::MenuTarget::Space(space_id))
                    }
                    _ => None,
                }
            });
        return match target {
            Some(target) => {
                palette.tab_menu = Some(menu::TabMenu {
                    target,
                    column: event.column,
                    row: event.row,
                });
                Action::Redraw
            }
            None => Action::None,
        };
    }

    if palette.open {
        // The palette is centred in the *pane* area, the menu is anchored
        // anywhere on screen — so each is hit-tested against the rect its own
        // renderer used. Passing one rect for both is what made every space
        // menu click miss: the menu was drawn at the click and hit-tested
        // against a box clamped into the pane column.
        return route_within_open_palette(
            event,
            is_wheel,
            crate::ui::palette_area(screen, columns),
            palette,
            model,
        );
    }

    // Any press outside a pane drops a previous selection: leaving the last
    // highlight on screen after the user has moved on reads as a live one.
    palette.selection = None;

    // A toast sits over the pane area, so it is resolved before the pane
    // under it can arm a selection. Clicking one focuses the pane it names
    // and dismisses it — it has done its job.
    if !is_wheel {
        let toasts = crate::ui::toast::toast_layout(crate::ui::pane_area(screen, columns), model);
        if let Some(pane_id) = crate::ui::toast::toast_at(&toasts, event.column, event.row) {
            let pane_id = pane_id.to_string();
            model.dismiss_toast(&pane_id);
            return Action::Command(Command::FocusPane(pane_id));
        }
    }

    // The sidebar's edge is a handle, not part of either section: a press
    // there arms a width drag, and the drag samples that follow move it.
    if !is_wheel && rect_contains(sidebar::edge(sidebar_column), event.column, event.row) {
        model.sidebar.drag = Some(SidebarDrag::Width);
        return Action::Redraw;
    }

    let hit = sidebar::hit_test(
        sidebar,
        model,
        model.sidebar_scroll,
        model.agents_scroll,
        event.column,
        event.row,
    );

    if is_wheel {
        // A wheel notch over a pane scrolls that pane's own scrollback — the
        // grid has kept 10,000 lines of it all along (see
        // `term::PaneTerm::scroll`), and nothing until now could reach it.
        //
        // The pane under the *pointer*, not the focused one: with a split on
        // screen, scrolling the half you are pointing at is the only behaviour
        // that is not a surprise. Checked before the sidebar so a pane region
        // can never be shadowed by it.
        if let Some((_, pane_id)) = panes
            .iter()
            .find(|(rect, _)| rect_contains(*rect, event.column, event.row))
        {
            return Action::ScrollPane {
                pane_id: pane_id.clone(),
                // Positive is back into history, which is what pushing the
                // wheel up means everywhere else.
                delta: if event.kind == MouseEventKind::ScrollUp {
                    PANE_SCROLL_LINES
                } else {
                    -PANE_SCROLL_LINES
                },
            };
        }
        return scroll_sidebar(event.kind, sidebar, model, &hit);
    }

    match hit {
        // Selecting a space is local: it decides which space the tab bar
        // shows and its `+` creates into. Focusing that space's first pane on
        // top of it is what makes one click do the obvious thing — but a
        // space with no panes yet still becomes the selected one, which is
        // the whole reason `selected_space` exists (see its doc comment).
        SidebarHit::Space(node, id) => open_space(model, node, id),
        SidebarHit::Agent(pane) => Action::Command(Command::FocusPane(pane)),
        // A space needs a name, which a click cannot carry, so `new` opens
        // the palette's naming prompt. `Action::Redraw`, not a `Command`:
        // nothing has been asked of the server yet, only of the user.
        SidebarHit::New => {
            open_space_prompt(palette);
            Action::Redraw
        }
        // `menu` is this client's global menu, and the command palette
        // already is one — a second menu listing a subset of it would be a
        // second place to keep in sync.
        SidebarHit::Menu => {
            palette.open = true;
            palette.tab_menu = None;
            Action::Redraw
        }
        // The divider is the split's handle: the press arms the drag, and
        // `route_sidebar_drag` moves it.
        SidebarHit::Divider => {
            model.sidebar.drag = Some(SidebarDrag::Split);
            Action::Redraw
        }
        SidebarHit::SortToggle => {
            model.toggle_agent_sort();
            Action::Redraw
        }
        // Inside a list but past its last entry: nothing to select, and
        // nothing changed, so not even a redraw.
        SidebarHit::SpacesBody | SidebarHit::AgentsBody => Action::None,
        SidebarHit::None => {
            if let Some(direction) = tabs::scroll_at(tabs, event.column, event.row) {
                return scroll_tabs(direction, tabs, model);
            }
            if let Some(pane) = tabs::tab_at(tabs, event.column, event.row) {
                return Action::Command(Command::FocusPane(pane.to_string()));
            }
            if tabs::is_files_tab(tabs, event.column, event.row) {
                return Action::Command(Command::ShowFiles);
            }
            if tabs::is_new_tab(tabs, event.column, event.row) {
                // Opens the agent picker rather than creating a pane outright.
                // A click cannot carry *which agent*, and the runtime's
                // default for a missing `agentType` is `shell` — so the
                // straight-to-`CreatePane` version of this button produced
                // bare shells with no agent in them, every time.
                //
                // `space_id` is `Some` whenever `is_new_tab` is true — see
                // that function — but resolved with a `match` rather than an
                // `unwrap`, per this module's no-panic rule.
                return match &tabs.space_id {
                    Some(space) => {
                        open_agent_picker(palette, space.clone());
                        Action::Redraw
                    }
                    None => Action::None,
                };
            }
            pane_region_click(event, panes, palette, model.focused.as_deref())
        }
    }
}

/// Moves whatever a sidebar drag is holding to follow the pointer, or ends
/// the drag on release.
///
/// - [`SidebarDrag::Width`]: the edge lands under the pointer, so the column
///   becomes `pointer - sidebar.x + 1` wide, through
///   [`sidebar::clamp_width`] — stored already clamped, so a drag far past
///   the limit does not leave a width the user cannot see building up.
/// - [`SidebarDrag::Split`]: the divider lands on the pointer's row, so the
///   spaces section becomes `pointer - sidebar.y` rows tall, through
///   [`sidebar::clamp_spaces_height`] against this terminal's sidebar height.
///
/// `sidebar` is the sidebar's *content* area for this frame. Every sample
/// answers [`Action::Redraw`]; `main.rs` notices a changed pane area and
/// resizes the visible panes to match.
fn route_sidebar_drag(
    event: MouseEvent,
    is_up: bool,
    drag: SidebarDrag,
    sidebar: Rect,
    screen_width: u16,
    model: &mut Model,
) -> Action {
    if is_up {
        model.sidebar.drag = None;
        return Action::Redraw;
    }
    match drag {
        SidebarDrag::Width => {
            let requested = event.column.saturating_sub(sidebar.x).saturating_add(1);
            model.sidebar.width = sidebar::clamp_width(requested, screen_width);
        }
        SidebarDrag::Split => {
            let requested = event.row.saturating_sub(sidebar.y);
            model.sidebar.spaces_height =
                Some(sidebar::clamp_spaces_height(requested, sidebar.height));
        }
    }
    Action::Redraw
}

/// Moves the tab strip one tab towards `direction`.
///
/// Steps from [`TabBar::first`] — what this frame actually drew, after the
/// focus reveal and the clamp — not from the raw [`Model::tab_scroll`],
/// which may be stale by any number of tabs. Pins the focused pane (see
/// [`Model::tab_scroll_pin`]) so the reveal does not drag the strip straight
/// back to it on the next frame. Local presentation only, so it works while
/// disconnected, like the sidebar's own scrolling.
///
/// A click on a button that has reached its end changes nothing and answers
/// [`Action::None`].
///
/// [`Model::tab_scroll`]: crate::model::Model::tab_scroll
/// [`Model::tab_scroll_pin`]: crate::model::Model::tab_scroll_pin
fn scroll_tabs(direction: tabs::TabScroll, tabs: &TabBar, model: &mut Model) -> Action {
    let target = match direction {
        tabs::TabScroll::Left => tabs.first.saturating_sub(1),
        tabs::TabScroll::Right => tabs.first.saturating_add(1).min(tabs.max_first),
    };
    if target == tabs.first {
        return Action::None;
    }
    model.tab_scroll = target;
    model.tab_scroll_pin = model.focused.clone();
    Action::Redraw
}

/// Extends or finishes a drag selection. See [`crate::ui::selection`].
///
/// The head is clamped into the selection's own pane region, so dragging out
/// of the pane selects to its edge instead of into the sidebar — and so the
/// text extraction downstream can never be handed coordinates outside the
/// grid it reads.
///
/// A release with a non-empty selection yields [`Action::CopySelection`],
/// which `main.rs` turns into clipboard bytes (it holds the pane grids; this
/// module does not). The selection is deliberately *kept* rather than cleared:
/// staying highlighted until the next click is the only durable feedback that
/// the copy happened. A release with nothing selected — an ordinary click —
/// clears it and reports nothing.
fn route_drag(event: MouseEvent, is_up: bool, palette: &mut Palette) -> Action {
    let Some(sel) = palette.selection.as_mut() else {
        return Action::None;
    };
    if !is_up {
        let region = sel.region;
        sel.head = (
            event.column.clamp(
                region.x,
                region.x.saturating_add(region.width).saturating_sub(1),
            ),
            event.row.clamp(
                region.y,
                region.y.saturating_add(region.height).saturating_sub(1),
            ),
        );
        return Action::Redraw;
    }
    if sel.is_empty() {
        palette.selection = None;
        return Action::None;
    }
    Action::CopySelection
}

/// Handles a mouse event while the tab context menu is open.
///
/// A left click **on an entry** fires it and closes the menu. Anywhere else —
/// inside the box on a border, outside it entirely, a second right-click, a
/// wheel notch — closes the menu and does nothing more. A context menu is
/// modal and short-lived: the one thing it must never do is stay open while
/// the click that missed it also does something underneath.
///
/// `screen` is the whole frame — the same rect `ui::frame::render` passes to
/// `render_menu`, so the rects tested here are the rects that were drawn. It
/// is deliberately *not* the palette overlay: the menu is anchored wherever
/// the right-click landed, including in the sidebar, and laying it out against
/// the pane column instead clamps it somewhere else entirely.
fn route_within_tab_menu(
    event: MouseEvent,
    is_wheel: bool,
    is_right: bool,
    screen: Rect,
    palette: &mut Palette,
    model: &Model,
) -> Action {
    let hit = palette
        .tab_menu
        .as_ref()
        .and_then(|menu| {
            let layout = menu::menu_layout(model, menu, screen)?;
            let index = menu::item_at(&layout, event.column, event.row)?;
            menu::items(model, menu)
                .get(index)
                .map(|(_, cmd)| cmd.clone())
        })
        .filter(|_| !is_wheel && !is_right);
    palette.tab_menu = None;
    match hit {
        // Through `dispatch_command`, not straight to `Action::Command`: the
        // menu offers `new pane…`, `rename…` and `colour…`, all of which need
        // a prompt rather than a request. Returning them raw sent them to
        // `handle_command`'s no-op arms and the entry did nothing at all.
        Some(cmd) => dispatch_command(cmd, palette, model),
        None => Action::Redraw,
    }
}

/// Handles a mouse event while the palette covers the screen.
///
/// A click **on a candidate row** confirms that candidate — the same
/// [`confirm_candidate`] path `enter` takes, so the two can never disagree
/// about what an entry does. This is what the overlay lacked: every click
/// while it was open simply dismissed it, so clicking `New pane in Default…`
/// looked like the entry did nothing at all.
///
/// A click **elsewhere inside the box** (the query line, a border) changes
/// nothing and dismisses nothing: the user is aiming at the modal, and
/// closing it out from under a near-miss is worse than ignoring the click.
/// A click **outside** it dismisses, mirroring `route`'s own `Esc`/`ctrl+k`
/// handling.
///
/// A wheel notch inside the box moves the selection, which
/// [`crate::ui::palette::palette_layout`]'s own `scroll_offset` turns into a
/// scrolled window — the list is longer than the ten rows it draws, and a
/// mouse-native client that cannot reach entry eleven with the mouse is only
/// half mouse-native. Outside the box the wheel does nothing, rather than
/// scrolling a sidebar list hidden underneath.
fn route_within_open_palette(
    event: MouseEvent,
    is_wheel: bool,
    overlay: Rect,
    palette: &mut Palette,
    model: &Model,
) -> Action {
    let Some(layout) = palette::palette_layout(model, palette, overlay) else {
        // Nowhere to hit-test against (a terminal too small to draw the box).
        // A click still dismisses, so the palette can never trap the user.
        if is_wheel {
            return Action::None;
        }
        close_palette(palette);
        return Action::Redraw;
    };

    if !palette::contains(&layout, event.column, event.row) {
        if is_wheel {
            return Action::None;
        }
        close_palette(palette);
        return Action::Redraw;
    }

    if is_wheel {
        let total = palette::visible_candidates(model, palette).len();
        let last = total.saturating_sub(1);
        palette.selected = match event.kind {
            MouseEventKind::ScrollUp => palette.selected.saturating_sub(1),
            // Clamped against the live list for the same reason `route`'s
            // `Down` arm is: `selected` must never point past the list
            // `enter` would look it up against.
            _ => palette.selected.saturating_add(1).min(last),
        };
        return Action::Redraw;
    }

    match palette::candidate_at(&layout, event.column, event.row) {
        Some(index) => confirm_candidate(model, palette, index),
        // Inside the box, on the query line or a border.
        None => Action::None,
    }
}

/// Applies one wheel notch to whichever sidebar list the pointer is over,
/// and reports whether anything moved.
///
/// Returns [`Action::Redraw`] whenever the pointer is over a list body — not
/// only when the offset actually changed — because a notch at the top of a
/// list is a legitimate no-op the user should not have to distinguish from a
/// dropped event, and re-rendering an unchanged frame costs nothing. Over
/// anything else, [`Action::None`] and no state is touched.
///
/// `hit` is the already-computed [`sidebar::hit_test`] result, reused rather
/// than recomputed so the notch and the click that follows it agree about
/// which list the pointer is in.
fn scroll_sidebar(
    kind: MouseEventKind,
    sidebar: Rect,
    model: &mut Model,
    hit: &SidebarHit,
) -> Action {
    let layout = sidebar::sidebar_layout(sidebar, model);
    match hit {
        SidebarHit::Space(..) | SidebarHit::SpacesBody => {
            // Entries here vary in height (a space on a git branch takes two
            // rows), so the clamp is the variable-height one the renderer and
            // the hit-tester use.
            let heights = sidebar::space_heights(&sidebar::sidebar_rows(model));
            let height = layout.spaces_body.height;
            model.sidebar_scroll = stepped(kind, model.sidebar_scroll, |offset| {
                sidebar::clamp_scroll_rows(offset, &heights, height)
            });
            Action::Redraw
        }
        SidebarHit::Agent(_) | SidebarHit::AgentsBody => {
            let entries = sidebar::agent_rows(model).len();
            let height = layout.agents_body.height;
            model.agents_scroll = stepped(kind, model.agents_scroll, |offset| {
                sidebar::clamp_scroll(offset, entries, height, sidebar::AGENT_ENTRY_ROWS)
            });
            Action::Redraw
        }
        // A header, the divider, a button, or outside the sidebar entirely.
        SidebarHit::New
        | SidebarHit::Menu
        | SidebarHit::SortToggle
        | SidebarHit::Divider
        | SidebarHit::None => Action::None,
    }
}

/// `current` after one wheel notch of `kind`, re-clamped against the list it
/// indexes into. The only place [`SCROLL_ROWS`] is added or subtracted.
///
/// `clamp` is that list's own clamp — [`sidebar::clamp_scroll`] for the
/// fixed-height agents panel, [`sidebar::clamp_scroll_rows`] for the spaces
/// list — applied whichever way the notch went, so
/// scrolling *up* also fixes an offset that was already stranded past the end
/// of a list that shrank — the case where a pane closed while the user was
/// scrolled to the bottom. A kind that is not a wheel event leaves `current`
/// alone (still clamped), so callers need no separate guard.
fn stepped(kind: MouseEventKind, current: usize, clamp: impl Fn(usize) -> usize) -> usize {
    let moved = match kind {
        MouseEventKind::ScrollUp => current.saturating_sub(SCROLL_ROWS),
        MouseEventKind::ScrollDown => current.saturating_add(SCROLL_ROWS),
        _ => current,
    };
    clamp(moved)
}

/// Focuses whichever of `panes`' regions a click landed in, or
/// [`Action::None`] for a click that missed all of them (the status bar, or
/// the empty half of a terminal too narrow to have split — see
/// `ui::frame::pane_regions`, which yields no second region there rather
/// than a zero-width one).
///
/// First match wins, and the caller's ordering is what decides ties. In
/// practice `pane_regions` never produces overlapping rects, so there are
/// none — but this must not depend on that: `panes` is a caller-supplied
/// slice, and "pick one deterministically" is the only behavior that can't
/// surprise.
fn pane_region_click(
    event: MouseEvent,
    panes: &[(Rect, String)],
    palette: &mut Palette,
    focused: Option<&str>,
) -> Action {
    let Some((rect, id)) = panes
        .iter()
        .find(|(rect, _)| rect_contains(*rect, event.column, event.row))
    else {
        return Action::None;
    };
    // Every press in a pane arms a selection, whether or not a drag follows.
    // Arming on the press rather than on the first drag event is what makes
    // the anchor the cell the user actually pressed on; a click that never
    // moves stays empty and is cleared on release.
    palette.selection = Some(selection::Selection {
        pane_id: id.clone(),
        region: *rect,
        anchor: (event.column, event.row),
        head: (event.column, event.row),
    });
    // Clicking the pane you are already typing into must not re-run the focus
    // path: `focus_pane` re-attaches, resizes, and clears `model.notice`, so a
    // stray click would wipe the status-bar line explaining why something just
    // failed — and now, the line saying what was copied. The rule lives here
    // rather than in the caller's choice of regions so that selection can be
    // offered in *both* panes while focus is only offered in the other one.
    if focused == Some(id.as_str()) {
        return Action::Redraw;
    }
    Action::Command(Command::FocusPane(id.clone()))
}

/// `true` iff `(column, row)` falls inside `rect` — checked against
/// [`Rect`]'s own `x`/`y`/`width`/`height` fields with saturating
/// arithmetic, so a `rect` sitting near `u16::MAX` (never produced by this
/// crate's own layout code, but every `rect` reaching here is a
/// caller-supplied parameter, not something this function can assume about)
/// can't overflow computing its far edge. A zero-sized rect contains
/// nothing, which is how every layout function in `ui` reports "not on
/// screen this frame".
fn rect_contains(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Handles a keystroke while the palette is open. No path out of this
/// function is `Action::PaneInput` — while open, every keystroke is chrome,
/// never pane input (the `typing_edits_the_query_only_while_open`
/// binding, generalized to every key, not just printable ones).
fn route_within_palette(key: KeyEvent, palette: &mut Palette, model: &Model) -> Action {
    match key.code {
        KeyCode::Esc => {
            close_palette(palette);
            Action::Redraw
        }
        KeyCode::Enter => {
            // See the module docs' "Enter in an open palette" section.
            // `.get`, never indexed — `selected` may point past a list
            // that shrank after the query last changed.
            confirm_candidate(model, palette, palette.selected)
        }
        KeyCode::Up => {
            palette.selected = palette.selected.saturating_sub(1);
            Action::Redraw
        }
        KeyCode::Down => {
            // Clamp against the *current* candidate count so `selected`
            // never points past the list `Enter` would look it up
            // against, however the query has changed since it was last
            // moved. `saturating_add` first so this can't overflow either.
            let max_index = visible_candidates(model, palette).len().saturating_sub(1);
            palette.selected = palette.selected.saturating_add(1).min(max_index);
            Action::Redraw
        }
        // In the folder browser, backspace on an empty query goes up a level,
        // like a file manager's.
        KeyCode::Backspace if palette.query.is_empty() && palette.browse.is_some() => {
            match (palette.asking_cwd.as_ref(), palette.browse.as_ref()) {
                (Some((workspace_id, agent_type)), Some(listing)) => match &listing.parent {
                    Some(parent) => Action::Command(Command::BrowseFolder {
                        workspace_id: workspace_id.clone(),
                        agent_type: agent_type.clone(),
                        path: Some(parent.clone()),
                    }),
                    None => Action::None,
                },
                _ => Action::None,
            }
        }
        KeyCode::Backspace => {
            palette.query.pop();
            Action::Redraw
        }
        // Printable text edits the query. `CONTROL`/`ALT` are excluded so a
        // stray control chord (ctrl+c, alt+something) while the palette is
        // open doesn't get typed into the search box character-by-character.
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            palette.query.push(c);
            Action::Redraw
        }
        // An unhandled key while open changes nothing — not chrome state,
        // not a command, not pane input (see the function doc comment).
        _ => Action::None,
    }
}

/// Acts on candidate `index` of the palette's currently visible list: the
/// single place that decides what confirming an entry means, shared by
/// `enter` and by a click on the entry itself.
///
/// `.get`, never indexed — `index` comes either from
/// [`Palette::selected`] (only ever *clamped* by `Down`, against the list as
/// it was when that key was pressed) or from a click resolved against the
/// frame that was drawn, and the list may have been rebuilt since either.
/// Out of range simply closes the palette with no command.
///
/// [`Command::PickAgent`] is intercepted rather than dispatched: it asks the
/// *user* for the half a click cannot carry (which agent), not the server.
/// Handled here for the same reason `main.rs` intercepts `Detach` before
/// `decide` can refuse it — no connection is involved either way.
fn confirm_candidate(model: &Model, palette: &mut Palette, index: usize) -> Action {
    let cmd = visible_candidates(model, palette)
        .get(index)
        .map(|(_, cmd)| cmd.clone());
    close_palette(palette);
    match cmd {
        Some(cmd) => dispatch_command(cmd, palette, model),
        None => Action::Redraw,
    }
}

/// Resolves one chosen [`Command`] into an [`Action`], opening a prompt
/// instead of dispatching when the command needs something more from the user.
///
/// **Shared by the palette and the context menu**, and that is the point.
/// These four were originally intercepted only on the palette's `enter` path,
/// so every one of them reached `handle_command` — which has no-op arms for
/// them — when chosen from a menu instead. Right-clicking a space and picking
/// `rename…` did nothing at all. Anything that asks the *user* rather than the
/// server has to be caught in one place, or each new entry point silently
/// misses some of them.
fn dispatch_command(cmd: Command, palette: &mut Palette, model: &Model) -> Action {
    match cmd {
        Command::PickAgent { workspace_id } => {
            open_agent_picker(palette, workspace_id);
            Action::Redraw
        }
        Command::AskPaneCwd {
            workspace_id,
            agent_type,
        } => {
            // Seeded from the machine that owns the space, not this one.
            // Offering local paths for a remote machine is worse than
            // offering nothing: they look plausible and are wrong.
            let owner = model
                .owner_of_space(&workspace_id)
                .unwrap_or_else(|| model.local_id_owned());
            let seed = model.suggested_cwd_on(&owner).unwrap_or_default();
            // The typed prompt opens at once and stays if the server cannot
            // list folders; when it can, the browser replaces it, starting
            // from the same suggestion.
            open_cwd_prompt(
                palette,
                workspace_id.clone(),
                agent_type.clone(),
                seed.clone(),
            );
            Action::Command(Command::BrowseFolder {
                workspace_id,
                agent_type,
                path: (!seed.is_empty()).then_some(seed),
            })
        }
        // Both ask the user for the half the menu entry could not carry, so
        // both are intercepted here rather than dispatched.
        Command::RenameSpace { workspace_id } => {
            // Pre-filled with the current name: renaming is usually editing,
            // and `enter` on an unedited prompt is then a harmless no-op
            // rather than a surprise.
            let seed = model
                .local_workspaces()
                .iter()
                .find(|w| w.id == workspace_id)
                .map(|w| w.name.clone())
                .unwrap_or_default();
            open_prompt(palette, seed);
            palette.renaming_space = Some(workspace_id);
            Action::Redraw
        }
        Command::PickSpaceColor { workspace_id } => {
            open_prompt(palette, String::new());
            palette.picking_color = Some(workspace_id);
            Action::Redraw
        }
        Command::PickTheme => {
            open_prompt(palette, String::new());
            palette.picking_theme = true;
            // Start on the chosen theme, so opening the picker changes
            // nothing on screen until the selection moves.
            let chosen = crate::ui::theme::current_name();
            palette.selected = crate::ui::theme::THEMES
                .iter()
                .position(|(name, _)| *name == chosen)
                .unwrap_or(0);
            Action::Redraw
        }
        Command::RenamePane { pane_id } => {
            // Pre-filled with whatever the pane is called now — which may be
            // the shell's own OSC title, and editing that into something
            // stable is the common case.
            let seed = model
                .local_panes()
                .iter()
                .find(|p| p.id == pane_id)
                .map(|p| p.title.clone())
                .unwrap_or_default();
            open_prompt(palette, seed);
            palette.renaming_pane = Some(pane_id);
            Action::Redraw
        }
        Command::PickPaneColor { pane_id } => {
            open_prompt(palette, String::new());
            palette.picking_pane_color = Some(pane_id);
            Action::Redraw
        }
        Command::NewSpace => {
            open_space_prompt(palette);
            Action::Redraw
        }
        // The right-click menu, opened from the keyboard: placed a third of
        // the way across, near the top, and clamped on screen like any other.
        Command::OpenMenu(target) => {
            close_palette(palette);
            palette.tab_menu = Some(menu::TabMenu {
                target,
                column: model.sidebar.screen_width / 3,
                row: 3,
            });
            Action::Redraw
        }
        cmd => Action::Command(cmd),
    }
}

/// Opens space `id` on `node`, as clicking its sidebar row does, and from the
/// palette's space search: selects it, then focuses the pane last focused in
/// it, or restarts its stopped panes, or shows it empty.
pub(crate) fn open_space(model: &mut Model, node: crate::model::NodeId, id: String) -> Action {
    // Selecting a space also selects the machine it is on, so a later
    // create knows where to go. Deriving the machine from the space id
    // later cannot work: ids are unique only within a machine.
    model.selected_node = node;
    // Whatever was last focused *in that space*, not its first pane —
    // clicking away and back should return you to what you were doing
    // there. See `Model::pane_to_focus_in`.
    let target = model.pane_to_focus_in(&id);
    // Nothing running, but panes a restart or reboot stopped: start
    // them again, as opening the space in the web app does.
    let stopped: Vec<String> = if model.selected_node == model.local_id_owned() {
        model
            .stopped_panes_in(&id)
            .iter()
            .map(|p| p.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    model.selected_space = Some(id);
    match target {
        Some(pane) => Action::Command(Command::FocusPane(pane)),
        None if !stopped.is_empty() => Action::Command(Command::RestartPanes(stopped)),
        None => {
            // An empty space shows an empty pane area. Leaving
            // `focused` pointing into the space you just left kept the
            // *previous* space's pane on screen under the new space's
            // (empty) tab bar — which reads as the click not having
            // worked, and worse, keystrokes still reached that pane.
            //
            // Not a detach: the socket and the grid stay put, so
            // selecting a space that has panes again re-focuses
            // instantly. `detached_pane` is left alone because the
            // status bar only reports it while something is focused.
            model.focused = None;
            Action::Redraw
        }
    }
}

/// Opens the palette as the directory prompt for a new `agent_type` pane in
/// `workspace_id` — the step a space's *first* pane goes through.
///
/// Every mode is set explicitly, so no two can ever be live at once (where
/// [`crate::ui::palette::visible_candidates`]' precedence would silently pick
/// one).
/// `seed` pre-fills the query — the most recent directory any live pane is
/// running in ([`Model::last_known_cwd`]), or empty on a session with no panes
/// at all. This is the only prompt that starts with text in it, and
/// deliberately: it asks about a filesystem this client cannot see, so an
/// empty box invites typing a path from memory. A real one that can be edited
/// is a far better starting point than a blank line, and `enter` on the
/// unedited seed puts the pane somewhere that definitely exists.
pub(crate) fn open_cwd_prompt(
    palette: &mut Palette,
    workspace_id: String,
    agent_type: String,
    seed: String,
) {
    open_prompt(palette, seed);
    palette.asking_cwd = Some((workspace_id, agent_type));
}

/// Opens the palette with every mode cleared and `seed` in the query — the
/// shared preamble each prompt-opener runs before setting its own mode.
///
/// Every mode is cleared *explicitly* here rather than in each opener. There
/// are five of them and, as of the space menu, five modes; the version where
/// each opener listed the others by hand needed all twenty-five assignments to
/// stay right, and a new mode had to be added to four places or it would
/// silently survive into the next prompt — with the title naming one mode
/// while [`crate::ui::palette::visible_candidates`] resolved another.
fn open_prompt(palette: &mut Palette, seed: String) {
    palette.open = true;
    palette.query = seed;
    palette.selected = 0;
    palette.naming_space = false;
    palette.picking_agent = None;
    palette.asking_cwd = None;
    palette.browse = None;
    palette.renaming_space = None;
    palette.picking_color = None;
    palette.renaming_pane = None;
    palette.picking_pane_color = None;
    palette.picking_theme = false;
    palette.tab_menu = None;
    palette.selection = None;
    palette.picking_session = None;
}

fn close_palette(palette: &mut Palette) {
    palette.open = false;
    palette.query.clear();
    palette.selected = 0;
    // Every close clears the naming mode, whichever way the palette was
    // closed (`esc`, `enter`, `ctrl+k`, a click elsewhere) — all four route
    // through here. A mode that outlived its own prompt would turn the next
    // plain `ctrl+k` into a space-naming box with no explanation.
    palette.naming_space = false;
    palette.picking_agent = None;
    palette.asking_cwd = None;
    palette.browse = None;
    palette.renaming_space = None;
    palette.picking_color = None;
    palette.renaming_pane = None;
    palette.picking_pane_color = None;
    palette.picking_session = None;
    palette.picking_theme = false;
    // The overlays are mutually exclusive: whichever one the user is opening
    // or closing, the others must not be left behind on screen.
    palette.tab_menu = None;
}

/// Opens the palette as the space-naming prompt — what the sidebar's `new`
/// button does. Sets the same fields [`close_palette`] clears, so the prompt
/// always starts empty rather than inheriting a query typed into some earlier
/// session of the palette.
fn open_space_prompt(palette: &mut Palette) {
    open_prompt(palette, String::new());
    palette.naming_space = true;
}

/// Opens the palette as the agent picker for `workspace_id` — what the tab
/// bar's `+` and the palette's own "New pane in ..." entry do.
///
/// Both modes are set explicitly rather than toggled, so arriving here from
/// the naming prompt (or from a second `+` click) can never leave the palette
/// in both modes at once, where
/// [`crate::ui::palette::visible_candidates`]' precedence would silently
/// decide which one the user got.
fn open_agent_picker(palette: &mut Palette, workspace_id: String) {
    open_prompt(palette, String::new());
    palette.picking_agent = Some(workspace_id);
}

/// Handles a keystroke while the palette is closed: pane input if a pane is
/// focused and the key is in [`encode_pane_input`]'s bounded set, otherwise
/// nothing happens.
fn route_as_pane_input(key: &KeyEvent, focused: Option<&str>) -> Action {
    if focused.is_none() {
        return Action::None;
    }
    match encode_pane_input(key) {
        Some(bytes) => Action::PaneInput(bytes),
        None => Action::None,
    }
}

/// Encodes one key event as PTY input bytes, for the bounded set of keys
/// the client supports: printable characters as UTF-8; `ctrl`+letter as the
/// matching control code (0x01-0x1a); the named keys below as their usual
/// terminal escape sequences. Anything outside that set (function keys,
/// mouse, bracketed paste, an `alt`-chorded character, ...) returns `None`
/// — deliberately out of scope, not an oversight.
fn encode_pane_input(key: &KeyEvent) -> Option<Vec<u8>> {
    match key.code {
        KeyCode::Enter => Some(b"\r".to_vec()),
        KeyCode::Tab => Some(b"\t".to_vec()),
        KeyCode::BackTab => Some(b"\x1b[Z".to_vec()),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        KeyCode::Home => Some(b"\x1b[H".to_vec()),
        KeyCode::End => Some(b"\x1b[F".to_vec()),
        KeyCode::PageUp => Some(b"\x1b[5~".to_vec()),
        KeyCode::PageDown => Some(b"\x1b[6~".to_vec()),
        KeyCode::Delete => Some(b"\x1b[3~".to_vec()),
        KeyCode::Insert => Some(b"\x1b[2~".to_vec()),
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let lower = c.to_ascii_lowercase();
            lower
                .is_ascii_alphabetic()
                .then(|| vec![lower as u8 - b'a' + 1])
        }
        // A meta/alt chord is not in the bounded set this function covers —
        // "anything else -> None" governs rather than downgrading it to a
        // spurious literal keystroke (e.g. `alt+b` must not send plain
        // `b`).
        KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::ALT) => None,
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            Some(c.encode_utf8(&mut buf).as_bytes().to_vec())
        }
        _ => None,
    }
}

#[cfg(test)]
// Same convention as `model.rs`'s test module: build a `Palette::default()`
// and assign just the field(s) each case cares about, which reads clearer
// here than clippy's suggested single struct-literal-with-`..Default::default()`.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use spaces_proto::{AgentState, PaneInfo};

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// Opens the theme picker the way a user does: `ctrl+k`, type "theme",
    /// enter.
    fn open_theme_picker(m: &Model) -> Palette {
        let mut p = Palette::default();
        p.open = true;
        p.query = "theme".into();
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, m);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.open && p.picking_theme, "the picker is open");
        p
    }

    #[test]
    fn the_theme_picker_starts_on_the_chosen_theme_and_previews_the_selection() {
        let m = Model::default();
        let mut p = open_theme_picker(&m);
        let chosen = crate::ui::theme::current_name();
        assert_eq!(
            crate::ui::palette::previewed_theme(&m, &p).as_deref(),
            Some(chosen),
            "opening the picker changes nothing on screen"
        );
        route(key(KeyCode::Down, KeyModifiers::NONE), &mut p, None, &m);
        let next = crate::ui::palette::previewed_theme(&m, &p).expect("a theme is selected");
        assert_ne!(next, chosen, "moving the selection previews another theme");
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::SetTheme(name)) if *name == next),
            "{action:?}"
        );
        assert!(!p.open && !p.picking_theme);
    }

    #[test]
    fn escape_leaves_the_theme_picker_without_choosing() {
        let m = Model::default();
        let mut p = open_theme_picker(&m);
        route(key(KeyCode::Down, KeyModifiers::NONE), &mut p, None, &m);
        let action = route(key(KeyCode::Esc, KeyModifiers::NONE), &mut p, None, &m);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(!p.picking_theme);
        assert_eq!(crate::ui::palette::previewed_theme(&m, &p), None);
    }

    #[test]
    fn the_theme_picker_filters_by_name_and_marks_the_chosen_one() {
        let all = crate::ui::palette::theme_candidates("");
        assert_eq!(all.len(), crate::ui::theme::THEMES.len());
        let chosen = crate::ui::theme::current_name();
        let marked: Vec<_> = all
            .iter()
            .filter(|(l, _)| l.ends_with(" (current)"))
            .collect();
        assert_eq!(marked.len(), 1);
        assert!(marked[0].0.starts_with(chosen));
        let light = crate::ui::palette::theme_candidates("LIGHT");
        assert!(!light.is_empty());
        assert!(light.iter().all(|(l, _)| l.contains("light")), "{light:?}");
    }

    fn one_pane_model(id: &str) -> Model {
        let mut m = Model::default();
        m.local_mut().panes = vec![PaneInfo {
            id: id.into(),
            workspace_id: Some("w1".into()),
            agent_type: "claude".into(),
            agent_state: AgentState::Working,
            seen: true,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        }];
        m
    }

    #[test]
    fn ctrl_k_opens_and_closes_the_palette() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(!p.open);
    }

    #[test]
    fn typing_edits_the_query_only_while_open() {
        let m = Model::default();
        let mut p = Palette::default();
        p.open = true;
        route(
            key(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut p,
            None,
            &m,
        );
        assert_eq!(p.query, "a");

        let mut closed = Palette::default();
        let action = route(
            key(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut closed,
            Some("p1"),
            &m,
        );
        assert_eq!(closed.query, "");
        assert!(matches!(action, Action::PaneInput(b) if b == b"a"));
    }

    #[test]
    fn escape_closes_the_palette_and_clears_the_query() {
        // `Palette` has more fields than this test cares about (see its
        // doc comment in palette.rs), so the relevant fields are named and
        // the rest take their defaults.
        let m = Model::default();
        let mut p = Palette {
            open: true,
            query: "abc".into(),
            selected: 2,
            ..Default::default()
        };
        route(key(KeyCode::Esc, KeyModifiers::NONE), &mut p, None, &m);
        assert!(!p.open);
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn keystrokes_go_nowhere_when_no_pane_is_focused() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut p,
            None,
            &m,
        );
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn enter_in_a_pane_sends_a_carriage_return_not_a_newline() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Enter, KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(b) if b == b"\r"));
    }

    #[test]
    fn control_characters_reach_the_pane() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(
            matches!(action, Action::PaneInput(b) if b == vec![0x03]),
            "ctrl+c must reach the agent"
        );
    }

    // --- `ctrl+space s` toggles the split, on the same prefix as
    // the detach chord. ---

    /// Arms the `ctrl+space` prefix, then routes `code` as the completion,
    /// returning what the completion resolved to.
    fn chord(p: &mut Palette, m: &Model, code: KeyCode, mods: KeyModifiers) -> Action {
        route(key(KeyCode::Char(' '), KeyModifiers::CONTROL), p, None, m);
        assert!(p.awaiting_chord_key, "precondition: the prefix is armed");
        route(key(code, mods), p, Some("p1"), m)
    }

    #[test]
    fn ctrl_space_then_s_toggles_the_split() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = chord(&mut p, &m, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(matches!(action, Action::Command(Command::ToggleSplit)));
        assert!(
            !p.awaiting_chord_key,
            "the chord must not stay armed after firing"
        );
    }

    #[test]
    fn the_split_chord_survives_capslock_and_shift() {
        // Same reasoning as `ctrl+k`: on Windows the character is
        // case-flipped by CapsLock without `SHIFT` ever being set, and by
        // Shift with it. Both must still split.
        let m = Model::default();
        for mods in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            for c in ['s', 'S'] {
                let mut p = Palette::default();
                let action = chord(&mut p, &m, KeyCode::Char(c), mods);
                assert!(
                    matches!(action, Action::Command(Command::ToggleSplit)),
                    "{c:?} with {mods:?} must still toggle the split"
                );
            }
        }
    }

    #[test]
    fn a_ctrl_chorded_s_does_not_complete_the_split_chord() {
        // `ctrl+space` then `ctrl+s` is not the binding — it must cancel
        // and reach the pane as the 0x13 control code, not silently split.
        let m = Model::default();
        let mut p = Palette::default();
        let action = chord(&mut p, &m, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(
            matches!(action, Action::PaneInput(ref b) if b == &vec![0x13]),
            "{action:?}"
        );
        assert!(!p.awaiting_chord_key);
    }

    #[test]
    fn an_unarmed_s_is_just_pane_input() {
        // The chord's second key must mean nothing on its own, or every `s`
        // typed at an agent would resize the screen.
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('s'), KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(b) if b == b"s"));
    }

    #[test]
    fn the_chord_table_maps_only_its_own_letters() {
        assert_eq!(
            chord_completion(&key(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Command::Detach)
        );
        assert_eq!(
            chord_completion(&key(KeyCode::Char('s'), KeyModifiers::NONE)),
            Some(Command::ToggleSplit)
        );
        // `m` must be reachable from the keyboard specifically because what it
        // turns on is the mouse — a user who has just turned clicking off
        // cannot click their way back.
        assert_eq!(
            chord_completion(&key(KeyCode::Char('m'), KeyModifiers::NONE)),
            Some(Command::ToggleMouse)
        );
        assert_eq!(
            chord_completion(&key(KeyCode::Char('M'), KeyModifiers::SHIFT)),
            Some(Command::ToggleMouse),
            "CapsLock and Shift must not break the way back"
        );
        // `b` for bar: hide the sidebar, or show it again.
        for (c, mods) in [
            ('b', KeyModifiers::NONE),
            ('B', KeyModifiers::NONE),
            ('B', KeyModifiers::SHIFT),
        ] {
            assert_eq!(
                chord_completion(&key(KeyCode::Char(c), mods)),
                Some(Command::ToggleSidebar),
                "{c:?} {mods:?}"
            );
        }
        // `o` for order: the agents panel's sort, until now mouse-only.
        assert_eq!(
            chord_completion(&key(KeyCode::Char('o'), KeyModifiers::NONE)),
            Some(Command::ToggleAgentSort)
        );
        assert_eq!(
            chord_completion(&key(KeyCode::Char('O'), KeyModifiers::NONE)),
            Some(Command::ToggleAgentSort),
            "CapsLock alone flips the character without setting SHIFT"
        );
        assert_eq!(
            chord_completion(&key(KeyCode::Char('O'), KeyModifiers::SHIFT)),
            Some(Command::ToggleAgentSort)
        );
        assert_eq!(
            chord_completion(&key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            None,
            "a ctrl-chorded o is not the chord's second key"
        );
        for code in [
            KeyCode::Char('x'),
            KeyCode::Char(' '),
            KeyCode::Esc,
            KeyCode::Enter,
            KeyCode::Up,
        ] {
            assert_eq!(
                chord_completion(&key(code, KeyModifiers::NONE)),
                None,
                "{code:?} completes nothing"
            );
        }
    }

    // --- ctrl+space d detach chord ---

    #[test]
    fn ctrl_space_then_o_toggles_the_agent_sort() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        let action = route(
            key(KeyCode::Char('o'), KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(
            matches!(action, Action::Command(Command::ToggleAgentSort)),
            "{action:?}"
        );
        assert!(!p.awaiting_chord_key);
    }

    #[test]
    fn ctrl_space_then_d_detaches() {
        let m = Model::default();
        let mut p = Palette::default();
        let armed = route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(matches!(armed, Action::None));
        assert!(p.awaiting_chord_key);

        let action = route(
            key(KeyCode::Char('d'), KeyModifiers::NONE),
            &mut p,
            None,
            &m,
        );
        assert!(matches!(action, Action::Command(Command::Detach)));
        assert!(
            !p.awaiting_chord_key,
            "the chord must not stay armed after firing"
        );
    }

    #[test]
    fn ctrl_space_alone_arms_the_prefix_and_emits_nothing_yet() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::None));
        assert!(p.awaiting_chord_key);
    }

    #[test]
    fn ctrl_space_then_any_other_key_cancels_the_prefix_and_routes_it_normally() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.awaiting_chord_key);

        // Not `d` — must cancel the prefix AND still reach the pane, not be
        // swallowed.
        let action = route(
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(!p.awaiting_chord_key);
        assert!(matches!(action, Action::PaneInput(b) if b == b"x"));
    }

    #[test]
    fn ctrl_space_then_x_with_no_pane_focused_goes_nowhere_but_still_cancels() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        let action = route(
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            &mut p,
            None,
            &m,
        );
        assert!(!p.awaiting_chord_key);
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn a_second_ctrl_space_cancels_then_re_arms_the_prefix() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.awaiting_chord_key);

        // The second `ctrl+space` is "any other key" relative to the first
        // chord (cancels it), but is itself also `ctrl+space` while closed
        // (re-arms). Net effect: still armed, still `Action::None`.
        let action = route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.awaiting_chord_key);
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn esc_clears_an_armed_prefix_and_still_reaches_the_pane_as_input() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.awaiting_chord_key);

        let action = route(
            key(KeyCode::Esc, KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(!p.awaiting_chord_key, "esc must clear the armed prefix");
        assert!(matches!(action, Action::PaneInput(b) if b == vec![0x1b]));
    }

    #[test]
    fn ctrl_space_while_the_palette_is_open_does_not_arm_the_prefix() {
        let m = Model::default();
        let mut p = Palette::default();
        p.open = true;
        let action = route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(!p.awaiting_chord_key);
        assert!(p.open, "ctrl+space must not close the palette either");
        // Nothing changed (not bound to anything while open) -> None, not
        // Redraw.
        assert!(matches!(action, Action::None));
    }

    // --- CapsLock / Shift variants of ctrl+k and the chord's `d` key. ---

    #[test]
    fn ctrl_k_with_capslock_reported_as_uppercase_still_toggles_the_palette() {
        // CapsLock flips the reported char case but does not itself set
        // `KeyModifiers::SHIFT` on Windows — modifiers stay plain `CONTROL`.
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('K'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        assert!(matches!(action, Action::Redraw));
    }

    #[test]
    fn ctrl_shift_k_also_toggles_the_palette() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(
                KeyCode::Char('K'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        assert!(matches!(action, Action::Redraw));
    }

    #[test]
    fn ctrl_space_then_shift_d_detaches() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            key(KeyCode::Char(' '), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        let action = route(
            key(KeyCode::Char('D'), KeyModifiers::SHIFT),
            &mut p,
            None,
            &m,
        );
        assert!(matches!(action, Action::Command(Command::Detach)));
    }

    // --- ctrl+k must fully reset the palette on close, same as Esc/Enter. ---

    #[test]
    fn ctrl_k_closing_the_palette_clears_the_query_and_selection() {
        let m = Model::default();
        let mut p = Palette {
            open: true,
            query: "abc".into(),
            selected: 2,
            ..Default::default()
        };
        let action = route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(!p.open);
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
        assert!(matches!(action, Action::Redraw));
    }

    // --- Enter in an open palette resolves a real Command (which is why
    // route takes a &Model parameter). ---

    #[test]
    fn enter_in_the_open_palette_resolves_the_selected_command() {
        let m = one_pane_model("p1");
        // Typed, panes lead: index 0 of "focus" is the live pane's entry.
        let mut p = Palette {
            open: true,
            query: "focus".into(),
            selected: 0,
            ..Default::default()
        };
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(matches!(action, Action::Command(Command::FocusPane(id)) if id == "p1"));
        assert!(!p.open);
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn the_untyped_menu_keeps_its_commands_in_view_however_many_panes_run() {
        // The report: with panes running, "menu" no longer showed Detach —
        // a dozen pane entries had pushed it off the bottom.
        let mut m = one_pane_model("p1");
        for i in 2..30 {
            let mut extra = m.local().panes[0].clone();
            extra.id = format!("p{i}");
            m.local_mut().panes.push(extra);
        }
        let c = palette::visible_candidates(
            &m,
            &Palette {
                open: true,
                ..Default::default()
            },
        );
        let detach = c
            .iter()
            .position(|(_, cmd)| *cmd == Command::Detach)
            .unwrap();
        assert!(detach < 10, "Detach at row {detach}, past the visible rows");
        assert!(
            c.iter().take(10).any(|(_, cmd)| *cmd == Command::OpenFiles)
                || m.local_workspaces().is_empty()
        );
    }

    #[test]
    fn enter_with_an_out_of_range_selection_closes_without_a_command() {
        let m = Model::default();
        let mut p = Palette {
            open: true,
            selected: 999,
            ..Default::default()
        };
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(!p.open);
        assert!(matches!(action, Action::Redraw));
    }

    // --- the space-naming prompt (opened by the sidebar's
    // `+ new workspace` button). `Enter` must fire the thing the user
    // clicked for, which the ordinary list cannot guarantee: its
    // create-workspace entry is deliberately last and `selected` starts at
    // 0. ---

    fn naming(query: &str) -> Palette {
        Palette {
            open: true,
            naming_space: true,
            query: query.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn enter_in_the_naming_prompt_creates_the_typed_workspace() {
        // A model with panes and workspaces of its own: in the ordinary
        // list, index 0 would be a `FocusPane`, so this also pins that the
        // prompt is not just the same list with a different title.
        let mut m = one_pane_model("p1");
        m.local_mut().workspaces = vec![spaces_proto::WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec![],
        }];
        let mut p = naming("frontend");
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreateWorkspace { name })
                if name == "frontend"),
            "{action:?}"
        );
        assert!(!p.open);
        assert!(!p.naming_space, "the mode must not outlive the prompt");
    }

    #[test]
    fn typing_in_the_naming_prompt_builds_the_name() {
        let m = Model::default();
        let mut p = naming("");
        for c in ['a', 'p', 'i'] {
            route(key(KeyCode::Char(c), KeyModifiers::NONE), &mut p, None, &m);
        }
        assert_eq!(p.query, "api");
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreateWorkspace { name }) if name == "api"),
            "{action:?}"
        );
    }

    #[test]
    fn enter_with_nothing_typed_just_closes_the_naming_prompt() {
        // An unnamed workspace is not a request worth sending — and the
        // empty candidate list is what makes `enter` a no-op here rather
        // than firing whatever the ordinary list's first entry would be.
        let m = one_pane_model("p1");
        for query in ["", "   "] {
            let mut p = naming(query);
            let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
            assert!(matches!(action, Action::Redraw), "{query:?}: {action:?}");
            assert!(!p.open);
            assert!(!p.naming_space);
        }
    }

    #[test]
    fn escape_leaves_the_naming_prompt_and_the_next_palette_is_ordinary() {
        // The regression this guards: a mode that outlives its own prompt
        // turns the next plain `ctrl+k` into a naming box with no
        // explanation.
        let m = Model::default();
        let mut p = naming("half-typed");
        route(key(KeyCode::Esc, KeyModifiers::NONE), &mut p, None, &m);
        assert!(!p.naming_space);
        assert_eq!(p.query, "");

        route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        assert!(!p.naming_space);
    }

    #[test]
    fn leaving_the_session_picker_any_way_brings_back_the_ordinary_menu() {
        // The regression this guards: the resume-or-new picker outlived
        // itself, so every later `ctrl+k` opened it again instead of the menu.
        let m = one_pane_model("p1");
        for leave in [
            key(KeyCode::Esc, KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
        ] {
            let mut p = Palette {
                open: true,
                picking_session: Some(crate::ui::palette::PendingPane {
                    workspace_id: "w1".into(),
                    agent_type: "claude".into(),
                    cwd: Some("/work/api".into()),
                    sessions: vec![],
                }),
                ..Palette::default()
            };
            route(leave, &mut p, Some("p1"), &m);
            assert!(!p.open, "{leave:?}");
            assert!(
                p.picking_session.is_none(),
                "{leave:?}: the picker outlived itself"
            );

            route(
                key(KeyCode::Char('k'), KeyModifiers::CONTROL),
                &mut p,
                Some("p1"),
                &m,
            );
            assert!(p.open);
            assert!(
                p.picking_session.is_none(),
                "{leave:?}: ctrl+k reopened the picker"
            );
        }
    }

    #[test]
    fn ctrl_k_also_leaves_the_naming_prompt() {
        let m = Model::default();
        let mut p = naming("half-typed");
        route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(!p.open);
        assert!(!p.naming_space);
    }

    #[test]
    fn arrow_keys_in_the_naming_prompt_cannot_select_past_its_one_entry() {
        // `Down` clamps against the *visible* list, which here is one entry
        // long however many commands the ordinary palette offers.
        let m = one_pane_model("p1");
        let mut p = naming("api");
        for _ in 0..5 {
            route(key(KeyCode::Down, KeyModifiers::NONE), &mut p, None, &m);
        }
        assert_eq!(p.selected, 0);
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreateWorkspace { name }) if name == "api"),
            "{action:?}"
        );
    }

    // --- Palette navigation must never panic, even at usize's edges, and
    // Down must clamp to the live candidate list. ---

    #[test]
    fn selected_does_not_underflow_past_zero() {
        let m = Model::default();
        let mut p = Palette::default();
        p.open = true;
        route(key(KeyCode::Up, KeyModifiers::NONE), &mut p, None, &m);
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn selected_clamps_to_the_last_candidate_instead_of_overflowing() {
        // Derived from `candidates`, not a literal: a hardcoded index here
        // is what made adding a palette entry look expensive enough to
        // leave `ToggleSplit` undiscoverable. What this test is about is
        // the *clamp*, not how many commands the palette happens to offer.
        let m = Model::default();
        let last = crate::ui::palette::candidates(&m, "").len() - 1;
        let mut p = Palette {
            open: true,
            selected: usize::MAX,
            ..Default::default()
        };
        route(key(KeyCode::Down, KeyModifiers::NONE), &mut p, None, &m);
        assert_eq!(
            p.selected, last,
            "must clamp to the last live candidate index, not overflow"
        );
    }

    // --- No keystroke becomes pane input while the palette is open,
    // generalized beyond printable keys. ---

    #[test]
    fn control_characters_do_not_reach_the_pane_while_the_palette_is_open() {
        let m = Model::default();
        let mut p = Palette::default();
        p.open = true;
        let action = route(
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(!matches!(action, Action::PaneInput(_)));
        assert_eq!(
            p.query, "",
            "ctrl+c must not be typed into the query either"
        );
    }

    // --- The bounded pane-input encoding table. ---

    #[test]
    fn named_keys_encode_their_usual_escape_sequences() {
        let m = Model::default();
        let cases: &[(KeyCode, &[u8])] = &[
            (KeyCode::Tab, b"\t"),
            (KeyCode::BackTab, b"\x1b[Z"),
            (KeyCode::Backspace, &[0x7f]),
            (KeyCode::Esc, &[0x1b]),
            (KeyCode::Up, b"\x1b[A"),
            (KeyCode::Down, b"\x1b[B"),
            (KeyCode::Right, b"\x1b[C"),
            (KeyCode::Left, b"\x1b[D"),
            (KeyCode::Home, b"\x1b[H"),
            (KeyCode::End, b"\x1b[F"),
            (KeyCode::PageUp, b"\x1b[5~"),
            (KeyCode::PageDown, b"\x1b[6~"),
            (KeyCode::Delete, b"\x1b[3~"),
            (KeyCode::Insert, b"\x1b[2~"),
        ];
        for (code, expected) in cases {
            let mut p = Palette::default();
            let action = route(key(*code, KeyModifiers::NONE), &mut p, Some("p1"), &m);
            assert!(
                matches!(&action, Action::PaneInput(b) if b == expected),
                "{code:?} -> {action:?}, expected {expected:?}"
            );
        }
    }

    #[test]
    fn ctrl_plus_letter_maps_to_the_matching_control_code() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('a'), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(b) if b == vec![0x01]));

        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('z'), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(b) if b == vec![0x1a]));
    }

    #[test]
    fn ctrl_plus_a_non_letter_produces_no_pane_input() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('2'), KeyModifiers::CONTROL),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn alt_plus_printable_produces_no_pane_input() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('b'), KeyModifiers::ALT),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn a_multibyte_printable_character_is_utf8_encoded() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::Char('\u{00e9}'), KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(b) if b == "\u{00e9}".as_bytes()));
    }

    #[test]
    fn keys_outside_the_bounded_set_produce_no_pane_input() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            key(KeyCode::F(1), KeyModifiers::NONE),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::None));
    }

    // --- KeyEventKind::Release/Repeat. ---

    #[test]
    fn a_release_event_is_ignored_even_with_a_focused_pane() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            ),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::None));
    }

    #[test]
    fn a_release_event_does_not_toggle_the_palette() {
        let m = Model::default();
        let mut p = Palette::default();
        route(
            KeyEvent::new_with_kind(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press,
            ),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        route(
            KeyEvent::new_with_kind(
                KeyCode::Char('k'),
                KeyModifiers::CONTROL,
                KeyEventKind::Release,
            ),
            &mut p,
            None,
            &m,
        );
        assert!(p.open, "a release event must not re-toggle the palette");
    }

    #[test]
    fn a_repeat_event_still_reaches_the_pane() {
        let m = Model::default();
        let mut p = Palette::default();
        let action = route(
            KeyEvent::new_with_kind(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(b) if b == b"a"));
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod mouse_tests {
    use super::*;
    use crate::model::{AgentSort, DisplayState, SidebarDrag};
    use crate::ui::sidebar::{self, SidebarLayout};
    use crate::ui::tabs;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;
    use spaces_proto::{AgentState, PaneInfo, WorkspaceInfo};

    /// The keyboard half of these cases: the agent picker is opened by the
    /// mouse and confirmed by `enter`, so both routers are exercised here.
    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// The sidebar rect every case below is written against — an 80x24
    /// terminal's left column, tall enough for both sidebar sections, less
    /// the one-cell draggable edge on its right (see [`sidebar::edge`]).
    const SIDEBAR: Rect = Rect {
        x: 0,
        y: 0,
        width: sidebar::WIDTH - 1,
        height: 23,
    };

    /// The sidebar's draggable right edge on that same terminal.
    const EDGE: u16 = sidebar::WIDTH - 1;

    /// The tab bar's own row, beside it.
    const TABS: Rect = Rect {
        x: sidebar::WIDTH,
        y: 0,
        width: 80 - sidebar::WIDTH,
        height: 1,
    };

    /// The whole frame, as `main.rs` passes it. The router derives the
    /// palette''s overlay rect from this and hit-tests the context menu
    /// against it directly.
    const OVERLAY: Rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn ws(id: &str, name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            git: None,
            id: id.into(),
            name: name.into(),
            color: None,
            pane_ids: vec![],
        }
    }

    fn pane(id: &str, space: &str, agent: &str, state: AgentState) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(space.into()),
            agent_type: agent.into(),
            agent_state: state,
            seen: true,
            title: String::new(),
            // A real pane has a directory, and the ordinary create path
            // inherits it — a blank one would put every case below on the
            // first-pane prompt branch instead.
            cwd: "/work".into(),
            session_ref: None,
            live: true,
            color: None,
        }
    }

    /// One space, two panes in it, the first focused.
    fn populated() -> Model {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "api")];
        m.local_mut().panes = vec![
            pane("p1", "w1", "claude", AgentState::Working),
            pane("p2", "w1", "codex", AgentState::Idle),
        ];
        m.focused = Some("p1".into());
        m
    }

    fn layout(model: &Model) -> SidebarLayout {
        sidebar::sidebar_layout(SIDEBAR, model)
    }

    /// Routes `event` against `model` with an empty pane-target list and a
    /// freshly-derived tab bar — the same two inputs `main.rs` builds.
    fn route_at(model: &mut Model, palette: &mut Palette, event: MouseEvent) -> Action {
        let tabs = tabs::tab_bar(TABS, model);
        route_mouse(event, OVERLAY, &tabs, &[], palette, model)
    }

    // --- Kinds that must do nothing at all. ---

    #[test]
    fn a_right_click_and_mouse_movement_are_both_ignored() {
        let mut m = populated();
        let mut p = Palette::default();
        for kind in [
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Moved,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let action = route_at(&mut m, &mut p, wheel(kind, 3, 2));
            assert!(matches!(action, Action::None), "{kind:?} -> {action:?}");
        }
    }

    // --- The palette covers everything. ---

    #[test]
    fn a_click_dismisses_an_open_palette_wherever_it_lands() {
        let mut m = populated();
        let mut p = Palette {
            open: true,
            query: "abc".into(),
            selected: 2,
            naming_space: true,
            ..Default::default()
        };
        let action = route_at(&mut m, &mut p, click(3, 2));
        assert!(matches!(action, Action::Redraw));
        assert!(!p.open);
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
        assert!(!p.naming_space, "the prompt's mode dies with the prompt");
    }

    // --- Drag-to-select in a pane (2026-08-12: "I still can't highlight text
    // to copy it in the panes"). A terminal cannot both report mouse events to
    // us and use them for its own selection, so the client does the selecting.
    // ---

    fn drag(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn release(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// The focused pane's own region, as `main.rs` supplies it.
    fn pane_targets() -> Vec<(Rect, String)> {
        vec![(Rect::new(28, 1, 52, 22), "p1".to_string())]
    }

    fn route_pane(m: &mut Model, p: &mut Palette, event: MouseEvent) -> Action {
        let bar = tabs::tab_bar(TABS, m);
        route_mouse(event, OVERLAY, &bar, &pane_targets(), p, m)
    }

    #[test]
    fn pressing_in_a_pane_arms_a_selection_at_that_cell() {
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        let sel = p.selection.as_ref().expect("armed");
        assert_eq!(sel.pane_id, "p1");
        assert_eq!(sel.anchor, (40, 5));
        assert_eq!(sel.head, (40, 5), "and is empty until the drag moves");
        assert!(sel.is_empty());
    }

    #[test]
    fn dragging_extends_the_selection_and_releasing_copies_it() {
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        let action = route_pane(&mut m, &mut p, drag(46, 5));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(p.selection.as_ref().unwrap().head, (46, 5));

        let action = route_pane(&mut m, &mut p, release(46, 5));
        assert!(matches!(action, Action::CopySelection), "{action:?}");
        assert!(
            p.selection.is_some(),
            "the highlight stays until the next click — it is the only durable \
             sign the copy happened"
        );
    }

    #[test]
    fn a_click_that_never_moved_copies_nothing_and_leaves_no_highlight() {
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        let action = route_pane(&mut m, &mut p, release(40, 5));
        assert!(matches!(action, Action::None), "{action:?}");
        assert!(p.selection.is_none());
    }

    #[test]
    fn clicking_the_pane_you_are_typing_in_selects_without_refocusing_it() {
        // Re-running the focus path re-attaches, resizes, and clears the
        // status-bar notice — including the line saying what was just copied.
        let mut m = populated();
        assert_eq!(m.focused.as_deref(), Some("p1"), "precondition");
        let mut p = Palette::default();
        let action = route_pane(&mut m, &mut p, click(40, 5));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.selection.is_some(), "but it still arms a selection");
    }

    #[test]
    fn clicking_the_other_pane_both_focuses_it_and_arms_a_selection() {
        let mut m = populated();
        m.focused = Some("p2".into());
        let mut p = Palette::default();
        let action = route_pane(&mut m, &mut p, click(40, 5));
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p1"),
            "{action:?}"
        );
        assert_eq!(p.selection.as_ref().unwrap().pane_id, "p1");
    }

    #[test]
    fn a_drag_that_leaves_the_pane_selects_to_its_edge_not_into_the_sidebar() {
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        route_pane(&mut m, &mut p, drag(0, 0));
        let sel = p.selection.as_ref().expect("still selecting");
        let region = sel.region;
        assert_eq!(sel.head, (region.x, region.y), "clamped to the near edge");
        route_pane(&mut m, &mut p, drag(500, 500));
        let sel = p.selection.as_ref().unwrap();
        assert_eq!(
            sel.head,
            (region.x + region.width - 1, region.y + region.height - 1),
            "and to the far one"
        );
    }

    #[test]
    fn a_drag_keeps_going_when_the_pointer_crosses_the_sidebar() {
        // Resolved before every overlay, so a drag that began in a pane is not
        // mistaken for a click on whatever the pointer is over by now.
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        let action = route_pane(&mut m, &mut p, drag(3, 5));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.selection.is_some());
        assert_eq!(m.selected_space, None, "no space got selected by the drag");
    }

    #[test]
    fn a_drag_with_nothing_armed_does_nothing_at_all() {
        // The high-frequency case: the pointer is being dragged somewhere that
        // never started a selection.
        let mut m = populated();
        let mut p = Palette::default();
        for event in [drag(40, 5), release(40, 5), drag(3, 3)] {
            let action = route_pane(&mut m, &mut p, event);
            assert!(matches!(action, Action::None), "{event:?} -> {action:?}");
            assert!(p.selection.is_none());
        }
    }

    #[test]
    fn a_press_anywhere_but_a_pane_drops_a_previous_selection() {
        // Leaving the last highlight on screen after the user has moved on
        // reads as a live one.
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        route_pane(&mut m, &mut p, drag(46, 5));
        assert!(p.selection.is_some());
        let l = layout(&m);
        route_pane(&mut m, &mut p, click(3, l.spaces_body.y));
        assert!(p.selection.is_none());
    }

    // --- The tab context menu. Right-clicks were discarded outright before
    // this; a right-click anywhere but a tab still is. ---

    fn right_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Right-clicks the tab at `index` and returns the action.
    fn right_click_tab(m: &mut Model, p: &mut Palette, index: usize) -> Action {
        let bar = tabs::tab_bar(TABS, m);
        let (rect, _) = bar.tabs[index];
        route_mouse(right_click(rect.x + 1, rect.y), OVERLAY, &bar, &[], p, m)
    }

    /// The screen the menu is laid out against — `ui::render` passes the whole
    /// frame, so the box can always be pushed somewhere visible.
    const SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };

    #[test]
    fn right_clicking_a_tab_opens_its_menu() {
        let mut m = populated();
        let mut p = Palette::default();
        let action = right_click_tab(&mut m, &mut p, 1);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        let menu = p.tab_menu.as_ref().expect("menu opened");
        assert_eq!(
            menu.target,
            menu::MenuTarget::Pane("p2".into()),
            "the tab that was clicked"
        );
    }

    #[test]
    fn a_right_click_where_there_is_no_menu_does_nothing() {
        // Never "the same as a left click": a stray right-click must not focus
        // a pane, create anything, or open a prompt. Space rows and agent rows
        // *do* open menus now (see the space-menu cases below); these are the
        // places that still have none.
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let l = layout(&m);
        for (column, row) in [
            (3, l.spaces_header.y),
            (3, l.divider.y),
            (l.new_button.x, l.new_button.y),
            (bar.new_tab.x, bar.new_tab.y),
            (60, 10),
        ] {
            let action = route_mouse(right_click(column, row), OVERLAY, &bar, &[], &mut p, &mut m);
            assert!(
                matches!(action, Action::None),
                "({column},{row}) -> {action:?}"
            );
            assert!(p.tab_menu.is_none(), "({column},{row})");
            assert!(!p.open, "({column},{row})");
        }
    }

    #[test]
    fn clicking_close_in_the_menu_closes_that_pane() {
        let mut m = populated();
        let mut p = Palette::default();
        right_click_tab(&mut m, &mut p, 1);
        let menu = p.tab_menu.clone().expect("open");
        let l = menu::menu_layout(&m, &menu, SCREEN).expect("drawn");
        // By command, not by row index: "close" is no longer first now that
        // the menu leads with "new pane".
        let index = menu::items(&m, &menu)
            .iter()
            .position(|(_, cmd)| matches!(cmd, Command::ClosePane(_)))
            .expect("offered");
        let (rect, _) = l.rows[index];
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(click(rect.x + 1, rect.y), SCREEN, &bar, &[], &mut p, &mut m);
        assert!(
            matches!(&action, Action::Command(Command::ClosePane(id)) if id == "p2"),
            "{action:?}"
        );
        assert!(p.tab_menu.is_none(), "and the menu closes behind it");
    }

    /// Right-clicks the space row at `index` and returns the action.
    fn right_click_space(m: &mut Model, p: &mut Palette, index: u16) -> Action {
        let bar = tabs::tab_bar(TABS, m);
        let row = layout(m).spaces_body.y + index;
        route_mouse(right_click(3, row), OVERLAY, &bar, &[], p, m)
    }

    /// Clicks the menu entry whose command matches `want`.
    fn click_menu_entry(m: &mut Model, p: &mut Palette, want: impl Fn(&Command) -> bool) -> Action {
        let menu = p.tab_menu.clone().expect("menu open");
        let entries = menu::items(m, &menu);
        let index = entries
            .iter()
            .position(|(_, cmd)| want(cmd))
            .expect("entry offered");
        let l = menu::menu_layout(m, &menu, SCREEN).expect("drawn");
        let (rect, _) = l.rows[index];
        let bar = tabs::tab_bar(TABS, m);
        route_mouse(click(rect.x + 1, rect.y), SCREEN, &bar, &[], p, m)
    }

    #[test]
    fn right_clicking_a_space_row_opens_its_menu() {
        let mut m = populated();
        let mut p = Palette::default();
        let action = right_click_space(&mut m, &mut p, 0);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(
            p.tab_menu.as_ref().map(|menu| menu.target.clone()),
            Some(menu::MenuTarget::Space("w1".into()))
        );
        assert_eq!(
            m.selected_space, None,
            "and the space underneath is not also selected"
        );
    }

    #[test]
    fn right_clicking_an_agent_row_opens_that_panes_menu() {
        // The sidebar is where you reach a pane whose tab has scrolled out of
        // the strip, so it gets the same menu the tab would.
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let row = layout(&m).agents_body.y;
        route_mouse(right_click(3, row), OVERLAY, &bar, &[], &mut p, &mut m);
        assert_eq!(
            p.tab_menu.as_ref().map(|menu| menu.target.clone()),
            Some(menu::MenuTarget::Pane("p1".into()))
        );
    }

    #[test]
    fn renaming_a_space_opens_a_prompt_pre_filled_with_its_current_name() {
        // Renaming is usually editing, so `enter` on an unedited prompt is a
        // harmless no-op rather than a surprise.
        let mut m = populated();
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 0);
        let action = click_menu_entry(&mut m, &mut p, |c| matches!(c, Command::RenameSpace { .. }));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(p.renaming_space.as_deref(), Some("w1"));
        assert_eq!(p.query, "api", "pre-filled");
        assert!(p.tab_menu.is_none(), "the menu closed behind it");

        // Editing and confirming sends just the name — never a null color,
        // which `workspace.update` would take as "clear it".
        p.query = "backend".into();
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::UpdateSpace { workspace_id, name, color })
                if workspace_id == "w1" && name.as_deref() == Some("backend") && color.is_none()),
            "{action:?}"
        );
        assert!(p.renaming_space.is_none());
    }

    #[test]
    fn a_blank_rename_is_offered_as_nothing_rather_than_a_rejected_request() {
        // `workspace.update` refuses an empty name outright.
        let mut m = populated();
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 0);
        click_menu_entry(&mut m, &mut p, |c| matches!(c, Command::RenameSpace { .. }));
        p.query = "   ".into();
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(!p.open);
    }

    #[test]
    fn recoloring_a_space_offers_the_web_uis_own_palette() {
        let mut m = populated();
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 0);
        click_menu_entry(&mut m, &mut p, |c| {
            matches!(c, Command::PickSpaceColor { .. })
        });
        assert_eq!(p.picking_color.as_deref(), Some("w1"));
        let offered: Vec<String> = palette::visible_candidates(&m, &p)
            .into_iter()
            .map(|(label, _)| label)
            .collect();
        assert_eq!(offered.len(), sidebar::COLORS.len());
        assert!(offered[0].starts_with(sidebar::COLORS[0]), "{offered:?}");

        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::UpdateSpace { workspace_id, name, color })
                if workspace_id == "w1" && name.is_none()
                    && color.as_deref() == Some(sidebar::COLORS[0])),
            "color only, never a null name: {action:?}"
        );
    }

    #[test]
    fn the_colour_picker_marks_the_one_the_space_already_has() {
        let mut m = populated();
        m.local_mut().workspaces[0].color = Some(sidebar::COLORS[3].to_string());
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 0);
        click_menu_entry(&mut m, &mut p, |c| {
            matches!(c, Command::PickSpaceColor { .. })
        });
        let offered = palette::visible_candidates(&m, &p);
        assert!(offered[3].0.contains("current"), "{:?}", offered[3].0);
        assert_eq!(
            offered
                .iter()
                .filter(|(l, _)| l.contains("current"))
                .count(),
            1
        );
    }

    #[test]
    fn closing_a_space_is_dispatched_not_prompted() {
        // The one destructive entry, and the only space action that does not
        // ask for anything further.
        let mut m = populated();
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 0);
        let action = click_menu_entry(&mut m, &mut p, |c| matches!(c, Command::CloseSpace(_)));
        assert!(
            matches!(&action, Action::Command(Command::CloseSpace(id)) if id == "w1"),
            "{action:?}"
        );
        assert!(p.tab_menu.is_none());
        assert!(!p.open, "no prompt follows it");
    }

    #[test]
    fn a_space_menus_new_pane_opens_the_agent_picker_for_that_space() {
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "web"));
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 1);
        let action = click_menu_entry(&mut m, &mut p, |c| matches!(c, Command::PickAgent { .. }));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(p.picking_agent.as_deref(), Some("w2"));
    }

    #[test]
    fn opening_one_prompt_leaves_no_other_mode_behind() {
        // Five openers and five modes: the version where each listed the
        // others by hand needed all twenty-five assignments to stay right.
        let mut m = populated();
        let mut p = Palette::default();
        right_click_space(&mut m, &mut p, 0);
        click_menu_entry(&mut m, &mut p, |c| matches!(c, Command::RenameSpace { .. }));
        assert!(p.renaming_space.is_some());

        // Straight from the rename prompt into the color picker.
        right_click_space(&mut m, &mut p, 0);
        click_menu_entry(&mut m, &mut p, |c| {
            matches!(c, Command::PickSpaceColor { .. })
        });
        assert!(p.picking_color.is_some());
        assert!(p.renaming_space.is_none(), "the previous mode is gone");
        assert!(!p.naming_space);
        assert!(p.picking_agent.is_none());
        assert!(p.asking_cwd.is_none());
        assert!(p.tab_menu.is_none());
        assert!(p.selection.is_none());
    }

    #[test]
    fn clicking_move_to_a_space_moves_that_pane() {
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "web"));
        let mut p = Palette::default();
        right_click_tab(&mut m, &mut p, 0);
        let menu = p.tab_menu.clone().expect("open");
        let entries = menu::items(&m, &menu);
        let index = entries
            .iter()
            .position(|(label, _)| label == "move to web")
            .expect("offered");
        let l = menu::menu_layout(&m, &menu, SCREEN).expect("drawn");
        let (rect, _) = l.rows[index];
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(click(rect.x + 1, rect.y), SCREEN, &bar, &[], &mut p, &mut m);
        assert!(
            matches!(&action, Action::Command(Command::MovePane { pane_id, workspace_id })
                if pane_id == "p1" && workspace_id == "w2"),
            "{action:?}"
        );
    }

    #[test]
    fn a_click_that_misses_the_menu_only_closes_it() {
        // The one thing a context menu must never do is stay open while the
        // click that missed it also does something underneath.
        let mut m = populated();
        let mut p = Palette::default();
        right_click_tab(&mut m, &mut p, 1);
        let bar = tabs::tab_bar(TABS, &m);
        let l = layout(&m);
        let action = route_mouse(click(3, l.spaces_body.y), SCREEN, &bar, &[], &mut p, &mut m);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.tab_menu.is_none());
        assert_eq!(
            m.selected_space, None,
            "the space underneath must not have been selected too"
        );
    }

    #[test]
    fn a_wheel_notch_or_a_second_right_click_just_closes_the_menu() {
        let mut m = populated();
        for event in [wheel(MouseEventKind::ScrollDown, 30, 3), right_click(30, 3)] {
            let mut p = Palette::default();
            right_click_tab(&mut m, &mut p, 1);
            let bar = tabs::tab_bar(TABS, &m);
            let action = route_mouse(event, SCREEN, &bar, &[], &mut p, &mut m);
            assert!(matches!(action, Action::Redraw), "{event:?} -> {action:?}");
            assert!(p.tab_menu.is_none(), "{event:?}");
        }
    }

    #[test]
    fn the_menu_and_the_palette_are_never_both_open() {
        let mut m = populated();
        let mut p = Palette::default();
        right_click_tab(&mut m, &mut p, 0);
        assert!(p.tab_menu.is_some());

        // ctrl+k while the menu is open takes the menu with it.
        route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        assert!(p.tab_menu.is_none());

        // Via the mouse the exclusivity is structural rather than explicit:
        // the menu is modal, so the click that lands on `menu` is consumed
        // dismissing it, and only the *next* one opens the palette. Both
        // halves matter — the first must not open the palette *and* leave the
        // menu up, and the second must still work.
        p = Palette::default();
        right_click_tab(&mut m, &mut p, 0);
        let l = layout(&m);
        route_at(&mut m, &mut p, click(l.menu_button.x, l.menu_button.y));
        assert!(p.tab_menu.is_none(), "the menu is dismissed");
        assert!(!p.open, "and the click underneath it did not also fire");

        route_at(&mut m, &mut p, click(l.menu_button.x, l.menu_button.y));
        assert!(p.open, "the next click reaches the button");
        assert!(p.tab_menu.is_none());
    }

    #[test]
    fn a_menu_for_a_pane_that_has_since_vanished_closes_without_a_command() {
        // The pane exited between the right-click and the click on `close`.
        let mut m = populated();
        let mut p = Palette::default();
        right_click_tab(&mut m, &mut p, 1);
        let menu = p.tab_menu.clone().expect("open");
        let l = menu::menu_layout(&m, &menu, SCREEN).expect("drawn");
        let (rect, _) = l.rows[0];
        m.local_mut().panes.retain(|pane| pane.id != "p2");
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(click(rect.x + 1, rect.y), SCREEN, &bar, &[], &mut p, &mut m);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.tab_menu.is_none());
    }

    // --- Clicking the palette's own entries (2026-08-12: "when I click menu
    // and click 'New Pane in Default' it does nothing at all"). Every click
    // while the overlay was open simply dismissed it, so the whole candidate
    // list was keyboard-only in a client whose premise is being
    // mouse-native. ---

    /// The rect of candidate `index` in the overlay `model`/`palette` would
    /// draw right now, or `None` if it isn't on screen.
    fn candidate_rect(model: &Model, p: &Palette, index: usize) -> Option<Rect> {
        let layout =
            palette::palette_layout(model, p, crate::ui::palette_area(OVERLAY, sidebar::WIDTH))?;
        layout
            .rows
            .iter()
            .find(|(_, i)| *i == index)
            .map(|(rect, _)| *rect)
    }

    /// Clicks the row of candidate `index`.
    fn click_candidate(m: &mut Model, p: &mut Palette, index: usize) -> Action {
        let rect = candidate_rect(m, p, index).expect("candidate is on screen");
        let bar = tabs::tab_bar(TABS, m);
        route_mouse(click(rect.x + 2, rect.y), OVERLAY, &bar, &[], p, m)
    }

    #[test]
    fn clicking_a_candidate_fires_it() {
        let mut m = populated();
        let mut p = Palette {
            open: true,
            ..Default::default()
        };
        // The harmless "Split view" row.
        let index = palette::visible_candidates(&m, &p)
            .iter()
            .position(|(_, c)| matches!(c, Command::ToggleSplit))
            .expect("offered");
        let expected = palette::visible_candidates(&m, &p)[index].1.clone();
        let action = click_candidate(&mut m, &mut p, index);
        assert!(
            matches!(&action, Action::Command(cmd) if *cmd == expected),
            "{action:?}"
        );
        assert!(!p.open, "and the palette closes, as enter would");
    }

    #[test]
    fn clicking_new_pane_in_a_space_opens_the_agent_picker() {
        // The reported case, end to end: `menu` opens the list, and clicking
        // its "New pane in Default…" row must advance to the agent picker
        // rather than silently dismissing everything.
        let mut m = populated();
        let mut p = Palette::default();
        let l = layout(&m);
        route_at(&mut m, &mut p, click(l.menu_button.x, l.menu_button.y));
        assert!(p.open, "precondition: menu opened the command list");

        let index = palette::visible_candidates(&m, &p)
            .iter()
            .position(|(_, cmd)| matches!(cmd, Command::PickAgent { .. }))
            .expect("the list offers a new pane");
        let action = click_candidate(&mut m, &mut p, index);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.open, "it advances to the picker rather than closing");
        assert_eq!(p.picking_agent.as_deref(), Some("w1"));
    }

    #[test]
    fn a_spaces_first_pane_asks_where_to_start_from_a_real_directory() {
        // 2026-08-12: an empty box invited typing `C:`, which on Windows means
        // "the server's own directory on drive C" rather than `C:\`. Starting
        // from a path that definitely exists is what stops that.
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "fresh"));
        m.selected_space = Some("w2".into());
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        let index = palette::visible_candidates(&m, &p)
            .iter()
            .position(|(label, _)| label.starts_with("claude"))
            .expect("offered");
        let action = click_candidate(&mut m, &mut p, index);

        // Asks the server for a folder listing starting from the suggestion;
        // the typed prompt below is what stays when it cannot answer.
        assert!(
            matches!(&action, Action::Command(Command::BrowseFolder { workspace_id, agent_type, path })
                if workspace_id == "w2" && agent_type == "claude" && path.as_deref() == Some("/work")),
            "{action:?}"
        );
        assert_eq!(p.asking_cwd, Some(("w2".into(), "claude".into())));
        assert_eq!(
            p.query, "/work",
            "pre-filled with the most recent live pane's directory"
        );

        // And enter on the unedited seed creates the pane there.
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreatePane { workspace_id, agent_type, cwd })
                if workspace_id == "w2" && agent_type == "claude"
                    && cwd.as_deref() == Some("/work")),
            "{action:?}"
        );
    }

    fn browsing(listing: palette::FolderListing) -> Palette {
        Palette {
            open: true,
            asking_cwd: Some(("w1".into(), "claude".into())),
            browse: Some(listing),
            ..Default::default()
        }
    }

    fn listing(current: &str, parent: Option<&str>, folders: &[&str]) -> palette::FolderListing {
        palette::FolderListing {
            current: current.into(),
            parent: parent.map(str::to_string),
            folders: folders
                .iter()
                .map(|f| (f.to_string(), format!("{current}/{f}")))
                .collect(),
            roots: vec!["/home/me".into()],
        }
    }

    #[test]
    fn backspace_on_an_empty_query_goes_up_a_folder() {
        let m = populated();
        let mut p = browsing(listing("/home/me/dev", Some("/home/me"), &["a"]));
        let action = route(
            key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut p,
            None,
            &m,
        );
        assert!(
            matches!(&action, Action::Command(Command::BrowseFolder { path, .. })
                if path.as_deref() == Some("/home/me")),
            "{action:?}"
        );

        // With text typed it edits the text, as everywhere else.
        p.query = "ab".into();
        route(
            key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut p,
            None,
            &m,
        );
        assert_eq!(p.query, "a");

        // And at the top of a dev directory there is nowhere to go.
        let mut top = browsing(listing("/home/me", None, &["a"]));
        let action = route(
            key(KeyCode::Backspace, KeyModifiers::NONE),
            &mut top,
            None,
            &m,
        );
        assert!(matches!(action, Action::None), "{action:?}");
    }

    #[test]
    fn enter_in_the_browser_starts_here_or_opens_the_typed_match() {
        let m = populated();
        let mut p = browsing(listing(
            "/home/me/dev",
            Some("/home/me"),
            &["Webshop", "spaces"],
        ));
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreatePane { cwd, .. })
                if cwd.as_deref() == Some("/home/me/dev")),
            "an untouched enter starts the pane in the folder on screen: {action:?}"
        );

        let mut p = browsing(listing(
            "/home/me/dev",
            Some("/home/me"),
            &["Webshop", "spaces"],
        ));
        p.query = "web".into();
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::BrowseFolder { path, .. })
                if path.as_deref() == Some("/home/me/dev/Webshop")),
            "{action:?}"
        );
    }

    #[test]
    fn with_no_panes_anywhere_the_directory_prompt_starts_empty() {
        // Nothing to suggest, so nothing is suggested — rather than a
        // fabricated path that may not exist on the server.
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "first")];
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        let index = palette::visible_candidates(&m, &p)
            .iter()
            .position(|(label, _)| label.starts_with("claude"))
            .expect("offered");
        click_candidate(&mut m, &mut p, index);
        assert!(p.asking_cwd.is_some());
        assert_eq!(p.query, "");
    }

    #[test]
    fn clicking_an_agent_in_the_picker_creates_the_pane() {
        // And the second click of that two-click journey.
        let mut m = populated();
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        let index = palette::visible_candidates(&m, &p)
            .iter()
            .position(|(label, _)| label == "codex")
            .expect("codex is offered");
        let action = click_candidate(&mut m, &mut p, index);
        assert!(
            matches!(&action, Action::Command(Command::CreatePane { workspace_id, agent_type, .. })
                if workspace_id == "w1" && agent_type == "codex"),
            "{action:?}"
        );
        assert!(!p.open);
    }

    #[test]
    fn a_click_and_enter_resolve_the_same_candidate_to_the_same_command() {
        // One `confirm_candidate` for both, so the two paths cannot drift.
        let mut m = populated();
        // A command that closes the palette, so both paths return it.
        let index = palette::visible_candidates(
            &m,
            &Palette {
                open: true,
                ..Default::default()
            },
        )
        .iter()
        .position(|(_, cmd)| matches!(cmd, Command::ToggleSplit))
        .expect("offered");

        let mut clicked = Palette {
            open: true,
            selected: index,
            ..Default::default()
        };
        let by_click = click_candidate(&mut m, &mut clicked, index);

        let mut typed = Palette {
            open: true,
            selected: index,
            ..Default::default()
        };
        let by_enter = route(
            key(KeyCode::Enter, KeyModifiers::NONE),
            &mut typed,
            None,
            &m,
        );

        match (by_click, by_enter) {
            (Action::Command(a), Action::Command(b)) => assert_eq!(a, b),
            (a, b) => panic!("{a:?} vs {b:?}"),
        }
    }

    #[test]
    fn clicking_the_query_line_or_a_border_neither_fires_nor_dismisses() {
        // The user is aiming at the modal; closing it out from under a
        // near-miss is worse than ignoring the click.
        let mut m = populated();
        let mut p = Palette {
            open: true,
            query: "cla".into(),
            ..Default::default()
        };
        let layout =
            palette::palette_layout(&m, &p, crate::ui::palette_area(OVERLAY, sidebar::WIDTH))
                .expect("drawn");
        let bar = tabs::tab_bar(TABS, &m);
        for (column, row) in [
            // The query line: one row inside the top border.
            (layout.inner.x + 1, layout.inner.y),
            // The top-left border corner.
            (layout.outer.x, layout.outer.y),
        ] {
            let action = route_mouse(click(column, row), OVERLAY, &bar, &[], &mut p, &mut m);
            assert!(
                matches!(action, Action::None),
                "({column},{row}): {action:?}"
            );
            assert!(p.open, "({column},{row}) must not dismiss");
            assert_eq!(p.query, "cla", "and must not clear the query");
        }
    }

    #[test]
    fn a_click_outside_the_box_still_dismisses() {
        let mut m = populated();
        let mut p = Palette {
            open: true,
            query: "abc".into(),
            selected: 2,
            naming_space: true,
            ..Default::default()
        };
        let layout =
            palette::palette_layout(&m, &p, crate::ui::palette_area(OVERLAY, sidebar::WIDTH))
                .expect("drawn");
        let bar = tabs::tab_bar(TABS, &m);
        // One row above the box, inside the overlay area.
        let row = layout.outer.y.saturating_sub(1);
        let action = route_mouse(
            click(layout.outer.x, row),
            OVERLAY,
            &bar,
            &[],
            &mut p,
            &mut m,
        );
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(!p.open);
        assert_eq!(p.query, "");
        assert!(!p.naming_space);
    }

    #[test]
    fn the_wheel_moves_the_selection_inside_the_box() {
        // With more candidates than the ten rows drawn, a mouse-only user
        // could not reach entry eleven at all.
        let mut m = populated();
        let mut p = Palette {
            open: true,
            ..Default::default()
        };
        let total = palette::visible_candidates(&m, &p).len();
        assert!(total > 2, "precondition: {total}");
        let layout =
            palette::palette_layout(&m, &p, crate::ui::palette_area(OVERLAY, sidebar::WIDTH))
                .expect("drawn");
        let bar = tabs::tab_bar(TABS, &m);
        let notch = |p: &mut Palette, m: &mut Model, kind| {
            route_mouse(
                wheel(kind, layout.inner.x + 1, layout.inner.y + 1),
                OVERLAY,
                &bar,
                &[],
                p,
                m,
            )
        };
        let action = notch(&mut p, &mut m, MouseEventKind::ScrollDown);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(p.selected, 1);
        notch(&mut p, &mut m, MouseEventKind::ScrollUp);
        assert_eq!(p.selected, 0);
        notch(&mut p, &mut m, MouseEventKind::ScrollUp);
        assert_eq!(p.selected, 0, "cannot scroll above the first entry");
        for _ in 0..total + 10 {
            notch(&mut p, &mut m, MouseEventKind::ScrollDown);
        }
        assert_eq!(
            p.selected,
            total - 1,
            "clamped to the last live candidate, never past it"
        );
    }

    #[test]
    fn a_wheel_notch_while_the_palette_is_open_changes_nothing() {
        // Moving a list hidden under the overlay is a state change the user
        // cannot see happen, only notice later as a silent jump.
        let mut m = populated();
        m.sidebar_scroll = 4;
        let mut p = Palette {
            open: true,
            ..Default::default()
        };
        let action = route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollDown, 3, 2));
        assert!(matches!(action, Action::None));
        assert!(p.open, "and the palette is not dismissed either");
        assert_eq!(m.sidebar_scroll, 4);
    }

    // --- The sidebar's rows. ---

    #[test]
    fn clicking_a_space_selects_it_and_focuses_its_first_pane() {
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "web"));
        m.local_mut()
            .panes
            .push(pane("p3", "w2", "aider", AgentState::Idle));
        let mut p = Palette::default();
        let body = layout(&m).spaces_body;
        let action = route_at(&mut m, &mut p, click(3, body.y + 1));
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p3"),
            "{action:?}"
        );
        assert_eq!(m.selected_space.as_deref(), Some("w2"));
    }

    #[test]
    fn clicking_a_space_whose_panes_all_stopped_restarts_them() {
        // After a server restart or reboot every pane is stopped, and the
        // views list only live ones: the space looked empty and the click
        // did nothing, with the user's panes unreachable.
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "web"));
        for id in ["p3", "p4"] {
            let mut stopped = pane(id, "w2", "claude", AgentState::Idle);
            stopped.live = false;
            m.local_mut().panes.push(stopped);
        }
        let mut p = Palette::default();
        let body = layout(&m).spaces_body;
        let action = route_at(&mut m, &mut p, click(3, body.y + 1));
        assert!(
            matches!(&action, Action::Command(Command::RestartPanes(ids)) if ids == &["p3", "p4"]),
            "{action:?}"
        );
        assert_eq!(m.selected_space.as_deref(), Some("w2"));
    }

    #[test]
    fn a_space_with_a_running_pane_does_not_restart_its_stopped_ones() {
        // A pane the user quit on purpose stays quit; it comes back only
        // from the palette.
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "web"));
        m.local_mut()
            .panes
            .push(pane("p3", "w2", "aider", AgentState::Idle));
        let mut stopped = pane("p4", "w2", "claude", AgentState::Idle);
        stopped.live = false;
        m.local_mut().panes.push(stopped);
        let mut p = Palette::default();
        let body = layout(&m).spaces_body;
        let action = route_at(&mut m, &mut p, click(3, body.y + 1));
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p3"),
            "{action:?}"
        );
    }

    #[test]
    fn clicking_an_empty_space_still_selects_it() {
        // The whole reason `selected_space` exists: a space with no panes
        // cannot be identified by the focused pane, and it is exactly the
        // space whose `+` the user is about to reach for.
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "empty"));
        let mut p = Palette::default();
        let body = layout(&m).spaces_body;
        let action = route_at(&mut m, &mut p, click(3, body.y + 1));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.selected_space.as_deref(), Some("w2"));
        assert_eq!(m.active_space_id(), Some("w2"));
    }

    #[test]
    fn selecting_an_empty_space_blanks_the_pane_area() {
        // Leaving `focused` in the space you just left kept the previous
        // space's pane on screen under the new space's empty tab bar — which
        // reads as the click not having worked, and kept sending keystrokes
        // there.
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "empty"));
        let mut p = Palette::default();
        assert!(m.focused.is_some(), "precondition");
        let body = layout(&m).spaces_body;
        let action = route_at(&mut m, &mut p, click(3, body.y + 1));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.selected_space.as_deref(), Some("w2"));
        assert_eq!(m.focused, None);

        // And going back re-focuses, so it is a blank view rather than a
        // detach.
        let action = route_at(&mut m, &mut p, click(3, body.y));
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p1"),
            "{action:?}"
        );
    }

    #[test]
    fn clicking_an_agent_focuses_that_pane() {
        let mut m = populated();
        let mut p = Palette::default();
        let body = layout(&m).agents_body;
        let action = route_at(&mut m, &mut p, click(3, body.y + 2));
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p2"),
            "{action:?}"
        );
    }

    #[test]
    fn clicking_inside_a_list_but_past_its_entries_does_nothing() {
        // The agents panel takes the leftover height, so it has empty rows
        // below its last entry; the spaces list is sized to its contents, so
        // its only empty row is the one a zero-space client keeps.
        let mut m = populated();
        let mut p = Palette::default();
        let l = layout(&m);
        let row = l.agents_body.y + l.agents_body.height - 1;
        let action = route_at(&mut m, &mut p, click(3, row));
        assert!(matches!(action, Action::None), "row {row}: {action:?}");

        let mut empty = Model::default();
        let l = layout(&empty);
        let action = route_at(&mut empty, &mut p, click(3, l.spaces_body.y));
        assert!(matches!(action, Action::None), "{action:?}");
        assert_eq!(empty.selected_space, None, "and nothing got selected");
    }

    // --- The three sidebar buttons. ---

    #[test]
    fn clicking_new_opens_the_space_naming_prompt() {
        let mut m = populated();
        let mut p = Palette {
            query: "leftover".into(),
            selected: 4,
            ..Default::default()
        };
        let l = layout(&m);
        let action = route_at(&mut m, &mut p, click(l.new_button.x, l.new_button.y));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.open);
        assert!(p.naming_space);
        assert_eq!(p.query, "", "the prompt must not inherit a stale query");
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn clicking_menu_opens_the_ordinary_command_palette() {
        let mut m = populated();
        let mut p = Palette::default();
        let l = layout(&m);
        let action = route_at(&mut m, &mut p, click(l.menu_button.x, l.menu_button.y));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.open);
        assert!(!p.naming_space, "menu is the command list, not the prompt");
    }

    #[test]
    fn clicking_the_sort_toggle_flips_the_agent_order_and_rewinds_the_list() {
        // The list just reordered under the pointer; an offset into the old
        // order means nothing.
        let mut m = populated();
        m.agents_scroll = 3;
        let mut p = Palette::default();
        let l = layout(&m);
        let action = route_at(&mut m, &mut p, click(l.sort_toggle.x, l.sort_toggle.y));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.agent_sort, AgentSort::Priority);
        assert_eq!(m.agents_scroll, 0);

        // And back again — it is a toggle, not a one-way switch.
        let l = layout(&m);
        route_at(&mut m, &mut p, click(l.sort_toggle.x, l.sort_toggle.y));
        assert_eq!(m.agent_sort, AgentSort::Grouped);
    }

    #[test]
    fn the_sidebar_buttons_work_while_the_client_is_disconnected() {
        // None of the three needs a connection, and `main.rs::decide`
        // refuses every `Action::Command` while stale — so routing them as
        // commands would make the sidebar go dead exactly when a user starts
        // poking at it. `Action::Redraw` is the proof they took effect here
        // instead: it is the one outcome that cannot be refused downstream.
        let mut m = populated();
        m.local_mut().stale = true;
        let l = layout(&m);
        for (column, row) in [
            (l.new_button.x, l.new_button.y),
            (l.menu_button.x, l.menu_button.y),
            (l.sort_toggle.x, l.sort_toggle.y),
        ] {
            let mut p = Palette::default();
            let action = route_at(&mut m, &mut p, click(column, row));
            assert!(
                matches!(action, Action::Redraw),
                "({column},{row}) -> {action:?}"
            );
        }
        assert_eq!(
            m.agent_sort,
            AgentSort::Priority,
            "the toggle still flipped"
        );
    }

    // --- The tab bar. ---

    #[test]
    fn clicking_a_tab_focuses_its_pane() {
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let (rect, _) = bar.tabs[1];
        let action = route_mouse(
            click(rect.x + 1, rect.y),
            OVERLAY,
            &bar,
            &[],
            &mut p,
            &mut m,
        );
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p2"),
            "{action:?}"
        );
    }

    /// [`populated`] with forty panes, so the tab strip overflows [`TABS`].
    fn overflowing() -> Model {
        let mut m = populated();
        m.local_mut().panes = (0..40)
            .map(|i| pane(&format!("p{i}"), "w1", "claude", AgentState::Idle))
            .collect();
        m.focused = Some("p0".into());
        m
    }

    #[test]
    fn clicking_the_right_scroll_button_moves_the_strip_one_tab() {
        let mut m = overflowing();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        assert_eq!(bar.first, 0);
        let action = route_mouse(
            click(bar.scroll_right.x, bar.scroll_right.y),
            OVERLAY,
            &bar,
            &[],
            &mut p,
            &mut m,
        );
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.tab_scroll, 1);
        assert_eq!(
            m.tab_scroll_pin.as_deref(),
            Some("p0"),
            "pinned, so the focused tab does not pull the strip straight back"
        );
        let bar = tabs::tab_bar(TABS, &m);
        assert_eq!(bar.first, 1, "and the next frame draws it scrolled");

        // And back.
        let action = route_mouse(
            click(bar.scroll_left.x, bar.scroll_left.y),
            OVERLAY,
            &bar,
            &[],
            &mut p,
            &mut m,
        );
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(tabs::tab_bar(TABS, &m).first, 0);
    }

    #[test]
    fn a_scroll_button_at_the_end_of_the_strip_does_nothing() {
        let mut m = overflowing();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(
            click(bar.scroll_left.x, bar.scroll_left.y),
            OVERLAY,
            &bar,
            &[],
            &mut p,
            &mut m,
        );
        assert!(matches!(action, Action::None), "{action:?}");
        assert_eq!(m.tab_scroll, 0);

        m.tab_scroll = 999;
        m.tab_scroll_pin = Some("p0".into());
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(
            click(bar.scroll_right.x, bar.scroll_right.y),
            OVERLAY,
            &bar,
            &[],
            &mut p,
            &mut m,
        );
        assert!(matches!(action, Action::None), "{action:?}");
        assert_eq!(tabs::tab_bar(TABS, &m).first, bar.max_first);
    }

    // --- Dragging the sidebar's edge and its section split. ---

    #[test]
    fn dragging_the_edge_resizes_the_sidebar() {
        let mut m = populated();
        let mut p = Palette::default();
        let action = route_at(&mut m, &mut p, click(EDGE, 5));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.sidebar.drag, Some(SidebarDrag::Width));
        assert!(p.selection.is_none(), "the edge is not a pane to select in");

        // The edge follows the pointer: dragged to column 35, the column is
        // 36 wide and its edge sits under the pointer.
        let action = route_at(&mut m, &mut p, drag(35, 9));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.sidebar.width, 36);

        let action = route_at(&mut m, &mut p, release(35, 9));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.sidebar.drag, None, "the release ends it");
        assert_eq!(m.sidebar.width, 36, "and the width stays");

        // A drag with nothing armed no longer resizes anything.
        route_at(&mut m, &mut p, drag(50, 9));
        assert_eq!(m.sidebar.width, 36);
    }

    #[test]
    fn a_dragged_width_is_clamped_by_the_terminal_and_the_range() {
        let mut m = populated();
        let mut p = Palette::default();
        route_at(&mut m, &mut p, click(EDGE, 5));
        route_at(&mut m, &mut p, drag(79, 5));
        assert_eq!(m.sidebar.width, 40, "half of an 80-column terminal");
        route_at(&mut m, &mut p, drag(0, 5));
        assert_eq!(m.sidebar.width, sidebar::MIN_WIDTH);
    }

    #[test]
    fn dragging_the_divider_moves_the_split_between_the_sections() {
        let mut m = populated();
        let mut p = Palette::default();
        let divider = layout(&m).divider;
        let action = route_at(&mut m, &mut p, click(3, divider.y));
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(m.sidebar.drag, Some(SidebarDrag::Split));

        route_at(&mut m, &mut p, drag(3, 12));
        assert_eq!(m.sidebar.spaces_height, Some(12));
        assert_eq!(layout(&m).divider.y, 12, "the divider follows the pointer");

        route_at(&mut m, &mut p, drag(3, 999));
        assert_eq!(
            layout(&m).divider.y + 4,
            SIDEBAR.height,
            "clamped so the agents panel keeps a whole entry"
        );
        route_at(&mut m, &mut p, release(3, 999));
        assert_eq!(m.sidebar.drag, None);
    }

    #[test]
    fn a_sidebar_drag_whose_release_was_lost_ends_at_the_next_press() {
        let mut m = populated();
        let mut p = Palette::default();
        route_at(&mut m, &mut p, click(EDGE, 5));
        // No release: the pointer left the window. The next press is
        // somewhere ordinary, and the drag that follows it is its own.
        route_at(&mut m, &mut p, click(50, 20));
        assert_eq!(m.sidebar.drag, None);
        route_at(&mut m, &mut p, drag(40, 20));
        assert_eq!(m.sidebar.width, sidebar::WIDTH, "unchanged");
    }

    #[test]
    fn a_sidebar_drag_never_extends_a_text_selection() {
        let mut m = populated();
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(40, 5));
        assert!(p.selection.is_some(), "precondition: a selection is armed");
        route_at(&mut m, &mut p, click(EDGE, 5));
        assert!(p.selection.is_none(), "pressing the edge drops it");
        let action = route_at(&mut m, &mut p, release(EDGE, 5));
        assert!(!matches!(action, Action::CopySelection), "{action:?}");
    }

    #[test]
    fn a_collapsed_sidebar_is_never_hit_and_the_tab_bar_takes_its_place() {
        let mut m = populated();
        m.sidebar.collapsed = Some(true);
        let mut p = Palette::default();
        let tabs_area = crate::ui::tab_bar_area(OVERLAY, m.sidebar.columns(OVERLAY.width));
        assert_eq!(tabs_area.x, 0, "the strip starts at the left edge");
        let bar = tabs::tab_bar(tabs_area, &m);
        // Where the space row used to be is now the pane area: no space is
        // selected, no width drag starts.
        let action = route_mouse(click(3, 1), OVERLAY, &bar, &[], &mut p, &mut m);
        assert!(matches!(action, Action::None), "{action:?}");
        route_mouse(click(EDGE, 5), OVERLAY, &bar, &[], &mut p, &mut m);
        assert_eq!(m.sidebar.drag, None);
        // And the tabs, now at column 0, are clickable there.
        let (rect, _) = bar.tabs[1].clone();
        let action = route_mouse(click(rect.x, rect.y), OVERLAY, &bar, &[], &mut p, &mut m);
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p2"),
            "{action:?}"
        );
    }

    #[test]
    fn clicking_a_toast_focuses_its_pane_and_dismisses_it() {
        let mut m = populated();
        let payload = serde_json::json!({
            "signal": "needs_attention", "paneId": "p2", "agentType": "codex", "title": "",
        });
        assert!(m.push_toast(&payload, std::time::Instant::now()));
        let mut p = Palette::default();
        let area = crate::ui::pane_area(OVERLAY, m.sidebar.columns(OVERLAY.width));
        let layout = crate::ui::toast::toast_layout(area, &m);
        let (rect, _) = layout[0].clone();
        let action = route_mouse(
            click(rect.x + 1, rect.y),
            OVERLAY,
            &tabs::tab_bar(TABS, &m),
            &pane_targets(),
            &mut p,
            &mut m,
        );
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p2"),
            "{action:?}"
        );
        assert!(m.toasts.is_empty(), "the toast it acted on is gone");
        assert!(
            p.selection.is_none(),
            "and no text selection was armed under it"
        );
    }

    #[test]
    fn a_click_beside_a_toast_still_reaches_the_pane_under_it() {
        let mut m = populated();
        let payload = serde_json::json!({
            "signal": "finished", "paneId": "p2", "agentType": "codex", "title": "",
        });
        m.push_toast(&payload, std::time::Instant::now());
        let mut p = Palette::default();
        route_pane(&mut m, &mut p, click(30, 1));
        assert!(p.selection.is_some(), "the pane's own selection, as before");
        assert_eq!(m.toasts.len(), 1);
    }

    /// Clicks the tab bar's `+` and returns the palette state it left.
    fn click_plus(m: &mut Model, p: &mut Palette) -> Action {
        let bar = tabs::tab_bar(TABS, m);
        route_mouse(
            click(bar.new_tab.x, bar.new_tab.y),
            OVERLAY,
            &bar,
            &[],
            p,
            m,
        )
    }

    #[test]
    fn clicking_the_plus_opens_the_agent_picker_for_the_active_space() {
        // Not a `CreatePane`: a click cannot carry *which agent*, and the
        // runtime defaults a missing `agentType` to `shell` — so the
        // straight-to-create version of this button made bare shells with no
        // agent in them, every time.
        let mut m = populated();
        let mut p = Palette::default();
        let action = click_plus(&mut m, &mut p);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.open);
        assert_eq!(p.picking_agent.as_deref(), Some("w1"));
        assert!(!p.naming_space, "the two modes are exclusive");
        assert_eq!(p.query, "");
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn the_plus_picks_for_the_space_selected_in_the_sidebar() {
        let mut m = populated();
        m.local_mut().workspaces.push(ws("w2", "web"));
        m.selected_space = Some("w2".into());
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        assert_eq!(p.picking_agent.as_deref(), Some("w2"));
    }

    #[test]
    fn choosing_an_agent_creates_the_pane_with_it() {
        // The whole point: `enter` in the picker must produce a `CreatePane`
        // carrying a real agent type, not one the server will default away.
        let mut m = populated();
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreatePane { workspace_id, agent_type, .. })
                if workspace_id == "w1" && agent_type == "claude"),
            "{action:?}"
        );
        assert!(!p.open);
        assert_eq!(p.picking_agent, None, "the mode dies with the prompt");
    }

    #[test]
    fn typing_in_the_picker_chooses_a_different_agent() {
        let mut m = populated();
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        for c in ['c', 'o', 'd'] {
            route(key(KeyCode::Char(c), KeyModifiers::NONE), &mut p, None, &m);
        }
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(
            matches!(&action, Action::Command(Command::CreatePane { agent_type, .. })
                if agent_type == "codex"),
            "{action:?}"
        );
    }

    #[test]
    fn the_palettes_new_pane_entry_opens_the_picker_too() {
        // Same destination whichever way the user asked, so there is only one
        // path that can forget to send an agent type.
        let m = populated();
        let mut p = Palette {
            open: true,
            query: "New pane".into(),
            ..Default::default()
        };
        let action = route(key(KeyCode::Enter, KeyModifiers::NONE), &mut p, None, &m);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert!(p.open, "it reopens as the picker rather than closing");
        assert_eq!(p.picking_agent.as_deref(), Some("w1"));
        assert_eq!(p.query, "", "and the query that found it is cleared");
    }

    #[test]
    fn escaping_the_picker_leaves_no_mode_behind() {
        let mut m = populated();
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        route(key(KeyCode::Esc, KeyModifiers::NONE), &mut p, None, &m);
        assert!(!p.open);
        assert_eq!(p.picking_agent, None);

        // The next plain ctrl+k is the ordinary command list.
        route(
            key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            &mut p,
            None,
            &m,
        );
        assert!(p.open);
        assert_eq!(p.picking_agent, None);
        assert!(!p.naming_space);
    }

    #[test]
    fn a_click_elsewhere_dismisses_the_picker() {
        let mut m = populated();
        let mut p = Palette::default();
        click_plus(&mut m, &mut p);
        let bar = tabs::tab_bar(TABS, &m);
        route_mouse(click(3, 2), OVERLAY, &bar, &[], &mut p, &mut m);
        assert!(!p.open);
        assert_eq!(p.picking_agent, None);
    }

    #[test]
    fn a_wheel_notch_over_a_pane_scrolls_that_panes_scrollback() {
        // It used to do nothing at all, on the reasoning that a remote PTY is
        // not something this client scrolls back through — while the grid was
        // keeping 10,000 lines of history nothing could reach.
        let mut m = populated();
        let mut p = Palette::default();
        let up = route_pane(&mut m, &mut p, wheel(MouseEventKind::ScrollUp, 40, 5));
        assert!(
            matches!(&up, Action::ScrollPane { pane_id, delta }
                if pane_id == "p1" && *delta > 0),
            "up goes back into history: {up:?}"
        );
        let down = route_pane(&mut m, &mut p, wheel(MouseEventKind::ScrollDown, 40, 5));
        assert!(
            matches!(&down, Action::ScrollPane { pane_id, delta }
                if pane_id == "p1" && *delta < 0),
            "down goes back towards the live edge: {down:?}"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer_not_the_focused_one() {
        // With a split on screen, scrolling the half you are pointing at is
        // the only behaviour that is not a surprise.
        let mut m = populated();
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let both = vec![
            (Rect::new(28, 1, 26, 22), "p1".to_string()),
            (Rect::new(54, 1, 26, 22), "p2".to_string()),
        ];
        let action = route_mouse(
            wheel(MouseEventKind::ScrollUp, 60, 5),
            OVERLAY,
            &bar,
            &both,
            &mut p,
            &mut m,
        );
        assert!(
            matches!(&action, Action::ScrollPane { pane_id, .. } if pane_id == "p2"),
            "{action:?}"
        );
    }

    #[test]
    fn a_wheel_notch_over_the_tab_bar_does_nothing() {
        // Not even falling through to the sidebar, which the pointer is
        // nowhere near.
        let mut m = populated();
        m.sidebar_scroll = 2;
        let mut p = Palette::default();
        let action = route_at(
            &mut m,
            &mut p,
            wheel(MouseEventKind::ScrollDown, TABS.x + 2, TABS.y),
        );
        assert!(matches!(action, Action::None), "{action:?}");
        assert_eq!(m.sidebar_scroll, 2);
    }

    // --- Pane regions. ---

    fn split_target() -> Vec<(Rect, String)> {
        vec![(Rect::new(54, 1, 26, 20), "p2".to_string())]
    }

    #[test]
    fn clicking_the_split_panes_region_focuses_it() {
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(click(60, 4), OVERLAY, &bar, &split_target(), &mut p, &mut m);
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p2"),
            "{action:?}"
        );
    }

    #[test]
    fn clicking_the_focused_panes_own_half_hits_nothing() {
        // `main.rs` deliberately offers only the *unfocused* pane as a
        // target: re-running the focus path would re-attach, resize, and
        // wipe the status-bar notice explaining why something just failed.
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let action = route_mouse(click(30, 4), OVERLAY, &bar, &split_target(), &mut p, &mut m);
        assert!(matches!(action, Action::None), "{action:?}");
    }

    #[test]
    fn the_sidebar_wins_over_an_overlapping_pane_region() {
        // Regions can only reach the router alongside the sidebar they sit
        // beside, but nothing in the *type* stops a caller passing an
        // overlapping rect — a click on a space row must not focus a pane.
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let overlapping = vec![(Rect::new(0, 0, 80, 23), "p2".to_string())];
        let body = layout(&m).spaces_body;
        let action = route_mouse(
            click(3, body.y),
            OVERLAY,
            &bar,
            &overlapping,
            &mut p,
            &mut m,
        );
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "p1"),
            "the space's own first pane, not the region underneath: {action:?}"
        );
    }

    #[test]
    fn the_first_matching_pane_region_wins() {
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let overlapping = vec![
            (Rect::new(40, 5, 40, 10), "first".to_string()),
            (Rect::new(40, 5, 40, 10), "second".to_string()),
        ];
        let action = route_mouse(click(60, 8), OVERLAY, &bar, &overlapping, &mut p, &mut m);
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "first"),
            "{action:?}"
        );
    }

    #[test]
    fn a_zero_sized_pane_region_can_never_be_clicked() {
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let empty = vec![(Rect::new(0, 0, 0, 0), "ghost".to_string())];
        let action = route_mouse(
            click(0, 0),
            Rect::new(0, 0, 0, 0),
            &bar,
            &empty,
            &mut p,
            &mut m,
        );
        assert!(matches!(action, Action::None));
    }

    // --- Scrolling. ---

    /// A model with more spaces and agents than either list can show.
    fn crowded() -> Model {
        let mut m = Model::default();
        m.local_mut().workspaces = (0..30)
            .map(|i| ws(&format!("w{i}"), &format!("s{i}")))
            .collect();
        m.local_mut().panes = (0..30)
            .map(|i| pane(&format!("p{i}"), "w0", "claude", AgentState::Idle))
            .collect();
        m
    }

    #[test]
    fn the_wheel_scrolls_whichever_list_it_is_over() {
        let mut m = crowded();
        let mut p = Palette::default();
        let l = layout(&m);
        route_at(
            &mut m,
            &mut p,
            wheel(MouseEventKind::ScrollDown, 3, l.spaces_body.y),
        );
        assert_eq!(m.sidebar_scroll, 1);
        assert_eq!(m.agents_scroll, 0, "the other list must not move");

        route_at(
            &mut m,
            &mut p,
            wheel(MouseEventKind::ScrollDown, 3, l.agents_body.y),
        );
        assert_eq!(m.agents_scroll, 1);
        assert_eq!(m.sidebar_scroll, 1, "and neither must this one");
    }

    #[test]
    fn scrolling_up_steps_back_and_stops_at_the_top() {
        let mut m = crowded();
        m.sidebar_scroll = 1;
        let mut p = Palette::default();
        let row = layout(&m).spaces_body.y;
        route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollUp, 3, row));
        assert_eq!(m.sidebar_scroll, 0);
        route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollUp, 3, row));
        assert_eq!(m.sidebar_scroll, 0, "cannot scroll above the first entry");
    }

    #[test]
    fn scrolling_down_stops_at_the_last_page_rather_than_running_off_the_end() {
        let mut m = crowded();
        let mut p = Palette::default();
        let l = layout(&m);
        let row = l.spaces_body.y;
        for _ in 0..200 {
            route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollDown, 3, row));
        }
        let expected = 30 - usize::from(l.spaces_body.height);
        assert_eq!(m.sidebar_scroll, expected);
    }

    #[test]
    fn the_agents_wheel_steps_in_entries_not_rows() {
        // Each entry is two rows tall, so the last page is measured in
        // entries — clamping in rows would let the list scroll a page too far.
        let mut m = crowded();
        let mut p = Palette::default();
        let l = layout(&m);
        let row = l.agents_body.y;
        for _ in 0..200 {
            route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollDown, 3, row));
        }
        let per_page = usize::from(l.agents_body.height / sidebar::AGENT_ENTRY_ROWS);
        assert_eq!(m.agents_scroll, 30 - per_page);
    }

    #[test]
    fn scrolling_up_also_rescues_an_offset_stranded_by_a_shrunken_list() {
        // The pane closed while the user was scrolled to the bottom.
        let mut m = crowded();
        m.sidebar_scroll = 900;
        let mut p = Palette::default();
        let l = layout(&m);
        route_at(
            &mut m,
            &mut p,
            wheel(MouseEventKind::ScrollUp, 3, l.spaces_body.y),
        );
        assert!(
            m.sidebar_scroll <= 30 - usize::from(l.spaces_body.height),
            "{}",
            m.sidebar_scroll
        );
    }

    #[test]
    fn a_wheel_notch_over_a_header_or_button_scrolls_nothing() {
        let mut m = crowded();
        m.sidebar_scroll = 3;
        m.agents_scroll = 3;
        let mut p = Palette::default();
        let l = layout(&m);
        for row in [
            l.spaces_header.y,
            l.new_button.y,
            l.divider.y,
            l.agents_header.y,
        ] {
            let action = route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollDown, 3, row));
            assert!(matches!(action, Action::None), "row {row}: {action:?}");
        }
        assert_eq!(m.sidebar_scroll, 3);
        assert_eq!(m.agents_scroll, 3);
    }

    #[test]
    fn a_scrolled_list_focuses_the_entry_under_the_cursor() {
        let mut m = crowded();
        m.sidebar_scroll = 5;
        let mut p = Palette::default();
        let body = layout(&m).spaces_body;
        route_at(&mut m, &mut p, click(3, body.y + 2));
        assert_eq!(m.selected_space.as_deref(), Some("w7"));
    }

    // --- Degenerate geometry. ---

    #[test]
    fn routing_against_a_zero_sized_screen_never_matches_or_panics() {
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(Rect::new(0, 0, 0, 0), &m);
        for row in 0..4 {
            for column in 0..4 {
                let action = route_mouse(
                    click(column, row),
                    Rect::new(0, 0, 0, 0),
                    &bar,
                    &[],
                    &mut p,
                    &mut m,
                );
                assert!(matches!(action, Action::None));
            }
        }
    }

    #[test]
    fn every_cell_of_a_real_sidebar_routes_without_panicking() {
        let mut m = crowded();
        m.sidebar_scroll = usize::MAX;
        m.agents_scroll = usize::MAX;
        let mut p = Palette::default();
        for row in 0..SIDEBAR.height {
            for column in 0..SIDEBAR.width {
                route_at(&mut m, &mut p, click(column, row));
                route_at(
                    &mut m,
                    &mut p,
                    wheel(MouseEventKind::ScrollDown, column, row),
                );
                route_at(&mut m, &mut p, wheel(MouseEventKind::ScrollUp, column, row));
            }
        }
    }

    #[test]
    fn the_agents_panel_is_absent_on_a_short_sidebar_and_clicks_there_are_inert() {
        let short = Rect::new(0, 0, sidebar::WIDTH, 5);
        let mut m = populated();
        let mut p = Palette::default();
        let bar = tabs::tab_bar(TABS, &m);
        let l = sidebar::sidebar_layout(short, &m);
        assert_eq!(l.agents_body, Rect::default(), "precondition");
        for row in 0..short.height {
            route_mouse(click(3, row), OVERLAY, &bar, &[], &mut p, &mut m);
        }
        assert_eq!(m.agents_scroll, 0);
    }

    // --- `stepped`, the one place a notch is added or subtracted. ---

    #[test]
    fn a_notch_moves_one_entry_and_a_click_moves_none() {
        let step = |kind, current, entries, height, rows| {
            stepped(kind, current, |offset| {
                sidebar::clamp_scroll(offset, entries, height, rows)
            })
        };
        assert_eq!(step(MouseEventKind::ScrollDown, 5, 30, 10, 1), 6);
        assert_eq!(step(MouseEventKind::ScrollUp, 5, 30, 10, 1), 4);
        assert_eq!(
            step(MouseEventKind::Down(MouseButton::Left), 5, 30, 10, 1),
            5,
            "a click leaves the offset where it is"
        );
        assert_eq!(step(MouseEventKind::ScrollUp, 0, 30, 10, 1), 0);
        assert_eq!(step(MouseEventKind::ScrollDown, 20, 30, 10, 1), 20);
        assert_eq!(
            step(MouseEventKind::ScrollDown, 15, 5, 10, 1),
            0,
            "a list shorter than its viewport cannot scroll at all"
        );
        assert_eq!(
            step(MouseEventKind::ScrollDown, usize::MAX, 30, 10, 1),
            20,
            "saturates rather than wrapping"
        );
    }

    // --- The `DisplayState` import above is load-bearing for this one. ---

    #[test]
    fn priority_sort_puts_the_blocked_agent_under_the_first_click() {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![ws("w1", "api")];
        m.local_mut().panes = vec![
            pane("calm", "w1", "claude", AgentState::Idle),
            pane("stuck", "w1", "codex", AgentState::Blocked),
        ];
        assert_eq!(
            crate::model::display_state(&m.local_panes()[1]),
            DisplayState::Blocked
        );
        let mut p = Palette::default();
        let l = layout(&m);
        route_at(&mut m, &mut p, click(l.sort_toggle.x, l.sort_toggle.y));
        let action = route_at(&mut m, &mut p, click(3, l.agents_body.y));
        assert!(
            matches!(&action, Action::Command(Command::FocusPane(id)) if id == "stuck"),
            "{action:?}"
        );
    }
}
