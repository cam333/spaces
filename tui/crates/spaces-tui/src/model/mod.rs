//! Client-side state model.
//!
//! A [`Model`] is the TUI's local mirror of runtime truth: it starts from a
//! `session.snapshot()` response and is folded forward by the incremental
//! `events` stream (see `docs/protocol/README.md#events`). This module never
//! *infers* agent state — it only records what the snapshot and events say.
//!
//! The rollup priority, `done` derivation, and state labels mirror
//! `src/lib/proto/agent-state.ts` on the Node side of this repo; keep the
//! two consistent if either changes.
//!
//! Every function here is reachable from network input (a `pane.state`
//! event's JSON payload, ultimately). None of it panics: a malformed or
//! unrecognized frame degrades to [`ApplyOutcome::Ignored`] rather than
//! aborting the process.
//!
//! `WorkspaceInfo::pane_ids` is accurate right after [`Model::apply_snapshot`]
//! but is *not* incrementally maintained by [`Model::apply_event`] — no
//! function this module exposes (`display_state`, `rollup`, `counts`) reads
//! it, since workspace membership is derived from `PaneInfo::workspace_id`
//! instead, which *is* kept current. A future change that needs
//! `pane_ids` itself to stay live-accurate will need to add that
//! bookkeeping across `pane.created`/`pane.moved`/`pane.removed`/
//! `workspace.removed`.

use std::time::{Duration, Instant};

use serde_json::Value;
use spaces_proto::{AgentState, EventFrameInner, PaneInfo, Snapshot, WorkspaceInfo};

pub mod node;
pub use node::{conn_from_status, ConnState, NodeId, NodeState};

/// The state actually shown to the user for one pane, derived from
/// [`PaneInfo::agent_state`] and [`PaneInfo::seen`]. `Done` is not a wire
/// state — see [`display_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayState {
    Blocked,
    Working,
    Done,
    Idle,
    Unknown,
}

impl DisplayState {
    /// Rollup priority — higher wins. Mirrors `STATE_PRIORITY` in
    /// `src/lib/proto/agent-state.ts`.
    pub fn priority(self) -> u8 {
        match self {
            DisplayState::Blocked => 5,
            DisplayState::Working => 4,
            DisplayState::Done => 3,
            DisplayState::Idle => 2,
            DisplayState::Unknown => 1,
        }
    }

    /// Status-bar label. Mirrors `stateLabel` in
    /// `src/lib/proto/agent-state.ts`.
    pub fn label(self) -> &'static str {
        match self {
            DisplayState::Blocked => "Needs you",
            DisplayState::Working => "Working",
            DisplayState::Done => "Done",
            DisplayState::Idle => "Idle",
            DisplayState::Unknown => "\u{2014}",
        }
    }
}

/// Derives the display state for one pane. `done` is not a protocol state —
/// it is an idle pane nobody has looked at yet (`agent_state == Idle &&
/// !seen`), matching `displayState()` in `src/lib/proto/agent-state.ts` and
/// `PaneStateStore.rollup` in `bin/lib/runtime/state.js`.
pub fn display_state(pane: &PaneInfo) -> DisplayState {
    match pane.agent_state {
        AgentState::Blocked => DisplayState::Blocked,
        AgentState::Working => DisplayState::Working,
        AgentState::Idle if !pane.seen => DisplayState::Done,
        AgentState::Idle => DisplayState::Idle,
        AgentState::Unknown => DisplayState::Unknown,
    }
}

/// Which of three unrelated domains wrote whatever is currently in
/// [`Model::notice`].
///
/// Stored *with* the string rather than inferred from its `PANE ERROR` /
/// `SPLIT PANE ERROR` / `WORKSPACE ERROR` prefix: a prefix is presentation,
/// and re-parsing one to decide whether a clear applies would make the
/// status bar's wording load-bearing for control flow.
///
/// [`Model::notice`]'s original doc comment declined to carry an origin, on the
/// grounds that two writers with distinguishable prefixes read fine on screen,
/// and recorded an escalation rule: *if a third writer appears, revisit — the
/// slot would need to carry its origin.* The split pane added the third writer
/// ([`crate::model`] has no view of it; it is `main.rs`'s `attach_split`) **and
/// made it repeating**, which is what forced this. See [`Model::notice`] for
/// what the origin now buys and what it deliberately still doesn't.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeOrigin {
    /// A pane-socket error frame on the slot serving [`Model::focused`].
    FocusedPane,
    /// A pane-socket error frame on the slot serving [`Model::split`], or a
    /// failed/refused split attach.
    SplitPane,
    /// A rejected `workspace.create`.
    Workspace,
    /// A completed copy-to-clipboard.
    ///
    /// The fourth writer to this one-slot line, and the only one reporting a
    /// *success*. It earns the crowding because a drag-select's only other
    /// feedback is the highlight itself, which says what was selected but not
    /// that it reached the clipboard. Last-write-wins against the other three,
    /// same as they are against each other.
    Clipboard,
}

/// How the sidebar's agents panel orders its entries.
///
/// Follows the established agent-panel convention, labels included
/// (`grouped`/`priority`) — this pane of the sidebar is the one a user
/// coming from another multiplexer will reach for first, and a different
/// word for the same two modes would be a gratuitous difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentSort {
    /// Grouped by the space each agent belongs to, in `workspaces` order.
    /// The *label* is what users read; the variant itself is named for what
    /// it does to the list.
    #[default]
    Grouped,
    /// Ordered by how much attention each agent wants:
    /// `blocked > working > done > idle > unknown`, i.e. descending
    /// [`DisplayState::priority`] — the same ordering
    /// [`Model::rollup`] resolves a space's badge with.
    Priority,
}

impl AgentSort {
    /// The word drawn on the panel's own toggle, kept to the convention.
    pub fn label(self) -> &'static str {
        match self {
            AgentSort::Grouped => "grouped",
            AgentSort::Priority => "priority",
        }
    }

    /// The other mode — what clicking the toggle switches to.
    pub fn toggled(self) -> AgentSort {
        match self {
            AgentSort::Grouped => AgentSort::Priority,
            AgentSort::Priority => AgentSort::Grouped,
        }
    }
}

/// What a left-button drag that began on the sidebar's own chrome is moving.
/// Set by the press, read by every drag sample, cleared by the release — see
/// `input::route_mouse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarDrag {
    /// The sidebar's right edge: the column follows the pointer.
    Width,
    /// The divider between the spaces and agents sections.
    Split,
}

/// The sidebar's user-adjustable geometry. Presentation state, like the
/// scroll offsets: never sent, never received, untouched by
/// [`Model::apply_snapshot`].
///
/// Stored as what the user *asked for*; every reader goes through a clamp at
/// use time ([`SidebarState::columns`], and `ui::sidebar`'s layout for the
/// split), because the terminal it has to fit can be resized at any moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarState {
    /// Requested width of the whole sidebar column, its one-cell draggable
    /// edge included. Starts at [`crate::ui::sidebar::WIDTH`].
    pub width: u16,
    /// Rows the spaces section takes, once the user has dragged the divider.
    /// `None` until then: the section is sized to its contents, which is the
    /// right default and stays the default until someone disagrees with it.
    pub spaces_height: Option<u16>,
    /// The drag in progress, if one began on the sidebar's edge or divider.
    pub drag: Option<SidebarDrag>,
    /// The user's explicit hide/show choice (`ctrl+space b`, or the palette),
    /// or `None` to follow the terminal's width — collapsed below
    /// [`crate::ui::sidebar::COLLAPSE_BELOW`] columns. See
    /// [`SidebarState::is_collapsed`].
    pub collapsed: Option<bool>,
    /// The terminal width last reported to [`SidebarState::note_screen_width`].
    /// Kept so a resize can tell whether it crossed the collapse threshold,
    /// and so a toggle — which arrives from a keystroke that knows nothing of
    /// the terminal — can flip the state that is actually on screen.
    pub screen_width: u16,
}

impl Default for SidebarState {
    /// A hand-written `Default`, not a derived one: the derived `0` width
    /// would hide the sidebar of every `Model::default()` built in a test.
    fn default() -> Self {
        SidebarState {
            width: crate::ui::sidebar::WIDTH,
            spaces_height: None,
            drag: None,
            collapsed: None,
            screen_width: 0,
        }
    }
}

impl SidebarState {
    /// How many columns the sidebar actually takes on a terminal
    /// `screen_width` wide: none at all while it is collapsed, else
    /// [`SidebarState::width`] through [`crate::ui::sidebar::clamp_width`].
    /// Every layout call site passes this to the `ui` layout functions, which
    /// take the width as a plain parameter so they stay pure.
    pub fn columns(&self, screen_width: u16) -> u16 {
        if self.is_collapsed(screen_width) {
            return 0;
        }
        crate::ui::sidebar::clamp_width(self.width, screen_width)
    }

    /// Whether the sidebar is hidden on a terminal `screen_width` wide: the
    /// user's explicit choice if they have made one since the last resize
    /// across the threshold, else automatically below
    /// [`crate::ui::sidebar::COLLAPSE_BELOW`] columns — where a 28-column
    /// sidebar would leave an agent too little room to be worth looking at.
    pub fn is_collapsed(&self, screen_width: u16) -> bool {
        self.collapsed
            .unwrap_or(screen_width < crate::ui::sidebar::COLLAPSE_BELOW)
    }

    /// Records the terminal's width — at startup and on every resize.
    ///
    /// A resize that crosses the collapse threshold, in either direction,
    /// drops the user's explicit choice and hands the sidebar back to the
    /// automatic rule. That is the one moment the automatic state means
    /// something new: someone who hid the sidebar on a wide terminal and then
    /// narrowed it wants it hidden anyway, and someone who forced it open on
    /// a narrow terminal and then widened it gets it open anyway — but a
    /// terminal that went narrow, wide and narrow again should collapse the
    /// way it did the first time, not remember a choice made two sizes ago.
    pub fn note_screen_width(&mut self, screen_width: u16) {
        let threshold = crate::ui::sidebar::COLLAPSE_BELOW;
        if (screen_width < threshold) != (self.screen_width < threshold) {
            self.collapsed = None;
        }
        self.screen_width = screen_width;
    }

    /// Hides the sidebar if it is showing on the last-noted terminal, shows
    /// it if it is hidden — relative to what is on screen, whichever rule put
    /// it there. A drag in progress is dropped with it: there is no edge left
    /// to be dragging.
    pub fn toggle_collapsed(&mut self) {
        self.collapsed = Some(!self.is_collapsed(self.screen_width));
        self.drag = None;
    }
}

/// How long a toast stays on screen.
pub const TOAST_TTL: Duration = Duration::from_secs(4);

/// The most toasts shown at once. A fourth pushes the oldest out: past three
/// the corner of the pane area stops being a corner, and the agents panel —
/// whose `priority` order puts exactly these panes first — is the place to
/// see everything that wants you.
pub const MAX_TOASTS: usize = 3;

/// One pending toast: a pane elsewhere that blocked or finished, raised by a
/// `pane.notify` event and drawn by [`crate::ui::toast`]. Presentation state:
/// never sent, never part of a snapshot.
///
/// The names are captured when the toast is raised rather than looked up
/// when it is drawn, so a toast keeps saying what it said for its four
/// seconds even if the pane is renamed or closes in the meantime.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub pane_id: String,
    /// The pane's title if it has one, else its agent type — what its tab
    /// calls it.
    pub pane: String,
    /// The space it is in, when this client knows the pane.
    pub space: Option<String>,
    /// [`DisplayState::Blocked`] for `needs_attention`, [`DisplayState::Done`]
    /// for `finished` — the two signals the runtime raises.
    pub state: DisplayState,
    pub expires_at: Instant,
}

/// Outcome of folding one event into a [`Model`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// The event was understood and applied (even if it turned out to be a
    /// no-op, e.g. it referenced a pane the model doesn't know about).
    Applied,
    /// `seq` was not `self.seq + 1` — the model has missed one or more
    /// events and must be rebuilt from a fresh `session.snapshot()`. `seq`
    /// is left untouched so the gap is still detectable if this event is
    /// (harmlessly) applied again after a resnapshot.
    NeedsResnapshot,
    /// The event's `type` wasn't recognized, or its `payload` was missing a
    /// required field or had the wrong type for one. `seq` still advances —
    /// an unrecognized-but-in-order event must not be mistaken for a gap.
    Ignored,
}

/// The TUI's local mirror of the whole fleet: one [`NodeState`] per node,
/// plus the UI state that spans all of them. See the module docs for how
/// this is meant to be used.
///
/// **The local node is an ordinary entry.** It is reached through
/// [`Model::local`], which is a map lookup keyed by [`Model::local_id`] — not
/// a separate field, not a branch. Every place that would otherwise say "if
/// this is the local node" is a place a later phase would have to find and
/// undo.
///
/// The `local_*` accessors are deliberately named to be greppable: each one
/// is a site that still assumes a single node, so `grep -rn local_panes` is
/// the next phase's work list. Do not re-expose them under neutral names.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// Every node this client knows about, keyed by id. A `BTreeMap` rather
    /// than a `HashMap` so the sidebar's node order is stable between frames
    /// — a list that reorders under the cursor is a list you cannot click.
    pub nodes: std::collections::BTreeMap<NodeId, NodeState>,
    /// Which entry of `nodes` is the node this client connected to directly
    /// at startup. Private: reads go through [`Model::local_id`], so the
    /// field cannot be reassigned from outside and strand the map.
    local: NodeId,
    /// The pane the local TUI has focused. This is UI state, not runtime
    /// truth — it never arrives via snapshot or event, so [`apply_snapshot`]
    /// leaves it untouched.
    ///
    /// Unread until the rendering/input-routing tasks land (nothing in this
    /// crate draws a UI yet); part of the public model per the plan.
    ///
    /// [`apply_snapshot`]: Model::apply_snapshot
    pub focused: Option<String>,
    /// The id of a *second* simultaneously-visible pane, drawn beside
    /// [`Model::focused`] (see `ui::frame::pane_regions`). `None` — the
    /// default — means one pane fills the whole pane area.
    ///
    /// **Client-local presentation state, and a deliberate divergence.**
    /// Line 57 of
    /// `docs/superpowers/specs/2026-08-07-spaces-reimagining-design.md`
    /// lists the layout tree among the facts every client shares, but there
    /// is no layout concept anywhere in the protocol today — not in
    /// `docs/protocol/README.md`, not in the schema — so there is nothing to
    /// synchronize this against. Adding one spans the runtime, the schema,
    /// the web UI and this client, which is a phase-sized change rather than
    /// part of this one. Until then a split lives and dies with this
    /// process, exactly like `focused`, `sidebar_scroll` and the palette's
    /// own state: [`Model::apply_snapshot`] and [`Model::apply_event`] never
    /// write it, and the only event-driven clearing is
    /// [`Model::forget_focus_if_gone`] dropping a pane whose record vanished.
    ///
    /// Invariant maintained by `main.rs`, relied on by both the renderer and
    /// the socket bookkeeping: this is never equal to `focused`. Focusing
    /// the pane currently in this slot *swaps* the two rather than pointing
    /// both at the same pane — see `main.rs::focus_pane`.
    ///
    /// Which *side* of the screen this pane is drawn on is not implied by
    /// the field it sits in — see [`Model::split_draws_left`].
    pub split: Option<String>,
    /// Set by `main.rs`'s event loop when `focused` names a pane but there
    /// is no live pane-socket attachment to it right now — the reconnect
    /// succeeded but the pane socket didn't re-attach, the socket died
    /// while the pane stayed alive per the control channel (another
    /// controller took over, a hiccup, ...), or an attach attempt itself
    /// failed. Never set by `apply_snapshot`/`apply_event`, which know
    /// nothing about pane-socket connectivity — local UI state, exactly
    /// like `focused` and `stale`. Distinct from `stale`: the control
    /// channel can be perfectly healthy while this is `true`.
    pub detached_pane: bool,
    /// A short, already-sanitized line explaining the most recent failure
    /// of something the user just tried, shown on the status bar until
    /// something clears it — paired with the [`NoticeOrigin`] that wrote it.
    ///
    /// A single shared slot with **three** writers in three unrelated
    /// domains, all in `main.rs`, none in `apply_snapshot`/`apply_event`
    /// (which know nothing about any of them) — local UI state, exactly like
    /// `focused`, `stale` and `detached_pane`:
    ///   - The event loop, from a `PaneEvent::Error` frame on *either* pane
    ///     socket (`unauthorized`/`not_found`/`conflict`/...) —
    ///     [`NoticeOrigin::FocusedPane`] or [`NoticeOrigin::SplitPane`]
    ///     depending on which slot the frame arrived on.
    ///   - `handle_command`'s `Command::CreateWorkspace` arm, from a
    ///     rejected `workspace.create` request (see
    ///     `main.rs::workspace_create_notice`):
    ///     [`NoticeOrigin::Workspace`].
    ///   - `main.rs::attach_split`'s failure branch, from a split attach the
    ///     server refused or that timed out:
    ///     [`NoticeOrigin::SplitPane`]. This one is **repeating** — it fires
    ///     again on every rung of the split slot's reattach ladder, forever,
    ///     as long as `split` names a live pane the server keeps refusing.
    ///
    /// Still one slot, not one per domain, and **setting is still
    /// last-write-wins**: whichever writer ran most recently is what the
    /// status bar shows. What the origin buys is *clearing*. Each of the two
    /// success paths that resolve a failure — `main.rs::focus_pane`'s and
    /// `main.rs::attach_split`'s — now clears via
    /// [`Model::clear_notice_from`] and so can only ever retract **its own
    /// domain's** line. Before that, a successful split attach wiped a
    /// `PANE ERROR` the focused slot had raised milliseconds earlier about a
    /// different pane, and symmetrically a successful focus wiped a
    /// `SPLIT PANE ERROR` that was still true — and that the repeating
    /// writer above then re-raised seconds later. Both were reachable with
    /// no user action at all.
    ///
    /// What the origin deliberately does *not* buy, recorded so the next
    /// reader doesn't assume otherwise: a one-shot writer is still starved
    /// by the repeating one. A `WORKSPACE ERROR` the user needs to read is
    /// still overwritten by the next `SPLIT PANE ERROR` rung within at most
    /// the ladder's 8s cap. Fixing *that* means ranking the domains (or
    /// keeping more than one line), which is a real notification model
    /// rather than a status line, and is not what this slot is.
    ///
    /// [`Model::forget_focus_if_gone`] is the one clear site that stays
    /// unconditional; see its own doc comment for why that is still right.
    ///
    /// Those three pane-socket error codes have *no* control-channel
    /// counterpart — the runtime writes the error frame on the pane socket
    /// and closes it — so dropping them (as an earlier version of this
    /// client did) left the client silently retrying
    /// forever with nothing on screen to explain why the pane never came
    /// back.
    ///
    /// Every string that lands here is already sanitized (control
    /// characters stripped, length bounded — see `main.rs::sanitize_notice`)
    /// by whichever side wrote it, before it ever reaches this field: a
    /// status bar is a terminal, and a raw `\x1b[` in a server-supplied
    /// message would be an escape-sequence injection.
    pub notice: Option<(NoticeOrigin, String)>,
    /// How many leading rows of the sidebar's [`crate::ui::sidebar::sidebar_rows`]
    /// output are scrolled out of view above the visible window — presentation
    /// state, not runtime truth. Like `focused`, `stale`, `detached_pane` and
    /// `notice`, this never arrives via snapshot or event: line 57 of
    /// `docs/superpowers/specs/2026-08-07-spaces-reimagining-design.md` draws
    /// exactly this line — "presentation state (sidebar selection, scroll
    /// position, theme) lives in each client" — so [`Model::apply_snapshot`]
    /// leaves it untouched. Read and clamped
    /// (never trusted as-is) by `ui::sidebar::render_sidebar` on every
    /// render, and by `main.rs`'s mouse arm when a wheel event or a sidebar
    /// click needs to agree with what the last render actually drew — see
    /// `ui::sidebar::clamp_scroll`'s own doc comment for why both call sites
    /// must run the identical clamp rather than trusting this field alone.
    pub sidebar_scroll: usize,
    /// The same, for the sidebar's *agents* panel, which scrolls
    /// independently of the spaces list above it — two lists sharing one
    /// offset would jump one of them every time the user scrolled the other.
    /// Presentation state, on the same footing as [`Model::sidebar_scroll`];
    /// clamped at render time by the same [`crate::ui::sidebar::clamp_scroll`].
    pub agents_scroll: usize,
    /// How the agents panel orders its entries — toggled by clicking the
    /// `grouped`/`priority` label in that panel's own header. Presentation
    /// state, same as the two scroll offsets above: never sent, never
    /// received, untouched by [`Model::apply_snapshot`].
    pub agent_sort: AgentSort,
    /// Index of the first tab the tab strip draws once its tabs overflow —
    /// moved one tab at a time by the strip's `<`/`>` buttons. Presentation
    /// state, like the sidebar's two scroll offsets: never sent, never
    /// received, and clamped (never trusted as-is) by
    /// [`crate::ui::tabs::tab_bar`] on every frame and every click.
    pub tab_scroll: usize,
    /// The pane that was focused when the user last scrolled the tab strip
    /// by hand.
    ///
    /// The strip scrolls the focused pane's tab into view on its own — focus
    /// moves from the sidebar, the palette and next/previous, none of which
    /// can see the strip. But revealing it on *every* frame would undo every
    /// click on `<`/`>` that moved the focused tab off the strip. So a manual
    /// scroll pins the pane that was focused at the time, and the reveal is
    /// skipped while that pane is still the focused one; focusing anything
    /// else releases it. See [`crate::ui::tabs::tab_bar`].
    pub tab_scroll_pin: Option<String>,
    /// The sidebar's width and section split, and any drag adjusting them.
    pub sidebar: SidebarState,
    /// Pending toasts, newest first, at most [`MAX_TOASTS`] — see
    /// [`Model::push_toast`] and [`Model::expire_toasts`].
    pub toasts: Vec<Toast>,
    /// Which node a new space is created on — set by clicking a node header
    /// in by-node view. Presentation state, like [`Model::selected_space`];
    /// read through [`Model::target_node`], never directly, so a selection
    /// left pointing at a node that has gone away cannot send a `new`
    /// nowhere.
    pub selected_node: NodeId,
    /// The space the user last clicked in the sidebar, if it still exists —
    /// what [`Model::active_space_id`] prefers, and therefore which space's
    /// panes the tab bar shows and its `+` creates into.
    ///
    /// Needed as its own field because a space with *no panes* cannot be
    /// identified by the focused pane, and that is precisely the space whose
    /// `+` button the user is reaching for. Presentation state, like the two
    /// scroll offsets and [`Model::agent_sort`].
    pub selected_space: Option<String>,
    /// Each space's Files tab, while open. Not panes — see [`crate::files`]
    /// for why they live here rather than in `focused`/`split`.
    pub files: std::collections::HashMap<String, crate::files::FilesView>,
    /// How the active space's Files tab is shown, or `None` while its
    /// terminals are. Has no effect on a space without a Files tab.
    pub files_shown: Option<crate::files::FilesLayout>,
    /// The Files tab has the keyboard, rather than the focused terminal.
    pub files_keys: bool,
    /// Where file transfers dial and report to. Set once connected.
    pub transfers: Option<crate::transfer::Ctx>,
    /// Whether the terminal is currently reporting mouse events to this
    /// client. `true` for the whole session until the user turns it off.
    ///
    /// Mouse reporting is what makes the sidebar, tabs and palette clickable
    /// — and it is also what stops the *terminal's own* click-drag selection
    /// from working, because the drag is delivered here instead of to the
    /// terminal. There is no way to have both: the terminal decides who gets
    /// the event, and it cannot know that this particular drag was meant for
    /// it. So this is a mode, toggled by `ctrl+space m` or from the palette,
    /// and the status bar says which one you are in.
    ///
    /// Presentation state, like the scroll offsets: never sent, never
    /// received, untouched by [`Model::apply_snapshot`]. It lives here rather
    /// than as a local in `run` because [`crate::ui`] has to render the
    /// indicator, and a flag the renderer cannot see would be a mode with no
    /// way to tell you are in it.
    pub mouse_captured: bool,
}

impl Model {
    /// A model with one node — the local one — and the field defaults
    /// [`Default`] cannot express.
    ///
    /// [`Model::mouse_captured`] starts `true` — `main.rs` enables mouse
    /// reporting as it sets up the terminal, so a `false` default would have
    /// the indicator disagree with the terminal from the first frame. Rust's
    /// derived `Default` gives `bool` `false`, hence this.
    pub fn new() -> Model {
        let local = NodeId::new("local");
        let mut nodes = std::collections::BTreeMap::new();
        nodes.insert(local.clone(), NodeState::new("", "local"));
        Model {
            nodes,
            selected_node: local.clone(),
            local,
            mouse_captured: true,
            ..Model::default()
        }
    }

    /// Raises a toast for a `pane.notify` payload (`{signal, paneId,
    /// agentType, title}`), expiring [`TOAST_TTL`] after `now`. Returns
    /// whether one was raised.
    ///
    /// Nothing is raised for the *focused* pane — you are already looking at
    /// it — or for a payload that is malformed or carries a signal this
    /// client does not know: a notification is advisory, and guessing at one
    /// is worse than staying quiet. A pane that already has a toast has it
    /// replaced, at the top, so one pane never fills the stack.
    pub fn push_toast(&mut self, payload: &Value, now: Instant) -> bool {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return false;
        };
        if self.focused.as_deref() == Some(pane_id) {
            return false;
        }
        let state = match str_field(payload, "signal") {
            Some("needs_attention") => DisplayState::Blocked,
            Some("finished") => DisplayState::Done,
            _ => return false,
        };
        let known = self.local_panes().iter().find(|p| p.id == pane_id);
        let named = |title: &str, agent: &str| {
            if title.is_empty() {
                agent.to_string()
            } else {
                title.to_string()
            }
        };
        let pane = match known {
            Some(p) => named(&p.title, &p.agent_type),
            None => named(
                str_field(payload, "title").unwrap_or_default(),
                str_field(payload, "agentType").unwrap_or("pane"),
            ),
        };
        let space = known
            .and_then(|p| self.space_name_of(p))
            .map(str::to_string);
        let toast = Toast {
            pane_id: pane_id.to_string(),
            pane,
            space,
            state,
            expires_at: now + TOAST_TTL,
        };
        self.toasts.retain(|t| t.pane_id != toast.pane_id);
        self.toasts.insert(0, toast);
        self.toasts.truncate(MAX_TOASTS);
        true
    }

    /// Drops every toast whose time is up at `now`, and says whether any
    /// went — `main.rs` calls this on its existing redraw tick and redraws
    /// only when it returns `true`, so an idle screen with no toasts costs
    /// nothing extra.
    pub fn expire_toasts(&mut self, now: Instant) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.expires_at > now);
        self.toasts.len() != before
    }

    /// Removes `pane_id`'s toast — the one a click just acted on.
    pub fn dismiss_toast(&mut self, pane_id: &str) {
        self.toasts.retain(|t| t.pane_id != pane_id);
    }

    /// Flips [`Model::agent_sort`] and rewinds the agents panel to its top.
    ///
    /// The one implementation behind all three ways to do this — clicking the
    /// panel's header label, the palette entry, and `ctrl+space o` — so they
    /// cannot drift. The rewind is part of the toggle, not an afterthought:
    /// the list just reordered, so an offset into the old order means
    /// nothing, and the top is where the entry the user flipped to `priority`
    /// to find now is.
    pub fn toggle_agent_sort(&mut self) {
        self.agent_sort = self.agent_sort.toggled();
        self.agents_scroll = 0;
    }

    /// The local node's id, owned — for comparing against map keys while the
    /// map itself is borrowed, which a reference into `self` would prevent.
    pub fn local_id_owned(&self) -> NodeId {
        self.local.clone()
    }

    /// The local node's state.
    ///
    /// Falls back to a shared empty [`NodeState`] if the map has somehow lost
    /// its local entry — which [`Model::new`] makes unreachable, and which a
    /// `Model::default()` built by a test reaches until its first write.
    /// Returning a default beats panicking in a render path.
    pub fn local(&self) -> &NodeState {
        static EMPTY: std::sync::OnceLock<NodeState> = std::sync::OnceLock::new();
        self.nodes
            .get(&self.local)
            .unwrap_or_else(|| EMPTY.get_or_init(NodeState::default))
    }

    /// The local node's state, for writing.
    ///
    /// Inserts on first use rather than unwrapping, so a `Model::default()`
    /// (which has an empty map) behaves like a fresh single-node model — the
    /// shape most of this module's tests build.
    pub fn local_mut(&mut self) -> &mut NodeState {
        self.nodes.entry(self.local.clone()).or_default()
    }

    /// The local node's panes. Transitional — see [`Model`]'s docs.
    pub fn local_panes(&self) -> &[PaneInfo] {
        &self.local().panes
    }

    /// The local node's spaces. Transitional — see [`Model`]'s docs.
    pub fn local_workspaces(&self) -> &[WorkspaceInfo] {
        &self.local().workspaces
    }

    /// One node's state by id, or `None` if this client has no record of it.
    pub fn node(&self, id: &NodeId) -> Option<&NodeState> {
        self.nodes.get(id)
    }

    /// Folds a peer's `session.snapshot`, fetched through the local node, into
    /// that peer's slot.
    ///
    /// Deliberately not [`Model::apply_snapshot`], which writes the *local*
    /// node: a peer's spaces and panes must land under the machine that owns
    /// them, or a space id colliding across two machines would silently
    /// overwrite. The node's name and connection state are left alone — those
    /// come from `net.peers`, which knows about reachability, and this reply
    /// does not.
    pub fn apply_peer_snapshot(&mut self, node: &NodeId, s: Snapshot) {
        let Some(state) = self.nodes.get_mut(node) else {
            // A snapshot for a machine that has since left the fleet. Dropping
            // it is right: re-adding the node here would resurrect something
            // the server has already said is gone.
            return;
        };
        state.node_id = s.node_id;
        state.seq = s.seq;
        state.workspaces = s.workspaces;
        state.panes = s.panes;
        state.stale = false;
        self.sanitize_all();
    }

    /// Folds a `net.peers` reply into the node map.
    ///
    /// The entry flagged `self` is the machine this client is connected to, so
    /// it adopts that node's real id and name in place of the placeholder
    /// [`Model::new`] started with. Everything the local node already holds —
    /// its spaces, panes and connection state — moves with it, because the
    /// snapshot that filled it arrived over the very connection being renamed.
    ///
    /// Peers keep whatever state they have already accumulated; only their
    /// reported identity and status are refreshed. A peer that disappears from
    /// the fleet is dropped, so forgetting a machine on the server removes it
    /// from the sidebar without a reconnect.
    pub fn apply_peers(&mut self, peers: &[spaces_proto::NetNode]) {
        let Some(local) = peers.iter().find(|p| p.self_) else {
            // A reply that names no local machine is not usable: adopting it
            // would leave the client's own spaces filed under a node the
            // server never mentioned.
            return;
        };

        let new_local = NodeId::new(&local.node_id);
        if new_local != self.local {
            if let Some(mut state) = self.nodes.remove(&self.local) {
                state.node_id = local.node_id.clone();
                self.nodes.insert(new_local.clone(), state);
            }
            if self.selected_node == self.local {
                self.selected_node = new_local.clone();
            }
            self.local = new_local;
        }

        let reported: std::collections::BTreeSet<NodeId> =
            peers.iter().map(|p| NodeId::new(&p.node_id)).collect();
        self.nodes.retain(|id, _| reported.contains(id));

        for peer in peers {
            let id = NodeId::new(&peer.node_id);
            let entry = self.nodes.entry(id).or_default();
            entry.node_id = peer.node_id.clone();
            entry.name = peer.name.clone();
            if !peer.self_ {
                entry.conn = conn_from_status(&peer.status, peer.paired);
            }
        }
        self.sanitize_all();
    }

    /// Which machine owns `space_id`, or `None` when it is this one.
    ///
    /// Space ids are only unique within a machine, so this searches by id
    /// *within* each node rather than across a merged list — two machines can
    /// each have a `w1` and starting a pane on the wrong one is the failure
    /// this prevents.
    pub fn owner_of_space(&self, space_id: &str) -> Option<NodeId> {
        let has = |node: &NodeId| {
            self.nodes
                .get(node)
                .is_some_and(|s| s.workspaces.iter().any(|w| w.id == space_id))
        };
        // The selected machine wins when it actually has this space. Clicking a
        // space records the machine it was drawn under (see `SidebarHit::Space`),
        // so this is the user's own answer rather than a guess — and the guess
        // below cannot be right when two machines share an id.
        if has(&self.selected_node) {
            return if self.selected_node == self.local {
                None
            } else {
                Some(self.selected_node.clone())
            };
        }
        // No selection that fits — an id from a tab strip or a stale command.
        // Local first, so an ambiguous id resolves to this machine rather than
        // silently acting on someone else's.
        if has(&self.local) {
            return None;
        }
        self.nodes
            .iter()
            .find(|(id, state)| {
                **id != self.local && state.workspaces.iter().any(|w| w.id == space_id)
            })
            .map(|(id, _)| id.clone())
    }

    /// A directory to suggest for a pane on `node` — that machine's most recent
    /// working directory, falling back to its home directory.
    ///
    /// Per machine, because the prompt is about a filesystem the client cannot
    /// see and offering local paths for a remote machine is worse than offering
    /// nothing: they look plausible and are wrong.
    pub fn suggested_cwd_on(&self, node: &NodeId) -> Option<String> {
        let state = self.nodes.get(node)?;
        state
            .panes
            .iter()
            .rev()
            .find(|p| p.live && !p.cwd.is_empty())
            .map(|p| p.cwd.clone())
            .or_else(|| Some(state.home_dir.clone()).filter(|h| !h.is_empty()))
    }

    /// Which machine owns `pane_id`, or `None` when it is this one.
    ///
    /// `None` for a local pane rather than the local node's id, because the
    /// caller turns that straight into a URL parameter and a single-machine
    /// session should put exactly the bytes on the wire it always did. Also
    /// `None` for a pane nothing knows about — attaching then fails against
    /// the local machine with its ordinary "no live pane" error, which is a
    /// truer answer than naming some machine at random.
    pub fn owner_of_pane(&self, pane_id: &str) -> Option<NodeId> {
        self.nodes
            .iter()
            .find(|(id, state)| **id != self.local && state.panes.iter().any(|p| p.id == pane_id))
            .map(|(id, _)| id.clone())
    }

    /// The node a new space would land on, filtered at read time against the
    /// node actually still existing.
    ///
    /// Read-time filtering rather than pruning whenever a node goes away: the
    /// answer only has to be right at the moment it is asked, and there is
    /// one place to be correct instead of one per way a node can disappear —
    /// the rule [`Model::pane_to_focus_in`] already follows for panes.
    pub fn target_node(&self) -> NodeId {
        if self.nodes.contains_key(&self.selected_node) {
            self.selected_node.clone()
        } else {
            self.local.clone()
        }
    }
}

/// Reads a string field out of an event payload. Returns `None` if the key
/// is missing or its value isn't a JSON string — the caller treats both the
/// same way (a malformed/absent field), never distinguishing further.
fn str_field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload.get(key).and_then(Value::as_str)
}

/// Reads a bool field out of an event payload, same missing/wrong-type
/// handling as [`str_field`].
fn bool_field(payload: &Value, key: &str) -> Option<bool> {
    payload.get(key).and_then(Value::as_bool)
}

impl Model {
    /// Replaces all runtime-derived state with a fresh snapshot. Local UI
    /// state (`focused`) is untouched; `stale` is cleared since the model is
    /// caught up again.
    pub fn apply_snapshot(&mut self, s: Snapshot) {
        let local = self.local_mut();
        local.node_id = s.node_id;
        local.seq = s.seq;
        local.workspaces = s.workspaces;
        local.panes = s.panes;
        local.stale = false;
        self.sanitize_all();
    }

    /// Puts `text` on the status bar, recording which domain it came from.
    ///
    /// Unconditional — setting stays last-write-wins across all three
    /// origins (see [`Model::notice`]). Only *clearing* consults the origin.
    pub fn set_notice(&mut self, origin: NoticeOrigin, text: String) {
        self.notice = Some((origin, text));
    }

    /// Clears the status-bar notice **only if `origin` is what wrote it**,
    /// leaving another domain's line standing.
    ///
    /// This is the whole point of [`NoticeOrigin`]. The two callers are the
    /// success paths of `main.rs`'s `focus_pane` and `attach_split`: each is
    /// evidence that *its own* pane's last failure is over, and evidence of
    /// nothing whatsoever about the other slot or about a rejected
    /// `workspace.create`. `Command::CreateWorkspace` uses it too, as its
    /// pre-request pessimistic clear.
    pub fn clear_notice_from(&mut self, origin: NoticeOrigin) {
        if self.notice.as_ref().is_some_and(|(o, _)| *o == origin) {
            self.notice = None;
        }
    }

    /// The line currently on the status bar, whichever domain wrote it —
    /// what `ui::frame::render_status_bar` draws. Rendering is the one
    /// reader that must not care about the origin: exactly one notice is
    /// ever shown and it is shown the same way regardless of where it came
    /// from.
    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().map(|(_, text)| text.as_str())
    }

    /// Folds one event into the model. See [`ApplyOutcome`] for what each
    /// variant means and why `seq` advances (or doesn't) for it.
    pub fn apply_event(&mut self, e: &EventFrameInner) -> ApplyOutcome {
        let outcome = self.apply_event_unchecked(e);
        self.sanitize_all();
        outcome
    }

    /// Cleans every node's drawn strings — see [`NodeState::sanitize_strings`].
    /// Run after anything from a server lands in the model.
    pub fn sanitize_all(&mut self) {
        for state in self.nodes.values_mut() {
            state.sanitize_strings();
        }
    }

    fn apply_event_unchecked(&mut self, e: &EventFrameInner) -> ApplyOutcome {
        // `self.seq` originates from `Snapshot.seq` (`apply_snapshot`), which
        // is untrusted data straight off the wire — a malformed or hostile
        // `session.snapshot` response could set it to `u64::MAX`. A bare
        // `self.seq + 1` would then panic on overflow in a debug build (this
        // workspace has no `overflow-checks` profile override) or silently
        // wrap to `0` in release, desyncing every event after it.
        // `checked_add` treats that boundary as just another gap instead.
        let node = self.local_mut();
        if node.seq.checked_add(1) != Some(e.seq) {
            node.stale = true;
            return ApplyOutcome::NeedsResnapshot;
        }
        node.seq = e.seq;

        match e.r#type.as_str() {
            "pane.state" => self.apply_pane_state(&e.payload),
            "pane.seen" => self.apply_pane_seen(&e.payload),
            "pane.title" => self.apply_pane_title(&e.payload),
            "pane.updated" => self.apply_pane_updated(&e.payload),
            "pane.opened" | "pane.created" => self.apply_pane_opened_or_created(&e.payload),
            "pane.closed" => self.apply_pane_closed(&e.payload),
            "pane.removed" => self.apply_pane_removed(&e.payload),
            "pane.moved" => self.apply_pane_moved(&e.payload),
            "workspace.created" => self.apply_workspace_created(&e.payload),
            "workspace.updated" => self.apply_workspace_updated(&e.payload),
            "workspace.removed" => self.apply_workspace_removed(&e.payload),
            // Known, and in sequence, but nothing to fold: a notification is
            // a moment rather than state. `main.rs` raises its toast through
            // [`Model::push_toast`], which needs a clock this function does
            // not take.
            "pane.notify" => ApplyOutcome::Applied,
            // A future server may emit event types this client doesn't know
            // about yet. `seq` has already advanced above — an unknown type
            // is not a gap, and treating it like one would force a needless
            // resnapshot on every older client each time the server grows a
            // new event.
            _ => ApplyOutcome::Ignored,
        }
    }

    fn apply_pane_state(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        let Some(seen) = bool_field(payload, "seen") else {
            return ApplyOutcome::Ignored;
        };
        // `agentState` gets two different treatments depending on *how* it's
        // wrong:
        //   - missing, or present but not a JSON string: the frame is
        //     malformed, nothing to act on -> Ignored, pane untouched.
        //   - a string, but not one of AgentState's variants (e.g. a future
        //     server adds a new state an old client doesn't know about):
        //     not malformed, just unrecognized -> downgrades to
        //     AgentState::Unknown and the event still Applies. Showing "—"
        //     beats leaving a stale "Working" on screen forever.
        let agent_state = match payload.get("agentState") {
            Some(Value::String(s)) => s.parse::<AgentState>().unwrap_or(AgentState::Unknown),
            _ => return ApplyOutcome::Ignored,
        };

        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            pane.agent_state = agent_state;
            pane.seen = seen;
        }
        ApplyOutcome::Applied
    }

    fn apply_pane_seen(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            pane.seen = true;
        }
        ApplyOutcome::Applied
    }

    fn apply_pane_title(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        let Some(title) = str_field(payload, "title") else {
            return ApplyOutcome::Ignored;
        };
        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            pane.title = title.to_string();
        }
        ApplyOutcome::Applied
    }

    /// Folds `pane.updated` — a title or color the *user* set through
    /// `pane.update`, as opposed to `pane.title`'s shell-emitted OSC name.
    ///
    /// Each field is applied only when the payload actually carries it: the
    /// runtime emits one event per field changed, so a color change arrives
    /// with no `title` key and must not blank the name. An empty `title` is
    /// meaningful rather than absent — it is how a rename is undone, handing
    /// the pane back to its OSC title — so it is applied like any other
    /// string, and only a *missing* key is ignored.
    ///
    /// A payload naming a pane this client has no record of is `Applied`, not
    /// `Ignored`: nothing was wrong with the event, there was simply nothing
    /// to fold it into, and reporting `Ignored` would suggest an unrecognised
    /// event type to a caller that treats the two differently.
    fn apply_pane_updated(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        let title = str_field(payload, "title").map(str::to_string);
        // `color` arrives as a string or as JSON null — the latter clears it.
        let color = payload.get("color").map(|value| {
            value
                .as_str()
                .filter(|hex| !hex.is_empty())
                .map(str::to_string)
        });
        if title.is_none() && color.is_none() {
            return ApplyOutcome::Ignored;
        }
        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            if let Some(title) = title {
                pane.title = title;
            }
            if let Some(color) = color {
                pane.color = color;
            }
        }
        ApplyOutcome::Applied
    }

    /// Shared by `pane.opened` and `pane.created`: the runtime emits both
    /// for a single pane spawn (`PaneStateStore.open()` then the
    /// `pane.create` handler in `bin/lib/runtime/index.js`) with the same
    /// `{paneId, workspaceId, agentType, cwd}` shape, so folding them
    /// identically keeps the model correct regardless of which arrives
    /// first — or if only one of the two ever does.
    fn apply_pane_opened_or_created(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        let Some(agent_type) = str_field(payload, "agentType") else {
            return ApplyOutcome::Ignored;
        };
        let Some(cwd) = str_field(payload, "cwd") else {
            return ApplyOutcome::Ignored;
        };
        let workspace_id = str_field(payload, "workspaceId").map(str::to_string);
        // A pane the web app added has a row but no process yet, and arrives
        // with `live: false`. Absent means live, as every runtime spawn is.
        let live = bool_field(payload, "live").unwrap_or(true);

        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            pane.workspace_id = workspace_id;
            pane.agent_type = agent_type.to_string();
            pane.cwd = cwd.to_string();
            // Only ever promotes: a repeated announcement of a running pane
            // must not mark it dead.
            pane.live = pane.live || live;
        } else {
            self.local_mut().panes.push(PaneInfo {
                id: pane_id.to_string(),
                workspace_id,
                agent_type: agent_type.to_string(),
                // Neither event's payload carries an agentState — a pane
                // that just opened hasn't reported one yet, exactly like
                // the runtime's own initial record (state.js: `agentState:
                // 'unknown'`).
                agent_state: AgentState::Unknown,
                seen: true,
                title: String::new(),
                cwd: cwd.to_string(),
                session_ref: None,
                live,
                color: None,
            });
        }
        ApplyOutcome::Applied
    }

    fn apply_pane_closed(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        // Keeps the record (unlike pane.removed) so a naturally-exited pane
        // still shows as recently exited, mirroring PaneStateStore.close().
        // The payload also carries `exitCode`, but PaneInfo has no field for
        // it — nothing in this client's interface surfaces exit codes, so it
        // is intentionally never read.
        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            pane.live = false;
        }
        ApplyOutcome::Applied
    }

    fn apply_pane_removed(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        self.local_mut().panes.retain(|p| p.id != pane_id);
        self.forget_focus_if_gone();
        ApplyOutcome::Applied
    }

    /// Clears `focused`/`detached_pane` if `focused` names a pane this
    /// model no longer has a record of at all, and clears `split` on the
    /// same condition.
    ///
    /// Called from the two events that *remove* pane records (`pane.removed`
    /// and `workspace.removed`) — not from `pane.closed`, which
    /// deliberately keeps the record so a naturally-exited pane's last
    /// frame stays on screen.
    ///
    /// `main.rs`'s `reattach_focused_pane` also clears a dangling `focused`,
    /// but only ever runs off the *pane socket's* channel closing — so a
    /// focused pane whose attach never succeeded (`detached_pane == true`, no
    /// socket, nothing to close) left `focused` pointing at a record that no
    /// longer exists: a dead pane's grid kept rendering and `PANE DISCONNECTED`
    /// stuck forever with no way to clear it.
    ///
    /// `notice` is cleared here too, and unconditionally — not only when
    /// `notice` happens to be *this* pane's own error. Originally that was
    /// exactly why it was safe to clear: clearing `focused` is precisely what
    /// stops anything else from ever clearing it, since the scheduled reattach
    /// that follows returns early on `focused == None` and never reaches
    /// `focus_pane`'s `notice = None`, while `render_status_bar` draws `notice`
    /// regardless of `focused`. Left here, the status bar went on advertising a
    /// removed pane's error until some unrelated pane happened to attach
    /// successfully.
    ///
    /// `notice` has since gained two more writers with nothing to do with
    /// the focused pane (a rejected `workspace.create`, and a refused split
    /// attach — see [`Model::notice`]'s own doc comment), so "this pane's
    /// own error" is no longer always true.
    ///
    /// This still clears **unconditionally** — it is the one clear site that
    /// does, now that the slot carries a [`NoticeOrigin`] and the two
    /// success paths use [`Model::clear_notice_from`] instead. That is a
    /// deliberate exception, not an oversight, and it rests on a different
    /// argument from theirs: those two are saying *"this failure is over"*,
    /// a claim about one domain only, whereas this one is saying *"the pane
    /// the status bar was hung off has ceased to exist"*, and it is the last
    /// thing that will ever run for it. Clearing `focused` is precisely what
    /// stops anything else from clearing the slot — the scheduled reattach
    /// returns early on `focused == None` and never reaches `focus_pane`'s
    /// clear, while `render_status_bar` draws the notice regardless of
    /// `focused`. A `SplitPane`- or `Workspace`-origin line wiped a little
    /// early here is a small, bounded price (both domains re-raise: the
    /// split ladder within a rung, a workspace create on the next attempt)
    /// next to a removed pane's error advertising itself for the rest of the
    /// session. Pinned by
    /// `removing_the_focused_pane_also_clears_an_unrelated_workspace_notice`
    /// (tests).
    ///
    /// [`Model::split`] gets the same treatment for the same reason, but
    /// none of the extra state: it has no `detached_pane` of its own (that
    /// flag is about the pane keystrokes go to, and none go here), and it
    /// does not clear `notice` — a split pane vanishing is not evidence
    /// that whatever the status bar is currently explaining stopped being
    /// true. Clearing it is what tells `main.rs`'s split-slot reattach to
    /// release the socket instead of climbing a ladder against a pane the
    /// server no longer has (see `main.rs::reattach_split_pane`), and what
    /// collapses the layout back to one full-width pane.
    fn forget_focus_if_gone(&mut self) {
        // Both questions are answered against the node's pane list *before*
        // any `Model` field is written, because the read borrows `self`
        // immutably through `local()` and the writes need it mutably. Two
        // booleans is the smallest thing that has to outlive the read — an
        // earlier draft cloned the whole pane vector here, which was a real
        // allocation on an event-folding path to avoid a borrow split that
        // costs nothing to do properly.
        let focus_gone = self
            .focused
            .as_deref()
            .is_some_and(|id| !self.local().panes.iter().any(|p| p.id == id));
        let split_gone = self
            .split
            .as_deref()
            .is_some_and(|id| !self.local().panes.iter().any(|p| p.id == id));
        if focus_gone {
            self.focused = None;
            self.detached_pane = false;
            self.notice = None;
        }
        if split_gone {
            self.split = None;
        }
    }

    fn apply_pane_moved(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(pane_id) = str_field(payload, "paneId") else {
            return ApplyOutcome::Ignored;
        };
        let Some(workspace_id) = str_field(payload, "workspaceId") else {
            return ApplyOutcome::Ignored;
        };
        if let Some(pane) = self.local_mut().panes.iter_mut().find(|p| p.id == pane_id) {
            pane.workspace_id = Some(workspace_id.to_string());
        }
        ApplyOutcome::Applied
    }

    fn apply_workspace_created(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(workspace_id) = str_field(payload, "workspaceId") else {
            return ApplyOutcome::Ignored;
        };
        let Some(name) = str_field(payload, "name") else {
            return ApplyOutcome::Ignored;
        };
        if let Some(ws) = self
            .local_mut()
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
        {
            ws.name = name.to_string();
        } else {
            self.local_mut().workspaces.push(WorkspaceInfo {
                git: None,
                id: workspace_id.to_string(),
                name: name.to_string(),
                // The wire event carries no color (see
                // `bin/lib/runtime/index.js`'s workspace.created emit).
                color: None,
                pane_ids: Vec::new(),
            });
        }
        ApplyOutcome::Applied
    }

    fn apply_workspace_updated(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(workspace_id) = str_field(payload, "workspaceId") else {
            return ApplyOutcome::Ignored;
        };
        let Some(ws) = self
            .local_mut()
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace_id)
        else {
            return ApplyOutcome::Applied;
        };
        if let Some(name) = str_field(payload, "name") {
            ws.name = name.to_string();
        }
        // `color` is optional and, unlike the other string fields here, an
        // explicit `null` is meaningful (clears the color) rather than
        // malformed — mirrors workspace.update's own permissiveness in
        // `bin/lib/runtime/index.js`. A present-but-wrong-typed value is
        // left alone rather than either clearing or erroring.
        match payload.get("color") {
            Some(Value::Null) => ws.color = None,
            Some(Value::String(s)) => ws.color = Some(s.clone()),
            _ => {}
        }
        // The space's working copy, refreshed by the runtime on its own clock.
        // `null` means "not a repository (any more)"; a malformed object is
        // ignored like a wrong-typed color.
        match payload.get("git") {
            Some(Value::Null) => ws.git = None,
            Some(value @ Value::Object(_)) => {
                if let Ok(git) = serde_json::from_value(value.clone()) {
                    ws.git = Some(git);
                }
            }
            _ => {}
        }
        ApplyOutcome::Applied
    }

    fn apply_workspace_removed(&mut self, payload: &Value) -> ApplyOutcome {
        let Some(workspace_id) = str_field(payload, "workspaceId") else {
            return ApplyOutcome::Ignored;
        };
        let node = self.local_mut();
        node.workspaces.retain(|w| w.id != workspace_id);
        // The payload also carries `closedPanes` (the ids the runtime
        // dropped), but this derives the same result directly from the
        // model's own state instead of trusting/parsing that array: every
        // pane this model still has on file for the removed workspace is,
        // by definition, one that no longer belongs anywhere.
        node.panes
            .retain(|p| p.workspace_id.as_deref() != Some(workspace_id));
        self.forget_focus_if_gone();
        ApplyOutcome::Applied
    }

    /// Tallies `(blocked, working, done)` across every **live** pane in
    /// every workspace, for the status bar. Dead panes are excluded for
    /// exactly the reasons spelled out on [`Model::rollup`] — without it
    /// the status bar reports "1 blocked" for a pane that exited an hour
    /// ago and isn't listed anywhere on screen.
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut blocked = 0;
        let mut working = 0;
        let mut done = 0;
        for pane in self.local_panes().iter().filter(|p| p.live) {
            match display_state(pane) {
                DisplayState::Blocked => blocked += 1,
                DisplayState::Working => working += 1,
                DisplayState::Done => done += 1,
                DisplayState::Idle | DisplayState::Unknown => {}
            }
        }
        (blocked, working, done)
    }

    /// The space whose panes the tab bar shows, in preference order:
    /// [`Model::selected_space`] if it still names a known space, else the
    /// focused pane's space, else the first space this client knows about.
    ///
    /// A fallback chain rather than an `Option`-on-focus, because the tab
    /// bar's `+` button has to create a pane *somewhere* — and a client that
    /// has just connected to a populated server has spaces but no focused
    /// pane yet, which is exactly the moment the button is most useful.
    /// Returns `None` only when there are no spaces at all; the tab bar
    /// draws nothing then, since there is no space to put a tab in.
    ///
    /// Both earlier steps are filtered against the known-space set, so a
    /// selection whose space was since removed, or a focused pane whose
    /// `workspace_id` names no known space (the "unassigned" case the
    /// sidebar already handles), falls through rather than yielding `None`.
    /// The tab bar must not vanish because one stale id outlived its space.
    pub fn active_space_id(&self) -> Option<&str> {
        let known = |id: &str| self.local_workspaces().iter().any(|w| w.id == id);
        self.selected_space
            .as_deref()
            .filter(|id| known(id))
            .or_else(|| {
                self.focused
                    .as_deref()
                    .and_then(|focused| self.local_panes().iter().find(|p| p.id == focused))
                    .and_then(|pane| pane.workspace_id.as_deref())
                    .filter(|id| known(id))
            })
            .or_else(|| self.local_workspaces().first().map(|w| w.id.as_str()))
    }

    /// Every live pane in `space_id`, in `panes` order — the tab bar's
    /// contents, and the only ordering it draws them in. `panes` order is
    /// what the split's left/right assignment already keys off (see
    /// [`Model::split_draws_left`]), so tabs and panes agree about which pane
    /// comes first without either consulting the other.
    pub fn live_panes_in(&self, space_id: &str) -> Vec<&PaneInfo> {
        self.local_panes()
            .iter()
            .filter(|p| p.live && p.workspace_id.as_deref() == Some(space_id))
            .collect()
    }

    /// The active space's Files tab and how it is shown, while it is.
    pub fn shown_files(&self) -> Option<(&str, crate::files::FilesLayout)> {
        let layout = self.files_shown?;
        let space = self.active_space_id()?;
        self.files.contains_key(space).then_some((space, layout))
    }

    /// Keys go to the Files tab rather than the focused terminal.
    pub fn files_have_keys(&self) -> bool {
        self.files_keys && self.shown_files().is_some()
    }

    /// Every stopped pane in `space_id` on this machine, in `panes` order.
    ///
    /// A server restart or a reboot stops every pane at once, and every view
    /// above lists only live ones, so a space whose panes all stopped looks
    /// empty. These are what selecting such a space starts again.
    pub fn stopped_panes_in(&self, space_id: &str) -> Vec<&PaneInfo> {
        self.local_panes()
            .iter()
            .filter(|p| !p.live && p.workspace_id.as_deref() == Some(space_id))
            .collect()
    }

    /// Every live pane, ordered for the sidebar's agents panel under `sort`.
    ///
    /// [`AgentSort::Grouped`] walks `workspaces` in order and emits each
    /// space's panes, then any pane whose `workspace_id` resolves to no known
    /// space — the same grouping, and the same orphan rule, the spaces list
    /// itself uses, so the two halves of the sidebar never disagree about
    /// which space a pane belongs to.
    ///
    /// [`AgentSort::Priority`] sorts by descending [`DisplayState::priority`]
    /// — blocked first, unknown last. The sort is **stable**, so panes that
    /// want equal attention keep `panes` order rather than shuffling between
    /// frames as unrelated state changes; a list that reorders under the
    /// cursor is a list you cannot click.
    pub fn agents_in_order(&self, sort: AgentSort) -> Vec<&PaneInfo> {
        match sort {
            AgentSort::Grouped => {
                let mut out: Vec<&PaneInfo> = Vec::new();
                for ws in self.local_workspaces() {
                    out.extend(self.live_panes_in(&ws.id));
                }
                let known: std::collections::HashSet<&str> = self
                    .local_workspaces()
                    .iter()
                    .map(|w| w.id.as_str())
                    .collect();
                out.extend(self.local_panes().iter().filter(|p| {
                    p.live
                        && !p
                            .workspace_id
                            .as_deref()
                            .is_some_and(|id| known.contains(id))
                }));
                out
            }
            AgentSort::Priority => {
                let mut out: Vec<&PaneInfo> =
                    self.local_panes().iter().filter(|p| p.live).collect();
                // `sort_by_key`, not `sort_unstable_by_key`: ties must keep
                // `panes` order (see the doc comment above). Negated so the
                // highest priority sorts first.
                out.sort_by_key(|p| std::cmp::Reverse(display_state(p).priority()));
                out
            }
        }
    }

    /// The name of the space a pane belongs to, or `None` for an orphan —
    /// what the agents panel prints beside each entry so a pane is
    /// identifiable when the list is sorted by priority and no longer
    /// grouped under a heading.
    /// The pane to focus when the user selects `space_id`: whichever pane was
    /// last focused there, or that space's first live pane if there is no
    /// usable memory of one.
    ///
    /// The remembered id is filtered at read time against *both* conditions
    /// that can invalidate it — the pane may have exited, and it may have been
    /// moved to another space by the tab menu — because either would otherwise
    /// send the user somewhere that is not in the space they just clicked.
    /// Filtering here rather than pruning on every pane event means there is
    /// one place to be correct instead of five.
    pub fn pane_to_focus_in(&self, space_id: &str) -> Option<String> {
        let live_here = |id: &str| {
            self.local_panes()
                .iter()
                .any(|p| p.id == id && p.live && p.workspace_id.as_deref() == Some(space_id))
        };
        self.local()
            .last_focused
            .get(space_id)
            .filter(|id| live_here(id))
            .cloned()
            .or_else(|| self.live_panes_in(space_id).first().map(|p| p.id.clone()))
    }

    pub fn space_name_of(&self, pane: &PaneInfo) -> Option<&str> {
        let id = pane.workspace_id.as_deref()?;
        self.local_workspaces()
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.name.as_str())
    }

    /// The live pane `direction` (`1` for next, `-1` for previous) away from
    /// `self.focused` in `self.panes`' own order, wrapping around. `None` if
    /// there are no live panes at all. Unfocused starts at the first pane
    /// for "next" and the last for "previous" — either direction from
    /// "nowhere" lands somewhere reasonable rather than requiring a pane to
    /// already be focused.
    ///
    /// `true` when [`Model::split`]'s pane should be drawn in the **left**
    /// region and [`Model::focused`]'s in the right — i.e. when the split
    /// pane comes first in `panes`.
    ///
    /// Left used to mean "the focused pane", full stop, which made
    /// *clicking* the right-hand pane relocate both panes' entire contents
    /// in a single frame: focusing it moved it into
    /// the `focused` field, and the field decided the side. That works
    /// against the feature's own purpose — a split exists so two agents can
    /// be watched at once, and telling two `claude` panes apart is a core
    /// concern of this client, so spatial identity is load-bearing here.
    /// Terminal multiplexers and editors conventionally keep positions fixed
    /// and move an *indicator* instead, and this client already has two
    /// (the cursor, and the sidebar's focus glyph).
    ///
    /// Deriving the side from `panes` order instead means a focus change
    /// swaps the two *fields* — and, with them, the two sockets, which is
    /// what avoids tearing down a working connection — while both panes
    /// stay exactly where they were on screen. The socket-to-field mapping
    /// stays fixed (`conn.focused` always serves `focused`); only the
    /// region assignment is order-derived.
    ///
    /// Falls back to `false` (focused on the left) whenever the ordering
    /// cannot be established: either slot empty, or either id absent from
    /// `panes` — which is a real, reachable state, since
    /// `Command::CreatePane` focuses the id `pane.create` returned before
    /// the matching `pane.created` event necessarily arrives.
    ///
    /// The one wart, accepted rather than hidden: `panes` order comes from
    /// the server, so a resnapshot that reorders it can still swap the two
    /// panes' sides. That is far rarer than a click, and unlike a click it
    /// is not something the user did.
    pub fn split_draws_left(&self) -> bool {
        let (Some(focused), Some(split)) = (self.focused.as_deref(), self.split.as_deref()) else {
            return false;
        };
        let position = |id: &str| self.local_panes().iter().position(|p| p.id == id);
        match (position(focused), position(split)) {
            (Some(focused_at), Some(split_at)) => split_at < focused_at,
            _ => false,
        }
    }

    /// Pure model navigation — moved here (from `main.rs`, where it first
    /// landed) so it can be tested directly rather than only indirectly through
    /// the event loop.
    pub fn adjacent_live_pane(&self, direction: i32) -> Option<String> {
        let live: Vec<&str> = self
            .local_panes()
            .iter()
            .filter(|p| p.live)
            .map(|p| p.id.as_str())
            .collect();
        if live.is_empty() {
            return None;
        }
        let current = self
            .focused
            .as_deref()
            .and_then(|id| live.iter().position(|p| *p == id));
        let next_index = match current {
            Some(i) => {
                let len = live.len() as i32;
                (((i as i32 + direction) % len + len) % len) as usize
            }
            None if direction >= 0 => 0,
            None => live.len() - 1,
        };
        live.get(next_index).map(|id| id.to_string())
    }
}

#[cfg(test)]
// These tests build a `Model::default()` and then assign
// the one or two fields each case cares about, which reads clearer here
// than clippy's suggested single struct-literal-with-..Default::default()
// once more than a couple of fields are involved.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    // --- Toasts from `pane.notify`. ---

    /// One space `api` with two `claude` panes, `p1` focused and `p2` titled
    /// `parser`.
    fn toast_model() -> Model {
        let mut m = Model::default();
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec![],
        }];
        let pane = |id: &str, title: &str| PaneInfo {
            id: id.into(),
            workspace_id: Some("w1".into()),
            agent_type: "claude".into(),
            agent_state: AgentState::Idle,
            seen: true,
            title: title.into(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        };
        m.local_mut().panes = vec![pane("p1", ""), pane("p2", "parser")];
        m.focused = Some("p1".into());
        m
    }

    fn notify(pane: &str, signal: &str) -> Value {
        serde_json::json!({
            "signal": signal,
            "paneId": pane,
            "agentType": "claude",
            "title": "",
        })
    }

    #[test]
    fn a_blocked_or_finished_pane_elsewhere_raises_a_toast() {
        let mut m = toast_model();
        let now = Instant::now();
        assert!(m.push_toast(&notify("p2", "needs_attention"), now));
        let toast = &m.toasts[0];
        assert_eq!(toast.pane_id, "p2");
        assert_eq!(toast.state, DisplayState::Blocked);
        assert_eq!(toast.pane, "parser", "a pane's title names it");
        assert_eq!(toast.space.as_deref(), Some("api"));
        assert_eq!(toast.expires_at, now + TOAST_TTL);

        assert!(m.push_toast(&notify("p2", "finished"), now));
        assert_eq!(m.toasts[0].state, DisplayState::Done);
        assert_eq!(m.toasts.len(), 1, "one toast per pane, the latest");
    }

    #[test]
    fn the_focused_pane_never_raises_a_toast() {
        let mut m = toast_model();
        assert!(!m.push_toast(&notify("p1", "needs_attention"), Instant::now()));
        assert!(m.toasts.is_empty());
    }

    #[test]
    fn a_pane_this_client_does_not_know_is_named_from_the_payload() {
        let mut m = toast_model();
        let payload = serde_json::json!({
            "signal": "finished", "paneId": "elsewhere", "agentType": "codex", "title": "",
        });
        assert!(m.push_toast(&payload, Instant::now()));
        assert_eq!(m.toasts[0].pane, "codex");
        assert_eq!(m.toasts[0].space, None);
    }

    #[test]
    fn toasts_stack_newest_first_and_at_most_three() {
        let mut m = toast_model();
        let now = Instant::now();
        for id in ["a", "b", "c", "d"] {
            m.push_toast(&notify(id, "finished"), now);
        }
        let ids: Vec<&str> = m.toasts.iter().map(|t| t.pane_id.as_str()).collect();
        assert_eq!(ids, vec!["d", "c", "b"]);
        assert_eq!(MAX_TOASTS, 3);
    }

    #[test]
    fn toasts_expire_after_their_time_and_say_so_only_when_one_did() {
        let mut m = toast_model();
        let now = Instant::now();
        m.push_toast(&notify("p2", "finished"), now);
        assert!(!m.expire_toasts(now + TOAST_TTL - Duration::from_millis(1)));
        assert_eq!(m.toasts.len(), 1);
        assert!(m.expire_toasts(now + TOAST_TTL));
        assert!(m.toasts.is_empty());
        assert!(
            !m.expire_toasts(now + TOAST_TTL * 2),
            "nothing left to expire"
        );
    }

    #[test]
    fn a_malformed_notification_raises_nothing_and_never_panics() {
        let mut m = toast_model();
        for payload in [
            serde_json::json!({}),
            serde_json::json!({"paneId": "p2"}),
            serde_json::json!({"paneId": "p2", "signal": "exploded"}),
            serde_json::json!({"paneId": 7, "signal": "finished"}),
            serde_json::json!(null),
        ] {
            assert!(!m.push_toast(&payload, Instant::now()), "{payload}");
        }
        assert!(m.toasts.is_empty());
    }

    #[test]
    fn a_notification_still_advances_seq_like_any_known_event() {
        let mut m = toast_model();
        let outcome = m.apply_event(&event(1, "pane.notify", notify("p2", "finished")));
        assert_eq!(outcome, ApplyOutcome::Applied);
    }

    // --- Collapsing the sidebar. ---

    /// A sidebar that has seen a terminal `width` columns wide.
    fn sidebar_on(width: u16) -> SidebarState {
        let mut s = SidebarState::default();
        s.note_screen_width(width);
        s
    }

    #[test]
    fn the_sidebar_collapses_by_itself_on_a_narrow_terminal() {
        let s = sidebar_on(79);
        assert!(s.is_collapsed(79));
        assert_eq!(s.columns(79), 0, "the panes take the full width");
        let s = sidebar_on(80);
        assert!(!s.is_collapsed(80));
        assert_eq!(s.columns(80), crate::ui::sidebar::WIDTH);
    }

    #[test]
    fn toggling_overrides_the_automatic_state_either_way() {
        let mut s = sidebar_on(120);
        s.toggle_collapsed();
        assert_eq!(s.columns(120), 0, "hidden on a wide terminal");
        s.toggle_collapsed();
        assert_eq!(s.columns(120), crate::ui::sidebar::WIDTH, "and back");

        let mut s = sidebar_on(60);
        s.toggle_collapsed();
        assert!(!s.is_collapsed(60), "shown on a narrow one");
        assert_eq!(s.columns(60), crate::ui::sidebar::WIDTH.min(30));
    }

    #[test]
    fn the_override_lasts_until_a_resize_crosses_the_threshold() {
        let mut s = sidebar_on(120);
        s.toggle_collapsed();
        s.note_screen_width(100);
        assert!(s.is_collapsed(100), "a resize on the same side keeps it");
        s.note_screen_width(70);
        assert!(s.is_collapsed(70), "narrow: automatic, which also hides");
        assert_eq!(s.collapsed, None, "the override is gone");
        s.note_screen_width(120);
        assert!(!s.is_collapsed(120), "so widening brings the sidebar back");

        let mut s = sidebar_on(60);
        s.toggle_collapsed();
        s.note_screen_width(65);
        assert!(!s.is_collapsed(65));
        s.note_screen_width(90);
        assert_eq!(s.collapsed, None);
        s.note_screen_width(60);
        assert!(s.is_collapsed(60), "crossing back down collapses it again");
    }

    #[test]
    fn a_dragged_width_survives_hiding_and_showing() {
        let mut s = sidebar_on(120);
        s.width = 44;
        s.toggle_collapsed();
        s.toggle_collapsed();
        assert_eq!(s.columns(120), 44);
    }

    #[test]
    fn a_space_is_owned_by_the_machine_it_lives_on() {
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.local_mut().workspaces = vec![workspace_named("w1", "api")];
        m.nodes.get_mut(&NodeId::new("gpu")).unwrap().workspaces =
            vec![workspace_named("w9", "trainer")];

        assert_eq!(m.owner_of_space("w9"), Some(NodeId::new("gpu")));
        assert_eq!(
            m.owner_of_space("w1"),
            None,
            "local reports None, not its own id"
        );
        assert_eq!(m.owner_of_space("nonexistent"), None);
    }

    #[test]
    fn a_colliding_space_id_resolves_to_the_local_machine() {
        // Both machines have "w1". Creating into it must not silently target
        // the peer — the local machine wins, which is what the sidebar showed
        // when the user picked it.
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.local_mut().workspaces = vec![workspace_named("w1", "api")];
        m.nodes.get_mut(&NodeId::new("gpu")).unwrap().workspaces =
            vec![workspace_named("w1", "also-api")];
        assert_eq!(m.owner_of_space("w1"), None);
    }

    #[test]
    fn a_directory_suggestion_comes_from_the_machine_it_is_for() {
        // Offering this machine's paths for a pane starting elsewhere is worse
        // than offering nothing: they look plausible and are wrong.
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.local_mut().home_dir = "/home/local".into();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
        m.local_mut().panes[0].cwd = "/local/api".into();

        let gpu = NodeId::new("gpu");
        {
            let peer = m.nodes.get_mut(&gpu).unwrap();
            peer.home_dir = "/home/dev".into();
            peer.panes = vec![pane("p9", "w9", AgentState::Idle, true)];
            peer.panes[0].cwd = "/srv/training".into();
        }

        assert_eq!(m.suggested_cwd_on(&gpu).as_deref(), Some("/srv/training"));
        assert_eq!(
            m.suggested_cwd_on(&m.local_id_owned()).as_deref(),
            Some("/local/api")
        );
    }

    #[test]
    fn a_machine_with_no_panes_suggests_its_own_home() {
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        let gpu = NodeId::new("gpu");
        m.nodes.get_mut(&gpu).unwrap().home_dir = "/home/dev".into();
        assert_eq!(m.suggested_cwd_on(&gpu).as_deref(), Some("/home/dev"));
    }

    #[test]
    fn a_machine_that_reported_nothing_suggests_nothing() {
        // Better an empty prompt than a path invented from this machine.
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        assert_eq!(m.suggested_cwd_on(&NodeId::new("gpu")), None);
    }

    #[test]
    fn a_quiet_machine_is_not_a_broken_one() {
        // Links are dialled lazily, so a machine nobody is looking at has no
        // link — that is the resting state, not a fault. Reporting it as
        // offline made a healthy peer look down, and refreshing could not
        // correct it because nothing had gone wrong.
        assert_eq!(conn_from_status("idle", true), ConnState::Idle);
        assert_eq!(conn_from_status("unknown", true), ConnState::Idle);
        assert_eq!(conn_from_status("online", true), ConnState::Direct);
        assert_eq!(conn_from_status("connecting", true), ConnState::Connecting);
        assert_eq!(
            conn_from_status("offline", true),
            ConnState::Offline("offline".into())
        );
    }

    #[test]
    fn every_paired_machine_is_worth_dialling_including_a_failed_one() {
        // Idle is the common case — everything paired sits idle until wanted.
        // Offline counts too: retrying it is what a refresh is for, and its
        // status cannot leave Offline unless something dials.
        assert!(ConnState::Idle.is_dialable());
        assert!(ConnState::Direct.is_dialable());
        assert!(ConnState::Connecting.is_dialable());
        assert!(ConnState::Offline("x".into()).is_dialable());
        assert!(!ConnState::Unpaired.is_dialable(), "no key to connect with");
    }

    // --- a peer's own snapshot ---

    fn snapshot_of(node: &str, space: &str) -> Snapshot {
        Snapshot {
            protocol_version: 1.into(),
            node_id: node.into(),
            seq: 9,
            workspaces: vec![WorkspaceInfo {
                git: None,
                id: space.into(),
                name: "trainer".into(),
                color: None,
                pane_ids: vec![],
            }],
            panes: vec![pane("p9", space, AgentState::Blocked, true)],
        }
    }

    #[test]
    fn a_peers_snapshot_lands_under_that_peer() {
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];

        m.apply_peer_snapshot(&NodeId::new("gpu"), snapshot_of("gpu", "w9"));

        let peer = m.node(&NodeId::new("gpu")).unwrap();
        assert_eq!(peer.panes.len(), 1);
        assert_eq!(peer.seq, 9);
        assert_eq!(m.local_panes().len(), 1, "the local machine is untouched");
        assert_eq!(m.local_panes()[0].id, "p1");
    }

    #[test]
    fn a_colliding_space_id_does_not_overwrite_the_other_machine() {
        // Two machines can each have a space called "w1". Folding a peer's
        // snapshot through the local path would silently replace one with the
        // other; this is why apply_peer_snapshot exists separately.
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec![],
        }];

        m.apply_peer_snapshot(&NodeId::new("gpu"), snapshot_of("gpu", "w1"));

        assert_eq!(m.local_workspaces()[0].name, "api");
        assert_eq!(
            m.node(&NodeId::new("gpu")).unwrap().workspaces[0].name,
            "trainer"
        );
    }

    #[test]
    fn a_snapshot_for_a_departed_machine_is_dropped() {
        // Re-adding it here would resurrect a machine the server already said
        // is gone.
        let mut m = Model::new();
        m.apply_peers(&[net_node("me", "laptop", true)]);
        m.apply_peer_snapshot(&NodeId::new("gpu"), snapshot_of("gpu", "w9"));
        assert_eq!(m.nodes.len(), 1);
    }

    #[test]
    fn a_peers_snapshot_clears_its_staleness() {
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.nodes.get_mut(&NodeId::new("gpu")).unwrap().stale = true;
        m.apply_peer_snapshot(&NodeId::new("gpu"), snapshot_of("gpu", "w9"));
        assert!(!m.node(&NodeId::new("gpu")).unwrap().stale);
    }

    // --- the fleet, learnt from net.peers ---

    fn net_node(id: &str, name: &str, is_self: bool) -> spaces_proto::NetNode {
        spaces_proto::NetNode {
            node_id: id.into(),
            name: name.into(),
            url: String::new(),
            status: if is_self {
                "online".into()
            } else {
                "unknown".into()
            },
            paired: !is_self,
            discovered_via: "manual".into(),
            version: None,
            last_seen: None,
            key_hint: None,
            self_: is_self,
        }
    }

    #[test]
    fn the_local_node_adopts_the_id_the_server_reports() {
        // Model::new starts with a placeholder id because the real one is not
        // known until the server answers. Adopting it must carry the spaces
        // and panes across — they arrived over the very connection being
        // renamed, so losing them would blank the sidebar on connect.
        let mut m = Model::new();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
        m.apply_peers(&[net_node("real-uuid", "laptop", true)]);

        assert_eq!(m.local().node_id, "real-uuid");
        assert_eq!(m.local().name, "laptop");
        assert_eq!(m.local_panes().len(), 1, "state moved with the identity");
        assert_eq!(m.target_node(), m.local_id_owned());
    }

    #[test]
    fn peers_are_added_with_their_reported_connection_state() {
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        assert_eq!(m.nodes.len(), 2);
        let peer = m.node(&NodeId::new("gpu")).expect("peer present");
        assert_eq!(peer.name, "gpu-box");
        // Paired with no link open yet: idle, not connecting — nothing is in
        // flight, and nothing has failed.
        assert_eq!(peer.conn, ConnState::Idle);
    }

    #[test]
    fn an_unpaired_machine_is_listed_but_marked_unreachable() {
        let mut m = Model::new();
        let mut seen = net_node("studio", "studio-pc", false);
        seen.paired = false;
        seen.status = "unpaired".into();
        m.apply_peers(&[net_node("me", "laptop", true), seen]);
        assert_eq!(
            m.node(&NodeId::new("studio")).unwrap().conn,
            ConnState::Unpaired
        );
    }

    #[test]
    fn a_peers_own_state_survives_a_refresh() {
        // net.peers reports identity and status, not contents. A refresh must
        // not throw away spaces already learnt from that machine.
        let mut m = Model::new();
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        m.nodes.get_mut(&NodeId::new("gpu")).unwrap().panes =
            vec![pane("p9", "w9", AgentState::Working, true)];
        m.apply_peers(&[
            net_node("me", "laptop", true),
            net_node("gpu", "gpu-box", false),
        ]);
        assert_eq!(m.node(&NodeId::new("gpu")).unwrap().panes.len(), 1);
    }

    // --- the node map (federation phase 1) ---

    #[test]
    fn the_local_node_is_reached_through_the_map_like_any_other() {
        // Not a tautology: it pins that `local()` is a map lookup rather than
        // a separate field, which is what stops a later phase growing a
        // special case for it.
        let mut m = Model::new();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
        let id = m.local_id_owned();
        assert_eq!(m.node(&id).expect("local is in the map").panes.len(), 1);
    }

    #[test]
    fn a_second_nodes_panes_do_not_leak_into_the_local_view() {
        let mut m = Model::new();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
        let peer = NodeId::new("peer");
        let mut state = NodeState::new("peer", "gpu-box");
        state.panes = vec![pane("p9", "w9", AgentState::Blocked, true)];
        m.nodes.insert(peer, state);
        assert_eq!(m.local_panes().len(), 1, "local still sees only its own");
    }

    #[test]
    fn each_node_numbers_its_own_event_stream() {
        // The reason NodeState exists: a gap on one machine says nothing
        // about any other, so `seq` and `stale` cannot be fleet-wide.
        let mut m = Model::new();
        m.local_mut().seq = 7;
        let peer = NodeId::new("peer");
        m.nodes
            .insert(peer.clone(), NodeState::new("peer", "gpu-box"));
        assert_eq!(m.node(&peer).unwrap().seq, 0);
        assert_eq!(m.local().seq, 7);
    }

    fn workspace_named(id: &str, name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            git: None,
            id: id.into(),
            name: name.into(),
            color: None,
            pane_ids: Vec::new(),
        }
    }

    /// One event frame at `seq`, for the folding cases below.
    fn event(seq: u64, kind: &str, payload: Value) -> EventFrameInner {
        EventFrameInner {
            seq,
            r#type: kind.into(),
            payload,
            ts: 0.0,
        }
    }

    // --- `pane.updated`: a title or color the *user* set, as opposed to
    // `pane.title`'s shell-emitted OSC name. ---

    #[test]
    fn a_user_set_title_and_color_are_folded_from_pane_updated() {
        // The runtime emits one event per field actually changed, so a color
        // change carries no `title` key and must not blank the name.
        let mut m = Model::new();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];

        let payload = serde_json::json!({"paneId":"p1","title":"the parser"});
        assert_eq!(
            m.apply_event(&event(1, "pane.updated", payload)),
            ApplyOutcome::Applied
        );
        assert_eq!(m.local_panes()[0].title, "the parser");
        assert_eq!(m.local_panes()[0].color, None);

        let payload = serde_json::json!({"paneId":"p1","color":"#ef4444"});
        assert_eq!(
            m.apply_event(&event(2, "pane.updated", payload)),
            ApplyOutcome::Applied
        );
        assert_eq!(m.local_panes()[0].title, "the parser", "the name survived");
        assert_eq!(m.local_panes()[0].color.as_deref(), Some("#ef4444"));
    }

    #[test]
    fn clearing_a_pane_title_hands_it_back_to_the_shell() {
        // An empty title is meaningful, not absent — it is the only way to
        // undo a rename — so it is applied rather than ignored.
        let mut m = Model::new();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
        m.local_mut().panes[0].title = "mine".into();
        let payload = serde_json::json!({"paneId":"p1","title":""});
        m.apply_event(&event(1, "pane.updated", payload));
        assert_eq!(m.local_panes()[0].title, "");
    }

    #[test]
    fn a_pane_updated_carrying_nothing_usable_is_ignored() {
        let mut m = Model::new();
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
        let payload = serde_json::json!({"paneId":"p1"});
        assert_eq!(
            m.apply_event(&event(1, "pane.updated", payload)),
            ApplyOutcome::Ignored
        );
        let payload = serde_json::json!({"color":"#fff"});
        assert_eq!(
            m.apply_event(&event(2, "pane.updated", payload)),
            ApplyOutcome::Ignored,
            "and so is one naming no pane at all"
        );
    }

    #[test]
    fn a_null_or_empty_color_clears_the_pane_accent() {
        // The runtime stores a cleared color as `''` (the column is NOT NULL);
        // either spelling has to reach the model as "no accent".
        for cleared in [serde_json::json!(null), serde_json::json!("")] {
            let mut m = Model::new();
            m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, true)];
            m.local_mut().panes[0].color = Some("#ef4444".into());
            let payload = serde_json::json!({"paneId":"p1","color": cleared});
            m.apply_event(&event(1, "pane.updated", payload));
            assert_eq!(m.local_panes()[0].color, None);
        }
    }

    fn pane(id: &str, ws: &str, state: AgentState, seen: bool) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: Some(ws.into()),
            agent_type: "claude".into(),
            agent_state: state,
            seen,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live: true,
            color: None,
        }
    }

    #[test]
    fn done_is_an_unseen_idle_pane() {
        assert_eq!(
            display_state(&pane("p", "w1", AgentState::Idle, false)),
            DisplayState::Done
        );
        assert_eq!(
            display_state(&pane("p", "w1", AgentState::Idle, true)),
            DisplayState::Idle
        );
        assert_eq!(
            display_state(&pane("p", "w1", AgentState::Blocked, false)),
            DisplayState::Blocked
        );
        assert_eq!(
            display_state(&pane("p", "w1", AgentState::Working, true)),
            DisplayState::Working
        );
        // A closed AgentState enum has no "zzz" variant to construct here —
        // the equivalent unrecognized-string case is exercised where it
        // actually enters the system: as a `pane.state` event payload, in
        // `pane_state_with_unrecognized_agent_state_string_maps_to_unknown_but_still_applies`.
        assert_eq!(
            display_state(&pane("p", "w1", AgentState::Unknown, true)),
            DisplayState::Unknown
        );
    }

    #[test]
    fn rollup_prefers_the_loudest_state() {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            pane("a", "w1", AgentState::Idle, true),
            pane("b", "w1", AgentState::Idle, false),
            pane("c", "w1", AgentState::Working, true),
        ];
        assert_eq!(m.local().rollup("w1"), DisplayState::Working);
        m.local_mut()
            .panes
            .push(pane("d", "w1", AgentState::Blocked, true));
        assert_eq!(m.local().rollup("w1"), DisplayState::Blocked);
        assert_eq!(m.local().rollup("nonexistent"), DisplayState::Unknown);
    }

    #[test]
    fn counts_tally_the_status_bar_numbers() {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            pane("a", "w1", AgentState::Blocked, true),
            pane("b", "w1", AgentState::Working, true),
            pane("c", "w1", AgentState::Idle, false),
            pane("d", "w1", AgentState::Idle, true),
        ];
        assert_eq!(m.counts(), (1, 1, 1));
    }

    // --- rollup/counts exclude dead panes (`apply_pane_closed` keeps a
    // closed pane's last agent_state and only flips `live`, and
    // `session.snapshot` returns `live: false` panes carrying one too). ---

    #[test]
    fn a_dead_blocked_pane_contributes_to_neither_the_rollup_nor_the_counts() {
        let mut m = Model::default();
        let mut dead = pane("gone", "w1", AgentState::Blocked, true);
        dead.live = false;
        m.local_mut().panes = vec![dead];
        assert_eq!(
            m.local().rollup("w1"),
            DisplayState::Unknown,
            "a workspace whose only pane exited must not keep its badge"
        );
        assert_eq!(
            m.counts(),
            (0, 0, 0),
            "the status bar must not tally a pane that no longer exists"
        );
    }

    #[test]
    fn a_pane_that_exits_while_blocked_stops_counting_from_that_moment() {
        // The exact sequence that triggered the bug, driven through the
        // event that actually produces it rather than a hand-built model.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Blocked, true)];
        assert_eq!(m.local().rollup("w1"), DisplayState::Blocked);
        assert_eq!(m.counts(), (1, 0, 0));

        let closed = EventFrameInner {
            seq: 1,
            r#type: "pane.closed".into(),
            payload: serde_json::json!({"paneId":"p1","exitCode":1}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&closed), ApplyOutcome::Applied);
        assert_eq!(m.local().rollup("w1"), DisplayState::Unknown);
        assert_eq!(m.counts(), (0, 0, 0));
    }

    #[test]
    fn a_dead_pane_does_not_drown_out_a_live_one_in_the_rollup() {
        let mut m = Model::default();
        let mut dead = pane("dead", "w1", AgentState::Blocked, true);
        dead.live = false;
        m.local_mut().panes = vec![dead, pane("live", "w1", AgentState::Idle, true)];
        assert_eq!(m.local().rollup("w1"), DisplayState::Idle);
        assert_eq!(m.counts(), (0, 0, 0));
    }

    // --- focused is cleared when the pane record itself disappears. ---

    #[test]
    fn pane_removed_clears_a_focus_pointing_at_it() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        m.focused = Some("p1".into());
        m.detached_pane = true;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.focused, None, "focus must not dangle on a gone pane");
        assert!(!m.detached_pane);
    }

    // --- The notice slot's origin.
    //
    // `notice` had three writers, one of them repeating, and two clear-sites
    // that retracted lines from domains they knew nothing about. These pin
    // the primitive both success clear-sites in `main.rs` now go through;
    // the sites themselves are pinned there. ---

    const ORIGINS: [NoticeOrigin; 3] = [
        NoticeOrigin::FocusedPane,
        NoticeOrigin::SplitPane,
        NoticeOrigin::Workspace,
    ];

    #[test]
    fn a_clear_retracts_only_its_own_domains_line() {
        // Every ordered pair, so no single origin can quietly become the one
        // that clears (or fails to clear) everything.
        for wrote in ORIGINS {
            for clears in ORIGINS {
                let mut m = Model::default();
                m.set_notice(wrote, "something went wrong".into());
                m.clear_notice_from(clears);
                if wrote == clears {
                    assert_eq!(m.notice, None, "{wrote:?} must retract its own line");
                } else {
                    assert_eq!(
                        m.notice_text(),
                        Some("something went wrong"),
                        "{clears:?} must not retract {wrote:?}'s line"
                    );
                }
            }
        }
    }

    #[test]
    fn a_focused_attach_success_leaves_a_split_panes_error_standing() {
        // The scenario, at the level the primitive decides it: `main.rs`'s
        // `focus_pane` success calls exactly this. Unconditional, it
        // retracted a `SPLIT PANE ERROR` that was still true — and the split
        // slot's own ladder re-raised it within a rung, so the line
        // flickered rather than staying readable.
        let mut m = Model::default();
        m.set_notice(
            NoticeOrigin::SplitPane,
            "SPLIT PANE ERROR (codex): no response within 2s".into(),
        );
        m.clear_notice_from(NoticeOrigin::FocusedPane);
        assert!(m.notice.is_some(), "the split slot's failure is still true");
    }

    #[test]
    fn a_split_attach_success_leaves_the_focused_panes_error_standing() {
        // The mirror image, and the reachable one: both slots stream at
        // once, so a `PANE ERROR` on the focused socket and a split attach
        // completing milliseconds later is an ordinary interleaving.
        let mut m = Model::default();
        m.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (claude, conflict): taken over".into(),
        );
        m.clear_notice_from(NoticeOrigin::SplitPane);
        assert_eq!(
            m.notice_text(),
            Some("PANE ERROR (claude, conflict): taken over")
        );
    }

    #[test]
    fn setting_is_still_last_write_wins_across_domains() {
        // Deliberately unchanged by the origin tag: the most recent failure
        // is what the status bar shows, whichever domain it came from.
        let mut m = Model::default();
        m.set_notice(NoticeOrigin::Workspace, "WORKSPACE ERROR: nope".into());
        m.set_notice(NoticeOrigin::SplitPane, "SPLIT PANE ERROR: nope".into());
        assert_eq!(m.notice_text(), Some("SPLIT PANE ERROR: nope"));
        assert_eq!(m.notice.map(|(o, _)| o), Some(NoticeOrigin::SplitPane));
    }

    #[test]
    fn clearing_an_empty_slot_is_a_no_op() {
        let mut m = Model::default();
        for origin in ORIGINS {
            m.clear_notice_from(origin);
        }
        assert_eq!(m.notice, None);
        assert_eq!(m.notice_text(), None);
    }

    #[test]
    fn removing_the_focused_pane_also_clears_its_error_notice() {
        // Clearing `focused` is what stops anything else from ever
        // clearing `notice` — the scheduled reattach returns early with no
        // focus and never reaches `focus_pane`'s reset — so the status bar
        // kept advertising a removed pane's error indefinitely.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        m.focused = Some("p1".into());
        m.detached_pane = true;
        m.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (unauthorized): token expired".into(),
        );
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.notice, None, "a gone pane's error must not linger");
    }

    #[test]
    fn removing_the_focused_pane_also_clears_an_unrelated_workspace_notice() {
        // `notice` gained a second writer (`main.rs`'s
        // `Command::CreateWorkspace` dispatch) in a domain that has nothing to
        // do with panes. The slot now *does* carry its origin and the two
        // success clear-sites consult it — but `forget_focus_if_gone`
        // deliberately remains the one that clears unconditionally; see both
        // `notice`'s and `forget_focus_if_gone`'s own doc comments for why the
        // argument is different there.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        m.focused = Some("p1".into());
        m.set_notice(
            NoticeOrigin::Workspace,
            "WORKSPACE ERROR: bad_request: name is required".into(),
        );
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(
            m.notice, None,
            "the shared slot clears regardless of which domain wrote it"
        );
    }

    // --- Which side each visible pane is drawn on comes from `panes`
    // order, not from which field holds its id — so a focus change never
    // relocates either pane's content. ---

    /// `p1`, `p2`, `p3` live, in that order.
    fn three_panes() -> Model {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
            pane("p3", "w1", AgentState::Working, true),
        ];
        m
    }

    #[test]
    fn the_earlier_pane_draws_on_the_left_whichever_field_holds_it() {
        // The whole point: the *same* pair of panes must map to the same
        // pair of sides before and after the focus moves between them.
        let mut m = three_panes();
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        assert!(!m.split_draws_left(), "p1 is earlier, so p1 is on the left");

        m.focused = Some("p2".into());
        m.split = Some("p1".into());
        assert!(
            m.split_draws_left(),
            "focusing p2 must not move it — p1 is still the earlier pane"
        );
    }

    #[test]
    fn an_unsplit_model_never_reports_a_left_hand_split() {
        let mut m = three_panes();
        m.focused = Some("p1".into());
        assert!(!m.split_draws_left());
    }

    #[test]
    fn nothing_focused_leaves_the_split_on_the_right() {
        // Degenerate but representable. The fallback keeps the focused
        // region (blank here) where it always is rather than inventing an
        // order from a single id.
        let mut m = three_panes();
        m.split = Some("p2".into());
        assert!(!m.split_draws_left());
    }

    #[test]
    fn a_pane_the_model_has_not_heard_of_yet_falls_back_to_focused_on_the_left() {
        // `Command::CreatePane` focuses the id `pane.create` returned before
        // the matching `pane.created` event necessarily arrives, so one of
        // the two ids genuinely can be absent from `panes`.
        let mut m = three_panes();
        m.focused = Some("brand-new".into());
        m.split = Some("p1".into());
        assert!(!m.split_draws_left());

        m.focused = Some("p1".into());
        m.split = Some("brand-new".into());
        assert!(!m.split_draws_left());
    }

    #[test]
    fn a_dead_pane_still_has_a_side() {
        // `split_draws_left` orders by record position, not by `live` — a
        // pane that exited keeps its last frame on screen, and that frame
        // must not jump to the other half when it does.
        let mut m = three_panes();
        m.local_mut().panes[0].live = false;
        m.focused = Some("p2".into());
        m.split = Some("p1".into());
        assert!(m.split_draws_left());
    }

    // --- `split` is forgotten on the same condition `focused` is —
    // a pane whose *record* vanished, not one that merely closed. ---

    #[test]
    fn removing_the_split_pane_clears_the_split() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p2"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.split, None, "the split must not dangle on a gone pane");
        assert_eq!(m.focused.as_deref(), Some("p1"), "focus is unaffected");
    }

    #[test]
    fn removing_the_split_pane_leaves_the_status_bar_notice_alone() {
        // Unlike the focused pane, whose removal clears `notice` (see
        // `forget_focus_if_gone`'s own doc comment), a split pane vanishing
        // is not evidence that whatever the status bar is explaining
        // stopped being true.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        m.set_notice(
            NoticeOrigin::Workspace,
            "WORKSPACE ERROR: bad_request: name is required".into(),
        );
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p2"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert!(m.notice.is_some());
    }

    #[test]
    fn a_split_pane_that_merely_closed_keeps_its_slot() {
        // `pane.closed` keeps the record precisely so a naturally-exited
        // pane's last frame stays readable — collapsing the layout out from
        // under the user would throw that away.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.closed".into(),
            payload: serde_json::json!({"paneId":"p2"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.split.as_deref(), Some("p2"));
    }

    #[test]
    fn removing_a_whole_workspace_clears_a_split_pointing_into_it() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec![],
        }];
        m.local_mut().panes = vec![
            pane("p1", "w2", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let ev = EventFrameInner {
            seq: 1,
            r#type: "workspace.removed".into(),
            payload: serde_json::json!({"workspaceId":"w1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.split, None);
        assert_eq!(m.focused.as_deref(), Some("p1"));
    }

    #[test]
    fn a_split_on_a_surviving_pane_is_left_alone() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
            pane("p3", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p3"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.split.as_deref(), Some("p2"));
    }

    #[test]
    fn a_snapshot_leaves_the_split_alone_like_every_other_local_slot() {
        // Presentation state: `focused`, `sidebar_scroll` and `split` all
        // survive a resnapshot, because none of them ever came from the
        // server in the first place.
        let mut m = Model::default();
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        m.apply_snapshot(Snapshot {
            protocol_version: 1.0,
            node_id: "n".into(),
            seq: 7,
            workspaces: vec![],
            panes: vec![],
        });
        assert_eq!(m.focused.as_deref(), Some("p1"));
        assert_eq!(m.split.as_deref(), Some("p2"));
    }

    #[test]
    fn a_notice_survives_the_removal_of_some_other_pane() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p2".into());
        m.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (conflict): taken over".into(),
        );
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert!(
            m.notice.is_some(),
            "the focused pane's error is still current"
        );
    }

    #[test]
    fn pane_removed_leaves_a_focus_on_a_different_pane_alone() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![
            pane("p1", "w1", AgentState::Working, true),
            pane("p2", "w1", AgentState::Working, true),
        ];
        m.focused = Some("p2".into());
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.focused.as_deref(), Some("p2"));
    }

    #[test]
    fn workspace_removed_clears_a_focus_on_one_of_its_panes() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "WS".into(),
            color: None,
            pane_ids: vec!["p1".into()],
        }];
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        m.focused = Some("p1".into());
        m.detached_pane = true;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "workspace.removed".into(),
            payload: serde_json::json!({"workspaceId":"w1","closedPanes":["p1"]}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.focused, None);
        assert!(!m.detached_pane);
    }

    #[test]
    fn pane_closed_keeps_the_focus_since_it_keeps_the_record() {
        // `pane.closed` deliberately keeps a naturally-exited pane's record
        // (and its last frame on screen) — only *removal* clears focus.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        m.focused = Some("p1".into());
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.closed".into(),
            payload: serde_json::json!({"paneId":"p1","exitCode":0}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.focused.as_deref(), Some("p1"));
    }

    #[test]
    fn an_in_order_event_applies() {
        let mut m = Model::default();
        m.local_mut().seq = 4;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 5,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"p1","agentState":"blocked","seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.local().seq, 5);
        assert_eq!(display_state(&m.local_panes()[0]), DisplayState::Blocked);
    }

    #[test]
    fn a_sequence_gap_demands_a_resnapshot() {
        let mut m = Model::default();
        m.local_mut().seq = 4;
        let ev = EventFrameInner {
            seq: 9,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"p1","agentState":"idle","seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::NeedsResnapshot);
        assert!(m.local().stale);
        assert_eq!(m.local().seq, 4, "a gapped event must not advance seq");
    }

    #[test]
    fn seq_at_u64_max_does_not_overflow_and_gap_detects_instead_of_panicking() {
        let mut m = Model::default();
        m.local_mut().seq = u64::MAX;
        let ev = EventFrameInner {
            // Whatever seq a real next event would carry, it can't equal
            // `u64::MAX + 1` (that value doesn't exist) — every event at
            // this boundary is definitionally a gap.
            seq: 0,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"p1","agentState":"idle","seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::NeedsResnapshot);
        assert!(m.local().stale);
        assert_eq!(
            m.local().seq,
            u64::MAX,
            "a gapped event must not advance seq"
        );
    }

    #[test]
    fn pane_lifecycle_events_add_and_remove() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let created = EventFrameInner {
            seq: 1,
            r#type: "pane.created".into(),
            payload: serde_json::json!({"paneId":"p9","workspaceId":"w1","agentType":"codex","cwd":"/tmp"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&created), ApplyOutcome::Applied);
        assert!(m.local_panes().iter().any(|p| p.id == "p9"));

        let removed = EventFrameInner {
            seq: 2,
            r#type: "pane.removed".into(),
            payload: serde_json::json!({"paneId":"p9"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&removed), ApplyOutcome::Applied);
        assert!(!m.local_panes().iter().any(|p| p.id == "p9"));
    }

    #[test]
    fn an_unknown_event_type_is_ignored_and_still_advances_seq() {
        let mut m = Model::default();
        m.local_mut().seq = 1;
        let ev = EventFrameInner {
            seq: 2,
            r#type: "some.future.event".into(),
            payload: serde_json::json!({}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Ignored);
        assert_eq!(
            m.local().seq,
            2,
            "an unknown but in-order event must not desync the stream"
        );
    }

    #[test]
    fn a_malformed_payload_does_not_panic() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.state".into(),
            payload: serde_json::json!("not an object"),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Ignored);
    }

    // --- An unrecognized agent-state string ("zzz"), exercised at the
    // event boundary where a raw string actually arrives. ---

    #[test]
    fn pane_state_with_unrecognized_agent_state_string_maps_to_unknown_but_still_applies() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"p1","agentState":"zzz","seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.local_panes()[0].agent_state, AgentState::Unknown);
        assert_eq!(display_state(&m.local_panes()[0]), DisplayState::Unknown);
    }

    #[test]
    fn pane_state_missing_agent_state_field_is_ignored() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"p1","seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Ignored);
        assert_eq!(
            m.local_panes()[0].agent_state,
            AgentState::Working,
            "an ignored event must not mutate state"
        );
    }

    #[test]
    fn pane_state_non_string_agent_state_is_ignored() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"p1","agentState":42,"seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Ignored);
        assert_eq!(m.local_panes()[0].agent_state, AgentState::Working);
    }

    #[test]
    fn pane_state_for_an_untracked_pane_id_applies_as_a_no_op() {
        // Pins the deliberate choice that a well-formed event about a pane
        // this model doesn't currently track still returns `Applied`, not
        // `Ignored` — the frame itself was understood, there's just nothing
        // to mutate.
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.state".into(),
            payload: serde_json::json!({"paneId":"unknown-pane","agentState":"blocked","seen":true}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert!(m.local_panes().is_empty(), "no pane existed to mutate");
    }

    #[test]
    fn pane_created_missing_required_field_is_ignored() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.created".into(),
            payload: serde_json::json!({"paneId":"p9"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Ignored);
        assert!(m.local_panes().is_empty());
    }

    #[test]
    fn pane_opened_creates_a_new_pane_when_absent() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.opened".into(),
            payload: serde_json::json!({"paneId":"p1","workspaceId":"w1","agentType":"claude","cwd":"/tmp"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        let p = m
            .local_panes()
            .iter()
            .find(|p| p.id == "p1")
            .expect("pane inserted");
        assert_eq!(p.agent_state, AgentState::Unknown);
        assert!(p.seen);
        assert!(p.live);
        assert_eq!(p.workspace_id.as_deref(), Some("w1"));
        assert_eq!(p.agent_type, "claude");
        assert_eq!(p.cwd, "/tmp");
    }

    #[test]
    fn pane_closed_marks_pane_not_live_without_removing_it() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.closed".into(),
            payload: serde_json::json!({"paneId":"p1","exitCode":0}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        let p = m
            .local_panes()
            .iter()
            .find(|p| p.id == "p1")
            .expect("pane still present");
        assert!(
            !p.live,
            "pane.closed keeps the record but marks it not live"
        );
    }

    #[test]
    fn pane_seen_marks_pane_seen() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Idle, false)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.seen".into(),
            payload: serde_json::json!({"paneId":"p1"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert!(m.local_panes()[0].seen);
        assert_eq!(display_state(&m.local_panes()[0]), DisplayState::Idle);
    }

    #[test]
    fn pane_title_updates_title() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.title".into(),
            payload: serde_json::json!({"paneId":"p1","title":"new title"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.local_panes()[0].title, "new title");
    }

    #[test]
    fn pane_moved_updates_workspace_id() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "pane.moved".into(),
            payload: serde_json::json!({"paneId":"p1","workspaceId":"w2"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.local_panes()[0].workspace_id.as_deref(), Some("w2"));
    }

    #[test]
    fn workspace_created_adds_a_workspace() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let ev = EventFrameInner {
            seq: 1,
            r#type: "workspace.created".into(),
            payload: serde_json::json!({"workspaceId":"w9","name":"New WS"}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        let ws = m
            .local_workspaces()
            .iter()
            .find(|w| w.id == "w9")
            .expect("workspace inserted");
        assert_eq!(ws.name, "New WS");
    }

    #[test]
    fn workspace_updated_changes_name_and_can_clear_color() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "Old".into(),
            color: Some("#fff".into()),
            pane_ids: vec![],
        }];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "workspace.updated".into(),
            payload: serde_json::json!({"workspaceId":"w1","name":"Renamed","color":null}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert_eq!(m.local_workspaces()[0].name, "Renamed");
        assert_eq!(m.local_workspaces()[0].color, None);
    }

    #[test]
    fn workspace_updated_carries_git_status_and_null_clears_it() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "WS".into(),
            color: None,
            pane_ids: vec![],
        }];
        let git = serde_json::json!({"branch":"main","ahead":2,"behind":0,"dirty":true});
        let ev = |seq, git: serde_json::Value| EventFrameInner {
            seq,
            r#type: "workspace.updated".into(),
            payload: serde_json::json!({"workspaceId":"w1","git":git}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev(1, git)), ApplyOutcome::Applied);
        let status = m.local_workspaces()[0].git.clone().expect("git folded");
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!((status.ahead, status.behind, status.dirty), (2, 0, true));
        assert_eq!(m.local_workspaces()[0].name, "WS", "other fields untouched");
        assert_eq!(
            m.apply_event(&ev(2, serde_json::Value::Null)),
            ApplyOutcome::Applied
        );
        assert!(m.local_workspaces()[0].git.is_none());
    }

    #[test]
    fn a_pane_created_as_not_live_stays_not_live_and_cannot_demote_a_live_one() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        let created = |seq, id: &str| EventFrameInner {
            seq,
            r#type: "pane.created".into(),
            payload: serde_json::json!({
                "paneId": id, "workspaceId": "w1", "agentType": "shell", "cwd": "/x", "live": false
            }),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&created(1, "p9")), ApplyOutcome::Applied);
        assert!(!m.local_panes().iter().find(|p| p.id == "p9").unwrap().live);
        m.local_mut()
            .panes
            .push(pane("p1", "w1", AgentState::Working, true));
        assert_eq!(m.apply_event(&created(2, "p1")), ApplyOutcome::Applied);
        assert!(m.local_panes().iter().find(|p| p.id == "p1").unwrap().live);
    }

    #[test]
    fn workspace_removed_drops_workspace_and_its_panes() {
        let mut m = Model::default();
        m.local_mut().seq = 0;
        m.local_mut().workspaces = vec![WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "WS".into(),
            color: None,
            pane_ids: vec!["p1".into()],
        }];
        m.local_mut().panes = vec![pane("p1", "w1", AgentState::Working, true)];
        let ev = EventFrameInner {
            seq: 1,
            r#type: "workspace.removed".into(),
            payload: serde_json::json!({"workspaceId":"w1","closedPanes":["p1"]}),
            ts: 0.0,
        };
        assert_eq!(m.apply_event(&ev), ApplyOutcome::Applied);
        assert!(!m.local_workspaces().iter().any(|w| w.id == "w1"));
        assert!(!m.local_panes().iter().any(|p| p.id == "p1"));
    }

    #[test]
    fn apply_snapshot_replaces_state_and_clears_stale() {
        let mut m = Model::default();
        m.local_mut().stale = true;
        let snap = Snapshot {
            node_id: "n1".into(),
            panes: vec![pane("p1", "w1", AgentState::Working, true)],
            protocol_version: 1.0,
            seq: 42,
            workspaces: vec![WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "WS".into(),
                color: None,
                pane_ids: vec!["p1".into()],
            }],
        };
        m.apply_snapshot(snap);
        assert_eq!(m.local().node_id, "n1");
        assert_eq!(m.local().seq, 42);
        assert!(!m.local().stale);
        assert_eq!(m.local_panes().len(), 1);
        assert_eq!(m.local_workspaces().len(), 1);
    }

    #[test]
    fn display_state_priority_and_label_match_the_node_reference() {
        assert!(DisplayState::Blocked.priority() > DisplayState::Working.priority());
        assert!(DisplayState::Working.priority() > DisplayState::Done.priority());
        assert!(DisplayState::Done.priority() > DisplayState::Idle.priority());
        assert!(DisplayState::Idle.priority() > DisplayState::Unknown.priority());
        assert_eq!(DisplayState::Blocked.priority(), 5);
        assert_eq!(DisplayState::Unknown.priority(), 1);
        assert_eq!(DisplayState::Blocked.label(), "Needs you");
        assert_eq!(DisplayState::Working.label(), "Working");
        assert_eq!(DisplayState::Done.label(), "Done");
        assert_eq!(DisplayState::Idle.label(), "Idle");
        assert_eq!(DisplayState::Unknown.label(), "\u{2014}");
    }

    // --- Model::adjacent_live_pane (pure model navigation belongs here,
    // where it can be tested directly). ---

    fn live_pane(id: &str, live: bool) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            workspace_id: None,
            agent_type: "claude".into(),
            agent_state: AgentState::Unknown,
            seen: true,
            title: String::new(),
            cwd: String::new(),
            session_ref: None,
            live,
            color: None,
        }
    }

    #[test]
    fn next_pane_wraps_around() {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            live_pane("a", true),
            live_pane("b", true),
            live_pane("c", true),
        ];
        m.focused = Some("c".into());
        assert_eq!(m.adjacent_live_pane(1).as_deref(), Some("a"));
    }

    #[test]
    fn prev_pane_wraps_around() {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            live_pane("a", true),
            live_pane("b", true),
            live_pane("c", true),
        ];
        m.focused = Some("a".into());
        assert_eq!(m.adjacent_live_pane(-1).as_deref(), Some("c"));
    }

    #[test]
    fn dead_panes_are_never_a_navigation_target() {
        let mut m = Model::default();
        m.local_mut().panes = vec![
            live_pane("a", true),
            live_pane("b", false),
            live_pane("c", true),
        ];
        m.focused = Some("a".into());
        assert_eq!(m.adjacent_live_pane(1).as_deref(), Some("c"));
    }

    #[test]
    fn no_live_panes_navigates_nowhere() {
        let mut m = Model::default();
        m.local_mut().panes = vec![live_pane("a", false)];
        assert_eq!(m.adjacent_live_pane(1), None);
    }

    #[test]
    fn nothing_focused_next_lands_on_the_first_live_pane() {
        let mut m = Model::default();
        m.local_mut().panes = vec![live_pane("a", true), live_pane("b", true)];
        assert_eq!(m.adjacent_live_pane(1).as_deref(), Some("a"));
    }

    #[test]
    fn nothing_focused_prev_lands_on_the_last_live_pane() {
        let mut m = Model::default();
        m.local_mut().panes = vec![live_pane("a", true), live_pane("b", true)];
        assert_eq!(m.adjacent_live_pane(-1).as_deref(), Some("b"));
    }

    #[cfg(test)]
    mod sanitize_tests {
        use super::*;

        #[test]
        fn escapes_in_server_strings_never_reach_the_screen() {
            let mut m = Model::default();
            m.apply_snapshot(Snapshot {
                protocol_version: 1.0,
                node_id: "n".into(),
                seq: 0,
                workspaces: vec![spaces_proto::WorkspaceInfo {
                    color: None,
                    git: Some(spaces_proto::WorkspaceInfoGit {
                        ahead: 0,
                        behind: 0,
                        branch: Some("main\u{1b}]52;c;aGk=\u{7}".into()),
                        dirty: false,
                    }),
                    id: "w1".into(),
                    name: "ops\u{1b}[2J".into(),
                    pane_ids: vec![],
                }],
                panes: vec![spaces_proto::PaneInfo {
                    id: "p1".into(),
                    workspace_id: Some("w1".into()),
                    agent_type: "claude".into(),
                    agent_state: spaces_proto::AgentState::Idle,
                    seen: true,
                    title: "build\u{9b}31m \u{202E}evil".into(),
                    cwd: "/srv".into(),
                    session_ref: None,
                    live: true,
                    color: None,
                }],
            });
            let w = &m.local().workspaces[0];
            let p = &m.local().panes[0];
            for s in [
                w.name.as_str(),
                p.title.as_str(),
                w.git.as_ref().unwrap().branch.as_deref().unwrap(),
            ] {
                assert!(!crate::files::needs_sanitizing(s), "{s:?}");
            }
            assert!(w.name.starts_with("ops"));
            assert_eq!(p.cwd, "/srv", "ordinary text is untouched");
        }
    }
}
