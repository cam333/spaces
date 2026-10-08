//! The command palette: chrome state, the list of things it can do, and the
//! overlay that draws it.
//!
//! [`Palette`] is pure UI state — it never arrives via snapshot or event,
//! same as [`crate::model::Model::focused`]. [`candidates`] turns the
//! current [`crate::model::Model`] into the palette's list of selectable
//! [`Command`]s, filtered by a case-insensitive substring match on the
//! label — though every consumer goes through [`visible_candidates`], which
//! swaps that list for a one-entry naming prompt while
//! [`Palette::naming_space`] is set;
//! [`super::super::input::route`] is what actually moves keystrokes
//! into and out of this struct; [`render_palette`] is what puts it on
//! screen, called from [`super::frame::render`] over the pane area.
//!
//! `candidates` is fed straight from protocol-derived state (`Model`, which
//! is itself folded forward from network input), so — like every function
//! in `model.rs` — it must not panic on any shape that state can take. It
//! never re-derives agent state itself: every label that mentions a pane's
//! status goes through [`crate::model::display_state`], not
//! `PaneInfo::agent_state`/`seen` directly. The same applies to
//! `render_palette`: `area` is whatever the terminal's own reported size
//! leaves for the pane region (down to nothing at all), and `selected` is
//! only ever *clamped* by `route`, never guaranteed in range against the
//! list this function rebuilds — so every dimension is checked, every
//! index is `.get`/`skip`/`take` rather than `[]`, and the whole overlay is
//! clipped to the buffer it draws into.

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::model::{display_state, Model};

/// Widest the overlay is allowed to grow, in terminal cells. Narrower
/// terminals get whatever the pane area actually has.
const MAX_WIDTH: u16 = 60;

/// At most this many candidate rows are drawn at once; the list scrolls to
/// keep [`Palette::selected`] visible past that (see [`scroll_offset`]).
const MAX_VISIBLE: usize = 10;

/// Drawn ahead of [`Palette::query`] on the overlay's first row.
const QUERY_PREFIX: &str = "> ";

/// Border title, so an overlay that appears over a pane's output is
/// self-explanatory rather than an unlabeled box.
const TITLE: &str = " commands ";

/// Border title while [`Palette::naming_space`] is set. Carries the
/// instruction as well as the mode because the candidate list is *empty*
/// until something is typed — an untitled empty box opened by a click would
/// leave the user with no idea what it wants from them.
const NAMING_TITLE: &str = " new space: type a name, then enter ";

/// The naming prompt's border title, naming the machine the space will land
/// on once the fleet has more than one.
///
/// Silent on a single-machine session: the suffix would be noise on the
/// prompt a user sees most often. Once there are two, saying nothing would
/// mean the space lands somewhere the user never chose and cannot see — which
/// is the one thing a create prompt must not do.
pub fn naming_title(model: &Model) -> String {
    if model.nodes.len() < 2 {
        return NAMING_TITLE.to_string();
    }
    match model.node(&model.target_node()) {
        Some(node) => format!(" new space on {}: type a name, then enter ", node.name),
        None => NAMING_TITLE.to_string(),
    }
}

/// Border title while [`Palette::picking_session`] is set.
///
/// Deliberately not "pick a session": the list comes from an index the web app
/// maintains, so a session started elsewhere since the last index will not be
/// in it. The wording offers a choice without claiming the list is complete.
const SESSION_TITLE: &str = " resume or start fresh ";

/// Border title while [`Palette::picking_agent`] is set.
const PICKING_TITLE: &str = " new pane: pick an agent ";

/// Border title while [`Palette::asking_cwd`] is set. Names the thing being
/// asked for rather than the mode, because the candidate list is empty until
/// something is typed.
const CWD_TITLE: &str = " new pane: type a directory, then enter ";
const BROWSE_TITLE: &str =
    " new pane: enter opens a folder \u{b7} backspace goes up \u{b7} or type a path ";

/// Border title while [`Palette::renaming_space`] is set.
const RENAME_TITLE: &str = " rename space: edit the name, then enter ";

/// Border title while [`Palette::picking_color`] is set.
const COLOR_TITLE: &str = " color: pick one ";

/// Border title while [`Palette::renaming_pane`] is set.
const PANE_RENAME_TITLE: &str = " rename pane: edit the name, then enter ";

/// The agents a new pane can be launched with, in the order offered.
///
/// Copied from `bin/lib/runtime/spawn-pane.js`'s `AGENTS` table — the server
/// resolves each of these to a command it types into the fresh PTY, and a
/// name absent from that table silently produces a bare shell. `shell` is
/// last and deliberately included: it is a legitimate choice, and naming it
/// explicitly is what makes the other six read as *agents* rather than as
/// the only options.
///
/// `custom` is the one table entry left out: it launches
/// `params.customCommand`, and there is nowhere in this client yet to type
/// one. Offering it would create panes that silently do nothing — exactly
/// the defect this list exists to fix.
pub const AGENT_TYPES: [&str; 7] = ["claude", "codex", "gemini", "aider", "forge", "pi", "shell"];

/// Marks the selected candidate. Drawn *in addition to* the reversed-video
/// style on that row: a terminal (or a `TestBackend` assertion) that
/// doesn't render styles still shows which entry `enter` would fire.
const SELECTION_MARKER: char = '\u{25b8}';

/// Chrome state for the command palette. Every field here is `pub`. One of
/// them is an odd fit: [`Palette::awaiting_chord_key`].
///
/// That field is *not* part of the palette's own concern (query/selection)
/// — it exists because the two-key `ctrl+space` chords (`d` to detach, `s`
/// to toggle the split) need somewhere to remember that `ctrl+space` was
/// already pressed, and that fact isn't something a `Model` snapshot could
/// ever hold (it's transient input history, not runtime state). `Palette`
/// is the only piece of mutable UI state [`super::super::input::route`]
/// owns across calls, so that is where the prefix bit lives, even though it
/// has nothing to do with the palette itself.
#[derive(Debug, Clone, Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub selected: usize,
    /// Set by `route` when `ctrl+space` is pressed while the palette is
    /// closed; cleared by the very next keystroke, whether that keystroke
    /// completes a chord (`d` -> detach, `s` -> toggle the split) or cancels
    /// it (anything else). Named for the prefix rather than for one of its
    /// completions, since there is more than one.
    pub awaiting_chord_key: bool,
    /// Set when the palette was opened by clicking the sidebar's
    /// `+ new workspace` button, cleared whenever it closes.
    ///
    /// While set, the palette is a *prompt* rather than a command list:
    /// [`visible_candidates`] offers exactly one entry — create a workspace
    /// named after the typed query — so `enter` does the thing the user
    /// clicked for. Without this mode the button could only open the
    /// ordinary list, where the create-workspace entry is deliberately
    /// *last* (see [`candidates`]) and `selected` defaults to 0: `enter`
    /// would fire whatever unrelated command happened to sit at the top.
    pub naming_space: bool,
    /// The pane about to be created, while the palette asks which session it
    /// should resume. `None` the rest of the time.
    ///
    /// Carries the choices the earlier steps already made — space, agent and
    /// the resolved directory — because this step is the last of three and
    /// re-deriving any of them here would be guessing at what the user picked.
    pub picking_session: Option<PendingPane>,
    /// The space a new pane is being created in, while the palette is the
    /// agent picker. `None` the rest of the time.
    ///
    /// Creating a pane needs one thing a click cannot carry: *which agent*.
    /// Without this step the client sent no `agentType` at all and the
    /// runtime defaulted it to `shell`
    /// (`bin/lib/runtime/spawn-pane.js`), so every pane the TUI made was a
    /// bare shell with no agent in it — the "new panes are not working"
    /// report of 2026-08-12. Carrying the space id here rather than
    /// re-deriving it on `enter` means the pane lands where the user asked
    /// even if they select a different space in the sidebar meanwhile.
    pub picking_agent: Option<String>,
    /// The tab context menu, when a right-click has opened one.
    ///
    /// Here for the same reason [`Palette::awaiting_chord_key`] is: this
    /// struct is the only mutable UI state the router owns across calls, so it
    /// is where every piece of chrome state lives, whether or not it belongs
    /// to the palette itself. Renaming the struct would be the honest
    /// alternative and is not worth the churn.
    pub tab_menu: Option<super::menu::TabMenu>,
    /// The drag-selection over a pane''s grid, if there is one.
    ///
    /// Kept after the mouse is released rather than cleared, so the text you
    /// just copied stays highlighted until the next click — otherwise the
    /// only feedback that anything happened is a status-bar line that scrolls
    /// past. Cleared by the next `Down`.
    ///
    /// Here rather than on `Model` for the same reason [`Palette::tab_menu`]
    /// is: this struct is where the router keeps chrome state across calls.
    pub selection: Option<super::selection::Selection>,
    /// `(space, agent)` while the palette is asking where a new pane should
    /// start. `None` the rest of the time.
    ///
    /// Only reached for a space's **first** pane. Every later pane inherits
    /// the directory of the most recent one there (`main.rs::inherited_cwd`),
    /// because that is nearly always right and a prompt you answer the same
    /// way every time is a prompt that trains you to ignore it. The first pane
    /// has nothing to inherit from, so it is the one time asking is the only
    /// way to get it right — see [`needs_cwd`].
    pub asking_cwd: Option<(String, String)>,
    /// The folder the directory step is showing, once the server has listed
    /// it (`fs.folders`). `None` leaves the step as a typed prompt, which is
    /// also what it falls back to when the server cannot list folders — a
    /// peer machine, or a server too old to know the method.
    pub browse: Option<FolderListing>,
    /// The space being renamed, while the palette is the rename prompt.
    /// Pre-filled with the current name, so `enter` on an unedited prompt is a
    /// no-op rather than a surprise.
    pub renaming_space: Option<String>,
    /// The space being recolored, while the palette is the color picker.
    pub picking_color: Option<String>,
    /// The pane being renamed. Pre-filled with its current title, and an
    /// empty confirmation is meaningful — it hands the pane back to its
    /// shell-set name.
    pub renaming_pane: Option<String>,
    /// The pane being recolored.
    pub picking_pane_color: Option<String>,
}

/// `true` iff creating a pane in `workspace_id` should ask for a directory
/// first: exactly when that space has no live pane to inherit one from.
///
/// The mirror image of `main.rs::inherited_cwd` — that function answers "what
/// would we inherit", this one answers "is there anything to inherit" — and
/// deliberately keyed on the same `live_panes_in`, so the prompt appears in
/// precisely the cases the inheritance cannot cover.
pub fn needs_cwd(model: &Model, workspace_id: &str) -> bool {
    model
        .live_panes_in(workspace_id)
        .iter()
        .all(|pane| pane.cwd.is_empty())
}

/// One level of the server's folder tree, as `fs.folders` returned it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderListing {
    pub current: String,
    /// `None` at the top of a dev directory: the browser goes no higher.
    pub parent: Option<String>,
    /// `(name, full path)`, sorted by the server.
    pub folders: Vec<(String, String)>,
    /// Every dev directory, offered at the top level so the user can switch.
    pub roots: Vec<String>,
}

/// A pane whose agent and directory are settled, waiting on a session choice.
///
/// `PartialEq` but not `Eq`: the generated `SessionSummary` it holds does not
/// derive `Eq`, and nothing here needs total equality.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPane {
    pub workspace_id: String,
    pub agent_type: String,
    /// The resolved directory. Sessions are scoped to one, which is why this
    /// step comes after the directory rather than before it.
    pub cwd: Option<String>,
    /// What the server offered, newest first. Never empty — an empty list
    /// means "no choice to make", and the caller creates the pane instead of
    /// opening this step at all.
    pub sessions: Vec<spaces_proto::SessionSummary>,
}

/// Something the palette can dispatch. `CreatePane`/`FocusPane`/`ClosePane`
/// carry the ids they act on; `CreateWorkspace` carries the (already
/// trimmed) name to create it with; `NextPane`/`PrevPane`/`Detach`/
/// `ToggleSplit` need none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    FocusPane(String),
    /// Create a pane running `agent_type` in `workspace_id`.
    ///
    /// `agent_type` must be one of [`AGENT_TYPES`]. It is **not** optional
    /// and there is no default: the runtime's own default is `shell`, so an
    /// omitted agent type is not "let the server choose", it is "make a pane
    /// with no agent in it".
    CreatePane {
        workspace_id: String,
        agent_type: String,
        /// Where the pane's shell starts. `None` leaves it to
        /// `main.rs::inherited_cwd`, which copies the space's most recent
        /// live pane and falls back to the runtime's own default.
        cwd: Option<String>,
    },
    /// Open the agent picker for `workspace_id` — the step that turns "new
    /// pane" into a [`Command::CreatePane`] carrying a real agent.
    ///
    /// Never reaches `handle_command`: `input` intercepts it and re-opens the
    /// palette instead, the same way `Detach` is intercepted before it can
    /// reach a connection it does not need. Kept in this enum rather than
    /// modelled as an `Action` because it is what the palette's own "New pane
    /// in ..." entry resolves to, and every such entry is a `Command`.
    PickAgent {
        workspace_id: String,
    },
    /// Open the directory prompt for a new `agent_type` pane in
    /// `workspace_id` — the step a space's *first* pane goes through, since it
    /// has no sibling to inherit a directory from.
    ///
    /// Intercepted by `input` exactly like [`Command::PickAgent`]: it asks the
    /// user, not the server.
    AskPaneCwd {
        workspace_id: String,
        agent_type: String,
    },
    ClosePane(String),
    /// Open the active space's Files tab, listing its folder the first time.
    OpenFiles,
    /// Bring the active space's (already open) Files tab to the front.
    /// Local: no server call.
    ShowFiles,
    /// Close a space's Files tab. Local.
    CloseFiles(String),
    /// List a folder on the space's machine (`fs.folders`) and show it in the
    /// directory step. `path: None` lists the first dev directory. Intercepted
    /// in `main.rs` before any command runs, since it needs both the
    /// connection and the palette.
    BrowseFolder {
        workspace_id: String,
        agent_type: String,
        path: Option<String>,
    },
    /// Start stopped panes again in place (`pane.restart`), then focus the
    /// first. Produced by selecting a space whose panes have all stopped,
    /// and by the palette's "Restart ..." entries.
    RestartPanes(Vec<String>),
    /// Move `pane_id` into `workspace_id` (`pane.move`). Produced only by the
    /// tab context menu — see [`super::menu`].
    MovePane {
        pane_id: String,
        workspace_id: String,
    },
    /// Close a space and every pane in it (`workspace.close`). The most
    /// destructive thing this client can do.
    CloseSpace(String),
    /// Open the rename prompt for a space. Intercepted by `input` like
    /// [`Command::PickAgent`] — it asks the user, not the server.
    RenameSpace {
        workspace_id: String,
    },
    /// Open the color picker for a space. Also intercepted.
    PickSpaceColor {
        workspace_id: String,
    },
    /// Open the rename prompt for a pane. Intercepted, like its space
    /// counterpart.
    RenamePane {
        pane_id: String,
    },
    /// Open the color picker for a pane. Intercepted.
    PickPaneColor {
        pane_id: String,
    },
    /// Set a pane's title or color (`pane.update`).
    ///
    /// An **empty** `title` is meaningful, not absent: it clears the user's
    /// name and hands the pane back to whatever its shell calls itself, which
    /// is the only way to undo a rename. That is why each field is its own
    /// `Option` rather than one "new value".
    UpdatePane {
        pane_id: String,
        title: Option<String>,
        color: Option<String>,
    },
    /// Set a space's name or color (`workspace.update`, which takes either or
    /// both). One command for both because it is one method, and because a
    /// future "rename and recolor in one step" would otherwise need a third.
    UpdateSpace {
        workspace_id: String,
        name: Option<String>,
        color: Option<String>,
    },
    CreateWorkspace {
        name: String,
    },
    NextPane,
    PrevPane,
    Detach,
    /// Show a second pane beside the focused one, or hide it again — see
    /// `main.rs::toggle_split`. Offered by [`candidates`] as well as by the
    /// `ctrl+space s` chord.
    ///
    /// It was chord-only at first, on the reasoning that "every entry that list
    /// grows shifts every index below it, and the palette's own tests pin
    /// specific positions". That was a test-maintenance concern deciding a
    /// product question, and it left a headline feature with no discoverable
    /// path at all: absent from the palette, absent from the status-bar hint,
    /// and absent from every document in the repo. `Detach` has a chord too and
    /// is listed here. The tests that pinned positions now match on label.
    ToggleSplit,
    /// Hand mouse reporting back to the terminal so its own click-drag
    /// selection works, or take it back again — see
    /// [`crate::model::Model::mouse_captured`].
    ///
    /// Needs no connection, so `run` acts on it directly rather than routing
    /// it through `handle_command`, the same way `Detach` is handled: a user
    /// reaching for this is often reaching for it *because* something has
    /// gone wrong and they want to copy the error out.
    ToggleMouse,
    /// Flip the agents panel between `grouped` and `priority` order — what
    /// clicking that panel's own header label does, for the keyboard: offered
    /// by [`candidates`] and by the `ctrl+space o` chord.
    ///
    /// Local presentation only, so like [`Command::ToggleMouse`] it is applied
    /// by `run` before `decide` and works while disconnected. See
    /// [`crate::model::Model::toggle_agent_sort`].
    ToggleAgentSort,
    /// Hide the sidebar so the panes take the full width, or show it again —
    /// offered by [`candidates`] and by the `ctrl+space b` chord. Local, like
    /// [`Command::ToggleAgentSort`]; see
    /// [`crate::model::SidebarState::toggle_collapsed`].
    ToggleSidebar,
    /// Create a pane whose session choice has already been made.
    ///
    /// Distinct from [`Command::CreatePane`], which is what *triggers* the
    /// session lookup. This one carries the answer and skips it, so choosing
    /// "new session" cannot loop back into being asked again.
    CreatePaneWithSession {
        workspace_id: String,
        agent_type: String,
        cwd: Option<String>,
        /// `None` starts fresh, which is exactly today's behaviour.
        session: Option<String>,
    },
}

/// Builds the full candidate list for `query`, then filters it down to a
/// case-insensitive substring match on each label — except the trailing
/// workspace-creation entry, which is appended *after* that filter and so
/// is never itself subject to it (see below).
///
/// One `FocusPane` and one `ClosePane` entry is produced per *live* pane —
/// a dead pane (the sidebar already excludes these from the pane list)
/// gets neither, since focusing or closing a pane that's already gone is
/// not a real action. One `CreatePane` entry is produced per workspace in
/// `model.local_workspaces()`. `NextPane`, `PrevPane`, and `Detach` need no
/// model-derived data, so they are always offered.
///
/// Last, whenever `query.trim()` is non-empty, a `CreateWorkspace` entry
/// offering to create a workspace named after the trimmed query. It is
/// deliberately appended *after* the substring filter above rather than
/// folded into the same filtered list: a raw query like `"  spaced  "`
/// would not itself contain-match a label built from the *trimmed* name,
/// and this entry must be offered regardless. Deliberately always last:
/// `route`'s `Enter` fires `candidates(..)[selected]`, `selected` defaults
/// to 0, and a live pane whose label happens to match the query is almost
/// always what the user actually meant — putting workspace creation first
/// would let `Enter` fire on it by accident.
pub fn candidates(model: &Model, query: &str) -> Vec<(String, Command)> {
    // The commands that are always there, whatever is running.
    let mut globals: Vec<(String, Command)> = vec![
        ("Next pane".to_string(), Command::NextPane),
        ("Previous pane".to_string(), Command::PrevPane),
    ];
    // A new pane where you are is the menu's most common use; the other
    // spaces' entries follow the panes below.
    let active = model.active_space_id().map(str::to_string);
    let new_pane = |ws: &spaces_proto::WorkspaceInfo| {
        (
            format!("New pane in {}\u{2026}", ws.name),
            Command::PickAgent {
                workspace_id: ws.id.clone(),
            },
        )
    };
    if let Some(ws) = model
        .local_workspaces()
        .iter()
        .find(|w| Some(&w.id) == active.as_ref())
    {
        globals.push(new_pane(ws));
    }
    globals.extend([(
        "Split the view / unsplit it".to_string(),
        Command::ToggleSplit,
    )]);
    if !model.local_workspaces().is_empty() {
        globals.push(("Browse files".to_string(), Command::OpenFiles));
    }
    globals.push(("Detach".to_string(), Command::Detach));
    globals.push((
        format!(
            "Select text to copy / restore clicking \u{2014} {}",
            if model.mouse_captured {
                "mouse on"
            } else {
                "mouse off"
            }
        ),
        Command::ToggleMouse,
    ));
    globals.push((
        if model.sidebar.is_collapsed(model.sidebar.screen_width) {
            "Show the sidebar"
        } else {
            "Hide the sidebar"
        }
        .to_string(),
        Command::ToggleSidebar,
    ));
    globals.push((
        format!(
            "Sort agents: {} \u{2192} {}",
            model.agent_sort.label(),
            model.agent_sort.toggled().label()
        ),
        Command::ToggleAgentSort,
    ));

    // One entry or two per pane and per space: a dozen panes make dozens.
    let mut items: Vec<(String, Command)> = Vec::new();
    for pane in model.local_panes().iter().filter(|p| p.live) {
        let state = display_state(pane).label();
        items.push((
            format!("Focus {} \u{2014} {state}", pane.agent_type),
            Command::FocusPane(pane.id.clone()),
        ));
        items.push((
            format!("Close {} \u{2014} {state}", pane.agent_type),
            Command::ClosePane(pane.id.clone()),
        ));
    }
    // Stopped panes come back from here one at a time; the sidebar restarts
    // a whole space only when nothing in it is running.
    for pane in model.local_panes().iter().filter(|p| !p.live) {
        items.push((
            format!("Restart {} \u{2014} {}", pane.agent_type, pane.cwd),
            Command::RestartPanes(vec![pane.id.clone()]),
        ));
    }
    for ws in model
        .local_workspaces()
        .iter()
        .filter(|w| Some(&w.id) != active.as_ref())
    {
        items.push(new_pane(ws));
    }

    // Opened with nothing typed, this is the menu: the commands first, so
    // Detach and the rest stay in view however many panes are running.
    // Typing searches, and then a pane is the likeliest target, so panes
    // lead and enter on a typed name focuses it.
    let mut out = if query.trim().is_empty() {
        globals.extend(items);
        globals
    } else {
        items.extend(globals);
        items
    };

    let lowered = query.to_lowercase();
    out.retain(|(label, _)| label.to_lowercase().contains(&lowered));

    let trimmed = query.trim();
    if !trimmed.is_empty() {
        out.push((
            format!("Create space \"{trimmed}\""),
            Command::CreateWorkspace {
                name: trimmed.to_string(),
            },
        ));
    }

    out
}

/// The candidate list for the palette's *current mode* — the one list
/// `route`'s `Enter`/`Down` and [`render_palette`] all resolve against, so
/// what `enter` fires is always what the user is looking at.
///
/// Ordinarily that is [`candidates`]. While [`Palette::naming_space`] is
/// set it is instead the single "create a workspace by this name" entry, or
/// nothing at all for a query that is empty or all whitespace — an empty
/// list is what makes `enter` on an unnamed workspace a no-op that just
/// closes the prompt, rather than a request the server would reject.
pub fn visible_candidates(model: &Model, palette: &Palette) -> Vec<(String, Command)> {
    if let Some(pane_id) = palette.picking_pane_color.as_deref() {
        let current = model
            .local_panes()
            .iter()
            .find(|p| p.id == pane_id)
            .and_then(|p| p.color.clone());
        return swatches(current.as_deref(), &palette.query, &mut |hex| {
            Command::UpdatePane {
                pane_id: pane_id.to_string(),
                title: None,
                color: Some(hex.to_string()),
            }
        });
    }
    if let Some(pane_id) = palette.renaming_pane.as_deref() {
        // No emptiness guard, unlike the space prompt: clearing a pane's name
        // is how you hand it back to its shell-set title, and `pane.update`
        // accepts an empty string for exactly that.
        let trimmed = palette.query.trim();
        let label = if trimmed.is_empty() {
            "Clear the name (use the shell's own)".to_string()
        } else {
            format!("Rename to \"{trimmed}\"")
        };
        return vec![(
            label,
            Command::UpdatePane {
                pane_id: pane_id.to_string(),
                title: Some(trimmed.to_string()),
                color: None,
            },
        )];
    }
    if let Some(workspace_id) = palette.picking_color.as_deref() {
        return color_candidates(model, workspace_id, &palette.query);
    }
    if let Some(workspace_id) = palette.renaming_space.as_deref() {
        return rename_candidates(workspace_id, &palette.query);
    }
    if let Some((workspace_id, agent_type)) = palette.asking_cwd.as_ref() {
        if let Some(listing) = palette.browse.as_ref() {
            return browse_candidates(workspace_id, agent_type, listing, &palette.query);
        }
        return cwd_candidates(workspace_id, agent_type, &palette.query);
    }
    if let Some(pending) = palette.picking_session.as_ref() {
        return session_candidates(pending, &palette.query);
    }
    if let Some(workspace_id) = palette.picking_agent.as_deref() {
        return agent_candidates(model, workspace_id, &palette.query);
    }
    if palette.naming_space {
        return space_name_candidates(&palette.query);
    }
    candidates(model, &palette.query)
}

/// The session picker's list: `new session` first, then one row per session.
///
/// `new session` leads because starting fresh is the safe choice and the one a
/// blind `enter` should land on — resuming the wrong conversation is a worse
/// mistake than starting one you did not need.
///
/// Each row carries the date and message count beside the label, which is what
/// keeps a row identifiable when the label itself is poor: `first_prompt` is
/// truncated where it is stored, so some sessions genuinely have no
/// description and arrive labelled `unnamed`.
fn session_candidates(pending: &PendingPane, query: &str) -> Vec<(String, Command)> {
    let fresh = |session: Option<String>| Command::CreatePaneWithSession {
        workspace_id: pending.workspace_id.clone(),
        agent_type: pending.agent_type.clone(),
        cwd: pending.cwd.clone(),
        session,
    };
    let lowered = query.trim().to_lowercase();
    let mut out = vec![("new session".to_string(), fresh(None))];
    for s in &pending.sessions {
        let day = s.modified.as_deref().unwrap_or("");
        let label = format!(
            "{}  {:>5} msg  {}",
            if day.len() >= 10 {
                &day[..10]
            } else {
                "          "
            },
            s.message_count,
            s.label
        );
        if !lowered.is_empty() && !label.to_lowercase().contains(&lowered) {
            continue;
        }
        out.push((label, fresh(Some(s.session_id.clone()))));
    }
    out
}

/// The agent picker's list: one [`Command::CreatePane`] per entry in
/// [`AGENT_TYPES`], filtered by a case-insensitive substring match so typing
/// `cl` and pressing enter lands on `claude`.
///
/// Never empty for an empty query, unlike the space-naming prompt — there is
/// nothing for the user to supply here, only to choose from, so the list is
/// immediately actionable and `selected` starting at 0 picks `claude`, the
/// agent this client is most used with.
fn agent_candidates(model: &Model, workspace_id: &str, query: &str) -> Vec<(String, Command)> {
    // A space with a pane to inherit a directory from creates straight away;
    // one without has to ask, so its entries carry the trailing ellipsis that
    // means "opens something" (the same convention "New pane in X…" uses).
    let ask = needs_cwd(model, workspace_id);
    let lowered = query.to_lowercase();
    let files = "files".contains(&lowered).then(|| {
        (
            "files \u{2014} browse and view this machine's files".to_string(),
            Command::OpenFiles,
        )
    });
    AGENT_TYPES
        .iter()
        .filter(|agent| agent.to_lowercase().contains(&lowered))
        .map(|agent| {
            let label = if ask {
                format!("{agent}\u{2026}")
            } else {
                (*agent).to_string()
            };
            let command = if ask {
                Command::AskPaneCwd {
                    workspace_id: workspace_id.to_string(),
                    agent_type: (*agent).to_string(),
                }
            } else {
                Command::CreatePane {
                    workspace_id: workspace_id.to_string(),
                    agent_type: (*agent).to_string(),
                    cwd: None,
                }
            };
            (label, command)
        })
        .chain(files)
        .collect()
}

/// The directory prompt's list: one entry, creating the pane in the typed
/// path. Empty until something is typed, like the space-naming prompt — an
/// empty path is not a directory, and `enter` on one must be a no-op rather
/// than a request the runtime resolves to somewhere arbitrary.
///
/// The path is trimmed but otherwise passed through untouched. No attempt is
/// made to validate or expand it: this client has no view of the *server's*
/// filesystem (they are routinely different machines — see `--remote`), so any
/// check here would be answering the wrong question. A path the server cannot
/// use comes back as a `pane.create` error.
fn cwd_candidates(workspace_id: &str, agent_type: &str, query: &str) -> Vec<(String, Command)> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let path = normalize_bare_drive(trimmed);
    vec![(
        format!("Start {agent_type} in \"{path}\""),
        Command::CreatePane {
            workspace_id: workspace_id.to_string(),
            agent_type: agent_type.to_string(),
            cwd: Some(path),
        },
    )]
}

/// The directory browser's list.
///
/// With nothing typed: "Start here" first, so `enter` on a folder you have
/// just opened starts the pane in it; then the way up, the other dev
/// directories when at the top of one, and the subfolders. Typing filters the
/// subfolders and puts them first, so `web` then `enter` opens `Webshop`.
/// Typing something that looks like a path switches to the typed prompt's
/// single entry, so a path known by heart still works.
fn browse_candidates(
    workspace_id: &str,
    agent_type: &str,
    listing: &FolderListing,
    query: &str,
) -> Vec<(String, Command)> {
    let q = query.trim();
    if looks_like_path(q) {
        return cwd_candidates(workspace_id, agent_type, q);
    }
    let open = |path: &str| Command::BrowseFolder {
        workspace_id: workspace_id.to_string(),
        agent_type: agent_type.to_string(),
        path: Some(path.to_string()),
    };
    let start = (
        format!("Start {agent_type} here \u{2014} {}", listing.current),
        Command::CreatePane {
            workspace_id: workspace_id.to_string(),
            agent_type: agent_type.to_string(),
            cwd: Some(listing.current.clone()),
        },
    );
    let lowered = q.to_lowercase();
    let folders = listing
        .folders
        .iter()
        .filter(|(name, _)| name.to_lowercase().contains(&lowered))
        .map(|(name, path)| (format!("{name}/"), open(path)));
    if !q.is_empty() {
        let mut out: Vec<_> = folders.collect();
        out.push(start);
        return out;
    }
    let mut out = vec![start];
    match listing.parent.as_deref() {
        Some(parent) => out.push(("../".to_string(), open(parent))),
        None => out.extend(
            listing
                .roots
                .iter()
                .filter(|root| **root != listing.current)
                .map(|root| (format!("{root}/"), open(root))),
        ),
    }
    out.extend(folders);
    out
}

/// `true` for text that names a path rather than filters folder names.
fn looks_like_path(q: &str) -> bool {
    q.starts_with('/')
        || q.starts_with('~')
        || q.starts_with('\\')
        || q.as_bytes().get(1) == Some(&b':')
}

/// The rename prompt's list: one entry, setting the space's name to the typed
/// text. Empty for an empty or all-whitespace query — the runtime rejects a
/// blank name outright (`workspace.update`: "name must be a non-empty
/// string"), so offering it would be offering a guaranteed error.
fn rename_candidates(workspace_id: &str, query: &str) -> Vec<(String, Command)> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    vec![(
        format!("Rename to \"{trimmed}\""),
        Command::UpdateSpace {
            workspace_id: workspace_id.to_string(),
            name: Some(trimmed.to_string()),
            color: None,
        },
    )]
}

/// The color picker's list: every entry in [`super::sidebar::COLORS`], each
/// labelled with its hex value and marked if it is the space's current one.
///
/// Filtered by substring on the hex, so typing `f4` narrows it — not much use,
/// but it costs nothing and makes the prompt behave like every other one here
/// rather than being the odd list that ignores what you type.
///
/// The swatch is the label: a terminal cannot draw a color chip, so each row
/// is the hex string itself, *rendered in that color* by
/// [`render_palette`]. On a styling-stripped terminal the hex is still the
/// answer, which is why it is text rather than a block character.
fn color_candidates(model: &Model, workspace_id: &str, query: &str) -> Vec<(String, Command)> {
    let current = model
        .local_workspaces()
        .iter()
        .find(|w| w.id == workspace_id)
        .and_then(|w| w.color.clone());
    swatches(current.as_deref(), query, &mut |hex| Command::UpdateSpace {
        workspace_id: workspace_id.to_string(),
        name: None,
        color: Some(hex.to_string()),
    })
}

/// The color list both pickers draw: every entry in
/// [`super::sidebar::COLORS`], filtered by substring on the hex, each marked
/// if it is `current`, and each paired with whatever command `command` builds
/// for it.
///
/// Shared by the space and pane pickers so the two can never offer different
/// palettes or mark "current" differently — the only thing that differs
/// between them is which method the choice resolves to.
fn swatches(
    current: Option<&str>,
    query: &str,
    command: &mut dyn FnMut(&str) -> Command,
) -> Vec<(String, Command)> {
    let lowered = query.to_lowercase();
    super::sidebar::COLORS
        .iter()
        .filter(|hex| hex.to_lowercase().contains(&lowered))
        .map(|hex| {
            let mark = if current == Some(*hex) {
                " (current)"
            } else {
                ""
            };
            (format!("{hex}{mark}"), command(hex))
        })
        .collect()
}

/// Turns a bare Windows drive specifier into that drive's root: `C:` becomes
/// `C:\`. Everything else is returned unchanged.
///
/// The one normalisation this prompt does, and it earns its place. On Windows
/// `C:` is *drive-relative*: it means "the current directory on drive C",
/// which is wherever the server process happens to have been started — so
/// typing `C:` and pressing enter silently starts the agent in the server's
/// own directory rather than at the root, with nothing on screen saying so.
/// Nobody types a bare drive meaning that.
///
/// The cost is that a POSIX server with a directory literally named `C:` —
/// legal, and vanishingly unlikely — could not be reached by typing its exact
/// name. That trade is worth it: this client cannot see the server's
/// filesystem (they are routinely different machines), so it cannot
/// distinguish the two cases, and one of them is a real footgun while the
/// other is a curiosity. No other normalisation is attempted for the same
/// reason — no `~` expansion, no relative-path resolution, no existence
/// check; those would all be answering a question about the wrong machine.
fn normalize_bare_drive(path: &str) -> String {
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next()) {
        (Some(letter), Some(':'), None) if letter.is_ascii_alphabetic() => {
            format!("{letter}:\\")
        }
        _ => path.to_string(),
    }
}

/// the space-naming prompt's entire candidate list: one entry, or none.
/// The label and the trimming match [`candidates`]' own trailing
/// create-workspace entry exactly — the same action reached two ways must
/// not read as two different things, and must not disagree about what
/// `"  foo  "` names.
fn space_name_candidates(query: &str) -> Vec<(String, Command)> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    vec![(
        format!("Create space \"{trimmed}\""),
        Command::CreateWorkspace {
            name: trimmed.to_string(),
        },
    )]
}

/// Index of the first candidate to draw, so `selected` stays visible in a
/// window `visible` rows tall. Pure and saturating throughout — `selected`
/// is a `usize` that `route`'s `Down` handler only clamps against the
/// candidate list *as it was when the key was pressed*, so it can legally
/// be anything at all (including `usize::MAX`) by the time this runs.
fn scroll_offset(selected: usize, total: usize, visible: usize) -> usize {
    if visible == 0 {
        return 0;
    }
    let max_first = total.saturating_sub(visible);
    selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(max_first)
}

/// A `width` x `height` rect centered inside `area`. Callers pass
/// dimensions already clamped to `area`'s own; the saturating arithmetic
/// here means a caller that doesn't still gets a degenerate-but-valid rect
/// rather than an underflow.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area
        .y
        .saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width, height)
}

/// Where the palette's parts sit for one frame: the box itself, and one rect
/// per candidate row paired with that candidate's index into
/// [`visible_candidates`].
///
/// Computed by [`palette_layout`] and consumed by both [`render_palette`] and
/// the mouse router — the same discipline [`super::sidebar::sidebar_layout`]
/// and [`super::tabs::tab_bar`] follow, and the reason a click on a candidate
/// can be resolved at all. Before this existed the overlay was
/// keyboard-only: every click while it was open simply dismissed it, so
/// clicking `New pane in Default…` did nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteLayout {
    /// The whole box, borders included. A click inside it but not on a row
    /// (the query line, a border) must not be treated as a click *outside*
    /// the modal, which is what dismisses it.
    pub outer: Rect,
    /// [`PaletteLayout::outer`] minus its borders — the query line and the
    /// candidate rows live here.
    pub inner: Rect,
    /// Whether there was room to draw a border (and therefore a title).
    pub bordered: bool,
    /// `(row rect, index into `visible_candidates`)`, top to bottom.
    pub rows: Vec<(Rect, usize)>,
}

/// Lays the overlay out inside `area`, or `None` when the palette is closed
/// or `area` is too small to draw anything into.
///
/// Pure, and the single source of truth for the overlay's geometry.
/// `selected` is only ever *clamped* by `route`, never guaranteed in range
/// against the list this rebuilds, so every dimension is checked and every
/// index is `.get`/`skip`/`take` rather than `[]`.
pub fn palette_layout(model: &Model, palette: &Palette, area: Rect) -> Option<PaletteLayout> {
    if !palette.open || area.width == 0 || area.height == 0 {
        return None;
    }
    let items = visible_candidates(model, palette);
    let bordered = area.width >= 4 && area.height >= 3;
    let chrome = if bordered { 2 } else { 0 };
    // `visible` must be capped by the rows the box will *actually* have for
    // candidates — `area.height` minus the borders minus the query line —
    // and not only by `MAX_VISIBLE` and the candidate count.
    //
    // Without this third cap, `scroll_offset` positions its window as though
    // all `visible` rows exist while `Paragraph` silently clips the ones past
    // `inner.height`, so the selected row could be *outside the drawn box*: 19
    // candidates on a 13-row terminal (a 12-row pane area, 9 candidate rows)
    // with `selected = 9` produced `first = 0` and put the marker on a clipped
    // line. The user saw nine unmarked candidates and `enter` fired one that
    // was never on screen.
    let rows_for_candidates = usize::from(area.height.saturating_sub(chrome + 1));
    let visible = items.len().min(MAX_VISIBLE).min(rows_for_candidates);
    // One row for the query line, plus one per visible candidate. `visible`
    // is capped at MAX_VISIBLE, so the cast can't truncate.
    let content_height = (visible as u16).saturating_add(1);
    let width = area.width.min(MAX_WIDTH);
    let height = content_height.saturating_add(chrome).min(area.height);
    let outer = centered(area, width, height);
    let inner = if bordered {
        Block::default().borders(Borders::ALL).inner(outer)
    } else {
        outer
    };
    if inner.width == 0 || inner.height == 0 {
        return None;
    }

    // Row 0 of `inner` is the query line; candidates start below it, in the
    // same order and from the same `scroll_offset` the renderer draws them.
    let first = scroll_offset(palette.selected, items.len(), visible);
    let rows = (first..items.len())
        .take(visible)
        .enumerate()
        .filter_map(|(offset, index)| {
            let y = inner.y.checked_add(1)?.checked_add(offset as u16)?;
            (y < inner.y.saturating_add(inner.height))
                .then(|| (Rect::new(inner.x, y, inner.width, 1), index))
        })
        .collect();
    Some(PaletteLayout {
        outer,
        inner,
        bordered,
        rows,
    })
}

/// The candidate index a click at `(column, row)` landed on, if any.
pub fn candidate_at(layout: &PaletteLayout, column: u16, row: u16) -> Option<usize> {
    layout
        .rows
        .iter()
        .find(|(rect, _)| hit(*rect, column, row))
        .map(|(_, index)| *index)
}

/// `true` iff the click landed anywhere inside the overlay box — used to tell
/// "clicked in the modal" from "clicked outside it, dismiss".
pub fn contains(layout: &PaletteLayout, column: u16, row: u16) -> bool {
    hit(layout.outer, column, row)
}

fn hit(rect: Rect, column: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Draws the palette overlay centered in `area` (the pane region — see
/// [`super::frame::render`]) when `palette.open`, and returns where the
/// text cursor belongs on the query line so the caller can place a real
/// one. A closed palette draws nothing at all and returns `None`.
///
/// The overlay is opaque: [`Clear`] wipes whatever pane output was
/// underneath before the box is drawn, so the candidate list is readable
/// over a busy pane. Borders are only drawn when there is room for them
/// (a pane area under 4x3 gets the bare content instead), and the whole
/// rect is intersected with `buf`'s own area first, so no write here can
/// land out of bounds however small the terminal is.
///
/// Geometry comes from [`palette_layout`], so the rows the mouse router
/// hit-tests are the rows that were actually drawn.
pub fn render_palette(
    model: &Model,
    palette: &Palette,
    area: Rect,
    buf: &mut Buffer,
) -> Option<Position> {
    let clipped = area.intersection(buf.area);
    let layout = palette_layout(model, palette, clipped)?;
    let items = visible_candidates(model, palette);

    Clear.render(layout.outer, buf);
    if layout.bordered {
        // Same precedence `visible_candidates` resolves the list with, so the
        // title always names the mode the entries came from.
        let title: std::borrow::Cow<'_, str> =
            if palette.picking_color.is_some() || palette.picking_pane_color.is_some() {
                COLOR_TITLE.into()
            } else if palette.renaming_pane.is_some() {
                PANE_RENAME_TITLE.into()
            } else if palette.renaming_space.is_some() {
                RENAME_TITLE.into()
            } else if palette.asking_cwd.is_some() && palette.browse.is_some() {
                BROWSE_TITLE.into()
            } else if palette.asking_cwd.is_some() {
                CWD_TITLE.into()
            } else if palette.picking_session.is_some() {
                SESSION_TITLE.into()
            } else if palette.picking_agent.is_some() {
                PICKING_TITLE.into()
            } else if palette.naming_space {
                // The only title that depends on model state — see
                // `naming_title`.
                naming_title(model).into()
            } else {
                TITLE.into()
            };
        Block::default()
            .borders(Borders::ALL)
            .title(title.as_ref())
            .render(layout.outer, buf);
    }
    let inner = layout.inner;

    Paragraph::new(Line::raw(format!("{QUERY_PREFIX}{}", palette.query)))
        .render(Rect::new(inner.x, inner.y, inner.width, 1), buf);

    // One `Paragraph` per row rather than one for the whole box, so the rows
    // drawn are exactly the rects `candidate_at` hit-tests — a single
    // multi-line `Paragraph` would let `ratatui` clip a line the layout
    // believed was on screen, which is how a marker once landed on a row the
    // user could not see.
    for (rect, index) in &layout.rows {
        let Some((label, _)) = items.get(*index) else {
            continue;
        };
        // A color entry is drawn *in* its color — a terminal cannot show a
        // swatch, and a list of sixteen hex strings in one color is
        // unreadable as a picker. The hex text is still there, which is what
        // survives a styling-stripped terminal.
        let swatch = (palette.picking_color.is_some() || palette.picking_pane_color.is_some())
            .then(|| super::sidebar::accent(label.split_whitespace().next()))
            .flatten();
        let line = if *index == palette.selected {
            let mut style = Style::new().add_modifier(Modifier::REVERSED);
            if let Some(color) = swatch {
                style = style.fg(color);
            }
            Line::styled(format!("{SELECTION_MARKER} {label}"), style)
        } else if let Some(color) = swatch {
            Line::styled(format!("  {label}"), Style::new().fg(color))
        } else {
            Line::raw(format!("  {label}"))
        };
        Paragraph::new(line).render(*rect, buf);
    }

    // The cursor sits just past the query text, clamped inside the box —
    // `query` is user-typed and unbounded, the box is not.
    let column = u16::try_from(QUERY_PREFIX.chars().count() + palette.query.chars().count())
        .unwrap_or(u16::MAX)
        .min(inner.width.saturating_sub(1));
    Some(Position {
        x: inner.x.saturating_add(column),
        y: inner.y,
    })
}

#[cfg(test)]
// Same convention as `model.rs`'s test module: build a `Model::default()`
// and assign just the field(s) each case cares about, which reads clearer
// here than clippy's suggested single struct-literal-with-`..Default::default()`.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    /// `state` is a wire-format string ("working"/"idle"/...), not the
    /// closed `AgentState` enum (other helpers take the enum directly; the
    /// tests in this module pass strings, so this helper parses them the
    /// same way `model.rs::apply_pane_state` does). `live` controls whether the
    /// pane is eligible for `FocusPane`/`ClosePane` candidates at all.
    fn pane_named(
        id: &str,
        workspace: &str,
        agent_type: &str,
        state: &str,
        live: bool,
    ) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(workspace.into()),
            agent_type: agent_type.into(),
            agent_state: state.parse().unwrap_or(AgentState::Unknown),
            seen: true,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live,
            color: None,
        }
    }
    use spaces_proto::{AgentState, PaneInfo, WorkspaceInfo};

    // --- the session picker ---

    fn summary(
        id: &str,
        label: &str,
        msgs: u64,
        modified: Option<&str>,
    ) -> spaces_proto::SessionSummary {
        spaces_proto::SessionSummary {
            session_id: id.into(),
            label: label.into(),
            message_count: msgs,
            modified: modified.map(str::to_string),
            git_branch: None,
            agent_type: "claude".into(),
        }
    }

    fn pending(sessions: Vec<spaces_proto::SessionSummary>) -> Palette {
        Palette {
            open: true,
            picking_session: Some(PendingPane {
                workspace_id: "w1".into(),
                agent_type: "claude".into(),
                cwd: Some("/work/api".into()),
                sessions,
            }),
            ..Palette::default()
        }
    }

    #[test]
    fn starting_fresh_leads_the_list() {
        // A blind enter must not resume the wrong conversation: resuming
        // something you did not mean to is a worse mistake than starting one
        // you did not need.
        let m = Model::new();
        let p = pending(vec![summary(
            "s1",
            "fix the parser",
            12,
            Some("2026-08-13T10:00:00Z"),
        )]);
        let rows = visible_candidates(&m, &p);
        assert_eq!(rows[0].0, "new session");
        assert_eq!(
            rows[0].1,
            Command::CreatePaneWithSession {
                workspace_id: "w1".into(),
                agent_type: "claude".into(),
                cwd: Some("/work/api".into()),
                session: None,
            }
        );
    }

    #[test]
    fn a_session_row_carries_its_id_and_says_enough_to_identify_it() {
        let m = Model::new();
        let p = pending(vec![summary(
            "abc123",
            "fix the parser",
            12,
            Some("2026-08-13T10:00:00Z"),
        )]);
        let rows = visible_candidates(&m, &p);

        assert!(rows[1].0.contains("2026-08-13"), "the date: {}", rows[1].0);
        assert!(rows[1].0.contains("12"), "the message count: {}", rows[1].0);
        assert!(
            rows[1].0.contains("fix the parser"),
            "the label: {}",
            rows[1].0
        );
        match &rows[1].1 {
            Command::CreatePaneWithSession { session, .. } => {
                assert_eq!(session.as_deref(), Some("abc123"));
            }
            other => panic!("expected a session create, got {other:?}"),
        }
    }

    #[test]
    fn a_row_with_no_date_still_lines_up() {
        // `modified` is nullable on the wire, and a ragged column is harder to
        // scan than a blank one.
        let m = Model::new();
        let p = pending(vec![
            summary("a", "with a date", 1, Some("2026-08-13T10:00:00Z")),
            summary("b", "without one", 2, None),
        ]);
        let rows = visible_candidates(&m, &p);
        let dated = rows[1].0.find("with a date").unwrap();
        let undated = rows[2].0.find("without one").unwrap();
        assert_eq!(dated, undated, "labels start in the same column");
    }

    #[test]
    fn the_list_can_be_filtered_without_losing_the_fresh_option() {
        // Typing narrows the sessions, but starting fresh must stay reachable
        // — it is the escape hatch from a list you cannot find anything in.
        let m = Model::new();
        let mut p = pending(vec![
            summary("a", "fix the parser", 1, None),
            summary("b", "add federation", 2, None),
        ]);
        p.query = "parser".into();
        let rows = visible_candidates(&m, &p);
        assert_eq!(rows[0].0, "new session");
        assert_eq!(rows.len(), 2);
        assert!(rows[1].0.contains("fix the parser"));
    }

    #[test]
    fn the_title_does_not_claim_the_list_is_complete() {
        // It comes from an index the web app maintains, so a session started
        // elsewhere since the last index will not be in it.
        assert_eq!(SESSION_TITLE.trim(), "resume or start fresh");
    }

    #[test]
    fn candidates_include_focus_entries_for_live_panes() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let c = candidates(&m, "");
        assert!(c.iter().any(|(label, cmd)| label.contains("claude")
            && matches!(cmd, Command::FocusPane(id) if id == "p1")));
    }

    #[test]
    fn the_query_filters_case_insensitively() {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            pane_named("p1", "w1", "claude", "working", true),
            pane_named("p2", "w1", "codex", "idle", true),
        ];
        let c = candidates(&m, "CLA");
        assert!(c.iter().any(|(l, _)| l.contains("claude")));
        assert!(!c.iter().any(|(l, _)| l.contains("codex")));
    }

    #[test]
    fn detach_is_always_offered() {
        let m = Model::default();
        assert!(candidates(&m, "det")
            .iter()
            .any(|(_, c)| matches!(c, Command::Detach)));
    }

    #[test]
    fn candidates_include_close_entries_for_live_panes() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let c = candidates(&m, "");
        assert!(c.iter().any(|(label, cmd)| label.contains("claude")
            && matches!(cmd, Command::ClosePane(id) if id == "p1")));
    }

    #[test]
    fn candidates_offer_a_new_pane_per_space_and_it_opens_the_agent_picker() {
        // Not `CreatePane` directly: a pane needs an agent, and the runtime
        // defaults a missing `agentType` to `shell` — so the
        // straight-to-create version of this entry produced agent-less
        // shells.
        let mut m = Model::default();
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "frontend".into(),
            color: None,
            pane_ids: vec![],
        }];
        let c = candidates(&m, "");
        assert!(
            c.iter().any(|(label, cmd)| {
                label.contains("frontend")
                    && matches!(cmd, Command::PickAgent { workspace_id } if workspace_id == "w1")
            }),
            "{c:?}"
        );
        assert!(
            !c.iter()
                .any(|(_, cmd)| matches!(cmd, Command::CreatePane { .. })),
            "no entry may create a pane without an agent: {c:?}"
        );
    }

    // --- The agent picker. Every pane this client created before it existed
    // was a bare shell, because no `agentType` was ever sent. ---

    fn picking(space: &str, query: &str) -> Palette {
        Palette {
            open: true,
            picking_agent: Some(space.to_string()),
            query: query.to_string(),
            ..Palette::default()
        }
    }

    /// A model whose space `w1` already has a pane with a directory — so the
    /// picker creates straight away rather than asking where to start. This is
    /// the ordinary case: only a space's *first* pane needs the prompt.
    fn space_with_a_directory() -> Model {
        let mut m = Model::new();
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec![],
        }];
        let mut pane = pane_named("p0", "w1", "claude", "working", true);
        pane.cwd = "/work/api".into();
        m.local_mut().panes = vec![pane];
        m
    }

    #[test]
    fn a_spaces_first_pane_is_asked_where_to_start_and_later_ones_are_not() {
        // A prompt you answer the same way every time trains you to ignore it,
        // so it appears only where inheritance cannot cover: the first pane.
        let empty = Model::new();
        assert!(needs_cwd(&empty, "w1"));
        let c = visible_candidates(&empty, &picking("w1", ""));
        assert!(
            matches!(&c[0].1, Command::AskPaneCwd { workspace_id, agent_type }
                if workspace_id == "w1" && agent_type == "claude"),
            "{c:?}"
        );
        assert!(c[0].0.ends_with('\u{2026}'), "and says it opens something");

        let populated = space_with_a_directory();
        assert!(!needs_cwd(&populated, "w1"));
        let c = visible_candidates(&populated, &picking("w1", ""));
        assert!(
            matches!(&c[0].1, Command::CreatePane { cwd: None, .. }),
            "inherits instead of asking: {c:?}"
        );
        assert_eq!(
            c[0].0, "claude",
            "no ellipsis when nothing further is asked"
        );
    }

    #[test]
    fn a_sibling_with_no_directory_is_not_something_to_inherit() {
        // `needs_cwd` mirrors `main.rs::inherited_cwd`, which skips empty
        // directories — the two must agree or the prompt is skipped and
        // nothing is inherited either.
        let mut m = space_with_a_directory();
        m.local_mut().panes[0].cwd = String::new();
        assert!(needs_cwd(&m, "w1"));
    }

    #[test]
    fn the_directory_prompt_creates_the_pane_where_it_is_told() {
        let m = Model::new();
        let asking = Palette {
            open: true,
            asking_cwd: Some(("w1".into(), "codex".into())),
            query: "  /srv/app  ".into(),
            ..Palette::default()
        };
        let c = visible_candidates(&m, &asking);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            matches!(&c[0].1, Command::CreatePane { workspace_id, agent_type, cwd }
                if workspace_id == "w1" && agent_type == "codex"
                    && cwd.as_deref() == Some("/srv/app")),
            "trimmed, otherwise untouched: {c:?}"
        );
        assert!(c[0].0.contains("codex"), "{c:?}");
        assert!(c[0].0.contains("/srv/app"), "{c:?}");
    }

    #[test]
    fn a_bare_drive_letter_means_that_drives_root() {
        // 2026-08-12: typing `C:` started the agent in the *server's* own
        // directory. On Windows `C:` is drive-relative — "the current
        // directory on drive C" — and nobody types it meaning that.
        assert_eq!(normalize_bare_drive("C:"), "C:\\");
        assert_eq!(normalize_bare_drive("d:"), "d:\\");
        // Everything else is passed through untouched: this client cannot see
        // the server's filesystem, so any further normalisation would be
        // answering a question about the wrong machine.
        for path in [
            "C:\\",
            "C:\\projects",
            "/c/projects/app",
            "/srv/app",
            "~",
            "..",
            "",
            ":",
            "1:",
            "CC:",
            "C:x",
        ] {
            assert_eq!(normalize_bare_drive(path), path, "{path:?}");
        }
    }

    #[test]
    fn the_prompt_creates_the_pane_at_the_drive_root_not_the_servers_directory() {
        let m = Model::new();
        let asking = Palette {
            open: true,
            asking_cwd: Some(("w1".into(), "claude".into())),
            query: "C:".into(),
            ..Palette::default()
        };
        let c = visible_candidates(&m, &asking);
        assert!(
            matches!(&c[0].1, Command::CreatePane { cwd, .. } if cwd.as_deref() == Some("C:\\")),
            "{c:?}"
        );
        assert!(
            c[0].0.contains("C:\\"),
            "and the label shows where it will actually go: {c:?}"
        );
    }

    #[test]
    fn a_session_with_no_panes_at_all_falls_back_to_the_servers_home() {
        // The first pane of the first space: nothing to inherit from, and the
        // client cannot guess the server's filesystem — it is routinely a
        // different machine. `node.info` reports it, and it is where
        // `pane.create` would put the pane anyway given no `cwd`.
        let mut m = Model::new();
        let local = m.local_id_owned();
        assert_eq!(m.suggested_cwd_on(&local), None, "nothing known yet");
        m.local_mut().home_dir = "C:\\Users\\john".into();
        assert_eq!(
            m.suggested_cwd_on(&local).as_deref(),
            Some("C:\\Users\\john")
        );

        // A live pane still wins: "beside what you are working on" beats the
        // machine's default.
        let mut pane = pane_named("p0", "w1", "claude", "working", true);
        pane.cwd = "/work/api".into();
        m.local_mut().panes = vec![pane];
        assert_eq!(m.suggested_cwd_on(&local).as_deref(), Some("/work/api"));
    }

    #[test]
    fn an_older_runtime_reporting_no_home_offers_no_suggestion() {
        // Rather than substituting a path from *this* machine, which the
        // server may well not have.
        let m = Model::new();
        assert_eq!(m.local().home_dir, "");
        assert_eq!(m.suggested_cwd_on(&m.local_id_owned()), None);
    }

    #[test]
    fn the_last_known_directory_is_what_the_prompt_should_start_from() {
        // The prompt asks about a filesystem this client cannot see, so it
        // starts from a real path rather than a blank line. Any live pane will
        // do — unlike the per-space inheritance, which is deliberately empty
        // for a space's first pane, which is exactly when this is needed.
        let mut m = space_with_a_directory();
        let local = m.local_id_owned();
        assert_eq!(m.suggested_cwd_on(&local).as_deref(), Some("/work/api"));

        let mut newer = pane_named("p1", "w2", "codex", "working", true);
        newer.cwd = "/work/web".into();
        m.local_mut().panes.push(newer);
        assert_eq!(
            m.suggested_cwd_on(&local).as_deref(),
            Some("/work/web"),
            "the most recent one"
        );

        // A dead pane is not somewhere to start, and neither is a blank.
        m.local_mut().panes[1].live = false;
        assert_eq!(
            m.suggested_cwd_on(&m.local_id_owned()).as_deref(),
            Some("/work/api")
        );
        m.local_mut().panes[0].cwd = String::new();
        assert_eq!(m.suggested_cwd_on(&m.local_id_owned()), None);
        let fresh = Model::new();
        assert_eq!(fresh.suggested_cwd_on(&fresh.local_id_owned()), None);
    }

    #[test]
    fn the_directory_prompt_offers_nothing_until_a_path_is_typed() {
        // An empty path is not a directory, and `enter` on one must be a no-op
        // rather than a request the runtime resolves to somewhere arbitrary.
        let m = Model::new();
        for query in ["", "   ", "\t"] {
            let asking = Palette {
                open: true,
                asking_cwd: Some(("w1".into(), "claude".into())),
                query: query.to_string(),
                ..Palette::default()
            };
            assert!(visible_candidates(&m, &asking).is_empty(), "{query:?}");
        }
    }

    #[test]
    fn the_directory_prompt_outranks_the_agent_picker() {
        // Unreachable through `input`, which sets one mode at a time, but the
        // precedence must be defined rather than depending on field order.
        let m = Model::new();
        let both = Palette {
            open: true,
            picking_agent: Some("w9".into()),
            asking_cwd: Some(("w1".into(), "claude".into())),
            query: "/tmp".into(),
            ..Palette::default()
        };
        let c = visible_candidates(&m, &both);
        assert!(
            matches!(&c[0].1, Command::CreatePane { workspace_id, .. } if workspace_id == "w1"),
            "{c:?}"
        );
    }

    #[test]
    fn the_directory_prompt_says_what_it_wants() {
        let m = Model::new();
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        let asking = Palette {
            open: true,
            asking_cwd: Some(("w1".into(), "claude".into())),
            ..Palette::default()
        };
        assert!(render_palette(&m, &asking, area, &mut buf).is_some());
        let out = text(&buf);
        assert!(out.contains("type a directory"), "{out}");
    }

    #[test]
    fn the_picker_offers_every_agent_the_runtime_can_launch() {
        let m = space_with_a_directory();
        let c = visible_candidates(&m, &picking("w1", ""));
        let labels: Vec<&str> = c.iter().map(|(l, _)| l.as_str()).collect();
        let (agents, files) = labels.split_at(AGENT_TYPES.len());
        assert_eq!(agents, AGENT_TYPES, "{c:?}");
        // The Files tab is offered after every agent, as a kind of tab.
        assert!(
            files.len() == 1 && files[0].starts_with("files"),
            "{files:?}"
        );
        assert_eq!(c.last().map(|(_, cmd)| cmd), Some(&Command::OpenFiles));
        for (label, cmd) in &c[..AGENT_TYPES.len()] {
            assert!(
                matches!(cmd, Command::CreatePane { workspace_id, agent_type, .. }
                    if workspace_id == "w1" && agent_type == label),
                "{cmd:?}"
            );
        }
    }

    #[test]
    fn the_picker_never_offers_an_agent_the_runtime_would_silently_ignore() {
        // `custom` is in the runtime's table but launches
        // `params.customCommand`, which this client has nowhere to type — it
        // would create panes that do nothing, the exact defect the picker
        // exists to fix.
        assert!(!AGENT_TYPES.contains(&"custom"), "{AGENT_TYPES:?}");
        // And every name offered must be one the runtime's table knows, or it
        // falls through to `shell` again.
        for agent in AGENT_TYPES {
            assert!(
                ["claude", "codex", "gemini", "aider", "forge", "pi", "shell"].contains(&agent),
                "{agent} is not in spawn-pane.js's AGENTS table"
            );
        }
    }

    #[test]
    fn the_pickers_default_selection_is_immediately_actionable() {
        // Unlike the naming prompt there is nothing for the user to supply,
        // only to choose — so an empty query must still yield a list, with
        // `selected == 0` landing on a real agent rather than on nothing.
        let m = space_with_a_directory();
        let c = visible_candidates(&m, &picking("w1", ""));
        assert!(!c.is_empty());
        assert!(
            matches!(&c[0].1, Command::CreatePane { agent_type, .. } if agent_type == "claude"),
            "{c:?}"
        );
    }

    #[test]
    fn typing_filters_the_picker_case_insensitively() {
        let m = space_with_a_directory();
        for query in ["cl", "CLAUDE", "aud"] {
            let c = visible_candidates(&m, &picking("w1", query));
            assert_eq!(c.len(), 1, "{query:?}: {c:?}");
            assert_eq!(c[0].0, "claude");
        }
        assert!(visible_candidates(&m, &picking("w1", "zzz")).is_empty());
    }

    #[test]
    fn the_picker_creates_into_the_space_it_was_opened_for_not_the_active_one() {
        // The space id is carried on the palette, so selecting a different
        // space in the sidebar while the picker is open cannot redirect the
        // pane.
        let mut m = space_with_a_directory();
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w9".into(),
            name: "elsewhere".into(),
            color: None,
            pane_ids: vec![],
        }];
        m.selected_space = Some("w9".into());
        let c = visible_candidates(&m, &picking("w1", ""));
        assert!(
            matches!(&c[0].1, Command::CreatePane { workspace_id, .. } if workspace_id == "w1"),
            "{c:?}"
        );
    }

    #[test]
    fn the_picker_says_what_it_is_and_lists_agents_not_commands() {
        let m = Model::default();
        let area = Rect::new(0, 0, 60, 14);
        let mut buf = Buffer::empty(area);
        assert!(render_palette(&m, &picking("w1", ""), area, &mut buf).is_some());
        let out = text(&buf);
        assert!(out.contains("pick an agent"), "{out}");
        assert!(out.contains("claude"), "{out}");
        assert!(
            !out.contains("Detach"),
            "the ordinary command list must not be drawn here: {out}"
        );
    }

    #[test]
    fn the_picker_takes_precedence_over_the_naming_prompt() {
        // Both modes at once is unreachable through `input` (which sets them
        // explicitly, never toggles), but the precedence must still be
        // defined rather than depending on field order.
        let m = space_with_a_directory();
        let both = Palette {
            open: true,
            naming_space: true,
            picking_agent: Some("w1".into()),
            query: "claude".into(),
            ..Palette::default()
        };
        let c = visible_candidates(&m, &both);
        assert!(
            matches!(&c[0].1, Command::CreatePane { .. }),
            "the picker wins: {c:?}"
        );
    }

    #[test]
    fn an_empty_query_offers_no_workspace_creation() {
        let m = Model::default();
        assert!(!candidates(&m, "")
            .iter()
            .any(|(_, c)| matches!(c, Command::CreateWorkspace { .. })));
        assert!(!candidates(&m, "   ")
            .iter()
            .any(|(_, c)| matches!(c, Command::CreateWorkspace { .. })));
    }

    #[test]
    fn a_typed_query_offers_creating_a_workspace_by_that_name() {
        let m = Model::default();
        let c = candidates(&m, "frontend");
        let found = c
            .iter()
            .find(|(_, c)| matches!(c, Command::CreateWorkspace { .. }));
        let Some((label, Command::CreateWorkspace { name })) = found else {
            panic!("no create-workspace candidate: {c:?}")
        };
        assert_eq!(name, "frontend");
        assert!(label.contains("frontend"), "{label}");
    }

    #[test]
    fn the_creation_entry_is_last_so_it_never_steals_the_default_selection() {
        let mut m = Model::default();
        // `pane_named` takes the wire-format string rather than
        // `AgentState::Working` (see its own doc comment).
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let c = candidates(&m, "claude");
        let idx = c
            .iter()
            .position(|(_, c)| matches!(c, Command::CreateWorkspace { .. }));
        assert_eq!(idx, Some(c.len() - 1), "{c:?}");
        assert!(matches!(c[0].1, Command::FocusPane(_)), "{c:?}");
    }

    #[test]
    fn the_created_name_is_trimmed() {
        let m = Model::default();
        let c = candidates(&m, "  spaced  ");
        let found = c.iter().find_map(|(_, c)| match c {
            Command::CreateWorkspace { name } => Some(name.clone()),
            _ => None,
        });
        assert_eq!(found.as_deref(), Some("spaced"));
    }

    #[test]
    fn next_pane_is_always_offered() {
        let m = Model::default();
        assert!(candidates(&m, "")
            .iter()
            .any(|(_, c)| matches!(c, Command::NextPane)));
    }

    #[test]
    fn prev_pane_is_always_offered() {
        let m = Model::default();
        assert!(candidates(&m, "")
            .iter()
            .any(|(_, c)| matches!(c, Command::PrevPane)));
    }

    #[test]
    fn an_unmatched_query_returns_only_the_workspace_creation_candidate() {
        // Pins the state most likely to feed a shrinking-list hazard in
        // `route`'s `Up`/`Down`/`Enter` handling: a query nothing existing
        // matches used to yield an empty list; since this task's trailing
        // create-workspace entry is offered for any non-empty trimmed
        // query regardless of what else matches, it now yields exactly
        // that one entry — never a panic, and never a stale non-matching
        // one from before the query changed.
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let c = candidates(&m, "zzzz");
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(matches!(c[0].1, Command::CreateWorkspace { .. }), "{c:?}");
    }

    #[test]
    fn dead_panes_produce_no_focus_or_close_entries() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", false)];
        let c = candidates(&m, "");
        assert!(
            !c.iter()
                .any(|(_, cmd)| matches!(cmd, Command::FocusPane(_) | Command::ClosePane(_))),
            "a dead pane must not produce a focus or close candidate: {c:?}"
        );
    }

    // --- render_palette. The composed frame is covered in `frame.rs`'s own
    // tests; these pin the overlay's own arithmetic, which is where the
    // not-in-range/degenerate-size
    // hazards actually live. ---

    fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn open(query: &str, selected: usize) -> Palette {
        Palette {
            open: true,
            query: query.to_string(),
            selected,
            ..Palette::default()
        }
    }

    #[test]
    fn a_closed_palette_writes_nothing_and_places_no_cursor() {
        let m = Model::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let before = text(&buf);
        assert_eq!(
            render_palette(&m, &Palette::default(), area, &mut buf),
            None
        );
        assert_eq!(text(&buf), before);
    }

    #[test]
    fn the_cursor_sits_just_past_the_query_text() {
        // Both queries match exactly one candidate ("Detach"), so the box
        // is the same height and sits at the same row — isolating the
        // cursor's *column* as the only thing the extra characters move.
        let m = Model::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let short = render_palette(&m, &open("det", 0), area, &mut buf).expect("cursor placed");
        let mut buf = Buffer::empty(area);
        let long = render_palette(&m, &open("detac", 0), area, &mut buf).expect("cursor placed");
        assert_eq!(long.y, short.y, "same query row");
        assert_eq!(long.x, short.x + 2, "two characters further along");
    }

    #[test]
    fn a_selection_past_the_visible_window_scrolls_it_into_view() {
        // 8 live panes -> 16 focus/close entries + 3 always-on = 19
        // candidates, more than the 10 the overlay draws at once.
        let mut m = Model::default();
        m.local_mut().panes = (0..8)
            .map(|i| {
                pane_named(
                    &format!("p{i}"),
                    "w1",
                    &format!("agent{i}"),
                    "working",
                    true,
                )
            })
            .collect();
        let all = candidates(&m, "");
        assert!(all.len() > MAX_VISIBLE, "precondition: {}", all.len());
        let last = all.len() - 1;

        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        render_palette(&m, &open("", last), area, &mut buf);
        let out = text(&buf);
        assert!(
            out.contains(&format!("{SELECTION_MARKER} {}", all[last].0)),
            "the selected entry must be drawn: {out}"
        );
        assert!(
            !out.contains(&all[0].0),
            "the window must have scrolled past the first entry: {out}"
        );
    }

    #[test]
    fn a_selection_is_never_marked_on_a_row_the_box_is_too_short_to_draw() {
        // Verbatim repro: 19 candidates on a 13-row terminal — a 12-row pane
        // area, which after two border rows and the query line leaves nine rows
        // for candidates — with `selected = 9`. Before the height cap,
        // `visible` stayed 10, so `scroll_offset` returned 0 and the marker
        // landed on a row `Paragraph` clipped: nine unmarked candidates on
        // screen and `enter` firing one that was never displayed.
        let mut m = Model::default();
        m.local_mut().panes = (0..8)
            .map(|i| {
                pane_named(
                    &format!("p{i}"),
                    "w1",
                    &format!("agent{i}"),
                    "working",
                    true,
                )
            })
            .collect();
        let all = candidates(&m, "");
        // The precondition is the *relationship* — more candidates than the
        // box can draw, and a selection past the last drawable row — not a
        // literal total. Pinning the total is what made adding a palette
        // entry look expensive enough to leave `ToggleSplit` with no
        // discoverable path.
        const DRAWABLE_ROWS: usize = 9;
        assert!(
            all.len() > DRAWABLE_ROWS,
            "precondition: need more candidates ({}) than the box can draw",
            all.len()
        );

        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        render_palette(&m, &open("", 9), area, &mut buf);
        let out = text(&buf);

        let marked: Vec<&str> = out
            .lines()
            .filter(|line| line.contains(SELECTION_MARKER))
            .collect();
        assert_eq!(
            marked.len(),
            1,
            "the selected row must be drawn, not clipped off the box: {out}"
        );
        assert!(
            marked[0].contains(&all[9].0),
            "the marked row must be the selected candidate: {out}"
        );
    }

    #[test]
    fn the_split_is_reachable_by_name_not_only_by_its_chord() {
        // Regression test: `ToggleSplit` was once excluded from this list
        // for a test-maintenance reason, which left a headline feature with
        // no discoverable path anywhere — not the palette, not the
        // status-bar hint, not any document in the repo.
        let m = Model::default();
        assert!(
            candidates(&m, "")
                .iter()
                .any(|(_, c)| matches!(c, Command::ToggleSplit)),
            "the split must be offered with no query typed"
        );
        // Findable by the words a user would actually reach for.
        for query in ["spl", "SPLIT", "unsplit"] {
            assert!(
                candidates(&m, query)
                    .iter()
                    .any(|(_, c)| matches!(c, Command::ToggleSplit)),
                "querying {query:?} must find the split"
            );
        }
    }

    #[test]
    fn the_agent_sort_is_reachable_by_name_and_says_which_way_it_goes() {
        // Until now only the panel header's own label toggled it, which a
        // keyboard user cannot reach at all.
        let mut m = Model::default();
        let entry = |m: &Model, query: &str| {
            candidates(m, query)
                .into_iter()
                .find(|(_, c)| matches!(c, Command::ToggleAgentSort))
        };
        for query in ["", "sort", "SORT AGENTS", "priority", "grouped"] {
            assert!(entry(&m, query).is_some(), "{query:?} must find it");
        }
        let (label, _) = entry(&m, "").expect("offered");
        assert!(
            label.contains("grouped") && label.contains("priority"),
            "{label}"
        );
        assert!(
            label.find("grouped") < label.find("priority"),
            "the current mode first: {label}"
        );
        m.agent_sort = crate::model::AgentSort::Priority;
        let (label, _) = entry(&m, "").expect("offered");
        assert!(label.find("priority") < label.find("grouped"), "{label}");
    }

    #[test]
    fn the_sidebar_can_be_hidden_and_shown_from_the_palette() {
        let mut m = Model::default();
        m.sidebar.note_screen_width(120);
        let entry = |m: &Model| {
            candidates(m, "sidebar")
                .into_iter()
                .find(|(_, c)| matches!(c, Command::ToggleSidebar))
                .map(|(label, _)| label)
        };
        assert_eq!(entry(&m).as_deref(), Some("Hide the sidebar"));
        m.sidebar.toggle_collapsed();
        assert_eq!(entry(&m).as_deref(), Some("Show the sidebar"));
    }

    // --- the space-naming prompt: what the sidebar's `+ new workspace`
    // button opens. ---

    fn naming(query: &str) -> Palette {
        Palette {
            open: true,
            naming_space: true,
            query: query.to_string(),
            ..Palette::default()
        }
    }

    #[test]
    fn the_naming_prompt_offers_exactly_one_entry_whatever_else_exists() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec![],
        }];
        // The ordinary list for this model is long and starts with a pane.
        assert!(candidates(&m, "").len() > 1);
        let c = visible_candidates(&m, &naming("frontend"));
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            matches!(&c[0].1, Command::CreateWorkspace { name } if name == "frontend"),
            "{c:?}"
        );
    }

    #[test]
    fn the_naming_prompt_offers_nothing_until_something_is_typed() {
        // An empty list is what makes `enter` on an unnamed workspace a
        // no-op that closes the prompt, rather than a request the server
        // would reject.
        let m = Model::default();
        for query in ["", " ", "\t  "] {
            assert!(
                visible_candidates(&m, &naming(query)).is_empty(),
                "{query:?}"
            );
        }
    }

    #[test]
    fn the_naming_prompt_trims_the_name_the_same_way_the_command_list_does() {
        // The same action reached two ways must not disagree about what
        // `"  foo  "` names.
        let m = Model::default();
        let by_prompt = visible_candidates(&m, &naming("  spaced  "));
        let by_list = candidates(&m, "  spaced  ");
        let name_of = |c: &[(String, Command)]| {
            c.iter().find_map(|(_, c)| match c {
                Command::CreateWorkspace { name } => Some(name.clone()),
                _ => None,
            })
        };
        assert_eq!(name_of(&by_prompt).as_deref(), Some("spaced"));
        assert_eq!(name_of(&by_prompt), name_of(&by_list));
        assert_eq!(by_prompt[0].0, by_list[by_list.len() - 1].0, "same label");
    }

    #[test]
    fn a_palette_not_in_naming_mode_is_unaffected() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        assert_eq!(
            visible_candidates(&m, &open("cla", 0)),
            candidates(&m, "cla")
        );
    }

    #[test]
    fn the_naming_prompt_says_what_it_wants_before_anything_is_typed() {
        // With no query there are no candidate rows at all, so the border
        // title is the only thing telling the user what this box is —
        // opened by a click, with no keystroke to have implied it.
        let m = Model::default();
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        assert!(render_palette(&m, &naming(""), area, &mut buf).is_some());
        let out = text(&buf);
        assert!(out.contains("new space"), "{out}");
        assert!(out.contains("type a name"), "{out}");
        assert!(
            !out.contains("Detach"),
            "the ordinary command list must not be drawn here: {out}"
        );
    }

    #[test]
    fn the_naming_prompt_draws_the_entry_enter_would_fire() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        render_palette(&m, &naming("frontend"), area, &mut buf);
        let out = text(&buf);
        assert!(out.contains("> frontend"), "{out}");
        assert!(out.contains("Create space \"frontend\""), "{out}");
        assert!(
            !out.contains("Focus claude"),
            "a live pane must not be selectable from the naming prompt: {out}"
        );
    }

    #[test]
    fn the_naming_prompt_on_a_tiny_terminal_does_not_panic() {
        let m = Model::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 1, 1),
            Rect::new(0, 0, 4, 3),
            Rect::new(0, 0, 40, 2),
            Rect::new(200, 200, 40, 10),
        ] {
            render_palette(&m, &naming("x"), area, &mut buf);
            render_palette(&m, &naming(""), area, &mut buf);
        }
    }

    #[test]
    fn a_box_with_no_room_for_candidates_draws_only_the_query_and_claims_nothing() {
        // Three rows is exactly the borders plus the query line.
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::empty(area);
        assert!(render_palette(&m, &open("cla", 0), area, &mut buf).is_some());
        let out = text(&buf);
        assert!(out.contains("> cla"), "{out}");
        assert!(
            !out.contains(SELECTION_MARKER),
            "nothing may be marked when nothing is drawn: {out}"
        );
    }

    #[test]
    fn every_height_draws_a_marker_or_no_candidates_at_all() {
        // Sweeps the whole boundary the height cap governs rather than the
        // one row that happened to break: for any pane-area height, either
        // the selected row is on screen or no candidate row is.
        let mut m = Model::default();
        m.local_mut().panes = (0..8)
            .map(|i| {
                pane_named(
                    &format!("p{i}"),
                    "w1",
                    &format!("agent{i}"),
                    "working",
                    true,
                )
            })
            .collect();
        let all = candidates(&m, "");
        for height in 1..=20u16 {
            for selected in [0usize, 1, 9, 10, 18] {
                let area = Rect::new(0, 0, 60, height);
                let mut buf = Buffer::empty(area);
                render_palette(&m, &open("", selected), area, &mut buf);
                let out = text(&buf);
                let marked = out.lines().filter(|l| l.contains(SELECTION_MARKER)).count();
                let drew_any_candidate = all.iter().any(|(label, _)| out.contains(label.as_str()));
                assert!(
                    marked == 1 || !drew_any_candidate,
                    "height {height}, selected {selected}: drew candidates but not the \
                     selected one\n{out}"
                );
            }
        }
    }

    #[test]
    fn scroll_offset_keeps_the_selection_in_a_window_without_overflowing() {
        assert_eq!(scroll_offset(0, 19, 10), 0);
        assert_eq!(scroll_offset(9, 19, 10), 0);
        assert_eq!(scroll_offset(10, 19, 10), 1);
        assert_eq!(scroll_offset(18, 19, 10), 9);
        // Out of range in both directions — `selected` is only clamped by
        // `route` against the list as it was when a key was pressed.
        assert_eq!(scroll_offset(usize::MAX, 19, 10), 9);
        assert_eq!(scroll_offset(usize::MAX, 0, 0), 0);
        assert_eq!(scroll_offset(3, 3, 3), 0);
    }

    #[test]
    fn an_out_of_range_selection_draws_the_list_without_a_marker_and_without_panicking() {
        let m = Model::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        render_palette(&m, &open("", usize::MAX), area, &mut buf);
        let out = text(&buf);
        assert!(out.contains("Detach"), "{out}");
    }

    #[test]
    fn a_query_matching_nothing_still_draws_the_box_and_the_query() {
        let m = Model::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        assert!(render_palette(&m, &open("zzzz", 0), area, &mut buf).is_some());
        let out = text(&buf);
        assert!(out.contains("> zzzz"), "{out}");
    }

    #[test]
    fn degenerate_and_oversized_areas_do_not_panic() {
        let mut m = Model::default();
        m.local_mut().panes = vec![pane_named("p1", "w1", "claude", "working", true)];
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 40, 0),
            Rect::new(0, 0, 0, 10),
            Rect::new(0, 0, 1, 1),
            Rect::new(0, 0, 3, 2),
            Rect::new(0, 0, 4, 3),
            // Wider/taller than the buffer, and starting outside it — both
            // must clip rather than write out of bounds.
            Rect::new(0, 0, 400, 100),
            Rect::new(200, 200, 40, 10),
        ] {
            render_palette(&m, &open("a", 3), area, &mut buf);
        }
    }

    #[test]
    fn a_very_long_query_does_not_push_the_cursor_out_of_the_box() {
        let m = Model::default();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        let position =
            render_palette(&m, &open(&"q".repeat(500), 0), area, &mut buf).expect("cursor placed");
        assert!(position.x < area.right(), "{position:?}");
    }

    #[test]
    fn a_stopped_pane_is_offered_for_restart_and_a_live_one_is_not() {
        let mut m = Model::default();
        let mut dead = pane_named("dead", "w1", "claude", "idle", false);
        dead.cwd = "/srv/app".into();
        m.local_mut().panes.push(dead);
        m.local_mut()
            .panes
            .push(pane_named("up", "w1", "claude", "idle", true));
        let restarts: Vec<_> = candidates(&m, "")
            .into_iter()
            .filter_map(|(label, cmd)| match cmd {
                Command::RestartPanes(ids) => Some((label, ids)),
                _ => None,
            })
            .collect();
        assert_eq!(
            restarts,
            vec![(
                "Restart claude \u{2014} /srv/app".to_string(),
                vec!["dead".to_string()]
            )]
        );
    }

    fn listing(parent: Option<&str>) -> FolderListing {
        FolderListing {
            current: "/home/me/dev".into(),
            parent: parent.map(str::to_string),
            folders: vec![
                ("Webshop".into(), "/home/me/dev/Webshop".into()),
                ("spaces".into(), "/home/me/dev/spaces".into()),
            ],
            roots: vec!["/home/me/dev".into(), "/srv/work".into()],
        }
    }

    fn labels(c: &[(String, Command)]) -> Vec<&str> {
        c.iter().map(|(l, _)| l.as_str()).collect()
    }

    #[test]
    fn the_browser_offers_start_here_then_up_then_subfolders() {
        let c = browse_candidates("w1", "claude", &listing(Some("/home/me")), "");
        assert_eq!(
            labels(&c),
            vec![
                "Start claude here \u{2014} /home/me/dev",
                "../",
                "Webshop/",
                "spaces/"
            ]
        );
    }

    #[test]
    fn at_the_top_of_a_dev_directory_the_others_are_offered_instead_of_up() {
        let c = browse_candidates("w1", "claude", &listing(None), "");
        assert_eq!(
            labels(&c),
            vec![
                "Start claude here \u{2014} /home/me/dev",
                "/srv/work/",
                "Webshop/",
                "spaces/"
            ]
        );
    }

    #[test]
    fn typing_filters_folders_and_puts_them_first() {
        let c = browse_candidates("w1", "claude", &listing(Some("/home/me")), "SPA");
        assert_eq!(
            labels(&c),
            vec!["spaces/", "Start claude here \u{2014} /home/me/dev"]
        );
    }

    #[test]
    fn typing_a_path_switches_to_the_typed_entry() {
        let c = browse_candidates("w1", "claude", &listing(None), "/opt/app");
        assert_eq!(labels(&c), vec!["Start claude in \"/opt/app\""]);
        assert!(
            matches!(&c[0].1, Command::CreatePane { cwd, .. } if cwd.as_deref() == Some("/opt/app"))
        );
    }
}
