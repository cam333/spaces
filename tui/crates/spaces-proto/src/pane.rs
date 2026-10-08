//! Pane data-channel client: speaks the per-pane WebSocket protocol
//! documented in `docs/protocol/README.md` (`/proto/pane/<id>`) — the
//! replay/live-output sequence and the `input`/`resize` up-frames.
//!
//! Structurally this mirrors [`crate::client::ControlClient`]: a single
//! reader task owns the socket's read half for the connection's lifetime
//! and fans every inbound frame out over an `mpsc` channel a caller drains
//! via [`PaneSocket::events`]. There is no request/response correlation
//! here (the pane channel has no `id`-tagged replies), so unlike
//! `ControlClient` there is no pending map — every inbound frame becomes
//! at most one [`PaneEvent`].

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::client::{dial_request, percent_encode_query_value, ProtoError};
use crate::{PaneDownFrame, PaneDownFrameErrorCode, PaneUpFrame};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Bound on the pane event channel. Mirrors `EVENT_CHANNEL_CAPACITY` in
/// `client.rs`: a slow consumer applies backpressure to the reader task
/// (a full channel makes `events_tx.send` wait) rather than frames being
/// silently dropped.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// How to attach to a pane's data socket
/// (`docs/protocol/README.md#pane-data-channel-protopane-id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMode {
    /// Never claims the controller slot; any up-frame sent while attached
    /// this way is rejected by the server with `bad_request`.
    Observe,
    /// Attempts to claim the pane's single controller slot. Without
    /// `takeover`, a second simultaneous control attach is refused with a
    /// `conflict` error frame and the socket is closed; with `takeover:
    /// true`, it instead demotes the previous controller.
    Control { takeover: bool },
}

/// Events delivered over [`PaneSocket::events`]: the attach/replay
/// sequence, then live output, decoded from the wire's binary frames and
/// [`PaneDownFrame`] text frames (`docs/protocol/README.md#pane-data-channel-protopane-id`).
#[derive(Debug, Clone, PartialEq)]
pub enum PaneEvent {
    /// `{"type":"replay-start"}` — the next `Bytes` event is the pane's
    /// serialized screen state, not live output.
    ReplayStart,
    /// A binary frame: either the replay screen state (immediately after
    /// `ReplayStart`) or a live PTY chunk (after `ReplayEnd`).
    Bytes(Vec<u8>),
    /// `{"type":"replay-end"}` — replay finished; every following `Bytes`
    /// is live PTY output as it's produced.
    ReplayEnd,
    /// The pane's process exited while attached; the server closes the
    /// socket immediately after sending this.
    Exit { exit_code: i64 },
    /// A protocol-level error frame, e.g. `conflict` when a control attach
    /// without `takeover` loses the race for the controller slot. `code`
    /// is one of the wire's `ERROR_CODES` (snake_case, e.g. `"not_found"`).
    Error { code: String, message: String },
}

/// A connected pane data-socket client. Owns the socket's write half; the
/// read half is owned by a spawned reader task — see the module doc
/// comment for the fan-out design, which mirrors `ControlClient`.
#[derive(Debug)]
pub struct PaneSocket {
    write: SplitSink<WsStream, Message>,
    reader: JoinHandle<()>,
    events_rx: Option<mpsc::Receiver<PaneEvent>>,
}

impl Drop for PaneSocket {
    /// Aborts the reader task, which drops its half of the split socket.
    /// Combined with `write` dropping right after (as an ordinary field,
    /// once this method returns), both halves go away and the underlying
    /// connection actually closes instead of leaking a socket and a task
    /// for the lifetime of the process — see `ControlClient`'s identical
    /// `Drop` impl and the leak that motivated it.
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl PaneSocket {
    /// Opens the pane data socket at
    /// `<base_url>/proto/pane/<urlencoded pane_id>?mode=...` (see
    /// [`build_url`]) and starts the reader task. There is no handshake to
    /// await here (unlike `ControlClient::connect`) — the server either
    /// accepts the attach and starts streaming the replay sequence, or
    /// refuses it with an error frame that arrives as a [`PaneEvent::Error`]
    /// once a caller drains [`PaneSocket::events`].
    pub async fn attach(
        base_url: &str,
        pane_id: &str,
        mode: PaneMode,
        token: Option<&str>,
    ) -> Result<Self, ProtoError> {
        Self::attach_on(base_url, pane_id, mode, token, None).await
    }

    /// Attaches to a pane that lives on `node`, or locally when it is `None`.
    ///
    /// The only difference on the wire is a `nodeId` parameter; the local
    /// server relays the socket to the machine that owns the pane, byte for
    /// byte. Everything downstream of here — replay, scrollback, resize,
    /// error codes — is identical either way, which is the point: a remote
    /// pane is not a second kind of pane.
    pub async fn attach_on(
        base_url: &str,
        pane_id: &str,
        mode: PaneMode,
        token: Option<&str>,
        node: Option<&str>,
    ) -> Result<Self, ProtoError> {
        // The token travels in a header (see `dial_request`), so the URL
        // named in an error message never carries it.
        let url = build_url(base_url, pane_id, mode, node);
        let request = dial_request(&url, token)?;

        let (stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| ProtoError::Transport(format!("failed to connect to {url}: {e}")))?;

        let (write, read) = stream.split();
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let reader = tokio::spawn(read_loop(read, events_tx));

        Ok(Self {
            write,
            reader,
            events_rx: Some(events_rx),
        })
    }

    /// The event stream: the attach/replay sequence followed by live
    /// output (see [`PaneEvent`]). The underlying channel can only be
    /// handed out once — a second call returns a receiver whose sender was
    /// dropped immediately, so it reports the channel closed (`recv()`
    /// resolves to `None`) right away rather than panicking or silently
    /// reattaching a second listener.
    pub fn events(&mut self) -> mpsc::Receiver<PaneEvent> {
        match self.events_rx.take() {
            Some(rx) => rx,
            None => {
                let (_tx, rx) = mpsc::channel(1);
                rx
            }
        }
    }

    /// Sends an `{"t":"i","data":...}` up-frame: raw bytes to write to the
    /// PTY's stdin. Only accepted by the server from the current
    /// controller — sending this while attached with [`PaneMode::Observe`]
    /// (or after being demoted by a takeover) gets a `bad_request` error
    /// frame back over [`PaneSocket::events`], not a `Result::Err` here.
    pub async fn send_input(&mut self, data: &str) -> Result<(), ProtoError> {
        self.send_frame(&PaneUpFrame::Input {
            data: data.to_string(),
        })
        .await
    }

    /// Sends an `{"t":"r","cols":...,"rows":...}` up-frame to resize the
    /// PTY. The wire format requires positive integers
    /// ([`PaneUpFrame::Resize`] is typed `NonZeroU32`), so a `0` argument
    /// is clamped up to `1` — the smallest dimension the protocol allows —
    /// rather than this function panicking or failing on an otherwise
    /// harmless input.
    pub async fn send_resize(&mut self, cols: u32, rows: u32) -> Result<(), ProtoError> {
        let cols = std::num::NonZeroU32::new(cols).unwrap_or(std::num::NonZeroU32::MIN);
        let rows = std::num::NonZeroU32::new(rows).unwrap_or(std::num::NonZeroU32::MIN);
        self.send_frame(&PaneUpFrame::Resize { cols, rows }).await
    }

    async fn send_frame(&mut self, frame: &PaneUpFrame) -> Result<(), ProtoError> {
        let text = serde_json::to_string(frame)
            .map_err(|e| ProtoError::Protocol(format!("failed to encode pane up-frame: {e}")))?;
        self.write
            .send(Message::Text(text))
            .await
            .map_err(|e| ProtoError::Transport(format!("failed to send pane up-frame: {e}")))
    }
}

/// Builds the **token-free** pane data-socket URL:
/// `<ws base>/proto/pane/<urlencoded id>?mode=observe|control[&takeover=1]`
/// (`docs/protocol/README.md:203-217`). The pane id is percent-encoded —
/// today it's always a UUID, but the protocol permits any string, and
/// nothing here should silently corrupt the path for an id containing
/// reserved characters.
///
/// The token is spliced on separately by [`with_token`] rather than here,
/// so [`PaneSocket::attach`] has a URL it can safely put in an error
/// message — see the comment at its call site.
fn build_url(base_url: &str, pane_id: &str, mode: PaneMode, node: Option<&str>) -> String {
    let mode_str = match mode {
        PaneMode::Observe => "observe",
        PaneMode::Control { .. } => "control",
    };

    let mut url = format!(
        "{base_url}/proto/pane/{}?mode={mode_str}",
        percent_encode_query_value(pane_id)
    );

    if let PaneMode::Control { takeover: true } = mode {
        url.push_str("&takeover=1");
    }

    // Present only for a pane on another machine, so a single-machine session
    // puts exactly the bytes on the wire it always did.
    if let Some(node) = node {
        url.push_str("&nodeId=");
        url.push_str(&percent_encode_query_value(node));
    }

    url
}

/// Owns the socket's read half for the connection's lifetime. Binary
/// frames become [`PaneEvent::Bytes`]; text frames are parsed as
/// [`PaneDownFrame`] and mapped via [`map_down_frame`]. Anything else —
/// unparseable JSON, a `type` the server's contract doesn't define, a
/// close/ping/pong frame, a websocket error — is dropped without
/// panicking, exactly like `ControlClient`'s read loop: a malformed frame
/// must never kill this task.
async fn read_loop(mut read: SplitStream<WsStream>, events_tx: mpsc::Sender<PaneEvent>) {
    while let Some(next) = read.next().await {
        let Ok(msg) = next else {
            break;
        };

        let event = match msg {
            Message::Binary(data) => PaneEvent::Bytes(data),
            Message::Text(text) => match serde_json::from_str::<PaneDownFrame>(&text) {
                Ok(frame) => map_down_frame(frame),
                Err(_) => continue,
            },
            _ => continue,
        };

        if events_tx.send(event).await.is_err() {
            // No one is listening (the receiver was dropped) — nothing
            // left to do but stop reading.
            break;
        }
    }
}

/// Maps a decoded [`PaneDownFrame`] to its [`PaneEvent`] counterpart.
fn map_down_frame(frame: PaneDownFrame) -> PaneEvent {
    match frame {
        PaneDownFrame::ReplayStart => PaneEvent::ReplayStart,
        PaneDownFrame::ReplayEnd => PaneEvent::ReplayEnd,
        PaneDownFrame::Exit { exit_code } => PaneEvent::Exit { exit_code },
        PaneDownFrame::Error { code, message } => PaneEvent::Error {
            code: error_code_str(code).to_string(),
            message,
        },
    }
}

/// The wire's snake_case spelling for each `ERROR_CODES` member
/// (`bin/lib/runtime/protocol.js`), mirroring the `#[serde(rename_all =
/// "snake_case")]` on [`PaneDownFrameErrorCode`] itself.
fn error_code_str(code: PaneDownFrameErrorCode) -> &'static str {
    match code {
        PaneDownFrameErrorCode::BadRequest => "bad_request",
        PaneDownFrameErrorCode::Unauthorized => "unauthorized",
        PaneDownFrameErrorCode::NotFound => "not_found",
        PaneDownFrameErrorCode::Conflict => "conflict",
        PaneDownFrameErrorCode::VersionMismatch => "version_mismatch",
        PaneDownFrameErrorCode::Gap => "gap",
        PaneDownFrameErrorCode::Internal => "internal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- `attach`'s error message used to interpolate the *dialed* URL,
    // which carries `&terminalToken=…` — the exact inverse of the deliberate
    // choice in `ControlClient::connect` — and pane errors are now routed
    // onto the status bar. ---

    #[test]
    fn the_url_named_in_errors_carries_no_token() {
        let url = build_url(
            "ws://h:1",
            "pane-1",
            PaneMode::Control { takeover: true },
            None,
        );
        assert_eq!(url, "ws://h:1/proto/pane/pane-1?mode=control&takeover=1");
        assert!(!url.contains("terminalToken"));
        assert!(!url.contains("s3cret"));
    }

    #[test]
    fn the_token_goes_in_a_header_not_the_url() {
        let url = build_url("ws://h:1", "pane-1", PaneMode::Observe, None);
        let request = dial_request(&url, Some("s3cret")).unwrap();
        assert_eq!(
            request.uri().to_string(),
            "ws://h:1/proto/pane/pane-1?mode=observe"
        );
        assert_eq!(request.headers()["authorization"], "Bearer s3cret");
    }

    #[test]
    fn a_remote_pane_names_the_machine_that_owns_it() {
        let url = build_url("ws://h:1", "p1", PaneMode::Observe, Some("gpu-uuid"));
        assert!(url.contains("nodeId=gpu-uuid"), "{url}");
    }

    #[test]
    fn a_local_pane_url_is_byte_for_byte_what_it_always_was() {
        // A single-machine session must put nothing new on the wire.
        let local = build_url("ws://h:1", "p1", PaneMode::Observe, None);
        assert_eq!(local, "ws://h:1/proto/pane/p1?mode=observe");
        assert!(!local.contains("nodeId"));
    }

    #[test]
    fn a_machine_id_is_encoded_like_any_other_parameter() {
        let url = build_url("ws://h:1", "p1", PaneMode::Observe, Some("a b&c=d"));
        assert!(!url.contains("a b&c=d"), "{url}");
        assert!(url.contains("nodeId=a%20b%26c%3Dd"), "{url}");
    }

    #[test]
    fn a_pane_id_with_reserved_characters_is_percent_encoded_in_the_path() {
        let url = build_url("ws://h:1", "a/b?c", PaneMode::Observe, None);
        assert_eq!(url, "ws://h:1/proto/pane/a%2Fb%3Fc?mode=observe");
    }
}
