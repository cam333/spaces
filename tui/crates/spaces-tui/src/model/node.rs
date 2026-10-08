//! One node's mirror of runtime truth, plus how this client is currently
//! reaching it.
//!
//! Split out of `model.rs` because the fleet's per-node state is a separable
//! unit with its own tests, and because `model.rs` was already the crate's
//! second-largest file.
//!
//! The local node is an ordinary [`NodeState`] in [`crate::model::Model`]'s
//! map — see that type's docs for why it is never special-cased.

use std::collections::HashMap;

use spaces_proto::{PaneInfo, WorkspaceInfo};

use super::{display_state, DisplayState};

/// A node's stable identifier — `node.json`'s `nodeId`, a UUID.
///
/// A newtype rather than a bare `String` so a node id and a pane id cannot be
/// silently swapped at a call site; both are UUID-shaped strings and the
/// compiler is the only thing that will ever notice the difference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(id: &str) -> NodeId {
        NodeId(id.to_string())
    }

    /// The bare id, for putting on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How this client is currently reaching one node.
///
/// For the local node this tracks the control channel directly. For a peer it
/// mirrors what the local node reports about its outbound link, since the
/// client never dials a peer itself — see `bin/lib/network/links.js`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConnState {
    /// A connection attempt is in flight.
    #[default]
    Connecting,
    /// Connected straight to the node's own `/proto`.
    Direct,
    /// Paired and believed reachable, with no link open right now.
    ///
    /// The resting state for a machine nobody is looking at: links are dialled
    /// lazily, so having none is normal rather than a fault. Distinct from
    /// `Offline`, which means an attempt actually failed — conflating the two
    /// made a perfectly healthy machine read as down.
    Idle,
    /// Not connected. Carries the reason, for the status bar.
    Offline(String),
    /// Known to the fleet but never paired — there is no key with which to
    /// connect, so it is listed and not reachable. Discovery is not trust.
    Unpaired,
}

impl ConnState {
    /// Whether there is any point dialling this machine.
    ///
    /// True for everything paired, **including offline** — an offline machine
    /// is exactly the one a refresh exists to retry, and its status only
    /// leaves `Offline` when a dial succeeds. `Unpaired` is the one real
    /// exclusion: there is no key, so asking can only fail.
    pub fn is_dialable(&self) -> bool {
        !matches!(self, ConnState::Unpaired)
    }
}

/// One node's mirror of runtime truth: what a `session.snapshot()` from that
/// node established, folded forward by its own `events` stream.
///
/// `seq` is per-node — each node numbers its own event stream, and a gap on
/// one says nothing about any other. That is the single most important reason
/// this type exists rather than the fields living flat on `Model`.
/// Replaces `s` with a terminal-safe copy when it holds anything that is not
/// (control characters, bidi overrides); leaves ordinary text untouched.
fn make_safe(s: &mut String) {
    if crate::files::needs_sanitizing(s) {
        *s = crate::files::sanitize(s);
    }
}

impl NodeState {
    /// Cleans every server-supplied string this node holds that is drawn on
    /// screen. ratatui writes cell text to the terminal as is, so a space
    /// name or pane title holding an escape sequence — set by a prompt-
    /// injected agent, or a paired machine — would otherwise reach the
    /// user's terminal: rewriting the screen, or setting the clipboard.
    pub fn sanitize_strings(&mut self) {
        make_safe(&mut self.name);
        for w in &mut self.workspaces {
            make_safe(&mut w.name);
            if let Some(branch) = w.git.as_mut().and_then(|g| g.branch.as_mut()) {
                make_safe(branch);
            }
        }
        for p in &mut self.panes {
            make_safe(&mut p.title);
            make_safe(&mut p.agent_type);
            make_safe(&mut p.cwd);
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NodeState {
    /// The node's own reported id, from `session.snapshot`/`node.info`. Equal
    /// to the map key it is stored under, once a snapshot has arrived.
    pub node_id: String,
    /// Display name — hostname, from `node.json`'s `nodeName`.
    pub name: String,
    pub seq: u64,
    pub workspaces: Vec<WorkspaceInfo>,
    pub panes: Vec<PaneInfo>,
    /// This node's model has missed events and needs a fresh snapshot. Scoped
    /// to the node, so one peer's gap never invalidates another's state.
    pub stale: bool,
    /// The last pane focused in each of this node's spaces, keyed by space id.
    /// Presentation state; never written by snapshot or event.
    pub last_focused: HashMap<String, String>,
    /// This node's home directory, from `node.info` — where `pane.create`
    /// puts a pane given no `cwd`. A property of the machine, so it is per
    /// node and not fleet-wide.
    pub home_dir: String,
    pub conn: ConnState,
}

/// Maps a server-reported peer status onto a [`ConnState`].
///
/// `status` is a bare string on the wire rather than an enum, so that a server
/// that grows a new state renders as something honest on an older client
/// instead of failing to parse the frame carrying it. Anything unrecognised
/// therefore has to mean *something*, and `Offline` carrying the raw word is
/// the honest choice: the machine is not usable and the sidebar says why.
pub fn conn_from_status(status: &str, paired: bool) -> ConnState {
    if !paired {
        return ConnState::Unpaired;
    }
    match status {
        "online" => ConnState::Direct,
        // Paired, nothing wrong, simply not linked at this moment.
        "idle" | "unknown" => ConnState::Idle,
        "connecting" => ConnState::Connecting,
        "offline" => ConnState::Offline("offline".to_string()),
        other => ConnState::Offline(other.to_string()),
    }
}

impl NodeState {
    pub fn new(node_id: &str, name: &str) -> NodeState {
        NodeState {
            node_id: node_id.to_string(),
            name: name.to_string(),
            ..NodeState::default()
        }
    }

    /// Rolls up every **live** pane in `space_id` on this node into a single
    /// [`DisplayState`] using the loudest-wins priority order (see
    /// [`DisplayState::priority`]). A space with no live panes (including one
    /// that doesn't exist) rolls up to `Unknown`, matching `rollupState([])`
    /// in `src/lib/proto/agent-state.ts`.
    ///
    /// Lives on the node rather than on `Model` because a space id only
    /// identifies a space *within* one node — searching the whole fleet by id
    /// alone would match a same-named space on another machine.
    ///
    /// Dead panes are excluded. [`crate::model::Model::apply_pane_closed`]
    /// keeps a closed pane's record with its *last* `agent_state` and only
    /// flips `live` — and `session.snapshot` genuinely returns `live: false`
    /// panes still carrying one — so counting them left a space whose only
    /// pane exited while blocked showing a permanent "needs you" badge above
    /// an empty pane list, forever. Filtering here diverges from nothing on
    /// the Node side: `rollupState(panes)` in `agent-state.ts` has no concept
    /// of `live` at all, its *caller* picks which panes to pass.
    pub fn rollup(&self, space_id: &str) -> DisplayState {
        self.loudest(|p| p.workspace_id.as_deref() == Some(space_id))
    }

    /// The shared body of the two rollups: loudest live pane matching
    /// `scope`, or `Unknown` when none match.
    fn loudest(&self, scope: impl Fn(&PaneInfo) -> bool) -> DisplayState {
        self.panes
            .iter()
            .filter(|p| p.live && scope(p))
            .map(display_state)
            .max_by_key(|s| s.priority())
            .unwrap_or(DisplayState::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_node_is_connecting_and_empty() {
        let n = NodeState::new("abc-123", "gpu-box");
        assert_eq!(n.node_id, "abc-123");
        assert_eq!(n.name, "gpu-box");
        assert_eq!(n.seq, 0);
        assert!(n.panes.is_empty());
        assert!(n.workspaces.is_empty());
        assert!(!n.stale);
        assert_eq!(n.conn, ConnState::Connecting);
    }

    #[test]
    fn an_offline_state_keeps_its_reason() {
        // Offline carries its reason so the status bar can show it, while
        // the sidebar label stays a fixed width — see `label`.
        assert_ne!(
            ConnState::Offline("a".into()),
            ConnState::Offline("b".into())
        );
        assert_eq!(ConnState::default(), ConnState::Connecting);
    }

    #[test]
    fn node_ids_order_by_their_string() {
        let mut ids = [NodeId::new("b"), NodeId::new("a")];
        ids.sort();
        assert_eq!(ids[0], NodeId::new("a"));
    }
}
