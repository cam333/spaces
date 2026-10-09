//! `spaces-tui --url ws://host:port [--token TOKEN]`
//!
//! Connects to the Spaces runtime's control channel, snapshots session
//! state, subscribes to incremental events, renders the sidebar/pane/status
//! UI (`ui::render`), and routes keystrokes (`input::route`) — the event
//! loop that finally calls into every module the earlier tasks in this
//! phase built. See the doc comments on [`run`], [`TerminalGuard`], and
//! [`install_panic_hook`] for the specific behaviors this file is
//! responsible for: terminal restoration on every exit path, reconnect with
//! backoff, resnapshot on a sequence gap, and refusing (not silently
//! dropping) input while disconnected.
//!
//! # The pane-connection state machine lives in [`panes`]
//!
//! Which panes are attached, keeping their sockets sized, and getting them
//! back after a failure is one cohesive unit, moved out of this file into
//! its own module as a pure, behaviour-free change. This file keeps the
//! event loop that drives it.
//!
//! # What the automated tests cannot verify
//!
//! Killing the binary with a window close button or with `ctrl+\`, then
//! confirming the shell is usable afterward, needs a real window and a
//! real interactive terminal, so it is not covered automatically.
//! What *is* verified (see this module's `#[cfg(test)]` block) is the exact
//! `Drop`/panic-hook mechanism [`TerminalGuard`] and
//! [`install_panic_hook`] are built from — that a guard's `Drop` runs its
//! restore action exactly once, including when it drops during a
//! panicking unwind, and that the panic hook's composition runs the
//! restore action before delegating to whatever hook was previously
//! installed. Whether a *real* `ctrl+\`/window-close, on a *real*
//! interactive terminal, ends up calling that same code path was not
//! exercised end-to-end. Recorded here rather than left unstated.
//!
//! # Peer client, not a shell for the Node runtime
//!
//! This binary never imports from `bin/`, never opens the runtime's
//! database, and never shells out to Node for anything the protocol already
//! exposes — everything it knows arrives over the two WebSocket connections
//! `spaces-proto` speaks (`ControlClient`, `PaneSocket`). Detection/agent
//! state in particular is never computed here: it only ever arrives via
//! `pane.state` events and `session.snapshot`, folded forward by
//! [`model::Model`].

mod editor;
mod files;
mod input;
mod model;
mod mouse_forward;
mod panes;
mod term;
mod transfer;
mod ui;

use std::collections::HashMap;
use std::io;
use std::io::Write;
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use spaces_proto::client::{ControlClient, EventFrameInner};
use spaces_proto::{Snapshot, SubscribeAck};

use input::{route, route_mouse, Action};
use model::{ApplyOutcome, ConnState, Model, NodeId, NoticeOrigin};
use panes::{
    focus_pane, on_slot_event, promote_split_if_unfocused, prune_terms, reattach_due,
    reattach_focused_pane, reattach_split_pane, resize_visible, toggle_split, PaneConn,
    Reattachers, Slot,
};
use term::PaneTerm;
use ui::{Command, Palette};

/// First reconnect attempt happens immediately (no wait); every attempt
/// after a failure backs off starting here, doubling up to
/// [`RECONNECT_CAP`] — see [`next_backoff`] for the exact sequence.
const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const RECONNECT_CAP: Duration = Duration::from_secs(8);

/// UI redraw cadence. Redraws are coalesced onto this tick (see `run`'s
/// `dirty` flag) rather than happening synchronously inside every branch
/// that changes visible state — a chatty pane streaming output every few
/// milliseconds would otherwise redraw the whole screen just as often.
const REDRAW_INTERVAL: Duration = Duration::from_millis(33);

/// Upper bound on how long a single UI-triggered network operation (a
/// palette command, or a resnapshot) is allowed to block this event loop's
/// `select!` body before the loop gives up on it and goes back to
/// redrawing/reading input. Every request `spaces-proto` itself makes
/// already bounds itself (`ControlClient::request`'s own 30s
/// `DEFAULT_REQUEST_TIMEOUT`, primarily) — this is a *tighter*, UI-scale
/// bound layered on top: these are local WebSocket round trips, and this
/// workspace's binding constraint is that the loop keeps
/// redrawing/responding to input/detach throughout, not that every request
/// gets the full 30s to prove it's hung before the user gets their
/// terminal back.
const UI_OP_TIMEOUT: Duration = Duration::from_secs(3);

/// Bounds `PaneSocket::attach` specifically, inside [`focus_pane`].
/// Deliberately shorter than [`UI_OP_TIMEOUT`], not equal to it: two
/// *equal-duration* nested timeouts — this one and `handle_command`'s outer
/// [`UI_OP_TIMEOUT`] wrap, for every command that reaches `focus_pane` — raced
/// non-deterministically (and for `Command::CreatePane` specifically,
/// deterministically: its `pane.create` request runs first and consumes part of
/// the shared budget, so the outer clock always has less remaining by the time
/// this one starts). Whichever fired first silently won. A shorter, distinct
/// duration means this timeout — not whatever happens to be left of an outer
/// one some caller wraps this call in — is what actually owns deciding "this
/// attach is taking too long". A bound here is still needed independently of
/// `UI_OP_TIMEOUT`, not just for clarity: two of `focus_pane`'s callers
/// (`reattach_focused_pane`, invoked directly from the reconnect-success and
/// pane-socket-close `select!` arms) have no outer timeout wrapping them at
/// all, and `PaneSocket::attach` has none of its own — without this, a stalled
/// attach would hang those two arms, and the whole event loop with them,
/// indefinitely rather than for a bounded few seconds.
const PANE_ATTACH_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest a status-bar notice built from a server-supplied message is
/// allowed to be. See [`sanitize_notice`].
const NOTICE_MAX_CHARS: usize = 120;

/// What a successful (re)connect produces: a live client plus the snapshot
/// and subscribe-ack needed to prime a [`Model`] with it. Named so
/// `reconnect_task`'s declaration and [`reconnect_attempt`]'s signature
/// don't spell out the same three-tuple-wrapped-in-`Result` inline.
type ConnectOutcome = Result<(ControlClient, Snapshot, SubscribeAck), String>;

fn main() {
    // Installed before anything else runs, so a panic during argument
    // parsing or terminal setup — before any `TerminalGuard` exists to hit
    // its own `Drop` — still restores whatever terminal state was entered
    // up to that point (which, this early, is none; the hook is still
    // correct to install first rather than after the first fallible step).
    install_panic_hook();

    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("usage: spaces-tui --url ws://host:port [--token TOKEN]");
            eprintln!(
                "       SPACES_TOKEN in the environment is preferred over --token, \
                 which puts the secret on this process's command line"
            );
            std::process::exit(2);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("failed to start the async runtime: {e}");
            std::process::exit(1);
        }
    };

    if let Err(message) = runtime.block_on(run(args)) {
        // By the time this prints, `run`'s `TerminalGuard` has already gone
        // out of scope and restored the terminal.
        eprintln!("{message}");
        std::process::exit(1);
    }
}

/// Parsed command-line arguments. `url` is normalized to have no trailing
/// slash — see the doc comment on [`Args::parse`]'s call site in `run` for
/// why that matters.
struct Args {
    url: String,
    token: Option<String>,
}

/// Environment variable read by [`Args::parse`] as the preferred source of
/// the terminal token.
const TOKEN_ENV_VAR: &str = "SPACES_TOKEN";

impl Args {
    /// Parses real process arguments, reading [`TOKEN_ENV_VAR`] from the
    /// real environment. See [`Args::parse_with_env`] for the behavior;
    /// this exists only so tests can supply an environment without
    /// mutating the process's own (`std::env::set_var` is global to the
    /// test binary and racy across parallel tests).
    fn parse<I: Iterator<Item = String>>(args: I) -> Result<Self, String> {
        Self::parse_with_env(args, |key| std::env::var(key).ok())
    }

    /// `SPACES_TOKEN` from `env` wins over `--token` on the command line.
    ///
    /// `--token` used to be the only source, and `bin/spaces.js`'s heap re-exec
    /// forwards its own argv verbatim, so `spaces tui --token S` put the secret
    /// on three command lines at once. `/proc/<pid>/cmdline` is world-readable
    /// on Linux by default, and Task Manager / WMI `CommandLine` expose the
    /// same thing on Windows; it also lands in shell history. An environment
    /// variable is readable by the same user's other processes on Linux too
    /// (`/proc/<pid>/environ` is owner-only, unlike `cmdline`), so this is a
    /// real narrowing, not a claim of secrecy.
    ///
    /// `--token` is kept, not removed: it is the documented interface, and
    /// a caller with no ability to set an environment variable still needs
    /// *some* way to authenticate. An empty `SPACES_TOKEN` is treated as
    /// unset rather than as an explicit empty token, matching how
    /// `spaces-proto` already treats `Some("")`.
    fn parse_with_env<I, F>(mut args: I, env: F) -> Result<Self, String>
    where
        I: Iterator<Item = String>,
        F: Fn(&str) -> Option<String>,
    {
        let mut url = None;
        let mut token = None;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--url" => {
                    url = Some(args.next().ok_or("--url requires a value")?);
                }
                "--token" => {
                    token = Some(args.next().ok_or("--token requires a value")?);
                }
                other => return Err(format!("unrecognized argument: {other}")),
            }
        }
        let token = env(TOKEN_ENV_VAR)
            .filter(|t| !t.is_empty())
            .or(token)
            .filter(|t| !t.is_empty());
        let url = url.ok_or("--url is required")?;
        // `ControlClient::connect` wants the full `/proto` path,
        // `PaneSocket::attach` wants a bare base and builds `/proto/pane/<id>`
        // itself. Stripping a trailing slash here once means both
        // `format!("{url}/proto")` below and handing this straight to
        // `PaneSocket::attach` are safe from a double slash, regardless of
        // whether the caller passed `--url` with one.
        let url = url.trim_end_matches('/').to_string();
        Ok(Self { url, token })
    }
}

// ─── Terminal restoration ──────────────────────────────────────────────

/// Disables mouse capture, disables raw mode, leaves the alternate screen,
/// and shows the cursor — in that order (see [`restore_sequence`]). Every
/// step is best-effort (`let _ = ...`): this runs from
/// [`TerminalGuard::drop`] (possibly mid-panic-unwind) and from
/// [`install_panic_hook`]'s hook (definitely mid-panic) — a *second*
/// failure while trying to clean up must never itself panic or replace the
/// original error/panic the caller is already unwinding from. Safe to call
/// even when the terminal was never put into raw mode / the alternate
/// screen / mouse capture in the first place (e.g. a very early startup
/// failure): crossterm treats disabling raw mode when not enabled as a
/// no-op rather than an error condition worth surfacing, and an unrequested
/// `DisableMouseCapture`/`LeaveAlternateScreen` is no worse than an inert
/// escape sequence on a terminal that never received the matching `Enable`.
///
/// Mouse capture is disabled **first**, as its own independent step, not
/// batched into the same `execute!` call as leaving the alternate screen.
/// This is load-bearing, not stylistic: on Windows, crossterm's
/// `DisableMouseCapture` bypasses `execute!`'s writer entirely and calls
/// `SetConsoleMode` directly, looking up a module-static "original console
/// mode" that only a prior, successful `EnableMouseCapture` ever sets — see
/// crossterm 0.28.1's `event/sys/windows.rs`. If [`TerminalGuard::enter`]'s
/// own best-effort mouse-capture enable never actually engaged (an
/// unsupported terminal, say), that lookup fails and `DisableMouseCapture`
/// returns `Err`. `execute!`'s multi-command form chains commands with
/// `.and_then`, so one failing command in a batch stops the rest of *that
/// same batch* from running — had this stayed batched with
/// `LeaveAlternateScreen`, a mouse-capture failure on entry would silently
/// turn into "never leaves the alternate screen" on exit: exactly the
/// "worse than refusing to start" failure this workspace's binding
/// constraint rules out. As its own statement, its failure can only ever
/// cost the mouse escape sequence, never the rest of the restoration.
fn restore_terminal() {
    restore_sequence(
        || {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        },
        || {
            let _ = disable_raw_mode();
        },
        || {
            let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        },
    );
}

/// Runs `disable_mouse`, then `disable_raw_mode`, then `leave_alt_screen` —
/// the three independent steps [`restore_terminal`] composes into real
/// crossterm calls. Generic over the three actions for the same reason
/// [`restore_then`] is generic over its two: so
/// `the_restoration_sequence_disables_mouse_capture_before_leaving_the_alternate_screen`
/// (below, in tests) can pin this exact ordering with fake, order-recording
/// closures, without a real terminal attached to the test process — and,
/// specific to mouse capture, without ever invoking the *real*
/// `DisableMouseCapture`/`EnableMouseCapture` commands, whose effect a test
/// cannot observe through any fake writer on Windows regardless (see
/// `restore_terminal`'s own doc comment): they bypass the writer argument
/// entirely and call a real `SetConsoleMode` instead.
fn restore_sequence<M: FnMut(), R: FnMut(), L: FnMut()>(
    mut disable_mouse: M,
    mut disable_raw_mode: R,
    mut leave_alt_screen: L,
) {
    disable_mouse();
    disable_raw_mode();
    leave_alt_screen();
}

/// RAII guard: [`TerminalGuard::enter`] enables raw mode, switches to the
/// alternate screen, and enables mouse capture; `Drop` restores all three,
/// running during a normal return *and* during a panicking unwind (`Drop`
/// always runs on unwind unless the process is built with `panic =
/// "abort"`, which this workspace is not).
///
/// Generic over the restore action rather than hardcoding a call to
/// [`restore_terminal`] so the exact `Drop` mechanism can be exercised by
/// this module's tests without a real terminal attached to the test
/// process — see the module doc comment's "What the automated tests cannot
/// verify" section for why that distinction matters here.
struct TerminalGuard<F: FnMut()> {
    restore: F,
}

impl<F: FnMut()> Drop for TerminalGuard<F> {
    fn drop(&mut self) {
        (self.restore)();
    }
}

impl TerminalGuard<fn()> {
    /// Enters raw mode, the alternate screen, and mouse capture, returning a
    /// guard whose `Drop` calls [`restore_terminal`] — the same function
    /// [`install_panic_hook`] calls directly, so a panic mid-session
    /// restores the terminal *before* the panic message prints (see that
    /// function's doc comment) rather than only once unwinding reaches this
    /// guard.
    ///
    /// Mouse capture, unlike the two steps above it, is best-effort
    /// (`let _ = ...`): a terminal that doesn't support it (unusual, but
    /// real — an embedded terminal, a stripped-down `TERM=dumb`-style
    /// environment) must not be prevented from starting the TUI at all over
    /// a capability that was never load-bearing — keystroke routing and
    /// pane input work identically with no mouse capture enabled. Contrast
    /// the alternate-screen switch just above, whose failure *does* abort
    /// startup (and unwinds raw mode) precisely because the whole UI is
    /// unusable without it. [`restore_terminal`]'s own `DisableMouseCapture`
    /// call is unconditionally best-effort too, so this asymmetry is safe
    /// either way: disabling something that was never successfully enabled
    /// is a no-op there, not a failure.
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        if let Err(e) = execute!(io::stdout(), EnterAlternateScreen) {
            // Raw mode already succeeded above — undo it rather than
            // leaving the caller's terminal half-modified on a failed
            // start.
            let _ = disable_raw_mode();
            return Err(e);
        }
        let _ = execute!(io::stdout(), EnableMouseCapture);
        Ok(Self {
            restore: restore_terminal,
        })
    }
}

/// Runs `restore`, then `then` — the ordering [`install_panic_hook`] needs
/// (restore the terminal, *then* let whatever hook was previously installed
/// print the panic). Pulled out into its own generic function so
/// `restore_then_runs_restore_before_the_delegated_hook` (below, in tests)
/// can prove that ordering without this test process installing a real,
/// process-global `std::panic::set_hook` — doing that from within a test
/// would leak into every other test in this binary and, in production,
/// runs against the real terminal (not something a unit test should touch,
/// and `std::panic::PanicHookInfo` has no public constructor to fake one
/// with anyway).
fn restore_then<R: FnMut(), N: FnMut()>(mut restore: R, mut then: N) {
    restore();
    then();
}

/// Installs a panic hook that restores the terminal (see [`restore_terminal`])
/// before delegating to whatever hook was previously installed — normally
/// the default hook, which prints the panic message. Without this, a panic
/// while the terminal is in raw mode / the alternate screen would print
/// (or corrupt) the panic message somewhere the user can't see it, and
/// leave their shell in raw mode after the process exits.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_then(restore_terminal, || previous(info));
    }));
}

// ─── Networking helpers ─────────────────────────────────────────────────

/// Requests a fresh `session.snapshot` and subscribes from its `seq` on an
/// already-connected `client`. Used both for the very first connection and
/// for [`ApplyOutcome::NeedsResnapshot`] (binding constraint: re-request
/// `session.snapshot` and re-subscribe from the new `seq`) — the two
/// situations differ only in whether `client` was just connected or has
/// been talking to the server for a while.
async fn snapshot_and_subscribe(
    client: &mut ControlClient,
) -> Result<(Snapshot, SubscribeAck), String> {
    let value = client
        .request("session.snapshot", serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    let snapshot: Snapshot = serde_json::from_value(value)
        .map_err(|e| format!("session.snapshot returned an unexpected shape: {e}"))?;
    let ack = client
        .subscribe(snapshot.seq)
        .await
        .map_err(|e| e.to_string())?;
    Ok((snapshot, ack))
}

/// The server's home directory, from `node.info`, or an empty string.
///
/// Asked once at connect: it describes the machine the runtime runs on, which
/// does not change for the life of a session. Used to seed the "where should
/// this pane start" prompt on a session with no panes to inherit from — see
/// [`Model::last_known_cwd`].
///
/// Every failure degrades to `""` rather than propagating: a runtime too old
/// to report one, a response of an unexpected shape, or the request failing
/// outright are all "no suggestion available", and none of them is a reason to
/// refuse to start a client whose actual job is elsewhere.
async fn server_home_dir(client: &mut ControlClient) -> String {
    client
        .request("node.info", serde_json::json!({}))
        .await
        .ok()
        .and_then(|info| {
            info.get("homeDir")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Asks the server which machines are in the fleet.
///
/// Returns an empty list when the server is older than `net.peers` or has no
/// peer database — both mean "one machine", which is what the sidebar already
/// draws. A federation-blind server therefore behaves exactly as it did before
/// federation existed, rather than erroring on connect.
/// Runs `method` on `node`, wherever that is.
///
/// A local request goes straight down this connection; a remote one is wrapped
/// in `net.request` and forwarded by the local node over its federated link,
/// where the owning machine's own policy decides whether to honour it.
///
/// Every write goes through here. Before it existed, selecting another machine
/// changed the prompt's title and nothing else: the space was created locally,
/// the pane started locally, and the directory picker offered local paths — the
/// selection was decoration.
async fn request_on(
    client: &mut ControlClient,
    node: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    match node {
        None => client
            .request(method, params)
            .await
            .map_err(|e| e.to_string()),
        Some(node) => client
            .request(
                "net.request",
                serde_json::json!({ "nodeId": node, "method": method, "params": params }),
            )
            .await
            .map_err(|e| e.to_string()),
    }
}

/// The machine a new space or pane should be created on, or `None` for this
/// one — the form [`request_on`] wants.
fn target_of(model: &Model) -> Option<String> {
    let target = model.target_node();
    if target == model.local_id_owned() {
        None
    } else {
        Some(target.as_str().to_string())
    }
}

/// Creates a pane and starts its agent, resuming `session` when one was chosen.
///
/// Shared by [`Command::CreatePane`] and [`Command::CreatePaneWithSession`] so
/// the session is the only difference between them: everything about focus,
/// ownership and error reporting behaves identically whether or not one was
/// picked.
async fn create_pane(
    client: &mut ControlClient,
    ctx: &mut PaneCtx<'_>,
    workspace_id: &str,
    agent_type: &str,
    cwd: Option<String>,
    session: Option<String>,
) {
    // Selected *before* the request, synchronously, for the same
    // reason `CreateWorkspace` clears its notice up front: this arm
    // is bounded by `UI_OP_TIMEOUT` while `request` bounds itself at
    // 30s, so the outer timeout can drop this future at the await
    // below and nothing after it ever runs. A create that succeeds
    // slowly still lands its `pane.created` on the control channel —
    // and the tab bar has to be pointing at the space that pane
    // appeared in, or the user sees no sign of it at all.
    //
    // `take_focus` cannot do this job here: the model has no record
    // of the new pane yet, so it has nothing to read a space from.
    ctx.model.selected_space = Some(workspace_id.to_string());
    // `agentType` is what makes this a pane with an agent in it. The
    // runtime defaults a missing one to `shell`
    // (`bin/lib/runtime/spawn-pane.js`'s `AGENTS` table), so omitting
    // it is not "let the server choose" — it is "spawn a bare shell",
    // which is what this client did for every pane it ever created
    // until the agent picker shipped.
    //
    // `cols`/`rows` are still left to the server's 120x30 default: the
    // pane is resized to its real region by `focus_pane` immediately
    // below, and guessing here would just be a wrong first size.
    //
    // `cwd` is inherited from the space's most recent live pane —
    // see `inherited_cwd`. Omitted when the space has none, which
    // leaves the runtime's own `os.homedir()` default.
    let mut params = serde_json::json!({
        "workspaceId": workspace_id,
        "agentType": agent_type,
    });
    // An explicit `cwd` from the directory prompt wins; otherwise
    // inherit the space's most recent pane. Both may be absent, which
    // leaves the runtime's own `os.homedir()` default.
    if let Some(cwd) = cwd.or_else(|| inherited_cwd(ctx.model, workspace_id)) {
        params["cwd"] = serde_json::Value::String(cwd);
    }
    // Started on the machine that owns the space, not on whichever
    // one this client is connected to.
    let target = ctx
        .model
        .owner_of_space(workspace_id)
        .map(|n| n.as_str().to_string());
    match request_on(client, target.as_deref(), "pane.create", params).await {
        Ok(result) => {
            if let Some(id) = result.get("paneId").and_then(|v| v.as_str()) {
                let id = id.to_string();
                // The pane exists on the far machine but this client
                // has not heard about it: that machine's events go to
                // its own stream. Re-read before focusing, or the
                // attach cannot tell which machine to relay to.
                if target.is_some() {
                    refresh_fleet(client, ctx.model).await;
                }
                focus_pane(&id, ctx).await;
                start_agent_on(
                    client,
                    target.as_deref(),
                    &id,
                    agent_type,
                    session.as_deref(),
                )
                .await;
            }
        }
        Err(e) => ctx.model.set_notice(
            NoticeOrigin::Workspace,
            sanitize_notice(&format!("PANE ERROR: {e}")),
        ),
    }
}

/// The next input event: one read ahead and given back first, else the
/// terminal's next. Cancel-safe, as `EventStream::next` is.
async fn next_input(
    pending: &mut std::collections::VecDeque<Event>,
    events: &mut EventStream,
) -> Option<std::io::Result<Event>> {
    if let Some(event) = pending.pop_front() {
        return Some(Ok(event));
    }
    events.next().await
}

/// Keys closer together than this are not being typed.
const BURST_GAP: Duration = Duration::from_millis(5);
/// A burst at least this long is a drop or paste.
const PASTE_MIN_KEYS: usize = 4;

/// True while the Files list has the keys and would read them as commands —
/// and nothing is open over it: a paste into the palette's search box, a
/// menu, or the key after the chord prefix belongs to those.
fn files_take_commands(model: &Model, palette: &Palette) -> bool {
    !palette.open
        && !palette.awaiting_chord_key
        && palette.tab_menu.is_none()
        && model.files_have_keys()
        && model
            .shown_files()
            .and_then(|(space, _)| model.files.get(space))
            .is_some_and(|v| v.takes_commands())
}

/// `first`, and every key that follows it within [`BURST_GAP`] of the last.
/// Anything else read on the way is kept, in order, for the loop.
async fn read_burst(
    first: crossterm::event::KeyEvent,
    events: &mut EventStream,
    pending: &mut std::collections::VecDeque<Event>,
) -> Vec<crossterm::event::KeyEvent> {
    let mut keys = vec![first];
    loop {
        match tokio::time::timeout(BURST_GAP, events.next()).await {
            Ok(Some(Ok(Event::Key(k)))) if k.kind == KeyEventKind::Press => keys.push(k),
            // Releases (Windows reports them) carry nothing.
            Ok(Some(Ok(Event::Key(_)))) => {}
            Ok(Some(Ok(other))) => {
                pending.push_back(other);
                break;
            }
            _ => break,
        }
    }
    keys
}

/// The text a burst spells, when it is long enough to be a paste and is
/// all plain characters (and the line breaks between dropped files).
fn paste_text(keys: &[crossterm::event::KeyEvent]) -> Option<String> {
    if keys.len() < PASTE_MIN_KEYS {
        return None;
    }
    let mut text = String::new();
    for k in keys {
        if k.modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && !files::is_altgr_char(k)
        {
            return None;
        }
        match k.code {
            KeyCode::Char(c) => text.push(c),
            KeyCode::Enter => text.push('\n'),
            KeyCode::Tab => text.push('\t'),
            _ => return None,
        }
    }
    Some(text)
}

/// `true` when the cell at `column`, `row` is inside `r`.
fn rect_contains(r: Rect, column: u16, row: u16) -> bool {
    r.width > 0
        && r.height > 0
        && column >= r.x
        && column < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

/// Opens the active space's Files tab, listing the space's folder the first
/// time (or the first allowed folder, when the server refuses that one).
async fn open_files(client: &mut ControlClient, model: &mut Model) {
    let Some(space) = model.active_space_id().map(str::to_string) else {
        return;
    };
    if !model.files.contains_key(&space) {
        let node = model.owner_of_space(&space).map(|n| n.as_str().to_string());
        model
            .files
            .insert(space.clone(), files::FilesView::new(node));
        let start = inherited_cwd(model, &space);
        let effect = start.map_or(files::Effect::Root, files::Effect::List);
        run_files_effect(Some(client), model, &space, effect).await;
        if model.files.get(&space).is_some_and(|v| v.listing.is_none()) {
            run_files_effect(Some(client), model, &space, files::Effect::Root).await;
        }
    }
    if model.files_shown.is_none() {
        model.files_shown = Some(files::FilesLayout::Full);
    }
    model.files_keys = true;
}

/// Performs what a key or click in a Files tab asked for.
async fn run_files_effect(
    client: Option<&mut ControlClient>,
    model: &mut Model,
    space: &str,
    effect: files::Effect,
) {
    let (method, params) = match &effect {
        files::Effect::None | files::Effect::Redraw => return,
        files::Effect::Download { path, size } | files::Effect::OpenLocal { path, size } => {
            let open = matches!(effect, files::Effect::OpenLocal { .. });
            start_transfer(
                model,
                space,
                transfer::JobKind::Download {
                    remote: path.clone(),
                    size: *size,
                    open,
                },
            );
            return;
        }
        files::Effect::DownloadFolder(path) => {
            start_transfer(
                model,
                space,
                transfer::JobKind::DownloadFolder {
                    remote: path.clone(),
                },
            );
            return;
        }
        files::Effect::UploadMany {
            locals,
            dir,
            overwrite,
        } => {
            start_transfer(
                model,
                space,
                transfer::JobKind::UploadMany {
                    locals: locals.iter().map(std::path::PathBuf::from).collect(),
                    remote_dir: dir.clone(),
                    overwrite: *overwrite,
                },
            );
            return;
        }
        files::Effect::Upload {
            local,
            dir,
            overwrite,
        } => {
            start_transfer(
                model,
                space,
                transfer::JobKind::Upload {
                    local: std::path::PathBuf::from(local),
                    remote_dir: dir.clone(),
                    overwrite: *overwrite,
                },
            );
            return;
        }
        files::Effect::Hide => {
            model.files_keys = false;
            if model.files_shown == Some(files::FilesLayout::Full) {
                model.files_shown = None;
            }
            return;
        }
        // Handled by `change` below, which returns before this pair is used.
        files::Effect::Mkdir(_) | files::Effect::Move { .. } | files::Effect::Delete(_) => {
            ("", serde_json::Value::Null)
        }
        files::Effect::Save {
            path,
            text,
            expect_mtime,
        } => {
            save_file(client, model, space, path, text, *expect_mtime).await;
            return;
        }
        files::Effect::Edit(path) => (
            "fs.read",
            serde_json::json!({ "path": path, "length": files::PREVIEW_BYTES }),
        ),
        files::Effect::Root => ("fs.list", serde_json::json!({})),
        files::Effect::List(path) => ("fs.list", serde_json::json!({ "path": path })),
        files::Effect::Read(path) => (
            "fs.read",
            serde_json::json!({ "path": path, "length": files::PREVIEW_BYTES }),
        ),
        files::Effect::Peek(path) => (
            "fs.read",
            serde_json::json!({ "path": path, "length": files::PEEK_BYTES }),
        ),
    };
    // Changes: one call, then the folder on screen again, with the entry
    // that changed selected.
    let change = match &effect {
        files::Effect::Mkdir(path) => Some((
            "fs.mkdir",
            serde_json::json!({ "path": path }),
            Some(path.clone()),
            "created",
        )),
        files::Effect::Move { from, to } => Some((
            "fs.move",
            serde_json::json!({ "from": from, "to": to }),
            Some(to.clone()),
            "moved to",
        )),
        files::Effect::Delete(path) => Some((
            "fs.delete",
            serde_json::json!({ "path": path }),
            None,
            "deleted",
        )),
        _ => None,
    };
    let Some(node) = model.files.get(space).map(|v| v.node.clone()) else {
        return;
    };
    if let Some((change_method, change_params, select, verb)) = change {
        let Some(client) = client else {
            if let Some(view) = model.files.get_mut(space) {
                view.status = Some("not connected".into());
            }
            return;
        };
        let reply = tokio::time::timeout(
            UI_OP_TIMEOUT,
            request_on(client, node.as_deref(), change_method, change_params),
        )
        .await
        .unwrap_or_else(|_| Err("timed out".into()));
        let Some(view) = model.files.get_mut(space) else {
            return;
        };
        let subject = match &effect {
            files::Effect::Delete(p) | files::Effect::Mkdir(p) => p.clone(),
            files::Effect::Move { to, .. } => to.clone(),
            _ => String::new(),
        };
        match reply {
            Ok(_) => {
                view.status = None;
                view.note = Some(format!("{verb} {subject}"));
                // A file shown in the viewer that just moved or went away.
                let gone = match &effect {
                    files::Effect::Delete(p) | files::Effect::Move { from: p, .. } => view
                        .preview
                        .as_ref()
                        .is_some_and(|pr| pr.path.starts_with(p.as_str())),
                    _ => false,
                };
                if gone {
                    view.preview = None;
                    view.side = files::Side::List;
                }
            }
            Err(e) => {
                view.status = Some(sanitize_notice(&e));
                return;
            }
        }
        let Some(dir) = view.listing.as_ref().map(|l| l.current.clone()) else {
            return;
        };
        if let Ok(Ok(v)) = tokio::time::timeout(
            UI_OP_TIMEOUT,
            request_on(
                client,
                node.as_deref(),
                "fs.list",
                serde_json::json!({ "path": dir }),
            ),
        )
        .await
        {
            if let (Some(listing), Some(view)) =
                (files::Listing::from_wire(&v), model.files.get_mut(space))
            {
                let previous = view.selected;
                view.set_listing(listing);
                // Stay where the change was, or on what it produced.
                view.selected = previous.min(view.visible().len().saturating_sub(1));
                if let Some(path) = select {
                    view.select_path(&path);
                }
            }
        }
        return;
    }
    let Some(client) = client else {
        if let Some(view) = model.files.get_mut(space) {
            view.status = Some("not connected".into());
        }
        return;
    };
    let reply = tokio::time::timeout(
        UI_OP_TIMEOUT,
        request_on(client, node.as_deref(), method, params),
    )
    .await
    .unwrap_or_else(|_| Err("timed out".into()));
    let Some(view) = model.files.get_mut(space) else {
        return;
    };
    match (reply, &effect) {
        (Ok(v), files::Effect::Edit(_)) => match files::decode_read(&v) {
            Some((path, size, mtime, bytes)) => view.start_editing(&path, size, mtime, &bytes),
            None => view.status = Some("the server sent an unreadable file".into()),
        },
        (Ok(v), files::Effect::Peek(path)) => match files::decode_read(&v) {
            // Only if the selection is still on it; the user may have moved on.
            Some((read, size, _, bytes))
                if view.selected_entry().is_some_and(|e| &e.path == path) =>
            {
                view.set_preview(&read, size, &bytes)
            }
            _ => {}
        },
        (Ok(v), files::Effect::Read(_)) => match files::decode_read(&v) {
            Some((path, size, _, bytes)) => view.set_preview(&path, size, &bytes),
            None => view.status = Some("the server sent an unreadable file".into()),
        },
        (Ok(v), _) => match files::Listing::from_wire(&v) {
            Some(listing) => {
                if let Some(roots) = v.get("roots").and_then(|r| r.as_array()) {
                    view.roots = roots
                        .iter()
                        .filter_map(|r| r.as_str().map(str::to_string))
                        .collect();
                }
                view.set_listing(listing)
            }
            None => view.status = Some("the server sent an unreadable listing".into()),
        },
        (Err(e), _) => view.status = Some(sanitize_notice(&e)),
    }
}

/// Writes the editor's text back to the server, in chunks the server
/// accepts. The first write carries `expect_mtime`, so a file changed by
/// someone else since it was opened is refused rather than overwritten.
async fn save_file(
    client: Option<&mut ControlClient>,
    model: &mut Model,
    space: &str,
    path: &str,
    text: &str,
    expect_mtime: Option<f64>,
) {
    let Some(node) = model.files.get(space).map(|v| v.node.clone()) else {
        return;
    };
    let Some(client) = client else {
        if let Some(view) = model.files.get_mut(space) {
            view.status = Some("not connected".into());
        }
        return;
    };
    let bytes = text.as_bytes();
    let result = write_whole_file(client, node.as_deref(), path, bytes, expect_mtime).await;
    let Some(view) = model.files.get_mut(space) else {
        return;
    };
    match result {
        Ok((size, mtime)) => view.saved(size, mtime),
        Err(e) if e.contains("changed since it was opened") => {
            if let Some(editor) = view.editor.as_mut() {
                editor.force = true;
            }
            view.status = Some(
                "the file changed on the server since you opened it \u{2014} ctrl+s again to overwrite it"
                    .into(),
            );
        }
        // Deleted or renamed meanwhile: saving again recreates it, rather
        // than every save failing until the edits are thrown away.
        Err(e) if e.contains("no such file") => {
            if let Some(editor) = view.editor.as_mut() {
                editor.force = true;
            }
            view.status = Some(
                "the file is gone from the server \u{2014} ctrl+s again to create it with your text".into(),
            );
        }
        Err(e) => view.status = Some(sanitize_notice(&e)),
    }
}

/// Writes `bytes` as the whole of `path`, returning its new size and mtime.
///
/// One chunk is one `fs.write`. More is written to a temporary name beside
/// it and moved into place only when complete, so a failure partway never
/// leaves the file cut short; the modification time is checked against
/// `expect_mtime` just before the move, as the one-chunk write checks it.
async fn write_whole_file(
    client: &mut ControlClient,
    node: Option<&str>,
    path: &str,
    bytes: &[u8],
    expect_mtime: Option<f64>,
) -> Result<(u64, f64), String> {
    use base64::Engine;
    let encode = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let stat = |v: &serde_json::Value| {
        (
            v.get("size")
                .and_then(|s| s.as_u64())
                .unwrap_or(bytes.len() as u64),
            v.get("mtime").and_then(|m| m.as_f64()).unwrap_or(0.0),
        )
    };
    let chunk = files::PREVIEW_BYTES as usize;
    if bytes.len() <= chunk {
        let mut params =
            serde_json::json!({ "path": path, "data": encode(bytes), "mode": "truncate" });
        if let Some(mtime) = expect_mtime {
            params["expectMtime"] = serde_json::json!(mtime);
        }
        return fs_call(client, node, "fs.write", params)
            .await
            .map(|v| stat(&v));
    }
    let temp = format!("{path}.spaces-save");
    let written = async {
        for (i, piece) in bytes.chunks(chunk).enumerate() {
            fs_call(
                client,
                node,
                "fs.write",
                serde_json::json!({
                    "path": temp,
                    "data": encode(piece),
                    "mode": if i == 0 { "truncate" } else { "append" },
                }),
            )
            .await?;
        }
        if let Some(mtime) = expect_mtime {
            let now = fs_call(
                client,
                node,
                "fs.read",
                serde_json::json!({ "path": path, "length": 1 }),
            )
            .await?;
            if (stat(&now).1 - mtime).abs() > 1.0 {
                return Err(format!("{path} changed since it was opened"));
            }
        }
        fs_call(
            client,
            node,
            "fs.move",
            serde_json::json!({ "from": temp, "to": path, "overwrite": true }),
        )
        .await?;
        let after = fs_call(
            client,
            node,
            "fs.read",
            serde_json::json!({ "path": path, "length": 1 }),
        )
        .await?;
        Ok(stat(&after))
    }
    .await;
    if written.is_err() {
        let _ = fs_call(
            client,
            node,
            "fs.delete",
            serde_json::json!({ "path": temp }),
        )
        .await;
    }
    written
}

/// One `fs.*` call, bounded like every UI request.
async fn fs_call(
    client: &mut ControlClient,
    node: Option<&str>,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    tokio::time::timeout(UI_OP_TIMEOUT, request_on(client, node, method, params))
        .await
        .unwrap_or_else(|_| Err("timed out".into()))
}

fn start_transfer(model: &mut Model, space: &str, kind: transfer::JobKind) {
    let Some(view) = model.files.get_mut(space) else {
        return;
    };
    let Some(ctx) = model.transfers.as_ref() else {
        view.status = Some("not connected".into());
        return;
    };
    view.status = None;
    view.note = Some("starting\u{2026}".into());
    transfer::spawn(
        ctx,
        transfer::Job {
            space: space.to_string(),
            node: view.node.clone(),
            kind,
        },
    );
}

/// Shows a transfer's progress or outcome in its space's Files tab.
async fn apply_transfer(
    msg: transfer::TransferMsg,
    client: Option<&mut ControlClient>,
    model: &mut Model,
) {
    match msg {
        transfer::TransferMsg::Progress {
            space,
            label,
            done,
            total,
        } => {
            if let Some(view) = model.files.get_mut(&space) {
                let pct = (done * 100).checked_div(total).unwrap_or(100);
                view.note = Some(format!(
                    "{label} \u{2014} {pct}% of {}",
                    files::human_size(total)
                ));
            }
        }
        transfer::TransferMsg::Finished {
            space,
            note,
            relist,
            open,
        } => {
            if let Some(view) = model.files.get_mut(&space) {
                view.note = Some(note);
                if let Some(path) = open {
                    if let Err(e) = transfer::open_locally(&path) {
                        view.status = Some(e);
                    }
                }
            }
            let showing = model
                .files
                .get(&space)
                .and_then(|v| v.listing.as_ref())
                .map(|l| l.current.clone());
            if let Some(dir) = relist.filter(|d| Some(d) == showing.as_ref()) {
                run_files_effect(client, model, &space, files::Effect::List(dir)).await;
            }
        }
        transfer::TransferMsg::Failed { space, note } => {
            if let Some(view) = model.files.get_mut(&space) {
                view.note = None;
                view.status = Some(note);
            }
        }
    }
}

/// Starts stopped panes again (`pane.restart`) and focuses the first that came
/// back. The server types each pane's agent command itself, so unlike
/// [`create_pane`] there is no `agent.start` to follow up with.
async fn restart_panes(client: &mut ControlClient, ctx: &mut PaneCtx<'_>, ids: &[String]) {
    let mut first: Option<String> = None;
    for id in ids {
        match client
            .request("pane.restart", serde_json::json!({ "paneId": id }))
            .await
        {
            Ok(_) => {
                // The `pane.opened` event says the same, but focusing below
                // must not wait for it: an attach to a pane the model still
                // thinks is stopped is refused locally.
                if let Some(pane) = ctx.model.local_mut().panes.iter_mut().find(|p| &p.id == id) {
                    pane.live = true;
                    if first.is_none() {
                        ctx.model.selected_space = pane.workspace_id.clone();
                    }
                }
                first.get_or_insert_with(|| id.clone());
            }
            Err(e) => ctx.model.set_notice(
                NoticeOrigin::Workspace,
                sanitize_notice(&format!("RESTART ERROR: {e}")),
            ),
        }
    }
    if let Some(id) = first {
        focus_pane(&id, ctx).await;
    }
}

/// Turns a bare pane creation into a session choice when there is one to make.
///
/// Returns the command to run, or `None` having opened the session picker
/// instead. Sits between the palette and `run_command` because that is the only
/// place with the palette, the connection and the resolved directory all in
/// scope at once — the directory is settled by the step before this one, and
/// sessions are scoped to a directory.
///
/// Asks about **every** agent rather than deciding which can resume. The
/// server answers empty for one with no resume flag, so `spawn-pane.js`'s
/// agent table stays the only place that knows, instead of a copy here that
/// drifts the first time an agent gains one.
async fn offer_sessions(
    cmd: Command,
    palette: &mut Palette,
    client: Option<&mut ControlClient>,
    model: &Model,
) -> Option<Command> {
    if let Command::BrowseFolder {
        workspace_id,
        agent_type,
        path,
    } = cmd
    {
        browse_folder(palette, client, model, workspace_id, agent_type, path).await;
        return None;
    }
    let Command::CreatePane {
        workspace_id,
        agent_type,
        cwd,
    } = &cmd
    else {
        return Some(cmd);
    };
    let Some(client) = client else {
        return Some(cmd);
    };

    // The directory this pane will actually start in, resolved the same way
    // `create_pane` resolves it — asking about a different one would list
    // sessions for somewhere the pane is not going.
    let resolved = cwd.clone().or_else(|| inherited_cwd(model, workspace_id));
    let Some(dir) = resolved.clone() else {
        // No directory to scope a list to. The runtime will use its own home
        // directory, and offering sessions for a path nobody named would be
        // guessing.
        return Some(cmd);
    };

    let sessions = client
        .request(
            "session.list",
            serde_json::json!({ "cwd": dir, "agentType": agent_type }),
        )
        .await
        .ok()
        .and_then(|v| v.get("sessions").cloned())
        .and_then(|v| serde_json::from_value::<Vec<spaces_proto::SessionSummary>>(v).ok())
        .unwrap_or_default();

    // Nothing to choose between, so do not put a one-entry menu in the way.
    // This is also the path for an agent that cannot resume at all, and for a
    // server too old to know the method — both come back empty.
    if sessions.is_empty() {
        return Some(cmd);
    }

    palette.open = true;
    palette.query.clear();
    palette.selected = 0;
    palette.picking_agent = None;
    palette.asking_cwd = None;
    palette.picking_session = Some(ui::palette::PendingPane {
        workspace_id: workspace_id.clone(),
        agent_type: agent_type.clone(),
        cwd: resolved,
        sessions,
    });
    None
}

/// Shows `path` (or the first dev directory) in the directory step, listed by
/// the machine that owns the space.
///
/// A path the server refuses — outside the dev directories, or gone — falls
/// back to the first dev directory. When nothing can be listed at all the
/// step stays a typed prompt seeded with `path`, which is how it worked
/// before the browser existed.
async fn browse_folder(
    palette: &mut Palette,
    client: Option<&mut ControlClient>,
    model: &Model,
    workspace_id: String,
    agent_type: String,
    path: Option<String>,
) {
    let target = model
        .owner_of_space(&workspace_id)
        .map(|n| n.as_str().to_string());
    let mut listing = None;
    if let Some(client) = client {
        listing = list_folders(client, target.as_deref(), path.as_deref()).await;
        if listing.is_none() && path.is_some() {
            listing = list_folders(client, target.as_deref(), None).await;
        }
    }
    input::open_cwd_prompt(palette, workspace_id, agent_type, path.unwrap_or_default());
    if let Some(listing) = listing {
        palette.query.clear();
        palette.browse = Some(listing);
    }
}

/// One `fs.folders` call, or `None` when it fails for any reason.
async fn list_folders(
    client: &mut ControlClient,
    node: Option<&str>,
    path: Option<&str>,
) -> Option<ui::palette::FolderListing> {
    let mut params = serde_json::json!({});
    if let Some(path) = path {
        params["path"] = serde_json::Value::String(path.to_string());
    }
    let result = tokio::time::timeout(
        UI_OP_TIMEOUT,
        request_on(client, node, "fs.folders", params),
    )
    .await
    .ok()?
    .ok()?;
    let text = |v: &serde_json::Value| v.as_str().map(str::to_string);
    Some(ui::palette::FolderListing {
        current: text(result.get("current")?)?,
        parent: result.get("parent").and_then(text),
        folders: result
            .get("folders")?
            .as_array()?
            .iter()
            .filter_map(|f| Some((text(f.get("name")?)?, text(f.get("path")?)?)))
            .collect(),
        roots: result
            .get("roots")
            .and_then(|r| r.as_array())
            .map(|r| r.iter().filter_map(text).collect())
            .unwrap_or_default(),
    })
}

/// Re-reads the fleet and each reachable machine's contents.
///
/// Run after anything that changes who is in the fleet, so the sidebar catches
/// up without a reconnect.
async fn refresh_fleet(client: &mut ControlClient, model: &mut Model) {
    let peers = fleet_peers(client).await;
    model.apply_peers(&peers);
    load_peer_snapshots(client, model).await;
}

/// Fills in each reachable peer's spaces and panes.
///
/// Sequential rather than concurrent: this runs on one control channel, which
/// serialises requests anyway, and a fleet is a handful of machines. A peer
/// that is offline or refuses simply keeps an empty slot — it still appears in
/// the sidebar with its status, which is more useful than omitting it.
async fn load_peer_snapshots(client: &mut ControlClient, model: &mut Model) {
    let local = model.local_id_owned();
    let targets: Vec<NodeId> = model
        .nodes
        .iter()
        // Every machine there is a key for, **including offline ones**. An
        // offline machine is exactly what someone is refreshing to retry, and
        // filtering on reachability here meant it could never come back: the
        // status only leaves `offline` when a dial succeeds, and no dial was
        // ever attempted. Unpaired is the one real exclusion — there is no key
        // to connect with, so asking can only fail.
        .filter(|(id, state)| **id != local && state.conn.is_dialable())
        .map(|(id, _)| id.clone())
        .collect();

    for node in targets {
        let reply = client
            .request(
                "net.snapshot",
                serde_json::json!({ "nodeId": node.as_str() }),
            )
            .await;
        match reply
            .ok()
            .and_then(|v| serde_json::from_value::<Snapshot>(v).ok())
        {
            Some(snapshot) => model.apply_peer_snapshot(&node, snapshot),
            None => {
                // Reachable a moment ago per `net.peers`, but not answering
                // now. Say so rather than leaving it looking merely empty.
                if let Some(state) = model.nodes.get_mut(&node) {
                    state.conn = ConnState::Offline("no snapshot".to_string());
                }
            }
        }
    }
}

async fn fleet_peers(client: &mut ControlClient) -> Vec<spaces_proto::NetNode> {
    client
        .request("net.peers", serde_json::json!({}))
        .await
        .ok()
        .and_then(|value| value.get("nodes").cloned())
        .and_then(|nodes| serde_json::from_value::<Vec<spaces_proto::NetNode>>(nodes).ok())
        .unwrap_or_default()
}

/// Folds a freshly-fetched snapshot and its subscribe backlog into `model`.
fn apply_prime(model: &mut Model, snapshot: Snapshot, ack: &SubscribeAck) {
    model.apply_snapshot(snapshot);
    for event in &ack.events {
        model.apply_event(event);
    }
    mark_connected(model);
}

/// Records that the control channel to the node being viewed is up.
///
/// `Direct` rather than `Proxied` because this client dialled that node's own
/// `/proto`. The local node is reached exactly the way a directly-reachable
/// peer will be, which is the point of not giving it a special case.
///
/// Clears `stale` as well: a node whose snapshot just landed is by definition
/// caught up. The two were previously set independently at four call sites,
/// which is precisely how they drift apart.
fn mark_connected(model: &mut Model) {
    let node = model.local_mut();
    node.conn = ConnState::Direct;
    node.stale = false;
}

/// Records that the control channel is down, with the reason for the status
/// bar.
///
/// Marks the node stale too: a model that stopped receiving events can no
/// longer be trusted to reflect live state, which is the same condition a
/// sequence gap produces.
fn mark_disconnected(model: &mut Model, reason: &str) {
    let node = model.local_mut();
    node.conn = ConnState::Offline(reason.to_string());
    node.stale = true;
}

/// One reconnect attempt: wait `backoff` (the very first attempt after a
/// disconnect uses `Duration::ZERO` — no wait), then connect and prime.
/// Run as a spawned task (see `run`'s `reconnect_task`), never awaited
/// directly inside a `tokio::select!` branch expression — a branch
/// expression is re-polled from scratch on every loop iteration where some
/// *other* branch wins the race, which would cancel and restart a
/// multi-second connect attempt every time a redraw tick or resize event
/// fires in the meantime.
async fn reconnect_attempt(
    url: String,
    token: Option<String>,
    backoff: Duration,
) -> ConnectOutcome {
    if !backoff.is_zero() {
        tokio::time::sleep(backoff).await;
    }
    let mut client = ControlClient::connect(&url, token.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let (snapshot, ack) = snapshot_and_subscribe(&mut client).await?;
    Ok((client, snapshot, ack))
}

/// Awaits `handle` if one is present, otherwise never resolves — the standard
/// "optional `select!` source" pattern used throughout this loop for
/// `recv_control_event`/`PaneSlot::recv`. Unlike those two, this *borrows*
/// `handle` rather than `.take()`-ing it: `tokio::select!` reconstructs every
/// branch expression from scratch on each loop iteration and drops whichever
/// ones lost the race. Taking the `JoinHandle` out of `handle` on first poll
/// would move it into *this* future, so losing a single race would drop it —
/// and with it, this caller's only way to ever observe the spawned task's
/// result, even though the task itself keeps running unobserved. `JoinHandle`
/// is `Unpin`, so polling it through `&mut` across many separate calls (one per
/// losing iteration, until it finally wins one) is both cancel-safe and
/// correct. Taking it instead was a real bug: the 33ms redraw tick is a
/// competing branch on *every* iteration, so any attempt with a non-zero
/// backoff lost this race deterministically, and reconnection never completed.
///
/// A `JoinHandle` failure (the spawned task panicked or was cancelled) is
/// folded into the same `Result<_, String>` the task's own body uses, so
/// callers only ever handle one error shape. `handle` is cleared only once
/// this actually resolves — the caller is expected to set a new `Some(...)`
/// itself for the next attempt, exactly as it set this one.
async fn recv_join<T>(handle: &mut Option<JoinHandle<Result<T, String>>>) -> Result<T, String> {
    match handle {
        Some(h) => {
            let result = h
                .await
                .unwrap_or_else(|e| Err(format!("background task failed: {e}")));
            *handle = None;
            result
        }
        None => std::future::pending().await,
    }
}

/// The backoff to use for the *next* reconnect attempt after one using
/// `current` just failed — doubling, capped at [`RECONNECT_CAP`]. Pure and
/// synchronous specifically so
/// `reconnect_backoff_sequence_matches_the_pinned_250ms_to_8s_contract`
/// (in tests) can pin the exact sequence the binding constraint requires
/// without spinning up a real reconnect loop, a real timer, or a real
/// socket.
fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RECONNECT_CAP)
}

/// Returns the backoff to use for the reconnect attempt about to be
/// spawned, then advances `*backoff` in place (via [`next_backoff`]) to
/// what the attempt *after that* (if this one also fails) should use.
///
/// Pulled out of `run`'s reconnect-failure arm specifically so
/// `take_and_advance_returns_the_pre_advance_value_and_still_advances` (in
/// tests) can pin the capture-*before*-mutate ordering directly. An earlier
/// version of that arm advanced `reconnect_backoff` *before* using it to spawn
/// the next attempt — `reconnect_backoff = next_backoff(reconnect_backoff)`
/// computed first, then the now-already-doubled value used for the spawn —
/// which skipped the mandated 250ms rung entirely (the 250ms seed got doubled
/// to 500ms before its first real use). `next_backoff`'s own test does not
/// cover this: it only exercises the pure doubling function in a loop, never
/// `run`'s `select!` body, so reintroducing the call-site ordering bug left
/// every test green (verified by actually doing this and reverting). This
/// function is what the call site now delegates to instead of inlining the
/// capture/advance itself, so an ordering regression — not just a
/// doubling-formula regression — has something concrete to fail against; see
/// the test's own verification note for confirmation this one actually catches
/// it.
fn take_and_advance(backoff: &mut Duration) -> Duration {
    let current = *backoff;
    *backoff = next_backoff(*backoff);
    current
}

/// Makes a server-supplied string safe to draw on the status bar: strips
/// every control character and truncates to [`NOTICE_MAX_CHARS`].
///
/// The status bar is a real terminal. A `message` field carrying `\x1b[`
/// would otherwise be written into ratatui cells verbatim and emitted as
/// an escape sequence — a server (or anything that can impersonate one) is
/// not permitted to move the user's cursor, recolor their screen, or worse.
/// A newline is just as unwelcome: the status area is one row tall and the
/// rest would be silently clipped mid-word.
fn sanitize_notice(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(NOTICE_MAX_CHARS)
        .collect()
}

// ─── Deciding what to do with a keystroke's resolved Action ──────────────

/// What `run`'s event loop should actually do with a keystroke's resolved
/// [`Action`] (see [`decide`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Disposition {
    /// Exit the event loop.
    Quit,
    /// Send these bytes to the currently-attached pane socket.
    SendPaneInput(Vec<u8>),
    /// Run this command against the control channel.
    RunCommand(Command),
    /// A key for the Files tab. Not refused while stale: moving around a
    /// listing already loaded needs no connection, and anything that does
    /// reports the missing connection itself.
    FilesKey(crossterm::event::KeyEvent),
    /// Nothing to do — a chrome-only change `route` already applied
    /// in-place (the palette opening/closing, a query edited, ...), an
    /// `Action::None`, or an `Action` refused because the model is stale.
    Nothing,
}

/// Applies the commands `run` handles itself — the ones that need no
/// connection — and hands back whatever `action` should be treated as
/// afterwards. Called by **both** the key arm and the mouse arm, before
/// [`decide`], so a command reachable from the keyboard and from a palette
/// click behaves identically either way.
///
/// - `Detach` becomes [`Action::Quit`]: it works whether or not a connection
///   exists, and `decide` only knows "stale or not", not *why* a command
///   might bypass that check.
/// - `ToggleMouse` flips [`Model::mouse_captured`] and tells the terminal, then
///   becomes [`Action::Redraw`]. Refusing it while `stale` would be exactly
///   backwards: a disconnected client is when a user most wants to select the
///   error text and copy it out.
///
/// - `ToggleAgentSort` flips the agents panel's order (see
///   [`Model::toggle_agent_sort`]) and becomes [`Action::Redraw`] — the
///   keyboard's way to what clicking the panel's header does, and just as
///   local.
///
/// - `ToggleSidebar` hides or shows the sidebar (see
///   [`crate::model::SidebarState::toggle_collapsed`]) and becomes
///   [`Action::Redraw`]; the arm that called this resizes the visible panes
///   to the pane area that results.
///
/// Everything else passes through untouched.
fn intercept_local(action: Action, model: &mut Model) -> Action {
    match action {
        Action::Command(Command::Detach) => Action::Quit,
        Action::Command(Command::ToggleMouse) => {
            model.mouse_captured = !model.mouse_captured;
            set_mouse_capture(model.mouse_captured);
            Action::Redraw
        }
        Action::Command(Command::ToggleAgentSort) => {
            model.toggle_agent_sort();
            Action::Redraw
        }
        Action::Command(Command::ToggleSidebar) => {
            model.sidebar.toggle_collapsed();
            Action::Redraw
        }
        Action::Command(Command::ShowFiles) => {
            if model.files_shown.is_none() {
                model.files_shown = Some(files::FilesLayout::Full);
            }
            model.files_keys = true;
            Action::Redraw
        }
        Action::Command(Command::CloseFiles(space)) => {
            // Unsaved edits are never thrown away by closing the tab.
            if let Some(view) = model
                .files
                .get_mut(&space)
                .filter(|v| v.editor.as_ref().is_some_and(|e| e.dirty))
            {
                view.status = Some(
                    "unsaved changes \u{2014} save (ctrl+s) or discard (esc twice) first".into(),
                );
                model.files_shown = Some(model.files_shown.unwrap_or(files::FilesLayout::Full));
                model.files_keys = true;
                return Action::Redraw;
            }
            model.files.remove(&space);
            if model.active_space_id() == Some(space.as_str()) {
                model.files_shown = None;
                model.files_keys = false;
            }
            Action::Redraw
        }
        // While Files is showing, split means Files beside the terminal
        // rather than a second terminal.
        Action::Command(Command::ToggleSplit) if model.shown_files().is_some() => {
            model.files_shown = match model.files_shown {
                Some(files::FilesLayout::Full) if model.focused.is_some() => {
                    Some(files::FilesLayout::Split)
                }
                Some(files::FilesLayout::Split) => Some(files::FilesLayout::Full),
                other => other,
            };
            Action::Redraw
        }
        other => other,
    }
}

/// Reads the text under `palette.selection` out of its pane's grid, asks the
/// terminal to put it on the system clipboard, and says so on the status bar.
///
/// The whole reason this lives in `main.rs` rather than in the router: it
/// needs `terms`, the pane grids, which nothing in `ui` or `input` holds.
///
/// Silent about everything except success. A selection naming a pane with no
/// grid (closed between the drag and the release) simply copies nothing —
/// there is no failure here worth a line on a status bar that has three other
/// writers, and the highlight the user is looking at is its own explanation.
/// OSC 52 has no reply either, so "the terminal accepted it" is not knowable;
/// the notice reports what was *sent*, which is why it counts characters
/// rather than claiming the clipboard changed.
fn copy_selection(palette: &Palette, terms: &HashMap<String, PaneTerm>, model: &mut Model) {
    let Some(sel) = palette.selection.as_ref() else {
        return;
    };
    let Some(term) = terms.get(&sel.pane_id) else {
        return;
    };
    let text = sel.text(&term.visible_lines());
    if text.is_empty() {
        return;
    }
    let escape = ui::selection::osc52(&text);
    // Written straight to stdout rather than through the ratatui backend: it
    // is a request to the terminal emulator, not a cell to draw, and the next
    // frame must not overwrite it. Best-effort, like every other terminal
    // escape here — a write that fails leaves the clipboard alone.
    let _ = io::stdout().write_all(escape.as_bytes());
    let _ = io::stdout().flush();
    model.set_notice(
        NoticeOrigin::Clipboard,
        format!("COPIED {} chars", text.chars().count()),
    );
}

/// Turns the terminal's mouse reporting on or off.
///
/// Best-effort: a terminal that refuses the escape sequence leaves the model
/// flag saying one thing and the terminal doing another, which is a wrong
/// status-bar indicator — not something worth tearing the session down for,
/// and not something this can detect anyway (the sequence has no reply).
/// `restore_terminal` disables capture unconditionally on the way out, so a
/// failure here cannot leave a terminal stuck reporting mouse events after
/// exit.
fn set_mouse_capture(on: bool) {
    if on {
        let _ = execute!(io::stdout(), EnableMouseCapture);
    } else {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
}

/// Decides what `run`'s event loop should do with `action`, given whether
/// the model is currently `stale`. Pure and synchronous — no network or
/// terminal access — specifically so this binding constraint ("input is
/// refused while disconnected rather than silently dropped") can be pinned
/// by tests without a real connection, a real pane socket, or a real
/// terminal. A reconnect that never completes looks, from here, identical
/// to one that's just slow, so this cannot catch a stuck reconnect loop,
/// but every `stale`-gating decision this function makes is pinned instead
/// of living only inside a 175-line `select!`.
///
/// `Action::PaneInput` and any `Action::Command` are refused (mapped to
/// `Disposition::Nothing`) while `stale` — there is no live connection for
/// either to reach. `Action::Quit` always quits regardless of `stale`:
/// `route` itself never produces it (see `input.rs`'s module doc comment),
/// but `run` translates `Action::Command(Command::Detach)` to it *before*
/// calling this function, precisely because detaching needs no connection
/// and must behave identically whether or not one exists — a decision
/// `decide` doesn't need to know the reason for, only that it always wins.
fn decide(action: Action, stale: bool) -> Disposition {
    match action {
        Action::Quit => Disposition::Quit,
        Action::PaneInput(bytes) => {
            if stale {
                Disposition::Nothing
            } else {
                Disposition::SendPaneInput(bytes)
            }
        }
        Action::Command(cmd) => {
            if stale {
                Disposition::Nothing
            } else {
                Disposition::RunCommand(cmd)
            }
        }
        Action::FilesKey(key) => Disposition::FilesKey(key),
        // Handled by the mouse arm before `decide` is reached — it needs the
        // pane grids, which this function has no view of. Named rather than
        // folded into the catch-all so adding a variant stays a compile error.
        Action::CopySelection | Action::ScrollPane { .. } => Disposition::Nothing,
        Action::Redraw | Action::None => Disposition::Nothing,
    }
}

/// `true` iff `kind` is one the mouse arm below might actually do something
/// with: a left click (sidebar hit-test / palette dismiss), or a vertical wheel
/// scroll. Checked *before* that arm's `terminal.size()` syscall and
/// `sidebar_rows` allocation (a `Vec` with a `String` clone per workspace and
/// pane) — mouse capture also delivers every `Moved`/`Drag` sample, so on a
/// fast pointer sweep skipping that work for the kinds that can never use it is
/// the difference between a handful of allocations a second and hundreds.
/// `ScrollLeft`/`ScrollRight` are deliberately excluded — the sidebar has no
/// horizontal scroll concept — same as every button but the left one and every
/// kind but `Down`. The bytes for the focused pane's program when `event` is
/// its to handle, or `None` to leave the event to the client — see
/// [`mouse_forward`].
///
/// Only the focused pane, never the split one: a click on the other half
/// still focuses it, as it always has. Anything drawn over the panes — the
/// palette and its pickers, a menu, a text selection in progress, a toast —
/// keeps its clicks, since the user is clicking what they can see.
fn forwarded_mouse(
    event: &MouseEvent,
    screen: Rect,
    palette: &Palette,
    model: &Model,
    terms: &HashMap<String, PaneTerm>,
) -> Option<Vec<u8>> {
    let pane_area = ui::pane_area(screen, model.sidebar.columns(screen.width));
    if matches!(model.shown_files(), Some((_, files::FilesLayout::Full)))
        || ui::files_region(pane_area, model)
            .is_some_and(|r| rect_contains(r, event.column, event.row))
    {
        return None;
    }
    if palette.open
        || palette.naming_space
        || palette.picking_session.is_some()
        || palette.picking_agent.is_some()
        || palette.tab_menu.is_some()
        || palette.selection.is_some()
        || palette.asking_cwd.is_some()
    {
        return None;
    }
    let mode = terms.get(model.focused.as_deref()?)?.mouse_reporting()?;
    let area = ui::pane_area(screen, model.sidebar.columns(screen.width));
    let toasts = ui::toast::toast_layout(area, model);
    if ui::toast::toast_at(&toasts, event.column, event.row).is_some() {
        return None;
    }
    let (focused_region, _) = ui::visible_pane_regions(area, model);
    mouse_forward::encode(event, focused_region, mode)
}

fn is_actionable_mouse_event(kind: MouseEventKind) -> bool {
    matches!(
        kind,
        MouseEventKind::Down(MouseButton::Left)
            // Right-down opens the tab context menu (see `ui::menu`), and
            // closes it again from anywhere. Admitted here as of that
            // feature; every other button and every other kind still
            // short-circuits before the syscall below.
            | MouseEventKind::Down(MouseButton::Right)
            // Drag and release drive text selection (see `ui::selection`).
            // Drag is the one high-frequency kind admitted here; `route_mouse`
            // gates it immediately on a selection already being armed, so a
            // drag that started nowhere costs one match and returns.
            //
            // Omitting these is what made in-app selection do nothing at all
            // while its own router tests passed: they call `route_mouse`
            // directly, so this gate — the thing that decides whether the
            // router is reached — was invisible to every one of them. That is
            // what `the_kinds_admitted_here_are_exactly_the_kinds_the_router_acts_on`
            // below now pins.
            | MouseEventKind::Drag(MouseButton::Left)
            | MouseEventKind::Up(MouseButton::Left)
            | MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
    )
}

// ─── The event loop ──────────────────────────────────────────────────────

/// Connects, then drives the terminal UI until the user detaches or a
/// fatal error occurs. See the module doc comment for the high-level
/// contract; the behaviors specific to this function:
///
/// - The *initial* connection failing (including a `hello` version
///   mismatch) is fatal — this returns `Err` with the failure's own message
///   (every [`spaces_proto::client::ProtoError`] variant already renders an
///   actionable one, `VersionMismatch` included) rather than retrying
///   forever against a server that may never speak this client's protocol.
/// - Once connected, losing the transport is *not* fatal: `model.local().stale` is
///   set, `RECONNECTING` starts rendering (the status bar already does
///   this from `model.local().stale`), and reconnecting-with-backoff runs as a
///   background task (see [`reconnect_attempt`]) so the loop keeps
///   redrawing and responding to resize/detach while it waits.
/// - While `model.local().stale`, keystrokes are still routed (so palette chrome —
///   open/close, query editing, navigation — stays responsive, which is
///   itself feedback that the client noticed the keystroke) but `focused`
///   is forced to `None` for routing purposes, so `route` never produces
///   `Action::PaneInput` — there is no live pane socket to deliver it to
///   anyway — and of any resulting `Action::Command`, only
///   `Command::Detach` is acted on — everything else is discarded. That is
///   "refused", not "silently dropped": the keystroke was received and did
///   something (or nothing, visibly, like an unmatched palette query), it
///   just never touched a connection that isn't there.
/// - On [`ApplyOutcome::NeedsResnapshot`], `session.snapshot` is re-fetched
///   and re-subscribed on the *same* still-live connection — no reconnect
///   needed for a sequence gap alone.
async fn run(args: Args) -> Result<(), String> {
    let control_url = format!("{}/proto", args.url);

    // Connect (and prime the model) *before* entering the terminal guard —
    // see the doc comment above about the initial connection being fatal.
    // A version mismatch or an unreachable server should fail fast with a
    // plain error message on the caller's ordinary terminal, not flash the
    // alternate screen for the duration of a failed handshake first.
    let mut client = ControlClient::connect(&control_url, args.token.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    // `Model::new`, not `Model::default`: `TerminalGuard::enter` below turns
    // mouse reporting on, and `mouse_captured` has to agree with it from the
    // first frame or the status bar advertises a mode the terminal is not in.
    let mut model = Model::new();
    model.local_mut().home_dir = server_home_dir(&mut client).await;
    // Learn the fleet before the first frame, so the sidebar's very first
    // render already knows how many machines there are — the node tag in flat
    // view depends on that count, and a list that gains tags a moment after
    // appearing reads as a glitch.
    let peers = fleet_peers(&mut client).await;
    model.apply_peers(&peers);
    load_peer_snapshots(&mut client, &mut model).await;
    let (snapshot, ack) = snapshot_and_subscribe(&mut client).await?;
    apply_prime(&mut model, snapshot, &ack);

    let guard =
        TerminalGuard::enter().map_err(|e| format!("failed to prepare the terminal: {e}"))?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal =
        Terminal::new(backend).map_err(|e| format!("failed to start the terminal UI: {e}"))?;

    // The sidebar's automatic collapse needs the terminal's width before the
    // first frame, or a narrow terminal flashes a sidebar it then hides.
    if let Ok(size) = terminal.size() {
        model.sidebar.note_screen_width(size.width);
    }

    let mut palette = Palette::default();
    let mut terms: HashMap<String, PaneTerm> = HashMap::new();
    let mut window_title = String::new();
    let mut conn = PaneConn::default();
    // Transfers dial their own connection and report progress here.
    let (transfer_tx, mut transfer_rx) = mpsc::channel::<transfer::TransferMsg>(64);
    model.transfers = Some(transfer::Ctx {
        url: control_url.clone(),
        token: args.token.clone(),
        tx: transfer_tx,
    });

    let mut event_rx: Option<mpsc::Receiver<EventFrameInner>> = Some(client.events());
    let mut client: Option<ControlClient> = Some(client);

    let mut reconnect_task: Option<JoinHandle<ConnectOutcome>> = None;
    let mut reconnect_backoff = RECONNECT_INITIAL_BACKOFF;
    // Deliberately *not* a field on `conn` — see `Reattachers`' own doc
    // comment for the borrow this loop's `select!` would otherwise fail.
    let mut reattach = Reattachers::default();

    let mut input_events = EventStream::new();
    // Input already read but not yet handled — keys read ahead to tell a
    // drop or a paste from typing, and given back when it was typing.
    let mut pending_input: std::collections::VecDeque<Event> = std::collections::VecDeque::new();
    // How many of `pending_input`'s keys were already looked at that way.
    let mut replayed_keys = 0usize;
    let mut redraw_tick = tokio::time::interval(REDRAW_INTERVAL);
    redraw_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut dirty = true;

    // Open on an agent, not an empty space: a server that already has live
    // panes gets the first of them focused, in its own space.
    if model.focused.is_none() {
        if let Some((id, space)) = model.opening_pane() {
            model.selected_space = Some(space);
            let pane_area = current_pane_area(&terminal, &model);
            let mut ctx = PaneCtx {
                base_url: &args.url,
                token: args.token.as_deref(),
                model: &mut model,
                conn: &mut conn,
                reattach: &mut reattach,
                terms: &mut terms,
                pane_area,
            };
            focus_pane(&id, &mut ctx).await;
        }
    }

    loop {
        tokio::select! {
            // ---- Terminal input ----
            maybe_event = next_input(&mut pending_input, &mut input_events) => {
                // A drop or paste onto the Files list arrives as a burst of
                // keys no one could type; it uploads, never runs commands.
                // `None` here: the burst was handled. (The inner `None` is
                // still the end of input, as before.)
                let unhandled = match maybe_event {
                    Some(Ok(Event::Key(key))) if replayed_keys > 0 => {
                        replayed_keys -= 1;
                        Some(Some(Ok(Event::Key(key))))
                    }
                    Some(Ok(Event::Key(key)))
                        if key.kind == KeyEventKind::Press && files_take_commands(&model, &palette) =>
                    {
                        let burst = read_burst(key, &mut input_events, &mut pending_input).await;
                        match paste_text(&burst) {
                            Some(text) => {
                                dirty = true;
                                if let Some(space) = model.shown_files().map(|(s, _)| s.to_string()) {
                                    let effect = model
                                        .files
                                        .get_mut(&space)
                                        .map(|v| v.handle_paste(&text, |p| std::path::Path::new(p).exists()))
                                        .unwrap_or(files::Effect::None);
                                    run_files_effect(client.as_mut(), &mut model, &space, effect).await;
                                }
                                None
                            }
                            None => {
                                // Typing after all: handle this key now and
                                // the rest in order, without looking again.
                                for (i, k) in burst.into_iter().skip(1).enumerate() {
                                    pending_input.insert(i, Event::Key(k));
                                    replayed_keys += 1;
                                }
                                Some(Some(Ok(Event::Key(key))))
                            }
                        }
                    }
                    other => Some(other),
                };
                let Some(maybe_event) = unhandled else {
                    continue;
                };
                match maybe_event {
                    Some(Ok(Event::Key(key))) => {
                        dirty = true;
                        // While disconnected, route() is still called (so
                        // chrome stays responsive — see the doc comment
                        // above) but with `focused` forced to `None`, so it
                        // can never produce `Action::PaneInput` — there is
                        // no socket to deliver it to anyway.
                        let focused = if model.local().stale { None } else { model.focused.clone() };
                        let panes_before = current_pane_area(&terminal, &model);
                        let regions_before = ui::visible_pane_regions(panes_before, &model);
                        let action = route(key, &mut palette, focused.as_deref(), &model);
                        // route() itself never produces `Action::Quit` — see
                        // its module doc comment — precisely so the
                        // connection-free commands are resolved in one shared
                        // place, before `decide` (which only knows about
                        // "stale or not", not about *why* a command might
                        // bypass that check). See `intercept_local`.
                        let action = intercept_local(action, &mut model);
                        if matches!(action, Action::Redraw) {
                            dirty = true;
                        }
                        // A local command can move the pane area without the
                        // terminal resizing; the panes are told exactly as a
                        // resize would tell them.
                        let panes_after = current_pane_area(&terminal, &model);
                        if panes_after != panes_before
                            || ui::visible_pane_regions(panes_after, &model) != regions_before
                        {
                            let mut ctx = PaneCtx {
                                base_url: &args.url,
                                token: args.token.as_deref(),
                                model: &mut model,
                                conn: &mut conn,
                                reattach: &mut reattach,
                                terms: &mut terms,
                                pane_area: panes_after,
                            };
                            resize_visible(&mut ctx).await;
                        }
                        match decide(action, model.local().stale) {
                            Disposition::Quit => break,
                            Disposition::SendPaneInput(bytes) => {
                                // Typing ends history-reading. A keystroke
                                // whose echo lands somewhere off-screen is the
                                // worst of both modes, so the view snaps back
                                // to the live edge before the bytes go out.
                                if let Some(term) = model
                                    .focused
                                    .as_deref()
                                    .and_then(|id| terms.get_mut(id))
                                {
                                    term.scroll_to_bottom();
                                }
                                // The *focused* slot only, never the split
                                // one: `route` resolved these bytes against
                                // `model.focused`, and the whole point of
                                // the second pane is that it is visible
                                // without being typed at.
                                //
                                // Bounded for the same reason the attach is
                                // (see `PANE_ATTACH_TIMEOUT`): a WebSocket
                                // sink blocked on a full send buffer would
                                // otherwise hang this whole loop — no
                                // redraw, no input, no detach — until the
                                // peer drains it. Losing a keystroke beats
                                // losing the terminal.
                                let _ = tokio::time::timeout(
                                    PANE_ATTACH_TIMEOUT,
                                    conn.focused.send_input(bytes),
                                )
                                .await;
                            }
                            Disposition::RunCommand(cmd) => {
                                let pane_area = current_pane_area(&terminal, &model);
                                let mut ctx = PaneCtx {
                                    base_url: &args.url,
                                    token: args.token.as_deref(),
                                    model: &mut model,
                                    conn: &mut conn,
                                    reattach: &mut reattach,
                                    terms: &mut terms,
                                    pane_area,
                                };
                                // May open the session picker instead of
                                // creating the pane — see `offer_sessions`.
                                if let Some(cmd) =
                                    offer_sessions(cmd, &mut palette, client.as_mut(), ctx.model).await
                                {
                                    run_command(cmd, client.as_mut(), &mut ctx).await;
                                }
                            }
                            Disposition::FilesKey(key) => {
                                let area = current_pane_area(&terminal, &model);
                                let rows = ui::files_region(area, &model)
                                    .map(|r| files::list_rows(r.height))
                                    .unwrap_or(0);
                                if let Some(space) = model.shown_files().map(|(s, _)| s.to_string()) {
                                    let effect = model
                                        .files
                                        .get_mut(&space)
                                        .map(|v| v.handle_key(key, rows))
                                        .unwrap_or(files::Effect::None);
                                    run_files_effect(client.as_mut(), &mut model, &space, effect).await;
                                    let after = current_pane_area(&terminal, &model);
                                    if ui::visible_pane_regions(after, &model) != regions_before {
                                        let mut ctx = PaneCtx {
                                            base_url: &args.url,
                                            token: args.token.as_deref(),
                                            model: &mut model,
                                            conn: &mut conn,
                                            reattach: &mut reattach,
                                            terms: &mut terms,
                                            pane_area: after,
                                        };
                                        resize_visible(&mut ctx).await;
                                    }
                                }
                            }
                            Disposition::Nothing => {}
                        }
                    }
                    Some(Ok(Event::Mouse(mouse_event))) => {
                        // Gate before any of the syscall/allocation work
                        // below — see `is_actionable_mouse_event`'s own doc
                        // comment. Everything else (mouse movement, a
                        // right-click, a horizontal scroll, ...) does
                        // nothing at all: no redraw, `dirty` untouched.
                        let screen_now = match terminal.size() {
                            Ok(size) => Rect::new(0, 0, size.width, size.height),
                            Err(_) => Rect::new(0, 0, 0, 0),
                        };
                        if let Some(bytes) =
                            forwarded_mouse(&mouse_event, screen_now, &palette, &model, &terms)
                        {
                            // The focused pane's program asked for the mouse,
                            // so this event is its, exactly as a keystroke
                            // would be — same slot, same bounded send. Refused
                            // while stale, as keystrokes are (see `decide`).
                            if !model.local().stale {
                                let _ = tokio::time::timeout(
                                    PANE_ATTACH_TIMEOUT,
                                    conn.focused.send_input(bytes),
                                )
                                .await;
                            }
                        } else if is_actionable_mouse_event(mouse_event.kind) {
                            // Every rect the router hit-tests against is
                            // derived from the terminal's *current* size, not
                            // cached from the last render, so a resize
                            // landing between render and click can never
                            // leave this testing against a stale rect. Falls
                            // back to a zero-sized screen (which matches
                            // nothing) if the size can't be read right now —
                            // same degrade-gracefully convention as
                            // `pane_size`.
                            let screen = match terminal.size() {
                                Ok(size) => Rect::new(0, 0, size.width, size.height),
                                Err(_) => Rect::new(0, 0, 0, 0),
                            };
                            let columns = model.sidebar.columns(screen.width);
                            // What the pane area is *before* this event, so a
                            // sidebar drag that moved it can be told apart
                            // from every event that did not — see below.
                            let panes_before = ui::pane_area(screen, columns);
                            let tabs = ui::tabs::tab_bar(ui::tab_bar_area(screen, columns), &model);
                            // The clickable pane regions, from the same
                            // `visible_pane_regions` the renderer and
                            // `slot_sizes` call.
                            //
                            // Only the *split* pane is ever a target: it is
                            // the one visible pane that is not already
                            // focused, and clicking the pane you are already
                            // typing into must not re-run the focus path
                            // (`focus_pane` re-attaches, resizes, and clears
                            // `model.notice`, so a stray click in the pane
                            // area would silently wipe the status-bar line
                            // explaining why something just failed).
                            // *Every* visible pane, not just the unfocused
                            // one: a drag has to be able to select text in
                            // the pane you are already typing into, which is
                            // usually the one you want to copy from.
                            // `pane_region_click` is what declines to re-focus
                            // the already-focused pane — the rule now lives
                            // where its reason does.
                            let (focused_region, split_region) =
                                ui::visible_pane_regions(panes_before, &model);
                            let mut pane_targets: Vec<(Rect, String)> = Vec::new();
                            if let Some(id) = model.focused.clone() {
                                pane_targets.push((focused_region, id));
                            }
                            if let (Some(rect), Some(id)) = (split_region, model.split.clone()) {
                                pane_targets.push((rect, id));
                            }
                            // The router owns the sidebar's two scroll
                            // offsets, its selected space and its sort mode:
                            // all four are local presentation, none of them
                            // needs a connection, and keeping the clamping in
                            // one place (shared with `render_sidebar`) is
                            // what stops a click's row maths disagreeing with
                            // the frame it landed on.
                            // The Files tab takes clicks and wheel notches over
                            // it, unless an overlay is open on top.
                            let overlay = palette.open || palette.tab_menu.is_some();
                            let files_area = ui::files_region(panes_before, &model).filter(|r| {
                                !overlay && rect_contains(*r, mouse_event.column, mouse_event.row)
                            });
                            let files_event = matches!(
                                mouse_event.kind,
                                MouseEventKind::Down(MouseButton::Left)
                                    | MouseEventKind::ScrollUp
                                    | MouseEventKind::ScrollDown
                            );
                            // Only beside Files, and only with nothing open on
                            // top: a click on a palette row over a full-width
                            // Files tab is not a click on a hidden terminal.
                            if files_area.is_none()
                                && !overlay
                                && matches!(model.shown_files(), Some((_, files::FilesLayout::Split)))
                                && matches!(mouse_event.kind, MouseEventKind::Down(MouseButton::Left))
                                && rect_contains(focused_region, mouse_event.column, mouse_event.row)
                            {
                                // A click on the terminal beside Files hands
                                // it the keys back.
                                model.files_keys = false;
                            }
                            let action = if let (Some(region), true) = (files_area, files_event) {
                                if let Some(space) = model.shown_files().map(|(s, _)| s.to_string()) {
                                    if matches!(mouse_event.kind, MouseEventKind::Down(_)) {
                                        model.files_keys = true;
                                    }
                                    let effect = model
                                        .files
                                        .get_mut(&space)
                                        .map(|v| v.handle_mouse(mouse_event, region))
                                        .unwrap_or(files::Effect::None);
                                    run_files_effect(client.as_mut(), &mut model, &space, effect).await;
                                }
                                Action::Redraw
                            } else { route_mouse(
                                mouse_event,
                                // The whole frame, and nothing else: the
                                // router derives each overlay's own rect from
                                // it with the same functions the renderer
                                // used, so no call site can hand over one that
                                // disagrees with what was drawn.
                                screen,
                                &tabs,
                                &pane_targets,
                                &mut palette,
                                &mut model,
                            ) };
                            // Mouse-arm-local: unlike the key arm above,
                            // `route`'s `Action::PaneInput` has no visible
                            // effect of its own (it arrives later as PTY
                            // output on a different event source), so
                            // deriving `dirty` from the action there would
                            // under-redraw. `route_mouse` never returns
                            // `PaneInput`, so no such case exists here —
                            // every actionable mouse event's visible effect
                            // (a focused pane, a dismissed palette, a
                            // scrolled list, a flipped sort) is fully
                            // captured by its returned `Action`.
                            // Same shared interception the key arm runs, so a
                            // command reached by clicking a palette row does
                            // the same thing as its keyboard binding.
                            // Copying needs the pane grids, which live here and
                            // nowhere the router can see — so it is resolved
                            // in this arm rather than through `decide`.
                            if matches!(action, Action::CopySelection) {
                                copy_selection(&palette, &terms, &mut model);
                            }
                            // Same reason: the pane grids live here, not in
                            // the router.
                            if let Action::ScrollPane { pane_id, delta } = &action {
                                if let Some(term) = terms.get_mut(pane_id) {
                                    term.scroll(*delta);
                                }
                            }
                            let action = intercept_local(action, &mut model);
                            if !matches!(action, Action::None) {
                                dirty = true;
                            }
                            // Dragging the sidebar's edge moves the pane area
                            // without the terminal resizing, so the visible
                            // panes are resized here exactly as the resize arm
                            // would. Compared rather than inferred from the
                            // action: the area is what the panes care about.
                            let panes_after =
                                ui::pane_area(screen, model.sidebar.columns(screen.width));
                            if panes_after != panes_before {
                                let mut ctx = PaneCtx {
                                    base_url: &args.url,
                                    token: args.token.as_deref(),
                                    model: &mut model,
                                    conn: &mut conn,
                                    reattach: &mut reattach,
                                    terms: &mut terms,
                                    pane_area: panes_after,
                                };
                                resize_visible(&mut ctx).await;
                            }
                            match decide(action, model.local().stale) {
                                Disposition::Quit => break,
                                Disposition::SendPaneInput(_) => {
                                    // `route_mouse` never produces
                                    // `Action::PaneInput` — see its own doc
                                    // comment — so this arm is unreachable in
                                    // practice; kept for exhaustiveness, the
                                    // same convention `handle_command`'s own
                                    // never-reached `Command::Detach` arm
                                    // follows.
                                }
                                // Likewise: keys only come from the key arm.
                                Disposition::FilesKey(_) => {}
                                Disposition::RunCommand(cmd) => {
                                    let pane_area = current_pane_area(&terminal, &model);
                                    let mut ctx = PaneCtx {
                                        base_url: &args.url,
                                        token: args.token.as_deref(),
                                        model: &mut model,
                                        conn: &mut conn,
                                        reattach: &mut reattach,
                                        terms: &mut terms,
                                        pane_area,
                                    };
                                    // May open the session picker instead of
                                // creating the pane — see `offer_sessions`.
                                if let Some(cmd) =
                                    offer_sessions(cmd, &mut palette, client.as_mut(), ctx.model).await
                                {
                                    run_command(cmd, client.as_mut(), &mut ctx).await;
                                }
                                }
                                Disposition::Nothing => {}
                            }
                        }
                    }
                    Some(Ok(Event::Resize(cols, rows))) => {
                        dirty = true;
                        // Before the pane area is derived: a resize across
                        // the collapse threshold changes whether the sidebar
                        // is there at all.
                        model.sidebar.note_screen_width(cols);
                        let pane_area = ui::pane_area(
                            Rect::new(0, 0, cols, rows),
                            model.sidebar.columns(cols),
                        );
                        let mut ctx = PaneCtx {
                            base_url: &args.url,
                            token: args.token.as_deref(),
                            model: &mut model,
                            conn: &mut conn,
                            reattach: &mut reattach,
                            terms: &mut terms,
                            pane_area,
                        };
                        // Both visible panes, each to its own half — see
                        // `resize_visible`, which is also what `toggle_split`
                        // calls when the *split* (not the terminal) is what
                        // changed the two regions' sizes.
                        resize_visible(&mut ctx).await;
                    }
                    Some(Ok(_other)) => {
                        // Focus-gained/lost, bracketed paste (mouse now has
                        // its own arm above) — none of the bindings this
                        // task pins react to these.
                    }
                    Some(Err(_)) => {
                        // A terminal I/O read error. Nothing to route;
                        // keep going rather than tearing down the session
                        // over one bad read.
                    }
                    None => {
                        // The input stream ended (stdin closed) — nothing
                        // left to read input from, so there is nothing left
                        // for this client to do.
                        break;
                    }
                }
            }

            // ---- File transfers ----
            Some(msg) = transfer_rx.recv() => {
                dirty = true;
                apply_transfer(msg, client.as_mut(), &mut model).await;
            }

            // ---- Control-channel events ----
            maybe_event = recv_control_event(&mut event_rx) => {
                match maybe_event {
                    Some(event) => {
                        dirty = true;
                        // Raised whatever `apply_event` makes of the
                        // sequence number: a notification that arrived just
                        // after a gap is still true, and the resnapshot
                        // below would never replay it.
                        if event.r#type == "pane.notify" {
                            model.push_toast(&event.payload, Instant::now());
                        }
                        if model.apply_event(&event) == ApplyOutcome::NeedsResnapshot {
                            if let Some(c) = client.as_mut() {
                                // Bounded to a UI-scale timeout (see
                                // `UI_OP_TIMEOUT`'s doc comment) — two
                                // sequential requests with no bound here
                                // could freeze the whole loop for up to 60s
                                // against a half-dead server, and that's
                                // exactly the TCP-alive-nothing-answering
                                // case where `event_rx` never yields `None`
                                // either, so the disconnect handling below
                                // wouldn't even kick in to show
                                // `RECONNECTING`.
                                match tokio::time::timeout(UI_OP_TIMEOUT, snapshot_and_subscribe(c)).await {
                                    Ok(Ok((snapshot, ack))) => apply_prime(&mut model, snapshot, &ack),
                                    Ok(Err(_)) | Err(_) => {
                                        // The resnapshot request failed or
                                        // timed out — most likely the
                                        // connection is dying or dead.
                                        // Nothing to do here directly: the
                                        // next `event_rx.recv()` (or this
                                        // same one, next iteration) will
                                        // observe the closed channel once
                                        // the reader task actually notices,
                                        // and this loop's disconnect
                                        // handling below takes over from
                                        // there.
                                    }
                                }
                            }
                        }
                        // `Model::forget_focus_if_gone` may have just cleared
                        // `focused` (its pane's record went away) while
                        // leaving a healthy split pane on the other half of
                        // the screen. Runs *before* the prune so the latter
                        // sees the settled state. Returns before its own
                        // `.await` in the overwhelmingly common case where
                        // there is nothing to promote.
                        let pane_area = current_pane_area(&terminal, &model);
                        let mut ctx = PaneCtx {
                            base_url: &args.url,
                            token: args.token.as_deref(),
                            model: &mut model,
                            conn: &mut conn,
                            reattach: &mut reattach,
                            terms: &mut terms,
                            pane_area,
                        };
                        promote_split_if_unfocused(&mut ctx).await;
                        // A pane whose record just disappeared
                        // (`pane.removed`, `workspace.removed`, or a
                        // snapshot that no longer lists it) must not keep
                        // its 10,000-line scrollback alive for the rest of
                        // the session.
                        prune_terms(&model, &conn, &mut terms);
                    }
                    None => {
                        // The reader task ended: the control channel is
                        // gone. Enter the reconnecting state.
                        // `conn.detach_all()` deliberately leaves
                        // `model.focused`/`model.split` untouched —
                        // `reattach_focused_pane`/`reattach_split_pane`
                        // (reached once a reconnect succeeds, below) are
                        // what decide whether those panes are still worth
                        // re-attaching to.
                        mark_disconnected(&mut model, "control channel closed");
                        dirty = true;
                        event_rx = None;
                        client = None;
                        conn.detach_all();
                        reconnect_backoff = RECONNECT_INITIAL_BACKOFF;
                        reconnect_task = Some(tokio::spawn(reconnect_attempt(
                            control_url.clone(),
                            args.token.clone(),
                            Duration::ZERO,
                        )));
                    }
                }
            }

            // ---- Reconnect attempts (only polled while one is in flight) ----
            result = recv_join(&mut reconnect_task) => {
                dirty = true;
                match result {
                    Ok((mut new_client, snapshot, ack)) => {
                        // `apply_prime` marks the node connected and clears
                        // staleness — see `mark_connected`.
                        apply_prime(&mut model, snapshot, &ack);
                        event_rx = Some(new_client.events());
                        client = Some(new_client);
                        prune_terms(&model, &conn, &mut terms);
                        // A fresh control channel is genuinely new
                        // information — start *both* pane ladders over
                        // rather than inheriting a rung climbed against the
                        // connection that just died.
                        reattach.focused.stand_down();
                        reattach.split.stand_down();
                        // The control channel is back, but `conn.detach_all()`
                        // ran when it went away — re-attach whatever was
                        // focused before, if it's still live per the fresh
                        // snapshot. Done inline, not scheduled: this fires once
                        // per successful reconnect (itself already backed off),
                        // so it can't spin.
                        let pane_area = current_pane_area(&terminal, &model);
                        let mut ctx = PaneCtx {
                            base_url: &args.url,
                            token: args.token.as_deref(),
                            model: &mut model,
                            conn: &mut conn,
                            reattach: &mut reattach,
                            terms: &mut terms,
                            pane_area,
                        };
                        reattach_focused_pane(&mut ctx).await;
                        // The split pane goes back through its own ladder
                        // instead of being attached inline alongside it.
                        // This arm can already spend `PANE_ATTACH_TIMEOUT`
                        // twice on the focused pane (attach, then resize);
                        // a second inline attach would double how long one
                        // reconnect can freeze the whole loop, for the pane
                        // nobody is typing into. 250ms later is soon enough.
                        if ctx.model.split.is_some() {
                            ctx.reattach.split.schedule();
                        }
                    }
                    Err(_) => {
                        let backoff = take_and_advance(&mut reconnect_backoff);
                        reconnect_task = Some(tokio::spawn(reconnect_attempt(
                            control_url.clone(),
                            args.token.clone(),
                            backoff,
                        )));
                    }
                }
            }

            // ---- Focused pane's socket ----
            //
            // `conn.focused` and `conn.split` are two disjoint fields, so
            // both of these branch expressions can hold `&mut` on their own
            // slot at once (see `PaneConn`'s doc comment). Each borrows —
            // never `.take()`s — and `mpsc::Receiver::recv` is cancel-safe,
            // so losing the race to another branch costs nothing: the
            // future is dropped, rebuilt next iteration, and no already-sent
            // event is lost.
            maybe_event = conn.focused.recv() => {
                dirty = true;
                on_slot_event(
                    Slot::Focused,
                    maybe_event,
                    &mut conn,
                    &mut reattach,
                    &mut model,
                    &mut terms,
                );
            }

            // ---- Split pane's socket ----
            maybe_event = conn.split.recv() => {
                dirty = true;
                on_slot_event(
                    Slot::Split,
                    maybe_event,
                    &mut conn,
                    &mut reattach,
                    &mut model,
                    &mut terms,
                );
            }

            // ---- Scheduled reattach, per slot (each only polled while its
            // own attempt is pending) ----
            //
            // `reattach_due` takes the deadline *by value* (`Instant` is
            // `Copy`), so neither of these branch expressions holds a borrow
            // its own handler — which needs `&mut reattach` to build a
            // `PaneCtx` — would have to wait for.
            _ = reattach_due(reattach.focused.at) => {
                reattach.focused.at = None;
                dirty = true;
                let pane_area = current_pane_area(&terminal, &model);
                let mut ctx = PaneCtx {
                    base_url: &args.url,
                    token: args.token.as_deref(),
                    model: &mut model,
                    conn: &mut conn,
                    reattach: &mut reattach,
                    terms: &mut terms,
                    pane_area,
                };
                reattach_focused_pane(&mut ctx).await;
            }

            _ = reattach_due(reattach.split.at) => {
                reattach.split.at = None;
                dirty = true;
                let pane_area = current_pane_area(&terminal, &model);
                let mut ctx = PaneCtx {
                    base_url: &args.url,
                    token: args.token.as_deref(),
                    model: &mut model,
                    conn: &mut conn,
                    reattach: &mut reattach,
                    terms: &mut terms,
                    pane_area,
                };
                reattach_split_pane(&mut ctx).await;
            }

            // ---- Redraw tick ----
            _ = redraw_tick.tick() => {
                // Toasts expire on this same tick — no timer of their own,
                // and no redraw at all unless one actually went.
                if model.expire_toasts(Instant::now()) {
                    dirty = true;
                }
                // A file the Files tab's selection has rested on.
                if let Some(space) = model.shown_files().map(|(s, _)| s.to_string()) {
                    let due = model.files.get_mut(&space).and_then(|v| v.due_peek(Instant::now()));
                    if let Some(path) = due {
                        run_files_effect(client.as_mut(), &mut model, &space, files::Effect::Peek(path)).await;
                        dirty = true;
                    }
                }
                if dirty {
                    let _ = terminal.draw(|f| ui::render(&model, &terms, &palette, f));
                    dirty = false;
                    // The terminal's own title says what is running and
                    // where, so a tab or window list does too. Only when it
                    // changes: it is an escape sequence per write.
                    let title = ui::brand::window_title(&model.local().node_id);
                    if title != window_title {
                        let _ = execute!(io::stdout(), crossterm::terminal::SetTitle(&title));
                        window_title = title;
                    }
                }
            }
        }
    }

    drop(guard);
    Ok(())
}

/// Awaits the control-channel event receiver if one is present, otherwise
/// never resolves — same "optional `select!` source" pattern as
/// [`recv_join`] and [`PaneSlot::recv`](crate::panes::PaneSlot::recv).
async fn recv_control_event(
    rx: &mut Option<mpsc::Receiver<EventFrameInner>>,
) -> Option<EventFrameInner> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// The whole pane area (the terminal minus the sidebar and the status bar)
/// as of right now, which every [`PaneCtx`] carries so its callees can
/// derive each visible pane's own region from it via [`slot_sizes`](crate::panes::slot_sizes).
///
/// Carried as the undivided rect, not as already-divided sizes, precisely
/// because `Model::split` can change *during* a command — `toggle_split` is
/// the whole point — and a pair of sizes snapshotted at the call site would
/// then be describing a layout that no longer exists. Falls back to a
/// zero-sized rect if the terminal's own size can't be read right now; that
/// is a degraded pane, never a crash.
fn current_pane_area(terminal: &Terminal<CrosstermBackend<io::Stdout>>, model: &Model) -> Rect {
    match terminal.size() {
        Ok(size) => ui::pane_area(
            Rect::new(0, 0, size.width, size.height),
            model.sidebar.columns(size.width),
        ),
        Err(_) => Rect::new(0, 0, 0, 0),
    }
}

/// Everything `handle_command`/`focus_pane` need beyond the specific
/// command (or pane id) being acted on, bundled into one struct so neither
/// function's argument list grows past what's reasonable to read at a call
/// site (clippy's `too_many_arguments` agreed at 8 — `client` stays a
/// separate top-level argument since it, not this bundle, is the thing a
/// command actually acts *on*).
struct PaneCtx<'a> {
    base_url: &'a str,
    token: Option<&'a str>,
    model: &'a mut Model,
    conn: &'a mut PaneConn,
    /// Both slots' reattach ladders. In here rather than a separate
    /// argument because `toggle_split` needs to schedule the split
    /// slot's first rung, and threading a second `&mut` through
    /// `run_command`/`handle_command` alongside this bundle would defeat
    /// the point of having the bundle.
    reattach: &'a mut Reattachers,
    terms: &'a mut HashMap<String, PaneTerm>,
    /// The undivided pane area — see [`current_pane_area`] for why this is
    /// carried as a rect rather than as already-computed sizes.
    pane_area: Rect,
}

/// The accent color to give the `nth` workspace, cycling
/// [`ui::sidebar::COLORS`].
///
/// `Command::CreateWorkspace` used to send `{"name": name}` and nothing
/// else, so *every* workspace created in this client arrived with
/// `color: null`, and the sidebar's accent colors only ever rendered for
/// workspaces created in the web UI.
///
/// Keyed on how many workspaces the model already has rather than on a hash
/// of the name: a hash gives the same name the same color forever, which
/// sounds appealing until two workspaces collide and sit side by side in the
/// sidebar wearing the same accent — the one thing the accent exists to
/// prevent. Cycling guarantees the next 16 are all distinct, which is the
/// property that actually matters, and it is what a user picking from that
/// grid by hand does anyway.
///
/// Indexed with `%`, never `[]` on an unchecked value: `nth` is a live count
/// that grows without bound.
fn workspace_accent(nth: usize) -> &'static str {
    ui::sidebar::COLORS[nth % ui::sidebar::COLORS.len()]
}

/// The status-bar notice to leave after a `workspace.create` attempt —
/// `None` on success, a sanitized `"SPACE ERROR: ..."` line on failure.
///
/// The first version of this arm only ever *set* `ctx.model.notice` on an
/// `Err`, with nothing to *clear* it on the next successful attempt
/// (`CreatePane` gets this for free by routing its success through
/// [`focus_pane`], which already clears `notice` — `CreateWorkspace` has no
/// such path). A rejected name followed by a retyped, accepted one left the
/// stale `SPACE ERROR: ...` message on screen next to the newly-created,
/// perfectly fine workspace, until some unrelated pane event happened to clear
/// it. Returning `Option<String>` unconditionally (`result.err().map(...)`,
/// `None` on `Ok`) rather than only writing on the error branch means the call
/// site's assignment always resolves the slot one way or the other.
///
/// Pulled out as its own pure function — no live [`ControlClient`]
/// involved — specifically so that clear-on-success behavior has a test
/// that does not need one.
///
/// `notice` is a single slot shared with two unrelated domains (either pane
/// socket's error frames — see [`crate::model::Model::notice`]'s own doc
/// comment). A *failure* here still overwrites whatever was there, pane-origin
/// or not: setting is last-write-wins across all three domains, unchanged. A
/// *success* no longer does — the call site clears
/// [`NoticeOrigin::Workspace`] specifically, so a workspace being created
/// cannot retract a pane error that is still true. This function's own
/// contract is unchanged and deliberately origin-free: it turns one request
/// result into `Some(line)`/`None`, and the caller decides what that means
/// for the shared slot.
fn workspace_create_notice(result: Result<serde_json::Value, String>) -> Option<String> {
    result
        .err()
        .map(|e| sanitize_notice(&format!("SPACE ERROR: {e}")))
}

/// Types the agent's own launch command into a freshly created pane.
///
/// **This is what actually starts the agent.** `pane.create` spawns the PTY
/// and *returns* a `launchCommand`, but — unlike the legacy `/ws` path, which
/// calls `term.write()` itself (`bin/terminal-server.js:1919`) — the protocol
/// path never writes it. `agent.start` is the method that does
/// (`bin/lib/runtime/index.js:760`, `writePane(paneId, launchCommand + "\r")`).
/// Without this call a `claude` pane was a bare shell sitting at a prompt: the
/// pane existed, streamed, and accepted input, and no agent was ever launched
/// in it. That was the whole of the 2026-08-12 "it doesn't actually start a
/// claude session" report.
///
/// Called *after* [`focus_pane`], not before: `agent.start` writes to the
/// PTY, and the replay a data socket receives on attach is what puts those
/// bytes on screen. Starting the agent first would type the command into a
/// pane nothing was listening to yet, and the output would only appear on the
/// next replay.
///
/// Errors are deliberately swallowed. The pane is already created and
/// focused, the user can see it, and they can type `claude` themselves — a
/// notice here would be the fourth writer to a one-slot status line (see
/// [`crate::model::Model::notice`]) for a failure whose consequence is
/// already visible on screen.
///
/// `agentSession` is not sent, so [`buildLaunchCommand`]'s resume branch is
/// never taken and every pane starts a *fresh* session. Resuming an existing
/// one needs a session id to offer, and nothing in `DISPATCH_METHODS`
/// enumerates them — recorded as a gap rather than guessed at.
///
/// [`buildLaunchCommand`]: https://github.com/#buildLaunchCommand
async fn start_agent_on(
    client: &mut ControlClient,
    node: Option<&str>,
    pane_id: &str,
    agent_type: &str,
    session: Option<&str>,
) {
    if !launches_a_command(agent_type) {
        return;
    }
    // Must run where the pane does. Sent locally for a remote pane it would
    // name a pane this machine has never heard of, and the agent would never
    // start — the pane would sit at a bare shell prompt looking like it worked.
    let mut params = serde_json::json!({
        "paneId": pane_id,
        "agentType": agent_type,
    });
    // Only sent when a session was chosen. Absent means fresh, which is what
    // `buildLaunchCommand` already treats as the default — so nothing changes
    // for an agent that cannot resume, or for a user who picked "new session".
    if let Some(session) = session {
        params["agentSession"] = serde_json::Value::String(session.to_string());
    }
    let _ = request_on(client, node, "agent.start", params).await;
}

/// `true` iff `agent_type` has a command for [`start_agent_on`] to type.
///
/// `shell` doesn't: its entry in the runtime's own `AGENTS` table has an empty
/// `command`, so `agent.start` refuses it with
/// `agentType "shell" has no launch command`. Asking anyway would be a
/// guaranteed round-trip to a guaranteed error on the one agent type where
/// the pane is *already* exactly what the user asked for.
///
/// Its own function so the rule is testable without a live connection — the
/// alternative is an `if` inside an `async fn` that needs a `ControlClient`
/// to reach at all.
fn launches_a_command(agent_type: &str) -> bool {
    agent_type != "shell"
}

/// The working directory a new pane in `workspace_id` should start in: the
/// `cwd` of that space's most recently listed live pane, or `None` when it
/// has none.
///
/// A space carries no directory of its own — `WorkspaceInfo` has `id`, `name`,
/// `color` and `pane_ids`, and nothing else — so there is no path to read for
/// a space whose first pane this is. Inheriting from a sibling is what makes
/// "new pane in this space" land beside the panes already there, which is
/// what a user means by it; `None` leaves the runtime's own `os.homedir()`
/// default, which is at least somewhere real.
///
/// Takes the *last* live pane rather than the first: it is the most recent
/// thing the user was working on in that space, and a space whose panes span
/// two directories is far more likely to want the newer one.
fn inherited_cwd(model: &Model, workspace_id: &str) -> Option<String> {
    // Scoped to the machine that owns the space: inheriting a local pane's
    // directory for a pane starting on another machine would name a path that
    // very likely does not exist there.
    let owner = model
        .owner_of_space(workspace_id)
        .unwrap_or_else(|| model.local_id_owned());
    model
        .node(&owner)?
        .panes
        .iter()
        .rev()
        .filter(|p| p.live && p.workspace_id.as_deref() == Some(workspace_id))
        .map(|pane| pane.cwd.clone())
        .find(|cwd| !cwd.is_empty())
}

/// Runs a resolved `Disposition::RunCommand` action against the live
/// control connection — shared by the keyboard and mouse arms of `run`'s
/// event loop (a sidebar click resolves to `Command::FocusPane` exactly like
/// the palette's own "Focus ..." entry does) so the [`UI_OP_TIMEOUT`] wrap
/// lives in exactly one place rather than two `select!` arms silently drifting
/// apart. `ctx` is built by the caller (it already needs
/// `pane_size(&terminal)`, which borrows `terminal` immutably while other loop
/// state is borrowed mutably for `ctx` — cleaner left at the call site than
/// threaded through here too).
///
/// Both ladders are `reset()` unconditionally: the user asked for a pane by
/// name — via the palette, a sidebar click, or now a `ctrl+space s` split —
/// which deserves the full backoff ladder rather than whatever rung an
/// unrelated failing reattach had climbed to (the key-input arm's own
/// reasoning before this was pulled out). `reset`, not `stand_down`: an
/// attempt already scheduled stays scheduled, it just starts counting from
/// the first rung again.
async fn run_command(cmd: Command, client: Option<&mut ControlClient>, ctx: &mut PaneCtx<'_>) {
    ctx.reattach.focused.reset();
    ctx.reattach.split.reset();
    let Some(client) = client else { return };
    // Bounded to a UI-scale timeout, not `ControlClient::request`'s own 30s
    // — see `UI_OP_TIMEOUT`'s doc comment:
    // without this, a half-dead server (TCP alive, nothing answering)
    // freezes the whole loop — no redraw, no input, not even detach — for
    // up to 30s.
    let _ = tokio::time::timeout(UI_OP_TIMEOUT, handle_command(cmd, client, ctx)).await;
}

/// Acts on a resolved palette command that needs a live connection. Only
/// reachable while connected (see `run`'s `Action::Command` arm).
/// `Command::Detach` never reaches here — `run` translates it to
/// `Action::Quit` right after `route` produces it (see that translation's
/// own doc comment), since it needs no connection at all and must work
/// identically whether or not one exists. The arm below is kept anyway for
/// exhaustiveness — matching this crate's habit elsewhere of naming a
/// should-never-happen case rather than catching it with `_`.
async fn handle_command(cmd: Command, client: &mut ControlClient, ctx: &mut PaneCtx<'_>) {
    match cmd {
        Command::Detach => {}
        Command::FocusPane(id) => {
            focus_pane(&id, ctx).await;
        }
        // Never reaches here: `input::route` intercepts each of these and
        // re-opens the palette in the relevant mode, the same way `Detach` is
        // intercepted before it can reach a connection it does not need. Named
        // rather than caught by `_`, this crate's habit for a
        // should-never-happen case.
        Command::PickAgent { .. }
        | Command::AskPaneCwd { .. }
        | Command::RenamePane { .. }
        | Command::PickPaneColor { .. }
        | Command::RenameSpace { .. }
        | Command::PickSpaceColor { .. }
        | Command::ToggleMouse
        | Command::ToggleAgentSort
        | Command::ToggleSidebar => {}
        Command::CreatePaneWithSession {
            workspace_id,
            agent_type,
            cwd,
            session,
        } => {
            create_pane(client, ctx, &workspace_id, &agent_type, cwd, session).await;
        }
        Command::CreatePane {
            workspace_id,
            agent_type,
            cwd,
        } => {
            // No session chosen — the caller either had none to offer or the
            // user has not been asked. `offer_sessions` intercepts this
            // command before it arrives when there is a choice to make.
            create_pane(client, ctx, &workspace_id, &agent_type, cwd, None).await;
        }
        Command::ClosePane(id) => {
            let _ = client
                .request("pane.close", serde_json::json!({ "paneId": id }))
                .await;
        }
        Command::RestartPanes(ids) => {
            restart_panes(client, ctx, &ids).await;
        }
        Command::OpenFiles => open_files(client, ctx.model).await,
        // Local; `intercept_local` handles both before a command runs.
        Command::ShowFiles | Command::CloseFiles(_) => {}
        // Intercepted by `offer_sessions` before any command runs.
        Command::BrowseFolder { .. } => {}
        Command::UpdatePane {
            pane_id,
            title,
            color,
        } => {
            // `pane.update` takes either field; only the ones actually chosen
            // are sent, so recoloring never clears a name. An **empty** title
            // is sent rather than skipped — it is how a rename is undone, and
            // the runtime reads it as "hand this pane back to its own OSC
            // title" (see `state.js`'s `setUserTitle`).
            let mut params = serde_json::json!({ "paneId": pane_id });
            if let Some(title) = title {
                params["title"] = serde_json::Value::String(title);
            }
            if let Some(color) = color {
                params["color"] = serde_json::Value::String(color);
            }
            let result = client.request("pane.update", params).await;
            ctx.model.clear_notice_from(NoticeOrigin::Workspace);
            if let Err(e) = result {
                ctx.model.set_notice(
                    NoticeOrigin::Workspace,
                    sanitize_notice(&format!("PANE ERROR: {e}")),
                );
            }
        }
        Command::CloseSpace(workspace_id) => {
            // Kills every pane in the space. Fire-and-forget like its
            // neighbours: `workspace.removed` arrives on the control channel
            // and `model.rs` folds it, dropping the space and its panes from
            // the sidebar without this reading anything back.
            let _ = client
                .request(
                    "workspace.close",
                    serde_json::json!({ "workspaceId": workspace_id }),
                )
                .await;
        }
        Command::UpdateSpace {
            workspace_id,
            name,
            color,
        } => {
            // One method for both fields — `workspace.update` takes `name`
            // and/or `color` and rejects a request carrying neither, so the
            // params are built from whichever are present rather than sending
            // nulls that would *clear* the other one.
            let mut params = serde_json::json!({ "workspaceId": workspace_id });
            if let Some(name) = name {
                params["name"] = serde_json::Value::String(name);
            }
            if let Some(color) = color {
                params["color"] = serde_json::Value::String(color);
            }
            let result = client.request("workspace.update", params).await;
            // Same clear-either-way discipline as `CreateWorkspace`: a
            // rejected rename must say so, and a later accepted one must not
            // leave the old complaint on screen.
            ctx.model.clear_notice_from(NoticeOrigin::Workspace);
            if let Err(e) = result {
                ctx.model.set_notice(
                    NoticeOrigin::Workspace,
                    sanitize_notice(&format!("SPACE ERROR: {e}")),
                );
            }
        }
        Command::MovePane {
            pane_id,
            workspace_id,
        } => {
            // Fire-and-forget: the runtime emits `pane.moved`, which
            // `model.rs` folds, so the pane changes space in the sidebar and
            // the tab bar without this reading anything back.
            let _ = client
                .request(
                    "pane.move",
                    serde_json::json!({
                        "paneId": pane_id,
                        "workspaceId": workspace_id,
                    }),
                )
                .await;
        }
        Command::CreateWorkspace { name } => {
            // Fire-and-forget like its neighbours above: `workspace.created`
            // arrives on the control channel and `model.rs` already folds
            // it, so the new workspace appears in the sidebar without this
            // function reading `workspaceId` back out of the response.
            //
            // The notice slot is resolved either way — see
            // `workspace_create_notice`'s own doc comment for why a
            // *successful* create must clear it too, not just a failed one set
            // it. Both halves are scoped to `NoticeOrigin::Workspace`: `notice`
            // is a slot this shares with two pane-socket domains, and "the
            // workspace I just asked for was created" says nothing about
            // whether a pane's error is still true. Setting is still
            // last-write-wins across all three — see `Model::notice`'s doc
            // comment.
            //
            // Cleared *before* the request, not only after it. `run_command`
            // bounds this whole arm at `UI_OP_TIMEOUT` (3s) while
            // `ControlClient::request` bounds itself at 30s, so the outer
            // timeout can drop this future at the await below and the
            // resolution afterwards never runs. A create that succeeds slowly
            // would then land its `workspace.created` on the control channel —
            // which `model.rs` folds, so the workspace appears in the sidebar —
            // while a *previous* attempt's `SPACE ERROR` was still on the
            // status bar describing a problem that no longer exists. Clearing
            // up front follows `take_focus`'s discipline: write the pessimistic
            // value synchronously, so a dropped future can only fail to *set* a
            // fresh error, never fail to clear a stale one.
            ctx.model.clear_notice_from(NoticeOrigin::Workspace);
            // Created on the machine the sidebar says it will be — see
            // `request_on`. The accent counts that machine's own spaces, so
            // colours cycle per machine rather than by whatever this client
            // happens to be connected to.
            let target = target_of(ctx.model);
            let existing = ctx
                .model
                .node(&ctx.model.target_node())
                .map(|n| n.workspaces.len())
                .unwrap_or(0);
            let result = request_on(
                client,
                target.as_deref(),
                "workspace.create",
                serde_json::json!({
                    "name": name,
                    // Without this, every workspace created here
                    // arrived with `color: null` and the sidebar's
                    // accents rendered only for workspaces made in the
                    // web UI.
                    "color": workspace_accent(existing),
                }),
            )
            .await;
            match workspace_create_notice(result) {
                Some(text) => ctx.model.set_notice(NoticeOrigin::Workspace, text),
                None => ctx.model.clear_notice_from(NoticeOrigin::Workspace),
            }
            // A remote create lands on that machine's event stream, not this
            // one's, so nothing folds it here — re-read the fleet instead.
            if target.is_some() {
                refresh_fleet(client, ctx.model).await;
            }
        }
        Command::NextPane => {
            if let Some(id) = ctx.model.adjacent_live_pane(1) {
                focus_pane(&id, ctx).await;
            }
        }
        Command::PrevPane => {
            if let Some(id) = ctx.model.adjacent_live_pane(-1) {
                focus_pane(&id, ctx).await;
            }
        }
        Command::ToggleSplit => {
            toggle_split(ctx).await;
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    // --- Mouse events for the pane's own program. ---

    const MOUSE_SCREEN: Rect = Rect {
        x: 0,
        y: 0,
        width: 160,
        height: 40,
    };

    /// A focused pane whose program has (or has not) asked for the mouse, and
    /// a click in the middle of the pane area.
    fn mouse_setup(program_wants_mouse: bool) -> (Model, HashMap<String, PaneTerm>, MouseEvent) {
        let mut model = Model::new();
        model.focused = Some("p1".into());
        let mut term = PaneTerm::new(80, 24);
        if program_wants_mouse {
            term.feed(b"\x1b[?1000h\x1b[?1006h");
        }
        let mut terms = HashMap::new();
        terms.insert("p1".to_string(), term);
        let area = ui::pane_area(MOUSE_SCREEN, model.sidebar.columns(MOUSE_SCREEN.width));
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + area.width / 2,
            row: area.y + area.height / 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        (model, terms, click)
    }

    #[test]
    fn a_click_reaches_a_program_that_asked_for_the_mouse() {
        let (model, terms, click) = mouse_setup(true);
        let bytes = forwarded_mouse(&click, MOUSE_SCREEN, &Palette::default(), &model, &terms)
            .expect("forwarded");
        assert!(
            bytes.starts_with(b"\x1b[<0;"),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    #[test]
    fn a_click_stays_with_the_client_when_the_program_did_not_ask() {
        let (model, terms, click) = mouse_setup(false);
        assert_eq!(
            forwarded_mouse(&click, MOUSE_SCREEN, &Palette::default(), &model, &terms),
            None
        );
    }

    #[test]
    fn an_open_palette_keeps_its_clicks() {
        let (model, terms, click) = mouse_setup(true);
        let palette = Palette {
            open: true,
            ..Palette::default()
        };
        assert_eq!(
            forwarded_mouse(&click, MOUSE_SCREEN, &palette, &model, &terms),
            None
        );
    }

    #[test]
    fn the_sidebar_is_never_the_programs() {
        let (model, terms, mut click) = mouse_setup(true);
        click.column = 2;
        assert_eq!(
            forwarded_mouse(&click, MOUSE_SCREEN, &Palette::default(), &model, &terms),
            None
        );
    }

    // --- Connection-free commands. ---

    #[test]
    fn the_agent_sort_command_flips_the_order_and_rewinds_the_list() {
        let mut model = Model::new();
        model.agents_scroll = 7;
        let action = intercept_local(Action::Command(Command::ToggleAgentSort), &mut model);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(model.agent_sort, crate::model::AgentSort::Priority);
        assert_eq!(model.agents_scroll, 0, "an offset into the old order");
        intercept_local(Action::Command(Command::ToggleAgentSort), &mut model);
        assert_eq!(model.agent_sort, crate::model::AgentSort::Grouped);
    }

    #[test]
    fn the_agent_sort_command_works_while_disconnected() {
        // Resolved before `decide`, which would refuse any `Command` while
        // stale — and reordering a local list needs no connection.
        let mut model = Model::new();
        model.local_mut().stale = true;
        let action = intercept_local(Action::Command(Command::ToggleAgentSort), &mut model);
        assert!(matches!(decide(action, true), Disposition::Nothing));
        assert_eq!(model.agent_sort, crate::model::AgentSort::Priority);
    }

    #[test]
    fn the_sidebar_command_hides_and_shows_it_without_a_connection() {
        let mut model = Model::new();
        model.sidebar.note_screen_width(120);
        model.local_mut().stale = true;
        let action = intercept_local(Action::Command(Command::ToggleSidebar), &mut model);
        assert!(matches!(action, Action::Redraw), "{action:?}");
        assert_eq!(model.sidebar.columns(120), 0);
        assert_eq!(ui::pane_area(Rect::new(0, 0, 120, 30), 0).x, 0);
        intercept_local(Action::Command(Command::ToggleSidebar), &mut model);
        assert_eq!(model.sidebar.columns(120), ui::sidebar::WIDTH);
    }

    // --- The viewed node's connection state (federation phase 1). ---

    #[test]
    fn a_successful_snapshot_marks_the_local_node_connected() {
        let mut model = Model::new();
        model.local_mut().conn = ConnState::Connecting;
        mark_connected(&mut model);
        assert_eq!(model.local().conn, ConnState::Direct);
    }

    #[test]
    fn losing_the_control_channel_marks_the_local_node_offline_with_a_reason() {
        let mut model = Model::new();
        model.local_mut().conn = ConnState::Direct;
        mark_disconnected(&mut model, "connection reset");
        assert_eq!(
            model.local().conn,
            ConnState::Offline("connection reset".into())
        );
        assert!(model.local().stale, "an offline node's model is stale");
    }

    #[test]
    fn reconnecting_clears_both_the_offline_state_and_the_staleness() {
        // The two used to be set independently at four call sites; the pair
        // of helpers exists so they cannot drift apart again.
        let mut model = Model::new();
        mark_disconnected(&mut model, "reset");
        mark_connected(&mut model);
        assert_eq!(model.local().conn, ConnState::Direct);
        assert!(!model.local().stale);
    }

    // --- TerminalGuard's Drop mechanism (see the module doc comment's
    // "What the automated tests cannot verify" section for what's
    // genuinely automatable here vs. not). ---

    #[test]
    fn dropping_the_terminal_guard_runs_its_restore_exactly_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        {
            let calls = calls.clone();
            let _guard = TerminalGuard {
                restore: move || {
                    calls.fetch_add(1, Ordering::SeqCst);
                },
            };
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dropping_the_terminal_guard_during_a_panicking_unwind_still_runs_its_restore() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_in_closure = calls.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = TerminalGuard {
                restore: move || {
                    calls_in_closure.fetch_add(1, Ordering::SeqCst);
                },
            };
            panic!("boom");
        }));
        assert!(result.is_err());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "Drop must run during a panicking unwind, not just a normal return"
        );
    }

    #[test]
    fn restore_then_runs_restore_before_the_delegated_hook() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        restore_then(
            move || o1.lock().unwrap_or_else(|e| e.into_inner()).push("restore"),
            move || {
                o2.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("delegated")
            },
        );
        assert_eq!(
            *order.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["restore", "delegated"]
        );
    }

    // --- The restoration sequence disables mouse capture, before leaving
    // the alternate screen. Mirrors
    // `restore_then_runs_restore_before_the_delegated_hook` immediately above:
    // crossterm's own `DisableMouseCapture`/`EnableMouseCapture` bypass
    // whatever writer they're given entirely on Windows (they hard-code
    // `Command::is_ansi_code_supported` to `false`, so `queue`/`execute` always
    // dispatch to a real `SetConsoleMode` WinAPI call — verified against
    // crossterm 0.28.1's own `event/sys/windows.rs`/`command.rs`), so there is
    // no fake writer that can observe a *real* `DisableMouseCapture` call's
    // effect without a real console attached to the test process.
    // `restore_sequence` is `restore_terminal`'s ordering pulled out generic
    // over its three actions, the same reason `restore_then` is generic over
    // its two — so
    // this can be pinned with order-recording fake closures instead. ---

    #[test]
    fn the_restoration_sequence_disables_mouse_capture_before_leaving_the_alternate_screen() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let o1 = order.clone();
        let o2 = order.clone();
        let o3 = order.clone();
        restore_sequence(
            move || {
                o1.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("mouse_capture_disabled")
            },
            move || {
                o2.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("raw_mode_disabled")
            },
            move || {
                o3.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push("left_alternate_screen")
            },
        );
        assert_eq!(
            *order.lock().unwrap_or_else(|e| e.into_inner()),
            vec![
                "mouse_capture_disabled",
                "raw_mode_disabled",
                "left_alternate_screen",
            ]
        );
    }

    // --- Args parsing ---

    /// Parses `argv` against an environment with nothing in it — the
    /// default for every case that isn't specifically about
    /// [`TOKEN_ENV_VAR`]. Never touches the real process environment: a
    /// `SPACES_TOKEN` in the developer's or CI's own shell must not change
    /// what these tests assert.
    fn parse_no_env(argv: &[&str]) -> Result<Args, String> {
        Args::parse_with_env(argv.iter().map(|s| s.to_string()), |_| None)
    }

    #[test]
    fn parses_url_and_token() {
        let args =
            parse_no_env(&["--url", "ws://127.0.0.1:3457", "--token", "abc"]).expect("valid args");
        assert_eq!(args.url, "ws://127.0.0.1:3457");
        assert_eq!(args.token.as_deref(), Some("abc"));
    }

    #[test]
    fn token_is_optional() {
        let args = parse_no_env(&["--url", "ws://127.0.0.1:3457"]).expect("valid args");
        assert_eq!(args.token, None);
    }

    #[test]
    fn a_missing_url_is_an_error_not_a_panic() {
        assert!(parse_no_env(&[]).is_err());
    }

    #[test]
    fn a_dangling_flag_with_no_value_is_an_error_not_a_panic() {
        assert!(parse_no_env(&["--url"]).is_err());
    }

    #[test]
    fn an_unrecognized_argument_is_an_error_not_a_panic() {
        assert!(parse_no_env(&["--bogus"]).is_err());
    }

    #[test]
    fn a_trailing_slash_on_url_is_stripped() {
        // ControlClient::connect gets `{url}/proto` appended by the caller,
        // PaneSocket::attach gets the base unchanged and appends its own
        // `/proto/pane/<id>` — a trailing slash on `--url` must not survive
        // into either, or both produce a double slash.
        let args = parse_no_env(&["--url", "ws://h:1/"]).expect("valid args");
        assert_eq!(args.url, "ws://h:1");
    }

    // --- SPACES_TOKEN (`--token` used to be the only source, and it ends
    // up on three process command lines). ---

    fn env_with(value: &str) -> impl Fn(&str) -> Option<String> + '_ {
        move |key| (key == TOKEN_ENV_VAR).then(|| value.to_string())
    }

    #[test]
    fn the_environment_token_is_used_when_no_flag_is_given() {
        let args = Args::parse_with_env(
            ["--url", "ws://h:1"].into_iter().map(String::from),
            env_with("from-env"),
        )
        .expect("valid args");
        assert_eq!(args.token.as_deref(), Some("from-env"));
    }

    #[test]
    fn the_environment_token_wins_over_the_flag() {
        let args = Args::parse_with_env(
            ["--url", "ws://h:1", "--token", "from-argv"]
                .into_iter()
                .map(String::from),
            env_with("from-env"),
        )
        .expect("valid args");
        assert_eq!(
            args.token.as_deref(),
            Some("from-env"),
            "the environment is the preferred source"
        );
    }

    #[test]
    fn an_empty_environment_token_falls_back_to_the_flag() {
        let args = Args::parse_with_env(
            ["--url", "ws://h:1", "--token", "from-argv"]
                .into_iter()
                .map(String::from),
            env_with(""),
        )
        .expect("valid args");
        assert_eq!(args.token.as_deref(), Some("from-argv"));
    }

    #[test]
    fn an_empty_token_from_either_source_is_treated_as_no_token() {
        let args = Args::parse_with_env(
            ["--url", "ws://h:1", "--token", ""]
                .into_iter()
                .map(String::from),
            |_| None,
        )
        .expect("valid args");
        assert_eq!(args.token, None);
    }

    // --- next_backoff (pin the exact 250ms -> 8s sequence the binding
    // constraint requires; this is also the regression test for doubling
    // the backoff *before* using it, which skipped the 250ms rung
    // entirely). ---

    #[test]
    fn reconnect_backoff_sequence_matches_the_pinned_250ms_to_8s_contract() {
        // The very first attempt after a disconnect is immediate
        // (`Duration::ZERO`, spawned directly in `run`'s disconnect arm) —
        // not covered by `next_backoff`, which only governs the backoff
        // for every attempt *after* a failure. This pins every rung after
        // that, including that it stays capped at 8s forever once reached.
        let mut backoff = RECONNECT_INITIAL_BACKOFF;
        let expected = [
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(8),
            Duration::from_secs(8),
        ];
        for expected_backoff in expected {
            assert_eq!(backoff, expected_backoff);
            backoff = next_backoff(backoff);
        }
    }

    // --- take_and_advance (pin the capture-before-mutate ordering at the
    // actual call-site granularity, not just the pure doubling formula —
    // see this function's own doc comment for the gap this closes). ---

    #[test]
    fn take_and_advance_returns_the_pre_advance_value_and_still_advances() {
        // Same sequence as next_backoff's own test, but observed through
        // take_and_advance — the thing `run`'s reconnect-failure arm
        // actually calls — so a call-site ordering regression (using the
        // post-advance value instead of the pre-advance one) fails this
        // test even though it would never fail next_backoff's.
        let mut backoff = RECONNECT_INITIAL_BACKOFF;
        let mut observed = Vec::new();
        for _ in 0..8 {
            observed.push(take_and_advance(&mut backoff));
        }
        assert_eq!(
            observed,
            vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(8),
                Duration::from_secs(8),
            ]
        );
        // `backoff` itself must also have advanced past the last returned
        // value (capped), not just the returned sequence being right.
        assert_eq!(backoff, Duration::from_secs(8));
    }

    // --- decide (pin "input is refused while disconnected" directly,
    // without a real connection/socket/terminal). ---

    #[test]
    fn quit_always_quits_regardless_of_staleness() {
        assert_eq!(decide(Action::Quit, true), Disposition::Quit);
        assert_eq!(decide(Action::Quit, false), Disposition::Quit);
    }

    #[test]
    fn pane_input_is_refused_while_stale() {
        assert_eq!(
            decide(Action::PaneInput(b"x".to_vec()), true),
            Disposition::Nothing
        );
    }

    #[test]
    fn pane_input_is_delivered_while_connected() {
        assert_eq!(
            decide(Action::PaneInput(b"x".to_vec()), false),
            Disposition::SendPaneInput(b"x".to_vec())
        );
    }

    #[test]
    fn a_command_is_refused_while_stale() {
        assert_eq!(
            decide(Action::Command(Command::NextPane), true),
            Disposition::Nothing
        );
    }

    #[test]
    fn a_command_runs_while_connected() {
        assert_eq!(
            decide(Action::Command(Command::NextPane), false),
            Disposition::RunCommand(Command::NextPane)
        );
    }

    #[test]
    fn redraw_is_always_a_no_op_disposition() {
        assert_eq!(decide(Action::Redraw, false), Disposition::Nothing);
        assert_eq!(decide(Action::Redraw, true), Disposition::Nothing);
    }

    #[test]
    fn none_is_always_a_no_op_disposition() {
        assert_eq!(decide(Action::None, false), Disposition::Nothing);
        assert_eq!(decide(Action::None, true), Disposition::Nothing);
    }

    // --- is_actionable_mouse_event (the mouse arm's `terminal.size()`
    // syscall and `sidebar_rows` allocation must not run for a kind that
    // can never produce more than `Action::None`). ---

    #[test]
    fn a_left_click_is_actionable() {
        assert!(is_actionable_mouse_event(MouseEventKind::Down(
            MouseButton::Left
        )));
    }

    #[test]
    fn the_vertical_wheel_is_actionable() {
        assert!(is_actionable_mouse_event(MouseEventKind::ScrollUp));
        assert!(is_actionable_mouse_event(MouseEventKind::ScrollDown));
    }

    #[test]
    fn a_right_click_is_actionable_now_that_tabs_have_a_context_menu() {
        assert!(is_actionable_mouse_event(MouseEventKind::Down(
            MouseButton::Right
        )));
    }

    #[test]
    fn everything_else_is_not_actionable() {
        for kind in [
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Right),
            MouseEventKind::Drag(MouseButton::Middle),
            MouseEventKind::Drag(MouseButton::Right),
            MouseEventKind::Moved,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            assert!(
                !is_actionable_mouse_event(kind),
                "{kind:?} must not be actionable"
            );
        }
    }

    /// The gate and the router must admit the same set, or a feature is
    /// unreachable while all of its own tests pass.
    ///
    /// This is not a hypothetical. Drag-to-select shipped with `route_mouse`
    /// fully implemented and tested, and this gate — which decides whether
    /// `route_mouse` is called at all — still discarding `Drag` and `Up`. Every
    /// selection test passed because they call the router directly. The user
    /// reported that dragging did nothing; what they were seeing when they
    /// held ctrl was their *terminal's* own selection bypassing the client
    /// entirely.
    ///
    /// Asserting against a literal list would just be the same omission
    /// written twice, so this drives the router with each kind and checks that
    /// anything the router acts on is a kind that can reach it.
    #[test]
    fn the_kinds_admitted_here_are_exactly_the_kinds_the_router_acts_on() {
        use crossterm::event::{KeyModifiers, MouseEvent};

        let screen = Rect::new(0, 0, 80, 24);
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Down(MouseButton::Right),
            MouseEventKind::Down(MouseButton::Middle),
            MouseEventKind::Up(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Right),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Right),
            MouseEventKind::Moved,
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
            MouseEventKind::ScrollLeft,
            MouseEventKind::ScrollRight,
        ] {
            let mut model = model_with_pane_ids(&["p1"]);
            model.focused = Some("p1".into());
            model.local_mut().workspaces = vec![spaces_proto::WorkspaceInfo {
                git: None,
                id: "w1".into(),
                name: "api".into(),
                color: None,
                pane_ids: vec![],
            }];
            let mut palette = ui::Palette::default();
            // A selection already in progress, so `Drag`/`Up` have something
            // to act on — which is exactly the state the gate must not filter.
            palette.selection = Some(ui::selection::Selection {
                pane_id: "p1".into(),
                region: Rect::new(28, 1, 52, 22),
                anchor: (40, 5),
                head: (40, 5),
            });
            let bar = ui::tabs::tab_bar(
                ui::tab_bar_area(Rect::new(0, 0, 80, 24), ui::sidebar::WIDTH),
                &model,
            );
            let event = MouseEvent {
                kind,
                column: 40,
                row: 5,
                modifiers: KeyModifiers::NONE,
            };
            let acted = !matches!(
                route_mouse(
                    event,
                    screen,
                    &bar,
                    &[(Rect::new(28, 1, 52, 22), "p1".to_string())],
                    &mut palette,
                    &mut model,
                ),
                Action::None
            );
            if acted {
                assert!(
                    is_actionable_mouse_event(kind),
                    "{kind:?} does something in the router but never reaches it"
                );
            }
        }
    }

    #[test]
    fn sanitize_notice_strips_control_characters_and_bounds_the_length() {
        assert_eq!(sanitize_notice("plain text"), "plain text");
        assert_eq!(sanitize_notice("a\u{1b}b\rc\nd\u{7}"), "abcd");
        assert_eq!(
            sanitize_notice(&"x".repeat(10_000)).chars().count(),
            NOTICE_MAX_CHARS
        );
        // Multi-byte characters must survive the truncation intact —
        // `take` on chars, never a byte slice.
        assert_eq!(sanitize_notice("\u{e9}\u{4e2d}"), "\u{e9}\u{4e2d}");
    }

    // --- `workspace_create_notice` (a successful create must clear a
    // stale failure notice, not just leave a failed one set). ---

    #[test]
    fn a_rejected_workspace_create_leaves_a_sanitized_notice() {
        let err = "bad_request: name is required".to_string();
        let notice = workspace_create_notice(Err(err)).expect("a rejection must produce a notice");
        assert!(notice.contains("bad_request"), "{notice}");
        assert!(notice.contains("name is required"), "{notice}");
    }

    #[test]
    fn a_successful_workspace_create_produces_no_notice() {
        assert_eq!(
            workspace_create_notice(Ok(serde_json::json!({ "workspaceId": "w1" }))),
            None
        );
    }

    /// Replays `handle_command`'s `Command::CreateWorkspace` arm against
    /// `model` with `result` standing in for what the request returned —
    /// everything that arm does to the notice slot, and nothing that needs a
    /// live `ControlClient`. Kept beside the arm's own two lines so the two
    /// cannot drift.
    fn apply_workspace_create(model: &mut Model, result: Result<serde_json::Value, String>) {
        match workspace_create_notice(result) {
            Some(text) => model.set_notice(NoticeOrigin::Workspace, text),
            None => model.clear_notice_from(NoticeOrigin::Workspace),
        }
    }

    #[test]
    fn a_successful_workspace_create_clears_a_notice_a_prior_failed_attempt_left() {
        // The exact regression: fail once (a notice appears), retype a valid
        // name, succeed — the stale notice must not survive next to the
        // newly-created workspace.
        let mut model = Model::default();
        let err = "bad_request: name is required".to_string();
        apply_workspace_create(&mut model, Err(err));
        assert!(
            model.notice.is_some(),
            "precondition: the failure left a notice"
        );

        apply_workspace_create(&mut model, Ok(serde_json::json!({ "workspaceId": "w1" })));
        assert_eq!(
            model.notice, None,
            "a later success must clear the earlier failure's notice"
        );
    }

    #[test]
    fn a_successful_workspace_create_leaves_a_pane_error_standing() {
        // The workspace half of notice origins: "the workspace I asked for was
        // created" is evidence about workspaces and nothing else. A pane the
        // server is still refusing has not stopped being refused.
        let mut model = Model::default();
        model.set_notice(
            NoticeOrigin::FocusedPane,
            "PANE ERROR (claude, unauthorized): token expired".into(),
        );
        apply_workspace_create(&mut model, Ok(serde_json::json!({ "workspaceId": "w1" })));
        assert_eq!(
            model.notice_text(),
            Some("PANE ERROR (claude, unauthorized): token expired"),
            "an unrelated domain's line must survive a workspace success"
        );
    }

    #[test]
    fn a_rejected_workspace_create_still_overwrites_another_domains_line() {
        // The other half, equally deliberate: *setting* stays
        // last-write-wins across all three origins. Only clearing is scoped.
        let mut model = Model::default();
        model.set_notice(
            NoticeOrigin::SplitPane,
            "SPLIT PANE ERROR (codex): refused".into(),
        );
        let err = "bad_request: name is required".to_string();
        apply_workspace_create(&mut model, Err(err));
        let (origin, text) = model.notice.clone().expect("the rejection is on screen");
        assert_eq!(origin, NoticeOrigin::Workspace);
        assert!(text.contains("bad_request"), "{text}");
    }

    pub(crate) fn model_with_pane_ids(ids: &[&str]) -> Model {
        let mut m = Model::default();
        m.local_mut().panes = ids
            .iter()
            .map(|id| spaces_proto::PaneInfo {
                id: (*id).into(),
                workspace_id: Some("w1".into()),
                agent_type: "claude".into(),
                agent_state: spaces_proto::AgentState::Unknown,
                seen: true,
                title: String::new(),
                cwd: String::new(),
                session_ref: None,
                live: true,
                color: None,
            })
            .collect();
        m
    }

    #[test]
    fn a_workspace_accent_cycles_the_web_uis_own_palette() {
        // `workspace.create` used to send only a name, so every workspace
        // created here got `color: null` and the sidebar's accents
        // rendered only for workspaces made in the web UI.
        assert_eq!(workspace_accent(0), ui::sidebar::COLORS[0]);
        assert_eq!(workspace_accent(15), ui::sidebar::COLORS[15]);
        // Wraps rather than indexing out of bounds — `nth` is a live count.
        assert_eq!(workspace_accent(16), ui::sidebar::COLORS[0]);
        assert_eq!(workspace_accent(usize::MAX), ui::sidebar::COLORS[15]);
        // The next 16 are all distinct, which is the property the accent
        // exists for: two workspaces side by side never share a color.
        let first16: std::collections::HashSet<&str> = (0..16).map(workspace_accent).collect();
        assert_eq!(first16.len(), 16);
    }

    #[test]
    fn every_accent_colour_is_one_the_sidebar_can_actually_parse() {
        // A color this client sends but cannot render would make
        // TUI-created workspaces permanently accent-less in the TUI.
        for c in ui::sidebar::COLORS {
            assert!(
                ui::sidebar::accent(Some(c)).is_some(),
                "sidebar cannot parse the color we send: {c}"
            );
        }
    }

    // --- Starting the agent, and starting it somewhere sensible
    // (2026-08-12: "it doesn't actually start a claude session nor would it
    // know where the session should be started"). ---

    #[test]
    fn every_agent_but_shell_gets_its_launch_command_typed() {
        // `shell`'s entry in the runtime's `AGENTS` table has an empty
        // command, so `agent.start` refuses it — asking would be a guaranteed
        // round-trip to a guaranteed error.
        assert!(!launches_a_command("shell"));
        for agent in ["claude", "codex", "gemini", "aider", "forge", "pi"] {
            assert!(launches_a_command(agent), "{agent} must be launched");
        }
        // Every type the palette can offer is covered one way or the other.
        for agent in ui::palette::AGENT_TYPES {
            let _ = launches_a_command(agent);
        }
    }

    #[test]
    fn a_new_pane_inherits_its_cwd_from_the_space_it_joins() {
        // A space carries no directory of its own, so a sibling's is the only
        // signal there is — and it is the right one: "new pane in this space"
        // means beside the panes already there.
        let mut m = model_with_pane_ids(&["p1", "p2"]);
        m.local_mut().panes[0].cwd = "/first".into();
        m.local_mut().panes[1].cwd = "/second".into();
        assert_eq!(
            inherited_cwd(&m, "w1").as_deref(),
            Some("/second"),
            "the most recent pane, not the first"
        );
    }

    #[test]
    fn a_spaces_first_pane_has_no_cwd_to_inherit() {
        // `None` leaves the runtime's own `os.homedir()` default rather than
        // inventing a path that may not exist.
        let m = model_with_pane_ids(&[]);
        assert_eq!(inherited_cwd(&m, "w1"), None);

        // And a sibling with an empty `cwd` is not a usable answer either.
        let mut blank = model_with_pane_ids(&["p1"]);
        blank.local_mut().panes[0].cwd = String::new();
        assert_eq!(inherited_cwd(&blank, "w1"), None);
    }

    #[test]
    fn a_dead_sibling_is_not_where_a_new_pane_should_start() {
        let mut m = model_with_pane_ids(&["gone", "alive"]);
        m.local_mut().panes[0].cwd = "/stale".into();
        m.local_mut().panes[1].cwd = "/current".into();
        m.local_mut().panes[1].live = true;
        m.local_mut().panes[0].live = false;
        assert_eq!(inherited_cwd(&m, "w1").as_deref(), Some("/current"));
    }

    #[test]
    fn another_spaces_panes_are_never_inherited_from() {
        let mut m = model_with_pane_ids(&["p1"]);
        m.local_mut().panes[0].cwd = "/elsewhere".into();
        m.local_mut().panes[0].workspace_id = Some("w9".into());
        assert_eq!(inherited_cwd(&m, "w1"), None);
    }

    #[test]
    fn a_remembered_pane_that_died_or_moved_falls_back_to_the_first() {
        // Filtered at read time rather than pruned on every pane event: both
        // of these would otherwise send the user to a pane that is not in the
        // space they just clicked.
        let mut model = model_with_pane_ids(&["a", "b"]);
        model
            .local_mut()
            .last_focused
            .insert("w1".into(), "b".into());
        assert_eq!(model.pane_to_focus_in("w1").as_deref(), Some("b"));

        let mut exited = model.clone();
        exited.local_mut().panes[1].live = false;
        assert_eq!(exited.pane_to_focus_in("w1").as_deref(), Some("a"));

        let mut moved = model.clone();
        moved.local_mut().panes[1].workspace_id = Some("w9".into());
        assert_eq!(moved.pane_to_focus_in("w1").as_deref(), Some("a"));

        // And a space with nothing live in it has nothing to focus.
        let mut empty = model.clone();
        for pane in &mut empty.local_mut().panes {
            pane.live = false;
        }
        assert_eq!(empty.pane_to_focus_in("w1"), None);
    }

    // --- The Files tab. ---

    fn with_files(layout: Option<files::FilesLayout>) -> Model {
        let mut m = model_with_pane_ids(&["p1", "p2"]);
        m.local_mut().workspaces = vec![spaces_proto::WorkspaceInfo {
            git: None,
            id: "w1".into(),
            name: "api".into(),
            color: None,
            pane_ids: vec!["p1".into(), "p2".into()],
        }];
        m.focused = Some("p1".into());
        m.selected_space = Some("w1".into());
        m.files.insert("w1".into(), files::FilesView::new(None));
        m.files_shown = layout;
        m.files_keys = layout.is_some();
        m
    }

    fn press(code: crossterm::event::KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE)
    }

    #[test]
    fn keys_go_to_the_files_tab_while_it_has_them_and_never_to_the_terminal() {
        let m = with_files(Some(files::FilesLayout::Full));
        let mut p = Palette::default();
        let action = route(
            press(crossterm::event::KeyCode::Char('x')),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::FilesKey(_)), "{action:?}");
        assert!(matches!(decide(action, true), Disposition::FilesKey(_)));

        // Handed back to the terminal, typing reaches it again.
        let mut m = m;
        m.files_keys = false;
        let action = route(
            press(crossterm::event::KeyCode::Char('x')),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(matches!(action, Action::PaneInput(_)), "{action:?}");
    }

    #[test]
    fn ctrl_k_still_opens_the_palette_over_files() {
        let m = with_files(Some(files::FilesLayout::Full));
        let mut p = Palette::default();
        let ctrl_k = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('k'),
            crossterm::event::KeyModifiers::CONTROL,
        );
        route(ctrl_k, &mut p, Some("p1"), &m);
        assert!(p.open);
    }

    #[test]
    fn the_chord_f_opens_files() {
        let m = model_with_pane_ids(&["p1"]);
        let mut p = Palette::default();
        let ctrl_space = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::CONTROL,
        );
        route(ctrl_space, &mut p, Some("p1"), &m);
        let action = route(
            press(crossterm::event::KeyCode::Char('f')),
            &mut p,
            Some("p1"),
            &m,
        );
        assert!(
            matches!(action, Action::Command(Command::OpenFiles)),
            "{action:?}"
        );
    }

    #[test]
    fn split_puts_the_terminal_beside_files_and_back() {
        let mut m = with_files(Some(files::FilesLayout::Full));
        intercept_local(Action::Command(Command::ToggleSplit), &mut m);
        assert_eq!(m.files_shown, Some(files::FilesLayout::Split));
        let area = Rect::new(30, 1, 100, 30);
        let (terminal, other) = ui::visible_pane_regions(area, &m);
        let files_area = ui::files_region(area, &m).unwrap();
        assert_eq!(other, None, "no second terminal while files is beside one");
        assert!(files_area.x < terminal.x, "files left, terminal right");
        assert_eq!(files_area.width + terminal.width, area.width);
        intercept_local(Action::Command(Command::ToggleSplit), &mut m);
        assert_eq!(m.files_shown, Some(files::FilesLayout::Full));
        assert_eq!(ui::files_region(area, &m), Some(area));
    }

    #[test]
    fn showing_and_closing_the_files_tab_are_local() {
        let mut m = with_files(None);
        assert!(matches!(
            intercept_local(Action::Command(Command::ShowFiles), &mut m),
            Action::Redraw
        ));
        assert_eq!(m.files_shown, Some(files::FilesLayout::Full));
        assert!(m.files_have_keys());
        intercept_local(Action::Command(Command::CloseFiles("w1".into())), &mut m);
        assert!(m.files.is_empty());
        assert!(m.shown_files().is_none());
        assert!(!m.files_have_keys());
    }

    #[test]
    fn the_terminal_gets_no_mouse_events_from_over_files() {
        let (mut model, terms, click) = mouse_setup(true);
        model.local_mut().workspaces = with_files(None).local().workspaces.clone();
        model.selected_space = Some("w1".into());
        model.files.insert("w1".into(), files::FilesView::new(None));
        model.files_shown = Some(files::FilesLayout::Full);
        let palette = Palette::default();
        assert!(forwarded_mouse(&click, MOUSE_SCREEN, &palette, &model, &terms).is_none());
    }

    #[test]
    fn the_files_tab_is_clickable_and_has_a_close_menu() {
        let mut m = with_files(None);
        let mut p = Palette::default();
        let area = Rect::new(30, 0, 100, 1);
        let bar = ui::tabs::tab_bar(area, &m);
        assert!(bar.files_tab.width > 0, "{bar:?}");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: bar.files_tab.x + 1,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        let screen = Rect::new(0, 0, 130, 30);
        let action = route_mouse(click, screen, &bar, &[], &mut p, &mut m);
        assert!(
            matches!(action, Action::Command(Command::ShowFiles)),
            "{action:?}"
        );

        let right = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            ..click
        };
        route_mouse(right, screen, &bar, &[], &mut p, &mut m);
        let menu = p.tab_menu.clone().expect("menu opened");
        let items = ui::menu::items(&m, &menu);
        assert_eq!(
            items,
            vec![("close files".to_string(), Command::CloseFiles("w1".into()))]
        );
    }
}
