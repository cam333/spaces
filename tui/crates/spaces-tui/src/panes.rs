//! The pane-connection subsystem: which panes have a data socket open,
//! keeping those sockets sized to the regions they are drawn in, and getting
//! them back after a failure. Moved out of `main.rs` as a pure,
//! behaviour-free extraction; the doc comments on [`PaneConn`], [`Reattach`]
//! and [`on_slot_event`] carry the invariants.

use std::collections::HashMap;
use std::time::Duration;

use ratatui::layout::Rect;
use tokio::sync::mpsc;

use spaces_proto::pane::{PaneEvent, PaneMode, PaneSocket};

use crate::model::{Model, NoticeOrigin};
use crate::term::PaneTerm;
use crate::ui;
use crate::{
    sanitize_notice, take_and_advance, PaneCtx, PANE_ATTACH_TIMEOUT, RECONNECT_INITIAL_BACKOFF,
};

/// When the next pane-socket reattach is due, plus the backoff ladder that
/// spaces successive attempts out.
///
/// `reattach_focused_pane` used to be called *synchronously* from the
/// pane-socket-close arm, with no delay, no attempt counter, and no ceiling.
/// That is fine for the case it was written for (a transport hiccup on a pane
/// that's still alive), and a hot spin for the case nobody looked at: the
/// runtime accepts the WebSocket upgrade and *then* refuses the attach with an
/// `unauthorized`/`not_found`/`conflict` error frame before closing
/// (`bin/lib/runtime/pane-socket.js`). `PaneSocket::attach` returns `Ok`, so
/// the close arrives as a channel close, which reattaches, which attaches
/// successfully, which is refused again — as fast as the network allows,
/// forever, invisibly.
///
/// Unlike the control channel's reconnect ladder there is no zero-delay
/// first attempt: a transport-level attach *success* is exactly what the
/// refusal case looks like, so this path cannot tell a hiccup from a
/// refusal at schedule time and always pays at least the first 250 ms rung.
/// [`Reattach::reset`] is the escape hatch, and it deliberately keys on
/// evidence the attachment actually works (an inbound frame) rather than
/// on `attach` returning `Ok` — see [`attachment_is_working`].
pub(crate) struct Reattach {
    /// Absolute deadline, so [`reattach_due`]'s future can be reconstructed
    /// on every `select!` iteration without restarting the wait — the same
    /// cancel-safety problem [`recv_join`](crate::recv_join)'s doc comment describes.
    pub(crate) at: Option<tokio::time::Instant>,
    backoff: Duration,
}

impl Default for Reattach {
    fn default() -> Self {
        Self {
            at: None,
            backoff: RECONNECT_INITIAL_BACKOFF,
        }
    }
}

impl Reattach {
    /// Schedules the next attempt one backoff rung out, and advances the
    /// ladder (250ms -> 500ms -> ... -> 8s, capped) for the attempt after
    /// that — same [`take_and_advance`] the control channel uses, so the
    /// two ladders can't drift apart.
    pub(crate) fn schedule(&mut self) {
        let delay = take_and_advance(&mut self.backoff);
        self.at = Some(tokio::time::Instant::now() + delay);
    }

    /// Drops back to the first rung. Called when something proves this
    /// client's pane attachments are working again — an inbound pane frame
    /// ([`attachment_is_working`]), a fresh control channel, or the user
    /// explicitly asking for a pane.
    pub(crate) fn reset(&mut self) {
        self.backoff = RECONNECT_INITIAL_BACKOFF;
    }

    /// [`Reattach::reset`], plus cancelling any attempt already scheduled.
    /// For when there is no longer anything to reattach *to* — the split
    /// was turned off, its pane was removed, or a fresh control channel
    /// made every rung climbed against the old one meaningless. Leaving
    /// `at` set instead would fire one pointless wakeup into a handler that
    /// can only decide it has nothing to do.
    pub(crate) fn stand_down(&mut self) {
        self.at = None;
        self.backoff = RECONNECT_INITIAL_BACKOFF;
    }
}

/// Which of [`PaneConn`]'s two slots a pane-socket event, or a scheduled
/// reattach, belongs to. The two `select!` arms for each are otherwise
/// identical, so this is what lets them share one handler instead of
/// becoming two copies that drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    Focused,
    Split,
}

/// Resolves when a scheduled reattach is due, and never resolves when none
/// is scheduled — the same "optional `select!` source" pattern as
/// [`recv_join`](crate::recv_join). Takes the deadline *by value* (`Instant` is `Copy`) so
/// this branch doesn't hold a borrow its own handler needs back.
pub(crate) async fn reattach_due(at: Option<tokio::time::Instant>) {
    match at {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// `true` for the pane events that prove an attachment is actually
/// functioning, as opposed to merely having connected.
///
/// This, not `PaneSocket::attach` returning `Ok`, is what resets the
/// reattach backoff. The distinction is the whole point of [`Reattach`]:
/// the server refuses an attach *after* the upgrade completes, so `Ok` is
/// exactly as likely to precede a refusal as a working pane. A replay
/// marker or a byte, on the other hand, only ever comes from a pane the
/// server actually let this client have.
fn attachment_is_working(event: &PaneEvent) -> bool {
    matches!(
        event,
        PaneEvent::Bytes(_) | PaneEvent::ReplayStart | PaneEvent::ReplayEnd
    )
}

// ─── Pane-socket management ─────────────────────────────────────────────

/// One pane's data socket. Two of these are open at once when the view is
/// split (see [`PaneConn`]) and one otherwise — never more: agent/detection
/// state always arrives over the control channel regardless of which panes
/// are attached, so there is nothing to gain from keeping every
/// previously-viewed pane's socket alive, only connections to manage.
/// Re-focusing a pane just re-attaches (the server replays its current
/// screen state on every attach, so nothing is lost).
#[derive(Default)]
pub(crate) struct PaneSlot {
    socket: Option<PaneSocket>,
    rx: Option<mpsc::Receiver<PaneEvent>>,
    attached_to: Option<String>,
}

impl PaneSlot {
    /// Drops the current attachment, if any (closing the socket via
    /// `PaneSocket`'s own `Drop`).
    fn detach(&mut self) {
        self.socket = None;
        self.rx = None;
        self.attached_to = None;
    }

    /// Attaches to `pane_id`'s data socket in `Control` mode with takeover
    /// — this client exists to let a user type into their agent, so it
    /// always claims the controller slot rather than attaching read-only.
    /// A no-op if already attached to `pane_id`. Replaces any existing
    /// attachment to a *different* pane.
    async fn focus(
        &mut self,
        base_url: &str,
        pane_id: &str,
        token: Option<&str>,
        node: Option<&str>,
    ) -> Result<(), String> {
        if self.attached_to.as_deref() == Some(pane_id) {
            return Ok(());
        }
        let mut socket = PaneSocket::attach_on(
            base_url,
            pane_id,
            PaneMode::Control { takeover: true },
            token,
            node,
        )
        .await
        .map_err(|e| e.to_string())?;
        let rx = socket.events();
        self.socket = Some(socket);
        self.rx = Some(rx);
        self.attached_to = Some(pane_id.to_string());
        Ok(())
    }

    /// Awaits the next event on the current attachment, or never resolves
    /// if nothing is attached — see [`recv_join`](crate::recv_join)'s doc comment for the
    /// same pattern.
    ///
    /// Cancel-safe, which is what makes it usable directly as a `select!`
    /// branch expression: `mpsc::Receiver::recv` is documented cancel-safe
    /// (no message is lost if the future is dropped before it resolves),
    /// and the `None` arm is `pending()`, which has no state to lose. It
    /// borrows `self` for the life of the future rather than taking
    /// anything out of it — the distinction [`recv_join`](crate::recv_join)'s doc comment
    /// spells out at length.
    pub(crate) async fn recv(&mut self) -> Option<PaneEvent> {
        match &mut self.rx {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    }

    pub(crate) async fn send_input(&mut self, bytes: Vec<u8>) {
        // route()'s `encode_pane_input` only ever produces printable UTF-8
        // or single-byte ASCII control codes/escape sequences — always
        // valid UTF-8 by construction — but this is terminal input, so it
        // degrades (drops the keystroke) rather than unwrapping regardless.
        let Ok(text) = String::from_utf8(bytes) else {
            return;
        };
        if let Some(socket) = self.socket.as_mut() {
            let _ = socket.send_input(&text).await;
        }
    }

    async fn resize(&mut self, cols: u16, rows: u16) {
        if let Some(socket) = self.socket.as_mut() {
            let _ = socket.send_resize(cols as u32, rows as u32).await;
        }
    }
}

/// The pane data sockets this client has open: [`PaneConn::focused`] serves
/// [`Model::focused`] and [`PaneConn::split`] serves [`Model::split`].
///
/// Two *named fields*, not an array or a map, and this is load-bearing
/// rather than stylistic: `run`'s `select!` builds every branch future up
/// front, so the two `recv()` branches hold `&mut` on their slots
/// simultaneously. The borrow checker can prove `conn.focused` and
/// `conn.split` are disjoint places; it cannot prove that of `slots[0]` and
/// `slots[1]`, and a runtime index would not compile there at all.
///
/// The field-to-model-slot mapping is fixed, so nothing ever has to search
/// for "the slot showing pane X". When the user focuses the pane currently
/// in the split slot, the two `Model` fields swap and [`PaneConn::swap`]
/// swaps these with them — the alternative (re-deriving which socket serves
/// which role) would mean tearing down and rebuilding two perfectly good
/// WebSocket connections for a focus change.
#[derive(Default)]
pub(crate) struct PaneConn {
    pub(crate) focused: PaneSlot,
    pub(crate) split: PaneSlot,
}

impl PaneConn {
    /// Exchanges the two slots — see this struct's own doc comment.
    /// Synchronous and infallible, so a caller can run it before its first
    /// `.await` and know a cancellation can never leave the model's two
    /// fields swapped while the sockets behind them are not.
    fn swap(&mut self) {
        std::mem::swap(&mut self.focused, &mut self.split);
    }

    /// Drops both attachments. Used when the control channel itself goes
    /// away: every pane socket rides on the same runtime, so a lost control
    /// channel means neither of these is worth keeping either.
    pub(crate) fn detach_all(&mut self) {
        self.focused.detach();
        self.split.detach();
    }

    /// `true` iff either slot currently has `pane_id`'s socket open — the
    /// check [`prune_terms`] needs so a grid being actively written to is
    /// never freed out from under the socket writing it.
    fn is_attached_to(&self, pane_id: &str) -> bool {
        self.focused.attached_to.as_deref() == Some(pane_id)
            || self.split.attached_to.as_deref() == Some(pane_id)
    }
}

/// The two slots' reattach ladders (see [`Reattach`]), kept as one struct of
/// loop-local state rather than as a field on [`PaneSlot`].
///
/// That placement is forced, not preferred. `run`'s `select!` needs to read
/// a deadline (`reattach_due(reattach.focused.at)`, an immutable read) in
/// one branch expression while another branch expression holds `&mut
/// conn.focused` for its `recv()` future — and every branch expression in a
/// `select!` is evaluated before any of them is polled. Living inside the
/// slot, `conn.focused.reattach.at` and `conn.focused.recv()` would be a
/// borrow of the same place, and the loop would not compile. Living here,
/// `reattach` and `conn` are two disjoint locals and both branches are
/// fine. The single-pane loop had exactly this shape already for its one
/// ladder; this only widens it to two.
#[derive(Default)]
pub(crate) struct Reattachers {
    pub(crate) focused: Reattach,
    pub(crate) split: Reattach,
}

/// The `(cols, rows)` the focused and split panes' grids and PTYs should be
/// sized to — the single place a pane's *region* is turned into a *size*, so
/// the socket resize, the `PaneTerm` resize and the rectangle the renderer
/// draws into cannot drift apart.
///
/// Takes the whole `model`, not a `split: bool`, because *which* region each
/// pane occupies is order-derived (see [`ui::visible_pane_regions`]) and the
/// sizes have to follow the same rule the renderer used. Getting that wrong
/// is not cosmetic: on an odd-width pane area the two halves differ by a
/// column, so a size taken from the wrong half tells a real PTY it is one
/// column narrower or wider than the cells it is being drawn into.
///
/// The focused size is clamped up to at least 1x1 (a terminal narrower than
/// the sidebar leaves a zero-width pane area — see `ui::frame`'s own doc
/// comment — and `PaneTerm::resize`/`PaneSocket::send_resize` each clamp
/// internally too; doing it here as well means two callees never have to
/// silently agree).
///
/// The split size, in contrast, is `None` rather than clamped when there is
/// no room for a second region. That asymmetry is deliberate: a `None` here
/// means "do not send a resize", whereas clamping would send a real remote
/// PTY a 1x1 resize and reflow its output to a single column — destroying
/// the very screen the split exists to show — every time the user dragged
/// their terminal narrow.
fn slot_sizes(pane_area: Rect, model: &Model) -> ((u16, u16), Option<(u16, u16)>) {
    let (focused, split_region) = ui::visible_pane_regions(pane_area, model);
    (
        (focused.width.max(1), focused.height.max(1)),
        split_region.map(|r| (r.width.max(1), r.height.max(1))),
    )
}

/// Folds one pane-socket event — or that socket's channel closing (`None`)
/// — from `slot` into the model, the terminal grids, and that slot's own
/// reattach ladder.
///
/// Synchronous on purpose. It is shared verbatim by the two `select!` arms
/// that differ only in which slot they read, and being synchronous means
/// neither arm can be cancelled part-way through it: there is no `.await`
/// inside for a competing branch to interrupt, so "detached, flagged, and
/// rescheduled" is always all three or none.
pub(crate) fn on_slot_event(
    slot: Slot,
    event: Option<PaneEvent>,
    conn: &mut PaneConn,
    reattach: &mut Reattachers,
    model: &mut Model,
    terms: &mut HashMap<String, PaneTerm>,
) {
    let (slot_conn, ladder) = match slot {
        Slot::Focused => (&mut conn.focused, &mut reattach.focused),
        Slot::Split => (&mut conn.split, &mut reattach.split),
    };
    // Which domain an error frame from this slot belongs to. Derived from
    // the slot rather than from `model.focused`/`model.split`, so it stays
    // right for a frame that arrives in the window between a swap and the
    // model settling — the socket is the thing that knows where the frame
    // came from.
    let origin = match slot {
        Slot::Focused => NoticeOrigin::FocusedPane,
        Slot::Split => NoticeOrigin::SplitPane,
    };
    match event {
        Some(event) => {
            // Per slot, and keyed on an inbound *frame* rather than on
            // `attach` having returned `Ok` — see `attachment_is_working`.
            // Keyed on the wrong thing, or shared between the two slots,
            // this is where the unbounded reattach loop described on
            // `Reattach` comes back.
            if attachment_is_working(&event) {
                ladder.reset();
            }
            // Bound to a local first: the call reads `model` immutably to
            // name the pane, and the write below needs it mutably.
            let notice = handle_pane_event(event, slot_conn.attached_to.as_deref(), model, terms);
            if let Some(notice) = notice {
                model.set_notice(origin, notice);
            }
        }
        None => {
            // Only reachable once this slot had something attached and its
            // channel then closed (pane exited, or the socket errored —
            // including `PaneEvent::Error { code: "conflict" }` when another
            // controller takes over). `PaneSlot::recv` never resolves at all
            // while nothing is attached, so this doesn't fire repeatedly
            // once already detached.
            slot_conn.detach();
            // Say "no live socket" now rather than waiting for `focus_pane`
            // to set it, which is not immediate: the reattach below is
            // scheduled, not inline. Without this the status bar would claim
            // everything is fine for up to a full backoff rung. Only the
            // focused slot has a flag to set — `detached_pane` is about the
            // pane keystrokes go to, and none go to the split one.
            if slot == Slot::Focused && model.focused.is_some() {
                model.detached_pane = true;
            }
            // The pane itself may still be alive per the control channel
            // even though its socket just died — reattach rather than
            // leaving the model pointed at a pane with no live socket — but
            // *on this slot's own ladder*, because a server that refuses the
            // attach after accepting the upgrade turns "reattach
            // immediately" into an unbounded hot loop (see `Reattach`).
            ladder.schedule();
        }
    }
}

/// Applies one pane-socket event to `terms`, returning a status-bar notice
/// when the event was something the user needs told about.
///
/// `ReplayStart`/`ReplayEnd` are pure sequencing markers — every `Bytes`
/// event, whether it's the replay dump or live streaming output, is fed
/// into the terminal grid identically (`PaneTerm::feed` parses whatever
/// bytes it's given the same way regardless of why the server sent them).
/// `attached_to` being `None`, or not matching an id `terms` already has an
/// entry for, only happens for an event that arrived in the brief window
/// between detaching and this branch next being polled — dropped rather
/// than misattributed to the wrong pane.
///
/// `Exit` needs no notice: the server closes the socket right after it, and
/// `pane.closed` on the *control* channel updates the sidebar with the same
/// fact, more accurately.
///
/// `Error` does. This arm was once empty, on the reasoning that the control
/// channel reports everything worth reporting. True for `Exit`; false for
/// `unauthorized`, `not_found` and `conflict`, which are only ever sent on
/// *this* socket and are the difference between "the pane is quiet" and "the
/// server will never let you have this pane". The message is sanitized here
/// rather than at the draw site because this is the boundary the untrusted
/// string crosses — see [`sanitize_notice`].
///
/// The notice **names the pane**. Either slot can write to the single shared
/// `model.notice`, so with two panes on screen a bare `PANE ERROR (conflict):
/// taken over by another client` left the user no way to tell which of the two
/// agents it was about. `model` is read for that name only, and only on the
/// `Error` arm — the per-byte `Bytes` path allocates nothing extra.
fn handle_pane_event(
    event: PaneEvent,
    attached_to: Option<&str>,
    model: &Model,
    terms: &mut HashMap<String, PaneTerm>,
) -> Option<String> {
    let pane_id = attached_to?;
    match event {
        PaneEvent::Bytes(bytes) => {
            terms
                .entry(pane_id.to_string())
                .or_insert_with(|| PaneTerm::new(1, 1))
                .feed(&bytes);
            None
        }
        PaneEvent::ReplayStart | PaneEvent::ReplayEnd | PaneEvent::Exit { .. } => None,
        PaneEvent::Error { code, message } => Some(sanitize_notice(&format!(
            "PANE ERROR ({}, {code}): {message}",
            pane_display_name(model, pane_id)
        ))),
    }
}

/// Drops `terms` entries for panes the model no longer has any record of.
///
/// Without this, nothing ever removed from this map.
/// Each [`PaneTerm`] owns an `alacritty_terminal::Term` with
/// `Config::default()`'s 10,000-line scrollback — tens of megabytes for a
/// chatty pane at 200 columns — and it survived `pane.closed`,
/// `pane.removed` and `workspace.removed` alike, for the lifetime of the
/// process.
///
/// Deliberately keyed on the pane *record* existing, not on `live`:
/// `pane.closed` keeps the record precisely so a naturally-exited pane's
/// last frame stays readable, and throwing its grid away would blank the
/// screen the user is still looking at. Only `pane.removed` and
/// `workspace.removed` (which drop the record) free anything.
///
/// The focused/attached pane is never pruned even if the model doesn't know
/// about it yet: `Command::CreatePane` focuses the id `pane.create` returned
/// before the corresponding `pane.created` event necessarily arrives, and
/// dropping that grid mid-flight would leave `handle_pane_event`'s defensive
/// 1x1 fallback to recreate it at the wrong size. The split pane and
/// *either* socket's attachment are protected on the same grounds — a grid
/// a live socket is still writing into must never be freed out from under
/// it (`conn.is_attached_to`, which covers both slots rather than the one id
/// this used to be handed).
pub(crate) fn prune_terms(model: &Model, conn: &PaneConn, terms: &mut HashMap<String, PaneTerm>) {
    terms.retain(|id, _| {
        model.local_panes().iter().any(|p| &p.id == id)
            || conn.is_attached_to(id)
            || model.focused.as_deref() == Some(id.as_str())
            || model.split.as_deref() == Some(id.as_str())
    });
}

/// Points `model.focused` at `pane_id` and flags the pane as having no live
/// socket — every state change [`focus_pane`] makes *before* it awaits
/// anything, in one synchronous function.
///
/// Its own function, rather than the first few lines of `focus_pane`, for
/// the same reason `take_and_advance` is one: the property that matters
/// here is an ordering, and an ordering that only exists as "these
/// statements happen to sit above that `.await`" has nothing to fail
/// against when someone later moves a line. As a separate call,
/// `focus_pane` reads as *one synchronous statement, then awaits*, and the
/// tests below can pin the whole preamble without a server.
///
/// Two things happen here, both pessimistically:
///
/// - **The swap.** When `pane_id` is the pane currently in the *split*
///   slot, the two model fields, the two sockets and the two reattach
///   ladders are exchanged rather than the split slot's perfectly good
///   socket being torn down and re-opened in the other slot. That keeps the
///   fixed slot-to-field mapping [`PaneConn`] documents intact
///   (`conn.focused` always serves `model.focused`) at the cost of the two
///   panes trading places on screen, which is the visible meaning of "the
///   pane you are typing into is always the left one".
/// - **`detached_pane = true`,** written here and cleared only once an attach
///   actually succeeds — not set in a failure branch reached *after* awaiting,
///   which is what an earlier version did. That mattered because `focus_pane`
///   can be cancelled by an *outer* timeout a caller wraps it in
///   (`run_command`'s [`UI_OP_TIMEOUT`](crate::UI_OP_TIMEOUT), for every
///   command that reaches it) as well as by its own [`PANE_ATTACH_TIMEOUT`] —
///   whichever fired first, the future was dropped before ever reaching a
///   post-`.await` arm that writes `detached_pane`, silently leaving it `false`
///   on a pane with no live socket (deterministically for
///   `Command::CreatePane`, which spends part of the same outer budget on
///   `pane.create` before ever calling it). Writing the pessimistic value first
///   removes the whole class rather than patching one instance: a dropped
///   future, from *any* timeout at *any* level, can now only ever fail to
///   *clear* `detached_pane` (safe — a `PANE DISCONNECTED` notice on a pane
///   that actually turned out fine, corrected the next time this runs), never
///   fail to *set* it.
fn take_focus(pane_id: &str, ctx: &mut PaneCtx<'_>) {
    // A terminal takes the keys back; a Files tab filling the area steps
    // aside so the terminal is actually on screen. Beside it, it stays.
    ctx.model.files_keys = false;
    if ctx.model.files_shown == Some(crate::files::FilesLayout::Full) {
        ctx.model.files_shown = None;
    }
    if ctx.model.split.as_deref() == Some(pane_id) {
        ctx.model.split = ctx.model.focused.take();
        ctx.conn.swap();
        std::mem::swap(&mut ctx.reattach.focused, &mut ctx.reattach.split);
    }
    ctx.model.focused = Some(pane_id.to_string());
    ctx.model.detached_pane = true;
    // The sidebar's selected space follows focus. Without this, focusing a
    // pane from the palette, from `ctrl+space` next/previous, or from a
    // reattach left `Model::selected_space` pinned to whatever the user last
    // *clicked* — and since `active_space_id` prefers the selection, the tab
    // bar kept showing a different space's panes than the one the focused
    // pane lives in. Creating a pane in a second space then looked like
    // nothing had happened: the pane existed, was focused, and was streaming,
    // but its tab was in a strip that wasn't on screen.
    //
    // Only when the model actually knows the pane. Right after
    // `Command::CreatePane` it does not yet — `pane.created` arrives on the
    // control channel a moment later — which is why that arm selects the
    // space itself, up front, rather than relying on this.
    if let Some(space) = ctx
        .model
        .local_panes()
        .iter()
        .find(|p| p.id == pane_id)
        .and_then(|p| p.workspace_id.clone())
    {
        // Remembered per space, so clicking away and back returns to what you
        // were doing there rather than to that space's first pane. Recorded
        // here rather than at the click, so it follows focus however it moved
        // — a tab, an agent row, `ctrl+space` next/previous, or a reattach.
        ctx.model
            .local_mut()
            .last_focused
            .insert(space.clone(), pane_id.to_string());
        ctx.model.selected_space = Some(space);
    }
}

/// Focuses `pane_id` locally and attaches its data socket, sizing the newly
/// attached pane to whichever region it now occupies. `model.focused` is set
/// unconditionally (the user's — or a reattach's — *intent* is still to
/// show this pane even if the attach itself fails).
///
/// Every state change made before anything is awaited — `model.focused`,
/// `model.detached_pane`, and the split-slot swap — lives in
/// [`take_focus`], called exactly once as this function's first statement.
/// See that function's own doc comment for why the ordering is load-bearing
/// and why it is a function rather than a comment.
///
/// The attach and the resize that follows it are each separately bounded by
/// [`PANE_ATTACH_TIMEOUT`]; nothing here is ever awaited unbounded. A failure
/// schedules the next reattach rung — see the `else` branch for why the
/// absence of one left a freshly created pane dead on arrival.
pub(crate) async fn focus_pane(pane_id: &str, ctx: &mut PaneCtx<'_>) {
    // One synchronous call, and only then any `.await` — see `take_focus`.
    take_focus(pane_id, ctx);
    // Which machine this pane lives on, resolved before the connection is
    // borrowed. `None` for a local pane — see `Model::owner_of_pane`.
    let owner = ctx
        .model
        .owner_of_pane(pane_id)
        .map(|n| n.as_str().to_string());
    // Read *after* `take_focus`: its swap can clear `model.split` (focusing
    // the split pane while nothing else was focused), and this pane's
    // region is full-width rather than half in that case.
    let (size, _) = slot_sizes(ctx.pane_area, ctx.model);
    let attach = tokio::time::timeout(
        PANE_ATTACH_TIMEOUT,
        ctx.conn
            .focused
            .focus(ctx.base_url, pane_id, ctx.token, owner.as_deref()),
    )
    .await;
    if matches!(attach, Ok(Ok(()))) {
        ctx.model.detached_pane = false;
        // Whatever *this pane's* last failure was, it is no longer the
        // current state of this pane. If the server is about to refuse this
        // attach too, the error frame will set a fresh notice within
        // milliseconds.
        //
        // Scoped to this slot's own origin. An unconditional clear here also
        // retracted a `SPLIT PANE ERROR` that was still true — reachable with
        // no user action, since `reattach_focused_pane` runs off a scheduled
        // wakeup — and the split slot's own ladder then re-raised it seconds
        // later, so the line flickered instead of staying readable.
        ctx.model.clear_notice_from(NoticeOrigin::FocusedPane);
        // Cancels a pending attempt — including one the swap in
        // `take_focus` may have carried in from the other slot — and
        // deliberately leaves the *rung* alone. See `attach_succeeded`.
        attach_succeeded(&mut ctx.reattach.focused);
        // `.and_modify` rather than only `.or_insert_with`: an existing
        // `PaneTerm` (from a previous focus) must be resized to match the
        // *current* pane area too, or re-focusing a pane after the window
        // was resized renders a mis-wrapped grid at its old dimensions.
        ctx.terms
            .entry(pane_id.to_string())
            .and_modify(|t| t.resize(size.0, size.1))
            .or_insert_with(|| PaneTerm::new(size.0, size.1));
        // Bounded, like the attach itself: `reattach_focused_pane`'s call
        // sites have no outer timeout at all, so a WebSocket sink blocked
        // on a full send buffer would hang the entire event loop here — one
        // line past the timeout that was introduced for exactly this class.
        let _ = tokio::time::timeout(PANE_ATTACH_TIMEOUT, ctx.conn.focused.resize(size.0, size.1))
            .await;
    } else {
        // A failed attach schedules the next rung, exactly as `attach_split`
        // does. Without this the focused slot had no retry at all: the
        // `select!` arm clears `reattach.focused.at` before this runs, and
        // only a socket *close* schedules a new attempt — of which there is
        // none, because no socket was ever opened. The pane stayed blank
        // under `PANE DISCONNECTED` until the user re-focused it by hand.
        //
        // This became the create-a-pane critical path when the tab bar's `+`
        // shipped: `pane.create` and `focus_pane` share one `UI_OP_TIMEOUT`
        // budget, so a PTY that takes a moment to spawn leaves the brand-new
        // pane permanently dead on arrival — the first thing a new user does,
        // failing with no way back.
        //
        // Safe to retry forever on the same two counts `attach_split` relies
        // on: the ladder caps its interval at 8s, and `reattach_focused_pane`
        // terminates the sequence as soon as `model.focused` stops naming a
        // live pane. No notice is set here — unlike the split slot, this one
        // already has a visible signal in `detached_pane`, which the status
        // bar renders as `PANE DISCONNECTED`, and a pane-socket error frame
        // will replace that with the server's own reason when there is one.
        ctx.reattach.focused.schedule();
    }
}

/// Attaches the *split* slot to `pane_id` and sizes it to the right-hand
/// region. The split-slot counterpart of [`focus_pane`], and deliberately
/// not the same function: this one never touches `model.focused`,
/// `model.detached_pane` or `model.notice`. `detached_pane` is specifically
/// "the pane keystrokes go to has no socket" — the status bar's
/// `PANE DISCONNECTED` line — and no keystroke ever reaches this slot, so
/// setting it here would report a problem the user does not have.
///
/// A `None` split size means the terminal is currently too narrow for a
/// second region at all (see [`slot_sizes`]). The socket is still attached
/// — the model wants this pane, and a replay costs nothing — but no resize
/// is sent: telling a real remote PTY it is one column wide would destroy
/// the output the split exists to show. The next `Event::Resize` that
/// leaves room sizes it properly through [`resize_visible`].
///
/// # A failed attach retries, and says so
///
/// This used to have no `else`: on an attach error or the inner timeout it set
/// nothing and rescheduled nothing, and since the `select!` arm has already
/// cleared `reattach.split.at` by the time it runs — and only a socket *close*
/// schedules a new attempt, of which there is none, because no socket was ever
/// opened — the slot went **permanently dead** until a reconnect or a manual
/// double-toggle.
///
/// That was justified at the time as symmetric with [`focus_pane`], which
/// then did not retry a failed attach either (it does now). It was never
/// symmetric, and the asymmetry ran the wrong way: `focus_pane`'s failure leaves
/// `detached_pane` set, which the status bar renders as `PANE DISCONNECTED`.
/// This slot deliberately owns no such flag, so its failure was *completely
/// silent* — and because `terms` still holds the pane's last grid, the
/// renderer kept drawing stale content into the other half. A frozen
/// snapshot of an agent is indistinguishable from a live agent that has
/// nothing to say, which is the worst possible failure mode for a feature
/// whose entire purpose is watching a second agent.
///
/// So the failure path now does both things the success path's absence left
/// undone: it puts a sanitized, pane-identifying line on the status bar, and it
/// schedules the next rung. Retrying forever is safe here rather than the
/// unbounded hot loop described on [`Reattach`], on two counts: the ladder caps
/// the interval at 8s, and [`reattach_split_pane`]'s give-up path terminates
/// the whole sequence the moment `model.split` stops naming a live pane.
///
/// It does make this the notice slot's one **repeating** writer, which is
/// what forced [`NoticeOrigin`] to exist — a pane the server keeps refusing
/// rewrites the status bar on every rung, forever. Tagging the origin stops
/// that rewrite from being *retracted* by an unrelated success (and stops
/// this one's own success from retracting an unrelated failure); it does not
/// stop a one-shot `SPACE ERROR` from being overwritten within a rung.
/// See [`crate::model::Model::notice`] for what is and isn't fixed.
async fn attach_split(pane_id: &str, ctx: &mut PaneCtx<'_>) {
    let (_, size) = slot_sizes(ctx.pane_area, ctx.model);
    // Resolved before the connection is borrowed; `None` for a local pane.
    let owner = ctx
        .model
        .owner_of_pane(pane_id)
        .map(|n| n.as_str().to_string());
    let attach = tokio::time::timeout(
        PANE_ATTACH_TIMEOUT,
        ctx.conn
            .split
            .focus(ctx.base_url, pane_id, ctx.token, owner.as_deref()),
    )
    .await;
    match attach {
        Ok(Ok(())) => {
            attach_succeeded(&mut ctx.reattach.split);
            // Resolve the slot the other way too: a previous rung's
            // `SPLIT PANE ERROR` must not outlive the attach that fixed it
            // (the same clear-on-success reasoning `workspace_create_notice`
            // is built around).
            //
            // Scoped to this slot's own origin. Unconditional, this retracted
            // a `PANE ERROR` the *focused* slot had raised milliseconds
            // earlier about a different pane — both slots stream at once, so
            // that is an ordinary interleaving rather than a rare one.
            ctx.model.clear_notice_from(NoticeOrigin::SplitPane);
            if let Some((cols, rows)) = size {
                ctx.terms
                    .entry(pane_id.to_string())
                    .and_modify(|t| t.resize(cols, rows))
                    .or_insert_with(|| PaneTerm::new(cols, rows));
                let _ =
                    tokio::time::timeout(PANE_ATTACH_TIMEOUT, ctx.conn.split.resize(cols, rows))
                        .await;
            }
        }
        failed => {
            let reason = match failed {
                Ok(Err(e)) => e,
                // `Err(Elapsed)`; `Ok(Ok(()))` is the arm above.
                _ => format!("no response within {}s", PANE_ATTACH_TIMEOUT.as_secs()),
            };
            let text = sanitize_notice(&format!(
                "SPLIT PANE ERROR ({}): {reason}",
                pane_display_name(ctx.model, pane_id)
            ));
            ctx.model.set_notice(NoticeOrigin::SplitPane, text);
            ctx.reattach.split.schedule();
        }
    }
}

/// The only ladder bookkeeping a *successful* attach is allowed to do:
/// cancel the attempt that is now pointless, and nothing else.
///
/// Emphatically **not** [`Reattach::reset`]. The rung this slot has climbed
/// records how hard the server has been refusing this pane, and
/// `PaneSocket::attach` returning `Ok` is precisely the evidence that does
/// not bear on that — the runtime accepts the WebSocket upgrade and *then*
/// refuses, so a successful attach is exactly as likely to precede a refusal
/// as a working pane (see [`attachment_is_working`], which is what does
/// reset the rung, on an inbound frame). Resetting here would rebuild the
/// unbounded attach/refuse/reattach loop described on [`Reattach`] one
/// indirection further out.
///
/// Its own named function, called by both [`focus_pane`] and
/// [`attach_split`], so that distinction has one place to live and one test
/// to fail against rather than being a bare field assignment repeated
/// twice.
fn attach_succeeded(ladder: &mut Reattach) {
    ladder.at = None;
}

/// A short, human-meaningful name for `pane_id` — its agent type, plus its
/// title when it has one — for status-bar lines that have to say *which*
/// pane they are about. Falls back to the raw id for a pane the model has no
/// record of yet (`Command::CreatePane` focuses an id before `pane.created`
/// necessarily arrives).
///
/// With two panes visible, a bare `PANE ERROR
/// (conflict): taken over by another client` gave the user no way to tell
/// which of the two agents on screen had just been taken over.
///
/// Deliberately built from the same two `PaneInfo` fields
/// `ui::sidebar::pane_label` draws, so the name in the status bar is one the
/// user can find in the sidebar — but not by calling it: that function's
/// output carries the sidebar's own focus and state glyphs, which have no
/// business in a status-bar sentence (and would violate this crate's glyph
/// discipline by putting `❯`/`■` somewhere neither belongs).
fn pane_display_name(model: &Model, pane_id: &str) -> String {
    match model.local_panes().iter().find(|p| p.id == pane_id) {
        Some(pane) if pane.title.is_empty() => pane.agent_type.clone(),
        Some(pane) => format!("{} \u{2014} {}", pane.agent_type, pane.title),
        None => pane_id.to_string(),
    }
}

/// Turns the side-by-side split on (filling the second slot with
/// [`Model::adjacent_live_pane`]) or off again — `ctrl+space s`.
///
/// Every model mutation here happens **before** the single `.await`, for
/// the same reason [`take_focus`]'s does: this runs inside [`run_command`](crate::run_command)'s
/// [`UI_OP_TIMEOUT`](crate::UI_OP_TIMEOUT), so the future can be dropped at that await, and "the
/// split is on but nothing ever attaches it" must not be a reachable state.
/// Turning it *on* therefore schedules the split slot's first ladder rung
/// rather than attaching inline: a cancelled [`resize_visible`] below then
/// costs at most a wrongly-sized frame or two, never the attach itself.
/// (Attaching inline would nest a [`PANE_ATTACH_TIMEOUT`] attach *and* two
/// bounded resizes inside a 3-second outer bound — the two-nested-timeouts
/// shape that once silently ate `detached_pane`.)
///
/// Refuses to split when there is no focused pane to split *from*, and when
/// `adjacent_live_pane` can only offer the focused pane back (one live pane
/// in the whole session): a pane cannot sit beside itself, and both slots
/// pointing at one id would break the invariant [`Model::split`] documents.
pub(crate) async fn toggle_split(ctx: &mut PaneCtx<'_>) {
    if ctx.model.split.is_some() {
        ctx.model.split = None;
        ctx.conn.split.detach();
        ctx.reattach.split.stand_down();
    } else {
        // Each refusal says so on the status bar. All three used to return
        // silently, which is the same failure class the split's status
        // notices exist to answer: press the chord with
        // one pane open and *nothing* happened — no split, no message, no
        // redraw — indistinguishable from an unbound key. Scoped to
        // `NoticeOrigin::SplitPane`, the domain a split refusal belongs to,
        // so a focused-pane or workspace message isn't clobbered by it.
        let Some(focused) = ctx.model.focused.clone() else {
            ctx.model.set_notice(
                NoticeOrigin::SplitPane,
                "CANNOT SPLIT: focus a pane first".to_string(),
            );
            return;
        };
        let Some(id) = ctx.model.adjacent_live_pane(1) else {
            ctx.model.set_notice(
                NoticeOrigin::SplitPane,
                "CANNOT SPLIT: no second live pane to show".to_string(),
            );
            return;
        };
        if id == focused {
            ctx.model.set_notice(
                NoticeOrigin::SplitPane,
                "CANNOT SPLIT: no second live pane to show".to_string(),
            );
            return;
        }
        // A refusal from an earlier attempt must not outlive the split it
        // was refusing to create.
        ctx.model.clear_notice_from(NoticeOrigin::SplitPane);
        ctx.model.split = Some(id);
        ctx.reattach.split.stand_down();
        ctx.reattach.split.schedule();
    }
    // The focused pane's own region just halved (or doubled), so this is
    // not only about the pane that appeared or vanished.
    resize_visible(ctx).await;
}

/// Resizes both visible panes' grids and sockets to their current regions.
/// Called when the terminal resized, and when [`toggle_split`] changed how
/// the same terminal is divided — the two are the same operation as far as
/// the panes are concerned.
///
/// Each socket write is bounded separately rather than the pair being
/// wrapped in one bound: a wedged focused socket must not consume the
/// budget the split one needs to be told its new width.
pub(crate) async fn resize_visible(ctx: &mut PaneCtx<'_>) {
    let (focused_size, split_size) = slot_sizes(ctx.pane_area, ctx.model);
    resize_slot(&mut ctx.conn.focused, ctx.terms, focused_size).await;
    if let Some(size) = split_size {
        resize_slot(&mut ctx.conn.split, ctx.terms, size).await;
    }
}

/// Resizes one slot's grid and PTY. The grid first and synchronously, so a
/// cancellation at the socket write below still leaves what is *drawn* at
/// the right size; the socket write bounded like every other one in this
/// file (see [`PANE_ATTACH_TIMEOUT`]).
async fn resize_slot(
    slot: &mut PaneSlot,
    terms: &mut HashMap<String, PaneTerm>,
    (cols, rows): (u16, u16),
) {
    if let Some(term) = slot.attached_to.as_deref().and_then(|id| terms.get_mut(id)) {
        term.resize(cols, rows);
    }
    let _ = tokio::time::timeout(PANE_ATTACH_TIMEOUT, slot.resize(cols, rows)).await;
}

/// Re-attaches `model.focused`'s pane-socket if that pane is still live per
/// the model, clearing `focused` (and `detached_pane`) if it isn't. Called
/// wherever the pane-socket attachment can have gone stale without
/// `focused` itself changing: after a reconnect (the control channel came
/// back but `pane.detach()` ran while it was down) and after the
/// pane-socket's own channel closes while the pane stays alive per the
/// control channel (another controller took over, a hiccup, ...). Without
/// this, `model.local().stale == false` and `model.focused == Some(id)` would make
/// `route` produce `Action::PaneInput` for a socket that doesn't exist —
/// refused nowhere, shown nowhere.
pub(crate) async fn reattach_focused_pane(ctx: &mut PaneCtx<'_>) {
    let Some(id) = ctx.model.focused.clone() else {
        return;
    };
    let still_live = ctx.model.local_panes().iter().any(|p| p.id == id && p.live);
    if still_live {
        focus_pane(&id, ctx).await;
    } else {
        ctx.model.focused = None;
        ctx.model.detached_pane = false;
        // Unconditional, like `Model::forget_focus_if_gone`'s and for the
        // same reason (see that function's doc comment): this is not "the
        // focused pane's failure is over" — the two success paths' claim,
        // which is why *they* are origin-scoped — it is "the pane the status
        // bar was hung off is gone, and nothing else will ever clear this
        // slot on its behalf".
        ctx.model.notice = None;
        // Nothing is focused now. If the other half is still showing a live
        // pane, it becomes the one pane — see `promote_split_if_unfocused`.
        promote_split_if_unfocused(ctx).await;
    }
}

/// One slot's expression of the invariant **a visible pane always either
/// has a socket or has a reattach pending**: schedules the next ladder rung
/// unless the model wants nothing in this slot, the slot already has a
/// socket, or an attempt is already on the way.
///
/// `promote_split_if_unfocused` once broke this, in the same failure family as
/// a split attach that failed without retrying — a visible pane left with no
/// socket and nothing scheduled to give it one. The sequence: the control
/// channel drops (`conn.detach_all()` empties both sockets), it comes back,
/// both ladders are stood down, and `reattach_focused_pane` finds the focused
/// pane's record intact but no longer `live` — the ordinary case for a pane
/// that exited while disconnected, since `forget_focus_if_gone` only clears on
/// record *absence*. Its not-live branch promotes the split pane, which sets
/// `model.split` to `None` — and the reconnect arm's
/// `if model.split.is_some() { schedule() }`, the only other thing that would
/// have re-attached that pane, has by then either already run or been made
/// false. No socket was ever opened, so `PaneSlot::recv` stays `pending()` and
/// the close arm cannot fire either. The pane was stranded until a manual
/// refocus.
///
/// Being a named function rather than three inline conditions is the point:
/// the same invariant is what the socket-close arm, `attach_split`'s failure
/// branch and the reconnect arm each maintain in their own way, and this
/// gives the rule one place to be read and one test to fail against. It is
/// *called* only from the promotion path today. Of the other three sites,
/// "rewiring would change behaviour" is true of exactly one:
///
/// - The **socket-close arm** ([`on_slot_event`]) really would change:
///   it schedules on every close, even when the model no longer wants that
///   pane in that slot, and self-corrects later via the give-up path. This
///   helper's `wanted.is_none()` guard would suppress that deliberate
///   "one pointless wakeup".
/// - **[`attach_split`]'s failure branch** would be a *pure no-op*: it is
///   only reachable from [`reattach_split_pane`], whose `select!` arm has
///   already cleared `ladder.at`, and a failed attach leaves
///   `slot.attached_to` `None` — so all three guards are already false.
/// - The **reconnect arm's** `if model.split.is_some() { schedule() }`
///   likewise: it runs after both `stand_down()`s and `detach_all()`.
///
/// So two of the three are behaviour-identical and could adopt this freely;
/// they simply have not, and their own tests cover them. The one site none
/// of them covered, a [`focus_pane`] whose attach fails, now schedules
/// its own retry (see `a_focus_that_cannot_attach_schedules_a_retry`).
///
/// Deliberately checks `ladder.at.is_none()` rather than scheduling
/// unconditionally: `Reattach::schedule` advances the rung, so a caller that
/// ran twice would climb the ladder without a single failed attempt in
/// between.
fn ensure_reattach_pending(wanted: Option<&str>, slot: &PaneSlot, ladder: &mut Reattach) {
    if wanted.is_none() || slot.attached_to.is_some() || ladder.at.is_some() {
        return;
    }
    ladder.schedule();
}

/// Moves the split pane into the focused slot when nothing is focused, so
/// the screen never sits half-blank with no way to explain itself.
///
/// `focused == None` with `split == Some(_)` is
/// genuinely reachable — `Model::forget_focus_if_gone` clears `focused` when
/// its pane's record vanishes, and so does [`reattach_focused_pane`]'s
/// not-live branch, and neither has any reason to touch a perfectly healthy
/// second pane. The result was a half-width pane hugging one edge with dead
/// space beside it and nothing on screen saying why.
///
/// Promotion, not just collapsing the layout: the split pane already has a
/// live socket and a live grid, and it is the only pane the user can still
/// see, so it is obviously the one that should now receive keystrokes.
///
/// The whole state change is synchronous and happens before the single
/// `.await`, the same discipline [`take_focus`] follows — a cancellation can
/// only ever cost the resize, never leave the model promoted while the
/// sockets are not (or vice versa). The sockets swap with the fields for the
/// usual reason: `conn.focused` must serve `model.focused`, and re-attaching
/// to do that would throw away a working connection. `detached_pane` is set
/// from the promoted slot's actual attachment rather than pessimistically —
/// there is no await between reading it and writing it, so the accurate
/// value is also the safe one.
///
/// A no-op — reached on almost every control-channel event — returns before
/// the `.await`, so the common case costs one `is_some()` and nothing else.
pub(crate) async fn promote_split_if_unfocused(ctx: &mut PaneCtx<'_>) {
    if ctx.model.focused.is_some() || ctx.model.split.is_none() {
        return;
    }
    ctx.model.focused = ctx.model.split.take();
    ctx.conn.swap();
    std::mem::swap(&mut ctx.reattach.focused, &mut ctx.reattach.split);
    ctx.model.detached_pane = ctx.conn.focused.attached_to.is_none();
    // A promotion can hand the focused slot an *empty* socket — that is the
    // whole reconnect case (see `ensure_reattach_pending`), and nothing else
    // in the loop would ever give it one.
    ensure_reattach_pending(
        ctx.model.focused.as_deref(),
        &ctx.conn.focused,
        &mut ctx.reattach.focused,
    );
    // The slot that just became the split slot holds the *old* focused
    // pane's socket, and `model.split` is now `None` — nothing will draw it
    // again, so let it go rather than leaving a socket streaming into a
    // grid `prune_terms` is about to free.
    ctx.conn.split.detach();
    ctx.reattach.split.stand_down();
    // The promoted pane's region just went from half-width to the whole
    // pane area.
    resize_visible(ctx).await;
}

/// The split slot's counterpart to [`reattach_focused_pane`]: re-attaches
/// `model.split`'s pane if it is still live, and otherwise gives the slot
/// up entirely.
///
/// Giving up is the important half, and it is what keeps the unbounded reattach
/// loop described on [`Reattach`] from coming back through a second door. This
/// runs off a *scheduled* wakeup, and the socket-close arm schedules one
/// unconditionally — so a split that was turned off (or whose pane was removed,
/// which `Model::forget_focus_if_gone` reports by clearing `split`) between the
/// close and this firing lands here with nothing to attach. Returning without
/// rescheduling is what makes that one wakeup and not a ladder that climbs
/// forever against a pane the server no longer has.
pub(crate) async fn reattach_split_pane(ctx: &mut PaneCtx<'_>) {
    let Some(id) = ctx.model.split.clone() else {
        ctx.conn.split.detach();
        ctx.reattach.split.stand_down();
        return;
    };
    let still_live = ctx.model.local_panes().iter().any(|p| p.id == id && p.live);
    if still_live {
        attach_split(&id, ctx).await;
    } else {
        // Unlike `reattach_focused_pane`'s equivalent branch this touches
        // neither `detached_pane` nor `notice`: see `attach_split` for why
        // the split slot owns neither.
        ctx.model.split = None;
        ctx.conn.split.detach();
        ctx.reattach.split.stand_down();
    }
}

#[cfg(test)]
// Same convention as model.rs/input.rs's test modules: build a
// `Model::default()` and assign just the field(s) each case cares about,
// which reads clearer here than clippy's suggested single
// struct-literal-with-`..Default::default()` once more than a couple of
// fields are involved.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;
    use crate::tests::model_with_pane_ids;

    // --- handle_pane_event ---

    #[test]
    fn bytes_feed_the_pane_terminal_for_the_attached_pane() {
        // In real use `focus_pane` always creates a properly-sized `PaneTerm`
        // entry before the socket it just attached could possibly deliver a
        // `Bytes` event — mirrored here rather than relying on
        // `handle_pane_event`'s own defensive (should-never-fire) 1x1
        // fallback for a pane with no entry yet, which is too small to hold
        // more than one visible character.
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), PaneTerm::new(20, 4));
        handle_pane_event(
            PaneEvent::Bytes(b"hello".to_vec()),
            Some("p1"),
            &Model::default(),
            &mut terms,
        );
        let lines = terms.get("p1").expect("pane term created").visible_lines();
        assert!(lines[0].starts_with("hello"));
    }

    #[test]
    fn replay_markers_do_not_panic_and_touch_no_pane_term() {
        let mut terms = HashMap::new();
        handle_pane_event(
            PaneEvent::ReplayStart,
            Some("p1"),
            &Model::default(),
            &mut terms,
        );
        handle_pane_event(
            PaneEvent::ReplayEnd,
            Some("p1"),
            &Model::default(),
            &mut terms,
        );
        assert!(terms.is_empty());
    }

    #[test]
    fn an_event_with_nothing_attached_is_dropped_without_panicking() {
        let mut terms = HashMap::new();
        handle_pane_event(
            PaneEvent::Bytes(b"hi".to_vec()),
            None,
            &Model::default(),
            &mut terms,
        );
        assert!(terms.is_empty());
    }

    #[test]
    fn exit_and_error_events_do_not_panic() {
        let mut terms = HashMap::new();
        handle_pane_event(
            PaneEvent::Exit { exit_code: 0 },
            Some("p1"),
            &Model::default(),
            &mut terms,
        );
        handle_pane_event(
            PaneEvent::Error {
                code: "conflict".into(),
                message: "taken over".into(),
            },
            Some("p1"),
            &Model::default(),
            &mut terms,
        );
    }

    // --- Pane errors reach the status bar (the `Error` arm was once empty,
    // and `unauthorized`/`not_found`/`conflict` have no control-channel
    // counterpart to report them). ---

    #[test]
    fn a_pane_error_frame_produces_a_notice_naming_the_code_and_message() {
        let mut terms = HashMap::new();
        let notice = handle_pane_event(
            PaneEvent::Error {
                code: "unauthorized".into(),
                message: "token expired".into(),
            },
            Some("p1"),
            &Model::default(),
            &mut terms,
        )
        .expect("an error frame must be surfaced, not dropped");
        assert!(notice.contains("unauthorized"), "{notice}");
        assert!(notice.contains("token expired"), "{notice}");
    }

    #[test]
    fn ordinary_pane_events_produce_no_notice() {
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), PaneTerm::new(20, 4));
        assert_eq!(
            handle_pane_event(
                PaneEvent::Bytes(b"hi".to_vec()),
                Some("p1"),
                &Model::default(),
                &mut terms
            ),
            None
        );
        assert_eq!(
            handle_pane_event(
                PaneEvent::ReplayStart,
                Some("p1"),
                &Model::default(),
                &mut terms
            ),
            None
        );
        // `pane.closed` on the control channel already reports an exit,
        // more accurately than this socket can.
        assert_eq!(
            handle_pane_event(
                PaneEvent::Exit { exit_code: 0 },
                Some("p1"),
                &Model::default(),
                &mut terms
            ),
            None
        );
    }

    #[test]
    fn a_hostile_error_message_cannot_inject_escape_sequences_into_the_status_bar() {
        let mut terms = HashMap::new();
        let notice = handle_pane_event(
            PaneEvent::Error {
                code: "internal".into(),
                message: "\u{1b}[2J\u{1b}[31mwiped\nyour screen".into(),
            },
            Some("p1"),
            &Model::default(),
            &mut terms,
        )
        .expect("surfaced");
        assert!(!notice.contains('\u{1b}'), "{notice:?}");
        assert!(!notice.contains('\n'), "{notice:?}");
        assert!(notice.contains("wiped"), "{notice:?}");
    }

    // --- The reattach ladder. ---

    #[test]
    fn a_fresh_reattach_starts_at_the_first_rung_and_climbs_to_the_cap() {
        let mut reattach = Reattach::default();
        assert_eq!(reattach.at, None, "nothing is scheduled until asked");
        let mut rungs = Vec::new();
        for _ in 0..8 {
            let before = reattach.backoff;
            reattach.schedule();
            rungs.push(before);
            assert!(reattach.at.is_some());
        }
        assert_eq!(
            rungs,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ],
            "the pane ladder must match the control channel's"
        );
    }

    #[test]
    fn resetting_drops_back_to_the_first_rung() {
        let mut reattach = Reattach::default();
        for _ in 0..5 {
            reattach.schedule();
        }
        assert!(reattach.backoff > RECONNECT_INITIAL_BACKOFF);
        reattach.reset();
        assert_eq!(reattach.backoff, RECONNECT_INITIAL_BACKOFF);
    }

    #[test]
    fn only_an_inbound_frame_counts_as_a_working_attachment() {
        // The distinction the reattach ladder turns on: the server accepts
        // the upgrade and *then* refuses, so `attach` returning Ok proves
        // nothing. A byte or a replay marker does.
        assert!(attachment_is_working(&PaneEvent::ReplayStart));
        assert!(attachment_is_working(&PaneEvent::ReplayEnd));
        assert!(attachment_is_working(&PaneEvent::Bytes(vec![])));
        assert!(!attachment_is_working(&PaneEvent::Exit { exit_code: 0 }));
        assert!(!attachment_is_working(&PaneEvent::Error {
            code: "unauthorized".into(),
            message: String::new(),
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn a_scheduled_reattach_waits_its_rung_before_coming_due() {
        // With tokio's clock paused, time only advances when something
        // awaits — so "did not resolve early" is deterministic here, not a
        // race against a real timer.
        let mut reattach = Reattach::default();
        reattach.schedule();
        assert!(
            tokio::time::timeout(Duration::from_millis(249), reattach_due(reattach.at))
                .await
                .is_err(),
            "must not fire before the first rung elapses"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(2), reattach_due(reattach.at))
                .await
                .is_ok(),
            "must fire once it does"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unscheduled_reattach_never_comes_due() {
        let reattach = Reattach::default();
        assert!(
            tokio::time::timeout(Duration::from_secs(3600), reattach_due(reattach.at))
                .await
                .is_err(),
            "no schedule means this select! arm never wins"
        );
    }

    // --- terms pruning. ---

    /// A [`PaneConn`] whose two slots claim to be attached to `focused` and
    /// `split`. No sockets are opened — `attached_to` is the only field
    /// [`prune_terms`] reads, and opening a real `PaneSocket` would need a
    /// server.
    fn conn_attached_to(focused: Option<&str>, split: Option<&str>) -> PaneConn {
        let mut conn = PaneConn::default();
        conn.focused.attached_to = focused.map(str::to_string);
        conn.split.attached_to = split.map(str::to_string);
        conn
    }

    #[test]
    fn a_pane_the_model_no_longer_knows_about_loses_its_terminal_grid() {
        let mut terms = HashMap::new();
        terms.insert("kept".to_string(), PaneTerm::new(20, 4));
        terms.insert("removed".to_string(), PaneTerm::new(20, 4));
        prune_terms(
            &model_with_pane_ids(&["kept"]),
            &PaneConn::default(),
            &mut terms,
        );
        assert!(terms.contains_key("kept"));
        assert!(
            !terms.contains_key("removed"),
            "a removed pane's 10k-line scrollback must not outlive it"
        );
    }

    #[test]
    fn a_closed_but_still_recorded_pane_keeps_its_grid() {
        // `pane.closed` keeps the record so the last frame stays readable;
        // pruning on `live` instead of on the record would blank the screen
        // the user is looking at.
        let mut model = model_with_pane_ids(&["p1"]);
        model.local_mut().panes[0].live = false;
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), PaneTerm::new(20, 4));
        prune_terms(&model, &PaneConn::default(), &mut terms);
        assert!(terms.contains_key("p1"));
    }

    #[test]
    fn the_attached_or_focused_pane_is_never_pruned_mid_flight() {
        // `Command::CreatePane` focuses and attaches the id `pane.create`
        // returned, which the model doesn't necessarily know about until
        // the matching `pane.created` event lands.
        let model = model_with_pane_ids(&[]);
        let mut terms = HashMap::new();
        terms.insert("brand-new".to_string(), PaneTerm::new(20, 4));
        prune_terms(
            &model,
            &conn_attached_to(Some("brand-new"), None),
            &mut terms,
        );
        assert!(terms.contains_key("brand-new"));

        let mut model = model_with_pane_ids(&[]);
        model.focused = Some("brand-new".into());
        prune_terms(&model, &PaneConn::default(), &mut terms);
        assert!(terms.contains_key("brand-new"));
    }

    #[test]
    fn pruning_an_empty_model_of_an_empty_map_is_a_no_op() {
        let mut terms: HashMap<String, PaneTerm> = HashMap::new();
        prune_terms(&Model::default(), &PaneConn::default(), &mut terms);
        assert!(terms.is_empty());
    }

    // --- The split pane's grid is protected on exactly the same
    // grounds the focused one's is. Pruning it would free a grid a live
    // socket is still writing into. ---

    #[test]
    fn the_split_panes_grid_survives_pruning_even_before_the_model_knows_it() {
        let mut model = model_with_pane_ids(&[]);
        model.split = Some("second".into());
        let mut terms = HashMap::new();
        terms.insert("second".to_string(), PaneTerm::new(20, 4));
        prune_terms(&model, &PaneConn::default(), &mut terms);
        assert!(terms.contains_key("second"));
    }

    #[test]
    fn a_grid_the_split_slots_socket_is_writing_into_is_never_pruned() {
        // The window after `model.split` was cleared (its record vanished)
        // but before the socket has actually closed: `handle_pane_event`
        // still feeds bytes into this entry, so freeing it here would have
        // the next byte recreate it at a defensive 1x1.
        let model = model_with_pane_ids(&[]);
        let mut terms = HashMap::new();
        terms.insert("dying".to_string(), PaneTerm::new(20, 4));
        prune_terms(&model, &conn_attached_to(None, Some("dying")), &mut terms);
        assert!(terms.contains_key("dying"));
    }

    // --- `slot_sizes` — the one place a region becomes a size. ---

    /// A model with no split: one pane, focused.
    fn unsplit_model() -> Model {
        let mut m = model_with_pane_ids(&["p1"]);
        m.focused = Some("p1".into());
        m
    }

    #[test]
    fn an_unsplit_pane_area_sizes_one_pane_to_the_whole_of_it() {
        let ((cols, rows), split) = slot_sizes(Rect::new(28, 0, 52, 11), &unsplit_model());
        assert_eq!((cols, rows), (52, 11));
        assert_eq!(split, None);
    }

    #[test]
    fn a_split_pane_area_sizes_each_pane_to_its_own_half() {
        let (focused, split) = slot_sizes(Rect::new(28, 0, 52, 11), &split_model());
        let split = split.expect("a split area has two sizes");
        assert_eq!(focused, (26, 11));
        assert_eq!(split, (26, 11));
    }

    #[test]
    fn each_panes_size_follows_the_half_it_is_actually_drawn_in() {
        // On an odd-width pane area the two halves differ by a column, so a
        // size read from the wrong half tells a real PTY it is one column
        // narrower than the cells it is drawn into. `p1` before `p2` in `panes`
        // puts `p1` on the left (25 columns) and `p2` on the right (26) — and
        // focusing `p2` must change which *field* holds which id without
        // changing either pane's size.
        let area = Rect::new(28, 0, 51, 11);
        let mut m = split_model();
        let (focused, split) = slot_sizes(area, &m);
        assert_eq!(focused, (25, 11), "p1 is focused and drawn on the left");
        assert_eq!(split, Some((26, 11)), "p2 is split and drawn on the right");

        // Now focus p2: the fields swap, the sides do not.
        m.focused = Some("p2".into());
        m.split = Some("p1".into());
        let (focused, split) = slot_sizes(area, &m);
        assert_eq!(
            focused,
            (26, 11),
            "p2 is focused now, but still the right-hand pane"
        );
        assert_eq!(split, Some((25, 11)), "and p1 is still the left-hand one");
    }

    #[test]
    fn a_degenerate_pane_area_still_sizes_the_focused_pane_to_at_least_one_cell() {
        // A terminal narrower than the sidebar leaves no pane area at all.
        // `PaneTerm`/`PaneSocket` each clamp internally too; clamping here
        // as well means the two never have to silently agree.
        let ((cols, rows), split) = slot_sizes(Rect::new(0, 0, 0, 0), &split_model());
        assert_eq!((cols, rows), (1, 1));
        assert_eq!(
            split, None,
            "no room for a second region means no resize to send, \
             not a 1x1 resize that would reflow a real PTY to one column"
        );
    }

    #[test]
    fn a_pane_area_one_column_wide_offers_no_split_size() {
        let (_, split) = slot_sizes(Rect::new(0, 0, 1, 20), &split_model());
        assert_eq!(split, None);
    }

    // --- A successful attach cancels the pending attempt and must never
    // touch the rung. Resetting here would rebuild the unbounded
    // attach/refuse/reattach loop one indirection out. ---

    #[test]
    fn a_successful_attach_cancels_the_attempt_but_keeps_the_rung() {
        let mut ladder = Reattach::default();
        for _ in 0..4 {
            ladder.schedule();
        }
        let climbed = ladder.backoff;
        assert!(climbed > RECONNECT_INITIAL_BACKOFF, "precondition");
        assert!(ladder.at.is_some(), "precondition");

        attach_succeeded(&mut ladder);

        assert_eq!(ladder.at, None, "the now-pointless attempt is cancelled");
        assert_eq!(
            ladder.backoff, climbed,
            "but `attach` returning Ok proves nothing about the pane — only \
             an inbound frame resets the rung (see attachment_is_working)"
        );
    }

    // --- `on_slot_event` — the shared, synchronous handler behind
    // both pane-socket `select!` arms. ---

    /// A model with two live panes, `p1` focused and `p2` split.
    fn split_model() -> Model {
        let mut m = model_with_pane_ids(&["p1", "p2"]);
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        m
    }

    #[test]
    fn an_inbound_frame_resets_only_its_own_slots_ladder() {
        // The per-pane half of the unbounded reattach loop: one pane
        // streaming happily must not reset the ladder of a *different* pane
        // the server keeps refusing, or that refusal goes back to being
        // an unbounded hot loop.
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), PaneTerm::new(20, 4));
        for _ in 0..4 {
            reattach.focused.schedule();
            reattach.split.schedule();
        }
        let climbed = reattach.split.backoff;
        assert!(climbed > RECONNECT_INITIAL_BACKOFF, "precondition");

        on_slot_event(
            Slot::Focused,
            Some(PaneEvent::Bytes(b"hi".to_vec())),
            &mut conn,
            &mut reattach,
            &mut model,
            &mut terms,
        );
        assert_eq!(reattach.focused.backoff, RECONNECT_INITIAL_BACKOFF);
        assert_eq!(
            reattach.split.backoff, climbed,
            "the other pane's ladder must be untouched"
        );
    }

    #[test]
    fn a_frame_that_proves_nothing_leaves_the_ladder_where_it_was() {
        let mut conn = conn_attached_to(Some("p1"), None);
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        reattach.focused.schedule();
        reattach.focused.schedule();
        let climbed = reattach.focused.backoff;
        on_slot_event(
            Slot::Focused,
            Some(PaneEvent::Error {
                code: "conflict".into(),
                message: "taken over".into(),
            }),
            &mut conn,
            &mut reattach,
            &mut model,
            &mut terms,
        );
        assert_eq!(reattach.focused.backoff, climbed);
        assert!(
            model.notice.is_some(),
            "the error still reaches the status bar"
        );
    }

    // --- An error frame is tagged with the slot it arrived on, so a
    // *later success on the other slot* cannot retract it. Derived from the
    // `Slot`, not from `model.focused`/`split`, which is what keeps it right
    // across a swap. ---

    /// The `(origin, text)` `on_slot_event` leaves after one error frame
    /// from `slot`, with both slots attached and both panes live.
    fn notice_after_error_on(slot: Slot) -> (NoticeOrigin, String) {
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        on_slot_event(
            slot,
            Some(PaneEvent::Error {
                code: "conflict".into(),
                message: "taken over".into(),
            }),
            &mut conn,
            &mut reattach,
            &mut model,
            &mut terms,
        );
        model
            .notice
            .expect("an error frame must reach the status bar")
    }

    #[test]
    fn an_error_on_the_focused_socket_is_tagged_as_the_focused_panes() {
        let (origin, text) = notice_after_error_on(Slot::Focused);
        assert_eq!(origin, NoticeOrigin::FocusedPane);
        assert!(text.contains("conflict"), "{text}");
    }

    #[test]
    fn an_error_on_the_split_socket_is_tagged_as_the_split_panes() {
        // The one that mattered: tagged `FocusedPane`, a later successful
        // focus attach would retract a split pane's error that is still true.
        let (origin, text) = notice_after_error_on(Slot::Split);
        assert_eq!(origin, NoticeOrigin::SplitPane);
        assert!(text.contains("conflict"), "{text}");
    }

    #[test]
    fn a_closed_focused_socket_detaches_flags_and_schedules() {
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        on_slot_event(
            Slot::Focused,
            None,
            &mut conn,
            &mut reattach,
            &mut model,
            &mut terms,
        );
        assert_eq!(conn.focused.attached_to, None);
        assert!(
            model.detached_pane,
            "the status bar must say so immediately"
        );
        assert!(reattach.focused.at.is_some(), "and a retry must be pending");
        assert_eq!(
            conn.split.attached_to.as_deref(),
            Some("p2"),
            "the other slot is untouched"
        );
        assert!(reattach.split.at.is_none());
    }

    #[test]
    fn a_closed_split_socket_schedules_without_claiming_the_focused_pane_is_down() {
        // `detached_pane` drives `PANE DISCONNECTED`, which is about the
        // pane keystrokes go to. The split pane's socket dying is not that.
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        on_slot_event(
            Slot::Split,
            None,
            &mut conn,
            &mut reattach,
            &mut model,
            &mut terms,
        );
        assert_eq!(conn.split.attached_to, None);
        assert!(reattach.split.at.is_some());
        assert!(
            !model.detached_pane,
            "the focused pane's socket is still perfectly fine"
        );
        assert_eq!(conn.focused.attached_to.as_deref(), Some("p1"));
    }

    #[test]
    fn a_closed_split_socket_climbs_its_own_ladder_rung_by_rung() {
        // Per pane, and on the same 250ms -> 8s ladder the focused slot and
        // the control channel use — get this wrong and the "server refuses
        // the attach after accepting the upgrade" case rebuilds the
        // unbounded hot loop, just on the other slot.
        let mut conn = conn_attached_to(None, Some("p2"));
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        let mut rungs = Vec::new();
        for _ in 0..4 {
            conn.split.attached_to = Some("p2".into());
            rungs.push(reattach.split.backoff);
            on_slot_event(
                Slot::Split,
                None,
                &mut conn,
                &mut reattach,
                &mut model,
                &mut terms,
            );
        }
        assert_eq!(
            rungs,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
            ]
        );
    }

    #[test]
    fn bytes_are_fed_into_the_grid_of_the_slot_they_arrived_on() {
        // Both slots stream at once; misattributing one pane's output to
        // the other's grid is the split-specific way to corrupt a screen.
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        let mut model = split_model();
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), PaneTerm::new(20, 4));
        terms.insert("p2".to_string(), PaneTerm::new(20, 4));
        on_slot_event(
            Slot::Split,
            Some(PaneEvent::Bytes(b"second".to_vec())),
            &mut conn,
            &mut reattach,
            &mut model,
            &mut terms,
        );
        assert!(terms["p2"].visible_lines()[0].starts_with("second"));
        assert!(
            terms["p1"].visible_lines()[0].trim().is_empty(),
            "the focused pane's grid must be untouched"
        );
    }

    // --- `Reattach::stand_down` — cancel *and* reset, for when
    // there is no longer anything to reattach to. ---

    #[test]
    fn standing_down_cancels_the_pending_attempt_and_the_climbed_rung() {
        let mut reattach = Reattach::default();
        for _ in 0..5 {
            reattach.schedule();
        }
        assert!(reattach.at.is_some() && reattach.backoff > RECONNECT_INITIAL_BACKOFF);
        reattach.stand_down();
        assert_eq!(reattach.at, None, "no pointless wakeup is left behind");
        assert_eq!(reattach.backoff, RECONNECT_INITIAL_BACKOFF);
    }

    // --- Swapping the two slots. Focusing the pane that is
    // currently split must move the *socket*, not rebuild it. ---

    #[test]
    fn swapping_exchanges_both_slots_attachments() {
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        conn.swap();
        assert_eq!(conn.focused.attached_to.as_deref(), Some("p2"));
        assert_eq!(conn.split.attached_to.as_deref(), Some("p1"));
    }

    #[test]
    fn either_slots_attachment_counts_as_attached() {
        let conn = conn_attached_to(Some("p1"), Some("p2"));
        assert!(conn.is_attached_to("p1"));
        assert!(conn.is_attached_to("p2"));
        assert!(!conn.is_attached_to("p3"));
        assert!(!PaneConn::default().is_attached_to("p1"));
    }

    #[test]
    fn detaching_everything_leaves_neither_slot_attached() {
        let mut conn = conn_attached_to(Some("p1"), Some("p2"));
        conn.detach_all();
        assert_eq!(conn.focused.attached_to, None);
        assert_eq!(conn.split.attached_to, None);
    }

    // --- `take_focus` — `focus_pane`'s whole synchronous preamble.
    //
    // This is where a cancel-safety defect once lived, so it is pinned directly
    // rather than only through a `focus_pane` that would need a server to
    // reach. ---

    /// Runs `f` against a `PaneCtx` built over the state it needs, then
    /// hands that state back for assertions. `PaneCtx` holds `&mut` on
    /// every piece, so a test cannot inspect them until the context is
    /// gone — this owns that scope so each case doesn't have to.
    fn with_ctx<F>(
        model: Model,
        conn: PaneConn,
        reattach: Reattachers,
        f: F,
    ) -> (Model, PaneConn, Reattachers)
    where
        F: FnOnce(&mut PaneCtx<'_>),
    {
        let (mut model, mut conn, mut reattach) = (model, conn, reattach);
        let mut terms = HashMap::new();
        {
            let mut ctx = PaneCtx {
                base_url: "ws://127.0.0.1:0",
                token: None,
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area: Rect::new(28, 0, 52, 11),
            };
            f(&mut ctx);
        }
        (model, conn, reattach)
    }

    #[test]
    fn taking_focus_flags_the_pane_as_detached_before_anything_is_awaited() {
        // The pessimistic write. Nothing has attached at this point, and
        // this must already say so — a cancellation between here and a
        // successful attach may only ever fail to *clear* this flag.
        let (model, _, _) = with_ctx(
            Model::default(),
            PaneConn::default(),
            Reattachers::default(),
            |ctx| take_focus("p1", ctx),
        );
        assert_eq!(model.focused.as_deref(), Some("p1"));
        assert!(model.detached_pane);
    }

    #[test]
    fn taking_focus_of_the_split_pane_swaps_both_slots_rather_than_reattaching() {
        let mut model = split_model();
        model.detached_pane = false;
        let conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        reattach.split.schedule();
        reattach.split.schedule();
        let split_rung = reattach.split.backoff;
        let split_at = reattach.split.at;

        let (model, conn, reattach) = with_ctx(model, conn, reattach, |ctx| take_focus("p2", ctx));

        assert_eq!(model.focused.as_deref(), Some("p2"));
        assert_eq!(
            model.split.as_deref(),
            Some("p1"),
            "the pane that was focused takes the other slot, it does not vanish"
        );
        assert_eq!(
            conn.focused.attached_to.as_deref(),
            Some("p2"),
            "the live socket moved slots; it was not torn down and rebuilt"
        );
        assert_eq!(conn.split.attached_to.as_deref(), Some("p1"));
        assert_eq!(
            (reattach.focused.backoff, reattach.focused.at),
            (split_rung, split_at),
            "each ladder follows its own socket across the swap"
        );
        assert_eq!(reattach.split.backoff, RECONNECT_INITIAL_BACKOFF);
    }

    #[test]
    fn taking_focus_of_the_split_pane_with_nothing_focused_collapses_the_split() {
        // `focused` is `None` (its pane was removed), so there is nothing to
        // put in the other slot — the layout goes back to one full-width
        // pane rather than to a split with an empty half.
        let mut model = Model::default();
        model.split = Some("p2".into());
        let conn = conn_attached_to(None, Some("p2"));
        let (model, conn, _) = with_ctx(model, conn, Reattachers::default(), |ctx| {
            take_focus("p2", ctx)
        });
        assert_eq!(model.focused.as_deref(), Some("p2"));
        assert_eq!(model.split, None);
        assert_eq!(conn.focused.attached_to.as_deref(), Some("p2"));
        assert_eq!(conn.split.attached_to, None);
    }

    #[test]
    fn taking_focus_of_a_third_pane_leaves_the_split_slot_alone() {
        let model = split_model();
        let conn = conn_attached_to(Some("p1"), Some("p2"));
        let (model, conn, _) = with_ctx(model, conn, Reattachers::default(), |ctx| {
            take_focus("p3", ctx)
        });
        assert_eq!(model.focused.as_deref(), Some("p3"));
        assert_eq!(model.split.as_deref(), Some("p2"), "still beside it");
        assert_eq!(
            conn.split.attached_to.as_deref(),
            Some("p2"),
            "the split socket must not be disturbed by an unrelated focus"
        );
    }

    #[test]
    fn refocusing_the_already_focused_pane_changes_nothing_structural() {
        let model = split_model();
        let conn = conn_attached_to(Some("p1"), Some("p2"));
        let (model, conn, _) = with_ctx(model, conn, Reattachers::default(), |ctx| {
            take_focus("p1", ctx)
        });
        assert_eq!(model.focused.as_deref(), Some("p1"));
        assert_eq!(model.split.as_deref(), Some("p2"));
        assert_eq!(conn.focused.attached_to.as_deref(), Some("p1"));
        assert_eq!(conn.split.attached_to.as_deref(), Some("p2"));
    }

    // --- `toggle_split` and `reattach_split_pane`'s give-up paths. ---

    /// Which split-slot operation [`run_split_op`] should run. An enum
    /// rather than a closure parameter: `PaneCtx` borrows mutably from four
    /// locals at once, and a `for<'a> FnOnce(&'a mut PaneCtx<'a>)` bound
    /// cannot be satisfied by an `async` block over it (mutable references
    /// are invariant in their type parameter). A handful of named
    /// operations is the whole set either way.
    enum SplitOp {
        Toggle,
        Reattach,
        Promote,
        ReattachFocused,
    }

    /// [`with_ctx`]'s async counterpart, for the `async fn`s reached here.
    ///
    /// `base_url` names port 0, which nothing can ever be listening on, so
    /// any attach these reach fails immediately and locally rather than
    /// hanging or reaching a real runtime. Most cases below never get that
    /// far at all (an empty split slot makes `resize_visible`'s socket
    /// writes no-ops, `toggle_split` schedules rather than attaching, and
    /// `reattach_split_pane`'s give-up paths return before any socket work);
    /// the exceptions are the two that deliberately exercise a *failed*
    /// attach, and they are named accordingly.
    async fn run_split_op(
        op: SplitOp,
        model: Model,
        conn: PaneConn,
        reattach: Reattachers,
    ) -> (Model, PaneConn, Reattachers) {
        let (mut model, mut conn, mut reattach) = (model, conn, reattach);
        let mut terms = HashMap::new();
        {
            let mut ctx = PaneCtx {
                base_url: "ws://127.0.0.1:0",
                token: None,
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area: Rect::new(28, 0, 52, 11),
            };
            match op {
                SplitOp::Toggle => toggle_split(&mut ctx).await,
                SplitOp::Reattach => reattach_split_pane(&mut ctx).await,
                SplitOp::Promote => promote_split_if_unfocused(&mut ctx).await,
                SplitOp::ReattachFocused => reattach_focused_pane(&mut ctx).await,
            }
        }
        (model, conn, reattach)
    }

    #[tokio::test]
    async fn toggling_the_split_on_picks_the_adjacent_live_pane() {
        let mut model = model_with_pane_ids(&["p1", "p2"]);
        model.focused = Some("p1".into());
        let (model, _, reattach) = run_split_op(
            SplitOp::Toggle,
            model,
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.split.as_deref(), Some("p2"));
        assert!(
            reattach.split.at.is_some(),
            "the attach is scheduled, not performed inline — see toggle_split"
        );
        assert_eq!(
            reattach.split.backoff,
            Duration::from_millis(500),
            "one rung consumed: the first attempt is 250ms out"
        );
    }

    #[tokio::test]
    async fn toggling_the_split_off_gives_the_slot_up_completely() {
        let model = split_model();
        let conn = conn_attached_to(Some("p1"), Some("p2"));
        let mut reattach = Reattachers::default();
        reattach.split.schedule();
        let (model, conn, reattach) = run_split_op(SplitOp::Toggle, model, conn, reattach).await;
        assert_eq!(model.split, None);
        assert_eq!(conn.split.attached_to, None);
        assert_eq!(
            reattach.split.at, None,
            "no wakeup may survive into a slot nothing wants"
        );
        assert_eq!(reattach.split.backoff, RECONNECT_INITIAL_BACKOFF);
    }

    #[tokio::test]
    async fn a_single_live_pane_cannot_be_split_against_itself() {
        // `adjacent_live_pane` wraps, so with one live pane it offers the
        // focused pane straight back — and both slots pointing at one id
        // would break the invariant `Model::split` documents.
        let mut model = model_with_pane_ids(&["p1"]);
        model.focused = Some("p1".into());
        let (model, _, reattach) = run_split_op(
            SplitOp::Toggle,
            model,
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.split, None);
        assert_eq!(reattach.split.at, None, "and nothing is scheduled either");
        // Refusing silently made the chord indistinguishable from an unbound
        // key.
        assert!(
            model
                .notice_text()
                .is_some_and(|t| t.contains("CANNOT SPLIT")),
            "the refusal must say so on the status bar: {:?}",
            model.notice_text()
        );
    }

    #[tokio::test]
    async fn splitting_with_nothing_focused_is_refused() {
        let model = model_with_pane_ids(&["p1", "p2"]);
        let (model, _, reattach) = run_split_op(
            SplitOp::Toggle,
            model,
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.split, None);
        assert_eq!(reattach.split.at, None);
        assert!(
            model
                .notice_text()
                .is_some_and(|t| t.contains("CANNOT SPLIT")),
            "the refusal must say so on the status bar: {:?}",
            model.notice_text()
        );
    }

    #[tokio::test]
    async fn a_dead_adjacent_pane_is_never_offered_as_the_split() {
        let mut model = model_with_pane_ids(&["p1", "p2"]);
        model.local_mut().panes[1].live = false;
        model.focused = Some("p1".into());
        let (model, _, _) = run_split_op(
            SplitOp::Toggle,
            model,
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(
            model.split, None,
            "the only other pane is dead, so there is nothing to put beside p1"
        );
    }

    #[tokio::test]
    async fn a_scheduled_split_reattach_with_no_split_wanted_stands_down() {
        let model = model_with_pane_ids(&["p1"]);
        let conn = conn_attached_to(None, Some("gone"));
        let mut reattach = Reattachers::default();
        reattach.split.schedule();
        reattach.split.schedule();
        let (model, conn, reattach) = run_split_op(SplitOp::Reattach, model, conn, reattach).await;
        assert_eq!(model.split, None);
        assert_eq!(conn.split.attached_to, None, "the socket is released");
        assert_eq!(
            reattach.split.at, None,
            "and crucially nothing is rescheduled: one wakeup, not a ladder"
        );
        assert_eq!(reattach.split.backoff, RECONNECT_INITIAL_BACKOFF);
    }

    #[tokio::test]
    async fn a_scheduled_split_reattach_for_a_dead_pane_gives_the_slot_up() {
        let mut model = model_with_pane_ids(&["p1", "p2"]);
        model.local_mut().panes[1].live = false;
        model.focused = Some("p1".into());
        model.split = Some("p2".into());
        model.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (conflict): taken over".into(),
        );
        let (model, conn, reattach) = run_split_op(
            SplitOp::Reattach,
            model,
            conn_attached_to(Some("p1"), None),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.split, None);
        assert_eq!(reattach.split.at, None);
        assert!(
            !model.detached_pane,
            "the split slot owns no `detached_pane` — that flag is the focused pane's"
        );
        assert!(
            model.notice.is_some(),
            "nor does it clear the status bar on the focused pane's behalf"
        );
        assert_eq!(
            conn.focused.attached_to.as_deref(),
            Some("p1"),
            "the focused slot is untouched"
        );
    }

    // --- A failed split attach must be visible and must retry, not leave
    // half the screen frozen and silent.
    //
    // These two reach a real (immediately refused, loopback, port 0) connect
    // — that is the point: the failure branch cannot be exercised without a
    // failing attach. ---

    /// A model wanting `p2` in the split slot, with `p2` live so
    /// `reattach_split_pane` actually calls through to `attach_split`.
    fn model_wanting_a_split() -> Model {
        let mut m = model_with_pane_ids(&["p1", "p2"]);
        m.local_mut().panes[1].title = "run the suite".into();
        m.focused = Some("p1".into());
        m.split = Some("p2".into());
        m
    }

    #[tokio::test]
    async fn a_failed_split_attach_says_so_on_the_status_bar() {
        // Silence is the defect: `terms` keeps the pane's last grid, so the
        // renderer goes on drawing a frozen snapshot that looks exactly like
        // a live agent with nothing to say.
        let (model, _, _) = run_split_op(
            SplitOp::Reattach,
            model_wanting_a_split(),
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        let (origin, notice) = model.notice.expect("a failed split attach must be visible");
        assert_eq!(
            origin,
            NoticeOrigin::SplitPane,
            "and must be tagged as the split slot's, so a *focused* attach \
             succeeding cannot retract it: {notice}"
        );
        assert!(notice.contains("SPLIT PANE ERROR"), "{notice}");
        assert!(
            notice.contains("claude") && notice.contains("run the suite"),
            "and must name which pane it is about: {notice}"
        );
        assert!(!notice.contains('\u{1b}'), "sanitized: {notice:?}");
    }

    #[tokio::test]
    async fn a_failed_split_attach_climbs_its_ladder_instead_of_giving_up() {
        // The `select!` arm cleared `at` before calling in, and no socket
        // was ever opened, so nothing else in the loop will ever reschedule
        // this slot. Without the `else`, the split stayed dead until a
        // reconnect or a manual double-toggle.
        let mut reattach = Reattachers::default();
        reattach.split.at = None;
        let (_, conn, reattach) = run_split_op(
            SplitOp::Reattach,
            model_wanting_a_split(),
            PaneConn::default(),
            reattach,
        )
        .await;
        assert_eq!(
            conn.split.attached_to, None,
            "precondition: nothing attached"
        );
        assert!(
            reattach.split.at.is_some(),
            "a retry must be pending after a failed attach"
        );
        assert_eq!(
            reattach.split.backoff,
            Duration::from_millis(500),
            "and it must consume a rung, so repeated failures back off"
        );
    }

    // --- The two *success* clear-sites, at the sites themselves rather
    // than only against the primitive.
    //
    // Both are reachable without a server: `PaneSlot::focus` short-circuits
    // to `Ok(())` when the slot is already attached to the pane it's being
    // asked for, and `PaneSlot::resize` is a no-op with no socket. So a slot
    // whose `attached_to` is pre-set walks the whole success branch —
    // including the notice clear — with no network at all. ---

    /// Runs `focus_pane(pane_id, ..)` with the focused slot already claiming
    /// `pane_id`, so the attach succeeds locally, and hands back the model.
    async fn focus_pane_succeeding(model: Model, pane_id: &str) -> Model {
        let mut model = model;
        let mut conn = conn_attached_to(Some(pane_id), None);
        let mut reattach = Reattachers::default();
        let mut terms = HashMap::new();
        {
            let mut ctx = PaneCtx {
                base_url: "ws://127.0.0.1:0",
                token: None,
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area: Rect::new(28, 0, 52, 11),
            };
            focus_pane(pane_id, &mut ctx).await;
        }
        model
    }

    // --- The sidebar's selected space follows focus (2026-08-12: "I added a
    // new pane in 2 spaces and nothing shows up"). `active_space_id` prefers
    // the selection, so a selection pinned to whatever the user last clicked
    // left the tab bar showing a different space's panes than the focused
    // pane lives in — a pane that existed, was focused, and was streaming,
    // with its tab in a strip that was not on screen. ---

    #[tokio::test]
    async fn focusing_a_pane_selects_the_space_it_lives_in() {
        let mut model = model_with_pane_ids(&["p1"]);
        model.local_mut().workspaces = vec![
            spaces_proto::WorkspaceInfo {
                git: None,
                id: "w0".into(),
                name: "other".into(),
                color: None,
                pane_ids: vec![],
            },
            spaces_proto::WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "home".into(),
                color: None,
                pane_ids: vec![],
            },
        ];
        // Pinned to a *different* space, exactly as a prior sidebar click
        // would leave it.
        model.selected_space = Some("w0".into());
        let model = focus_pane_succeeding(model, "p1").await;
        assert_eq!(model.selected_space.as_deref(), Some("w1"));
        assert_eq!(
            model.active_space_id(),
            Some("w1"),
            "so the tab bar shows the focused pane's own space"
        );
    }

    #[tokio::test]
    async fn returning_to_a_space_comes_back_to_the_pane_you_left_it_on() {
        let mut model = model_with_pane_ids(&["a", "b", "c"]);
        model.local_mut().panes[2].workspace_id = Some("w2".into());
        model.local_mut().workspaces = vec![
            spaces_proto::WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "one".into(),
                color: None,
                pane_ids: vec![],
            },
            spaces_proto::WorkspaceInfo {
                git: None,
                id: "w2".into(),
                name: "two".into(),
                color: None,
                pane_ids: vec![],
            },
        ];
        // Without a memory, w1 hands back its first pane.
        assert_eq!(model.pane_to_focus_in("w1").as_deref(), Some("a"));

        // Focus the second one there, then go elsewhere and come back.
        let model = focus_pane_succeeding(model, "b").await;
        assert_eq!(
            model.local().last_focused.get("w1").map(String::as_str),
            Some("b")
        );
        assert_eq!(model.pane_to_focus_in("w1").as_deref(), Some("b"));
        // And the other space is unaffected — one memory per space.
        assert_eq!(model.pane_to_focus_in("w2").as_deref(), Some("c"));
    }

    #[tokio::test]
    async fn focusing_a_pane_the_model_does_not_know_yet_leaves_the_selection_alone() {
        // The window between `pane.create` returning an id and `pane.created`
        // arriving on the control channel. Nothing to read a space from, so
        // the selection `handle_command` already made must survive.
        let mut model = model_with_pane_ids(&[]);
        model.selected_space = Some("w1".into());
        let model = focus_pane_succeeding(model, "brand-new").await;
        assert_eq!(model.selected_space.as_deref(), Some("w1"));
        assert_eq!(model.focused.as_deref(), Some("brand-new"));
    }

    #[tokio::test]
    async fn a_focus_that_cannot_attach_schedules_a_retry() {
        // The focused slot used to have no retry at all: the `select!` arm
        // clears `reattach.focused.at` before this runs, and only a socket
        // *close* schedules a new attempt — of which there is none, because
        // no socket was ever opened. A pane created by the tab bar's `+`
        // whose PTY was slow to spawn stayed blank under `PANE DISCONNECTED`
        // forever.
        let mut model = model_with_pane_ids(&["p1"]);
        // Attached to nothing, and pointed at a port nothing is listening on,
        // so `PaneSlot::focus` genuinely fails rather than short-circuiting.
        let mut conn = conn_attached_to(None, None);
        let mut reattach = Reattachers::default();
        let mut terms = HashMap::new();
        {
            let mut ctx = PaneCtx {
                base_url: "ws://127.0.0.1:1",
                token: None,
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area: Rect::new(28, 1, 52, 11),
            };
            focus_pane("p1", &mut ctx).await;
        }
        assert!(
            model.detached_pane,
            "precondition: the attach actually failed"
        );
        assert!(
            reattach.focused.at.is_some(),
            "a failed attach must leave something scheduled to try again"
        );
    }

    #[tokio::test]
    async fn a_successful_focus_clears_that_panes_own_error() {
        let mut model = model_with_pane_ids(&["p1"]);
        model.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (claude, conflict): taken over".into(),
        );
        let model = focus_pane_succeeding(model, "p1").await;
        assert!(
            !model.detached_pane,
            "precondition: the attach actually succeeded"
        );
        assert_eq!(model.notice, None);
    }

    #[tokio::test]
    async fn a_successful_focus_does_not_clear_a_split_panes_error() {
        // Reachable with no user action: `reattach_focused_pane` runs off a
        // scheduled wakeup, and the split slot's ladder re-raises its own
        // error a rung later — so unconditionally clearing here made a live
        // `SPLIT PANE ERROR` flicker instead of staying readable.
        let mut model = model_with_pane_ids(&["p1", "p2"]);
        model.split = Some("p2".into());
        model.set_notice(
            NoticeOrigin::SplitPane,
            "SPLIT PANE ERROR (claude): no response within 2s".into(),
        );
        let model = focus_pane_succeeding(model, "p1").await;
        assert!(!model.detached_pane, "precondition: the attach succeeded");
        assert_eq!(
            model.notice_text(),
            Some("SPLIT PANE ERROR (claude): no response within 2s"),
            "the other half of the screen is still broken and must say so"
        );
    }

    /// Runs `attach_split(pane_id, ..)` with the split slot already claiming
    /// `pane_id`, so the attach succeeds locally, and hands back the model.
    async fn attach_split_succeeding(model: Model, pane_id: &str) -> Model {
        let mut model = model;
        let mut conn = conn_attached_to(Some("p1"), Some(pane_id));
        let mut reattach = Reattachers::default();
        let mut terms = HashMap::new();
        {
            let mut ctx = PaneCtx {
                base_url: "ws://127.0.0.1:0",
                token: None,
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area: Rect::new(28, 0, 52, 11),
            };
            attach_split(pane_id, &mut ctx).await;
        }
        model
    }

    #[tokio::test]
    async fn a_successful_split_attach_clears_that_slots_own_error() {
        let mut model = split_model();
        model.set_notice(
            NoticeOrigin::SplitPane,
            "SPLIT PANE ERROR (claude): no response within 2s".into(),
        );
        let model = attach_split_succeeding(model, "p2").await;
        assert_eq!(model.notice, None);
    }

    #[tokio::test]
    async fn a_successful_split_attach_does_not_clear_the_focused_panes_error() {
        // Both slots stream at once, so a `PANE ERROR` on the focused socket
        // and a split attach completing milliseconds later is an ordinary
        // interleaving — and this used to wipe the line explaining why the
        // pane the user is *typing into* stopped responding.
        let mut model = split_model();
        model.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (claude, unauthorized): token expired".into(),
        );
        let model = attach_split_succeeding(model, "p2").await;
        assert_eq!(
            model.notice_text(),
            Some("PANE ERROR (claude, unauthorized): token expired")
        );
    }

    // --- Pin that `take_focus` runs before `focus_pane` awaits anything. ---

    #[tokio::test]
    async fn focus_pane_records_its_whole_preamble_within_its_very_first_poll() {
        // Polled exactly once and then dropped, rather than awaited to
        // completion. Whatever `focus_pane` has done by the end of that
        // single poll is everything it can guarantee against a cancellation,
        // because a future is only ever dropped at a suspension point.
        //
        // Inserting any not-immediately-ready `.await` above
        // `take_focus(...)` — which is precisely how an earlier
        // cancel-safety defect was introduced — makes the first poll suspend
        // before the preamble runs, and fails this. It cannot catch an
        // inserted await that completes on its first poll; that gap is a
        // known limitation of this test.
        let mut model = split_model();
        model.detached_pane = false;
        // The split slot is deliberately *un*attached, so the swap cannot
        // let `PaneSlot::focus` short-circuit on "already attached to this
        // pane" — the attach has to be genuinely reached for the poll to
        // suspend there.
        let mut conn = conn_attached_to(Some("p1"), None);
        let mut reattach = Reattachers::default();
        let mut terms = HashMap::new();
        {
            let mut ctx = PaneCtx {
                base_url: "ws://127.0.0.1:1",
                token: None,
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area: Rect::new(28, 0, 52, 11),
            };
            let mut focusing = std::pin::pin!(focus_pane("p2", &mut ctx));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let _ = std::future::Future::poll(focusing.as_mut(), &mut cx);
            // Dropped here — exactly what a losing `select!` branch or an
            // expiring outer timeout does to it.
        }
        assert!(
            model.detached_pane,
            "the pessimistic flag must already be set: nothing has attached yet"
        );
        assert_eq!(model.focused.as_deref(), Some("p2"));
        assert_eq!(model.split.as_deref(), Some("p1"), "the fields swapped");
        assert_eq!(
            conn.split.attached_to.as_deref(),
            Some("p1"),
            "and the sockets swapped with them, in the same poll"
        );
    }

    // --- A status-bar line about one of two visible panes has to say
    // which. ---

    #[test]
    fn a_pane_error_notice_names_the_pane_it_is_about() {
        let mut model = model_with_pane_ids(&["p1"]);
        model.local_mut().panes[0].title = "fix the login bug".into();
        let mut terms = HashMap::new();
        let notice = handle_pane_event(
            PaneEvent::Error {
                code: "conflict".into(),
                message: "taken over by another client".into(),
            },
            Some("p1"),
            &model,
            &mut terms,
        )
        .expect("surfaced");
        assert!(notice.contains("claude"), "{notice}");
        assert!(notice.contains("fix the login bug"), "{notice}");
        assert!(notice.contains("conflict"), "{notice}");
        assert!(notice.contains("taken over by another client"), "{notice}");
    }

    #[test]
    fn a_pane_the_model_has_no_record_of_is_named_by_its_id() {
        assert_eq!(pane_display_name(&Model::default(), "p-42"), "p-42");
    }

    #[test]
    fn an_untitled_pane_is_named_by_its_agent_type_alone() {
        let model = model_with_pane_ids(&["p1"]);
        assert_eq!(pane_display_name(&model, "p1"), "claude");
    }

    // --- Nothing focused with a healthy split pane once left a half-blank
    // screen with no explanation. ---

    #[tokio::test]
    async fn a_surviving_split_pane_is_promoted_when_focus_is_lost() {
        let mut model = model_with_pane_ids(&["p2"]);
        model.split = Some("p2".into());
        let (model, conn, reattach) = run_split_op(
            SplitOp::Promote,
            model,
            conn_attached_to(None, Some("p2")),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.focused.as_deref(), Some("p2"), "promoted");
        assert_eq!(model.split, None, "and the layout collapses to one pane");
        assert_eq!(
            conn.focused.attached_to.as_deref(),
            Some("p2"),
            "the socket moved with it rather than being rebuilt"
        );
        assert!(
            !model.detached_pane,
            "and it is attached, so the status bar must not claim otherwise"
        );
        assert_eq!(conn.split.attached_to, None);
        assert_eq!(reattach.split.at, None);
    }

    #[tokio::test]
    async fn promotion_is_a_no_op_while_something_is_focused() {
        // Reached on essentially every control-channel event, so the common
        // case must change nothing at all.
        let (model, conn, _) = run_split_op(
            SplitOp::Promote,
            split_model(),
            conn_attached_to(Some("p1"), Some("p2")),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.focused.as_deref(), Some("p1"));
        assert_eq!(model.split.as_deref(), Some("p2"));
        assert_eq!(conn.focused.attached_to.as_deref(), Some("p1"));
        assert_eq!(conn.split.attached_to.as_deref(), Some("p2"));
    }

    #[tokio::test]
    async fn promotion_does_nothing_when_there_is_no_split_to_promote() {
        let (model, _, _) = run_split_op(
            SplitOp::Promote,
            model_with_pane_ids(&["p1"]),
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.focused, None);
        assert_eq!(model.split, None);
    }

    // --- A promoted pane that arrived with no socket once had nothing
    // scheduled to give it one. Same failure family as a failed split
    // attach that never retried — a visible pane with no socket and no
    // pending attempt — so it is pinned by asserting the *schedule*,
    // which is exactly the assertion whose absence let it through. ---

    #[test]
    fn a_wanted_pane_with_no_socket_gets_an_attempt_scheduled() {
        let mut ladder = Reattach::default();
        ensure_reattach_pending(Some("p1"), &PaneSlot::default(), &mut ladder);
        assert!(ladder.at.is_some());
        assert_eq!(
            ladder.backoff,
            Duration::from_millis(500),
            "one rung consumed: the attempt itself is the first, 250ms out"
        );
    }

    #[test]
    fn a_slot_that_already_has_a_socket_needs_no_attempt() {
        let mut ladder = Reattach::default();
        let mut slot = PaneSlot::default();
        slot.attached_to = Some("p1".into());
        ensure_reattach_pending(Some("p1"), &slot, &mut ladder);
        assert_eq!(ladder.at, None);
    }

    #[test]
    fn a_slot_the_model_wants_nothing_in_needs_no_attempt() {
        let mut ladder = Reattach::default();
        ensure_reattach_pending(None, &PaneSlot::default(), &mut ladder);
        assert_eq!(ladder.at, None);
    }

    #[test]
    fn an_attempt_already_on_the_way_is_not_replaced_or_advanced() {
        // The subtle half: `schedule()` advances the rung, so a caller that
        // ran twice would climb the ladder without a single failed attempt
        // in between — turning an unrelated repeated call into an
        // ever-slower reattach.
        let mut ladder = Reattach::default();
        ladder.schedule();
        let (pending, rung) = (ladder.at, ladder.backoff);
        ensure_reattach_pending(Some("p1"), &PaneSlot::default(), &mut ladder);
        assert_eq!(ladder.at, pending, "the existing deadline is untouched");
        assert_eq!(ladder.backoff, rung, "and the rung does not creep");
    }

    #[tokio::test]
    async fn a_promoted_pane_with_no_socket_gets_a_pending_reattach() {
        // The regression's own shape, at the smallest scope that produces
        // it: both slots empty (what `conn.detach_all()` leaves behind when
        // the control channel drops) and a split pane to promote.
        let mut model = model_with_pane_ids(&["p2"]);
        model.split = Some("p2".into());
        let (model, conn, reattach) = run_split_op(
            SplitOp::Promote,
            model,
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.focused.as_deref(), Some("p2"));
        assert_eq!(conn.focused.attached_to, None, "precondition: no socket");
        assert!(
            reattach.focused.at.is_some(),
            "nothing else can ever schedule this pane: no socket was opened, \
             so `PaneSlot::recv` stays pending forever and the close arm \
             cannot fire"
        );
    }

    #[tokio::test]
    async fn the_reconnect_path_that_promotes_a_pane_still_reattaches_it() {
        // The full failing sequence, at the highest level reachable
        // without the `select!` loop. The control channel dropped and came
        // back; the focused pane's record survived but it is no longer
        // live, so `reattach_focused_pane` clears focus and promotes the
        // split pane — and the reconnect arm's own
        // `if model.split.is_some()` has already been evaluated by then, so
        // this is the last chance to schedule anything.
        let mut model = model_with_pane_ids(&["p1", "p2"]);
        model.local_mut().panes[0].live = false;
        model.focused = Some("p1".into());
        model.split = Some("p2".into());
        let (model, conn, reattach) = run_split_op(
            SplitOp::ReattachFocused,
            model,
            // Post-`conn.detach_all()`: the reconnect emptied both slots.
            PaneConn::default(),
            // Post-`stand_down()` on both ladders, as the reconnect arm
            // leaves them.
            Reattachers::default(),
        )
        .await;
        assert_eq!(
            model.focused.as_deref(),
            Some("p2"),
            "the dead pane's slot is given to the survivor"
        );
        assert_eq!(model.split, None);
        assert!(model.detached_pane, "and it has no socket yet");
        assert_eq!(conn.focused.attached_to, None);
        assert!(
            reattach.focused.at.is_some(),
            "so a reattach must be pending, or the pane is stranded until \
             the user refocuses it by hand"
        );
    }

    #[tokio::test]
    async fn promoting_a_pane_that_kept_its_socket_schedules_nothing() {
        // The other half: a pointless wakeup for a slot that is already
        // attached is exactly what `Reattach::stand_down` exists to avoid.
        let mut model = model_with_pane_ids(&["p2"]);
        model.split = Some("p2".into());
        let (_, _, reattach) = run_split_op(
            SplitOp::Promote,
            model,
            conn_attached_to(None, Some("p2")),
            Reattachers::default(),
        )
        .await;
        assert_eq!(reattach.focused.at, None);
        assert_eq!(reattach.focused.backoff, RECONNECT_INITIAL_BACKOFF);
    }

    #[tokio::test]
    async fn a_promoted_pane_with_no_socket_is_reported_as_detached() {
        // Promotion reads the slot's actual attachment rather than assuming;
        // there is no await between the read and the write, so the accurate
        // value is also the safe one.
        let mut model = model_with_pane_ids(&["p2"]);
        model.split = Some("p2".into());
        let (model, _, _) = run_split_op(
            SplitOp::Promote,
            model,
            PaneConn::default(),
            Reattachers::default(),
        )
        .await;
        assert_eq!(model.focused.as_deref(), Some("p2"));
        assert!(model.detached_pane);
    }
}
