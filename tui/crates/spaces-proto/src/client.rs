//! Control-channel client: speaks the `/proto` WebSocket handshake,
//! request/response, and event-push protocol documented in
//! `docs/protocol/README.md`.
//!
//! A single reader task owns the socket's read half for the lifetime of the
//! connection. It fans inbound frames out to whichever side is waiting:
//! replies (frames with an `id`) resolve a `oneshot` channel stashed in a
//! shared map keyed by request id; live pushes (frames with an `event` key)
//! go to an `mpsc` sender a caller can drain via [`ControlClient::events`].
//! Anything that is neither — malformed JSON, an unrecognized frame shape,
//! a non-text WebSocket frame — is dropped silently. A UI thread hands its
//! requests to `ControlClient` and must never be at the mercy of a server
//! that stops answering, so both the handshake and every request are
//! wrapped in a timeout rather than waiting forever.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::PROTOCOL_VERSION;

/// How long [`ControlClient::connect`] will wait for `hello-ok` (or a hello
/// error) before giving up. The handshake is a single round trip on an
/// already-open socket, so 10s is generous for a hung/unreachable runtime
/// without making a UI thread wait indefinitely.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default timeout for [`ControlClient::request`]. Chosen to comfortably
/// exceed the runtime's own longest synchronous wait method
/// (`agent.wait`'s `timeoutMs` clamps at 300s, but that one is awaited
/// server-side and replies as soon as it settles or errors — this timeout
/// exists to catch a server that stops answering entirely).
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the live-event channel. A slow consumer applies backpressure to
/// the reader task rather than events being dropped.
const EVENT_CHANNEL_CAPACITY: usize = 256;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type PendingMap = HashMap<String, oneshot::Sender<Result<Value, ProtoError>>>;

/// The inner event payload pushed on the control channel — both as an
/// element of an `events.subscribe` backlog and as a live `{"event": ...}`
/// push frame (`docs/protocol/README.md#events`): `{ seq, type, payload,
/// ts }`.
///
/// This is hand-written rather than reusing the build-generated
/// `EventFrameEvent` (from `spaces-proto`'s schema codegen — see
/// `build.rs`/`lib.rs`): that type names the tag field `type_` (`type` is a
/// reserved word, and typify renames rather than using a raw identifier),
/// which makes `ev.r#type` — the ergonomic accessor this client and later
/// phases (the client-side model, the event loop) want — unavailable, and
/// it types `seq` as `NonZeroU64` rather than a plain `u64`. Re-exported as
/// `spaces_proto::client::EventFrameInner` and `spaces_proto::EventFrameInner`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventFrameInner {
    pub seq: u64,
    #[serde(rename = "type")]
    pub r#type: String,
    pub payload: Value,
    pub ts: f64,
}

/// Result of a successful `events.subscribe` call.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscribeAck {
    /// `true` when the requested backlog (`afterSeq`) predates the
    /// server's event ring buffer — the caller must discard incremental
    /// state and re-fetch `session.snapshot` rather than apply `events` as
    /// deltas. See `docs/protocol/README.md#events`.
    pub gap: bool,
    /// The buffered backlog, in order, with `seq > afterSeq`. Entries that
    /// fail to deserialize as [`EventFrameInner`] are skipped rather than
    /// failing the whole call — see [`SubscribeAck::dropped_malformed_events`].
    pub events: Vec<EventFrameInner>,
    /// How many entries in the server's `events` array failed to
    /// deserialize and were skipped. Should always be `0` against a
    /// well-behaved server; a nonzero count means `events` is shorter than
    /// what the server actually sent, which matters to any seq/gap
    /// bookkeeping built on top of this call.
    pub dropped_malformed_events: usize,
}

/// Errors a [`ControlClient`] can produce. Every message is written to be
/// actionable on its own, since it may end up directly in front of a user
/// (e.g. in a status line) rather than only in a log.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    /// The WebSocket connection itself failed, timed out, or closed —
    /// nothing protocol-specific went wrong, there was simply no one (or
    /// no longer anyone) to talk to.
    #[error("{0}")]
    Transport(String),
    /// The server's `hello-ok` reply reported a `protocolVersion` this
    /// client doesn't speak.
    #[error(
        "runtime speaks protocol {runtime}, this client speaks {}, run: spaces update",
        PROTOCOL_VERSION
    )]
    VersionMismatch { runtime: u32 },
    /// A `Request` came back with an `error` frame — the server understood
    /// and rejected it. `code` is one of the protocol's `ERROR_CODES`.
    #[error("{code}: {message}")]
    Remote { code: String, message: String },
    /// The server said something that doesn't fit the protocol shape this
    /// client expects (outside of a `Remote` error) — e.g. an unparseable
    /// handshake reply.
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// A connected control-channel client. Owns the socket's write half; the
/// read half is owned by a spawned reader task — see the module doc
/// comment for the fan-out design.
///
/// `write` (this struct) and `read` (the reader task) are the two halves
/// of one [`futures_util::StreamExt::split`] pair, which share the
/// underlying socket via a lock the `split` combinator keeps alive as long
/// as *either* half exists — dropping only `write` does not close
/// anything. `reader` exists so [`Drop`] can abort the reader task,
/// dropping its half too, so the socket actually closes and the task
/// doesn't leak.
#[derive(Debug)]
pub struct ControlClient {
    write: SplitSink<WsStream, Message>,
    reader: JoinHandle<()>,
    pending: Arc<Mutex<PendingMap>>,
    events_rx: Option<mpsc::Receiver<EventFrameInner>>,
    next_id: u64,
    last_seq: Arc<AtomicU64>,
}

impl Drop for ControlClient {
    /// Aborts the reader task, which drops its half of the split socket.
    /// Combined with `write` dropping right after (as an ordinary field,
    /// once this method returns), both halves go away and the underlying
    /// connection actually closes instead of leaking a socket and a task
    /// for the lifetime of the process.
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl ControlClient {
    /// Opens a WebSocket to `url` (the `/proto` control-channel endpoint),
    /// sends `hello`, and awaits `hello-ok`. `token`, when present, is sent
    /// as `Authorization: Bearer <token>` — see [`dial_request`].
    ///
    /// The handshake read is wrapped in a 10s timeout: a UI thread must
    /// never hang forever waiting on a server that never replies.
    pub async fn connect(url: &str, token: Option<&str>) -> Result<Self, ProtoError> {
        let request = dial_request(url, token)?;

        let (stream, _response) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| {
                ProtoError::Transport(format!(
                    "failed to connect to {url}: {e}{}",
                    connect_failure_hint(&e)
                ))
            })?;

        let (mut write, mut read) = stream.split();

        let hello = json!({
            "type": "hello",
            "protocolVersion": PROTOCOL_VERSION,
            "client": concat!("spaces-tui/", env!("CARGO_PKG_VERSION")),
        });
        write
            .send(Message::Text(hello.to_string()))
            .await
            .map_err(|e| ProtoError::Transport(format!("failed to send hello: {e}")))?;

        let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next())
            .await
            .map_err(|_| {
                ProtoError::Transport(
                    "timed out waiting for the server's hello-ok (10s)".to_string(),
                )
            })?
            .ok_or_else(|| ProtoError::Transport("connection closed before hello-ok".to_string()))?
            .map_err(|e| ProtoError::Transport(format!("websocket error during handshake: {e}")))?;

        let seq = parse_hello_reply(&reply)?;

        let pending: Arc<Mutex<PendingMap>> = Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let last_seq = Arc::new(AtomicU64::new(seq));

        let reader = tokio::spawn(read_loop(
            read,
            pending.clone(),
            events_tx,
            last_seq.clone(),
        ));

        Ok(Self {
            write,
            reader,
            pending,
            events_rx: Some(events_rx),
            next_id: 0,
            last_seq,
        })
    }

    /// Calls `method` with `params`, correlating the reply by a
    /// monotonically increasing request id. Times out after 30s so a
    /// silent server never hangs the caller forever.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, ProtoError> {
        self.request_with_timeout(method, params, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    async fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ProtoError> {
        self.next_id += 1;
        let id = format!("r{}", self.next_id);

        let (tx, rx) = oneshot::channel();
        lock_pending(&self.pending).insert(id.clone(), tx);

        let frame = json!({ "id": id, "method": method, "params": params });
        if let Err(e) = self.write.send(Message::Text(frame.to_string())).await {
            lock_pending(&self.pending).remove(&id);
            return Err(ProtoError::Transport(format!(
                "failed to send '{method}': {e}"
            )));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                lock_pending(&self.pending).remove(&id);
                Err(ProtoError::Transport(format!(
                    "connection closed while waiting for a reply to '{method}'"
                )))
            }
            Err(_) => {
                lock_pending(&self.pending).remove(&id);
                Err(ProtoError::Transport(format!(
                    "'{method}' timed out after {}s",
                    timeout.as_secs()
                )))
            }
        }
    }

    /// Calls `events.subscribe`, returning the backlog after `after_seq`
    /// and whether it's contiguous (see [`SubscribeAck::gap`]). On
    /// success, the same socket starts pushing every future event — drain
    /// them with [`ControlClient::events`].
    pub async fn subscribe(&mut self, after_seq: u64) -> Result<SubscribeAck, ProtoError> {
        let result = self
            .request("events.subscribe", json!({ "afterSeq": after_seq }))
            .await?;

        let gap = result.get("gap").and_then(Value::as_bool).unwrap_or(false);
        let raw_events = result
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut events = Vec::with_capacity(raw_events.len());
        let mut dropped_malformed_events = 0usize;
        for raw in raw_events {
            match serde_json::from_value::<EventFrameInner>(raw) {
                Ok(event) => events.push(event),
                Err(_) => dropped_malformed_events += 1,
            }
        }

        if let Some(max_seq) = events.iter().map(|e| e.seq).max() {
            self.last_seq.fetch_max(max_seq, Ordering::SeqCst);
        }

        Ok(SubscribeAck {
            gap,
            events,
            dropped_malformed_events,
        })
    }

    /// The live push stream. Only meaningful after a successful
    /// [`ControlClient::subscribe`] — before that, the server never sends
    /// `{"event": ...}` frames, so the channel simply stays empty. Calling
    /// this more than once returns an already-closed receiver on every
    /// call after the first, since the underlying channel can only be
    /// handed out once.
    pub fn events(&mut self) -> mpsc::Receiver<EventFrameInner> {
        match self.events_rx.take() {
            Some(rx) => rx,
            None => {
                let (_tx, rx) = mpsc::channel(1);
                rx
            }
        }
    }

    /// The highest event sequence number observed so far: from `hello-ok`
    /// at connect time, and thereafter from `events.subscribe`'s backlog
    /// and any live event pushes.
    pub fn last_seq(&self) -> u64 {
        self.last_seq.load(Ordering::SeqCst)
    }
}

/// Appended to a handshake-time connection failure that looks like the
/// peer hung up without saying anything. The runtime refuses `/proto`
/// under `SPACES_MULTIUSER=1` by calling `socket.destroy()` inside its
/// `upgrade` handler (`bin/terminal-server.js`) — zero bytes written, no
/// HTTP status, no close frame — which reaches this client as a bare TCP
/// reset, indistinguishable from "nothing is listening there". That is a
/// documented standing restriction (`docs/protocol/README.md`), not a
/// transient fault, so naming it here is the difference between a user
/// restarting a server that was never the problem and one who knows what
/// to change. Deliberately phrased as one possibility among the others
/// rather than a diagnosis: this client cannot tell the two apart.
const RESET_HINT: &str = " — the server may not be running, or may be running with SPACES_MULTIUSER=1, which refuses /proto connections without a reply";

/// [`RESET_HINT`] for the failure shapes a mid-upgrade `socket.destroy()`
/// produces, and nothing for any other error (a DNS failure, a refused
/// connection, a TLS problem — all of which already say what they are).
/// `UnexpectedEof` is included alongside the reset kinds because a peer
/// that closes cleanly instead of resetting lands there.
fn connect_failure_hint(e: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    let is_abrupt_close = match e {
        tokio_tungstenite::tungstenite::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::UnexpectedEof
        ),
        tokio_tungstenite::tungstenite::Error::Protocol(_) => true,
        _ => false,
    };
    if is_abrupt_close {
        RESET_HINT
    } else {
        ""
    }
}

/// The WebSocket handshake request for `url`, carrying `token` as
/// `Authorization: Bearer <token>` when there is one.
///
/// A header, never the query string: a URL ends up in logs, process lists
/// and error messages, and a server in desktop mode accepts its owner's
/// credentials only from headers. The error never contains the token.
pub(crate) fn dial_request(
    url: &str,
    token: Option<&str>,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, ProtoError> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};

    let mut request = url
        .into_client_request()
        .map_err(|e| ProtoError::Transport(format!("invalid url {url}: {e}")))?;
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        let value = HeaderValue::from_str(&format!("Bearer {t}")).map_err(|_| {
            ProtoError::Transport("the token contains characters a header cannot carry".into())
        })?;
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    Ok(request)
}

/// Percent-encodes `s` for safe inclusion as a URL query-string value
/// (RFC 3986 unreserved set: `ALPHA / DIGIT / "-" / "." / "_" / "~"`;
/// everything else — including `&`, `=`, `#`, and `%` itself — is escaped).
/// Hand-rolled rather than pulling in a URL-encoding crate purely for one
/// query value. `pub(crate)` so `pane.rs` can reuse it for both the pane id
/// path segment and its own `terminalToken` query value rather than
/// duplicating the same escaping logic.
pub(crate) fn percent_encode_query_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Interprets the first frame received after sending `hello`, per the five
/// outcomes documented in `docs/protocol/README.md#handshake-control-channel-only`.
fn parse_hello_reply(msg: &Message) -> Result<u64, ProtoError> {
    let text = match msg {
        Message::Text(t) => t,
        other => {
            return Err(ProtoError::Protocol(format!(
                "expected a text frame for hello-ok, got {other:?}"
            )))
        }
    };

    let value: Value = serde_json::from_str(text)
        .map_err(|e| ProtoError::Protocol(format!("hello-ok frame was not valid json: {e}")))?;

    if value.get("type").and_then(Value::as_str) == Some("hello-ok") {
        return Ok(value.get("seq").and_then(Value::as_u64).unwrap_or(0));
    }

    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_str).unwrap_or("");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if code == "version_mismatch" {
            let runtime = trailing_version(&message).unwrap_or(0);
            return Err(ProtoError::VersionMismatch { runtime });
        }
        return Err(ProtoError::Protocol(format!("handshake failed: {message}")));
    }

    Err(ProtoError::Protocol(format!(
        "unexpected handshake reply: {value}"
    )))
}

/// Best-effort extraction of the trailing integer in a message like
/// `"runtime speaks protocol 2"`. The server's exact wording isn't part of
/// the documented contract (only the `version_mismatch` code and the
/// `runtime` number are), so this degrades to `None` rather than panicking
/// on a reworded message.
fn trailing_version(message: &str) -> Option<u32> {
    message
        .split_whitespace()
        .last()
        .and_then(|tok| tok.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
}

fn lock_pending(pending: &Arc<Mutex<PendingMap>>) -> MutexGuard<'_, PendingMap> {
    pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Owns the socket's read half for the connection's lifetime. Every frame
/// is one of: a reply (has `id`, resolves a pending `oneshot`), a live
/// event push (has `event`), or something else — malformed JSON, a
/// recognized-but-unmatched shape, a non-text WebSocket frame — which is
/// dropped without panicking. See the module doc comment.
///
/// `pending` is one of two live clones of the same `Arc<Mutex<..>>` —
/// `ControlClient` holds the other — so this task exiting does not, on its
/// own, drop anything and does not wake any outstanding `request()` call.
/// Once the loop below ends (peer closed the connection, or a websocket
/// error), every sender still in `pending` is drained and resolved with a
/// `Transport` error so in-flight requests fail promptly instead of idling
/// out their full timeout.
async fn read_loop(
    mut read: SplitStream<WsStream>,
    pending: Arc<Mutex<PendingMap>>,
    events_tx: mpsc::Sender<EventFrameInner>,
    last_seq: Arc<AtomicU64>,
) {
    while let Some(next) = read.next().await {
        let Ok(msg) = next else {
            // Connection broken; stop reading. Outstanding requests are
            // resolved below, once the loop exits.
            break;
        };
        let Message::Text(text) = msg else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            continue;
        };

        if let Some(event_value) = value.get("event") {
            if let Ok(inner) = serde_json::from_value::<EventFrameInner>(event_value.clone()) {
                last_seq.fetch_max(inner.seq, Ordering::SeqCst);
                let _ = events_tx.send(inner).await;
            }
            continue;
        }

        if let Some(id) = value.get("id").and_then(Value::as_str) {
            let sender = lock_pending(&pending).remove(id);
            if let Some(sender) = sender {
                let result = if let Some(error) = value.get("error") {
                    let code = error
                        .get("code")
                        .and_then(Value::as_str)
                        .unwrap_or("internal")
                        .to_string();
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    Err(ProtoError::Remote { code, message })
                } else {
                    Ok(value.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = sender.send(result);
            }
        }
    }

    // The loop above ended: the peer closed the connection or a websocket
    // error occurred. Fail every request still waiting on a reply now,
    // rather than letting each one idle out its own timeout — see the
    // doc comment above.
    let stranded: Vec<_> = lock_pending(&pending).drain().collect();
    for (_, sender) in stranded {
        let _ = sender.send(Err(ProtoError::Transport(
            "connection closed before a reply arrived".to_string(),
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_of(url: &str, token: Option<&str>) -> Option<String> {
        dial_request(url, token)
            .unwrap()
            .headers()
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_string())
    }

    #[test]
    fn token_is_omitted_when_absent_or_empty() {
        assert_eq!(auth_of("ws://h/proto", None), None);
        assert_eq!(auth_of("ws://h/proto", Some("")), None);
    }

    #[test]
    fn the_token_travels_as_a_bearer_header_and_never_in_the_url() {
        let request = dial_request("ws://h/proto?x=1", Some("s3cret.tok")).unwrap();
        assert_eq!(
            auth_of("ws://h/proto", Some("s3cret.tok")).as_deref(),
            Some("Bearer s3cret.tok")
        );
        assert_eq!(request.uri().to_string(), "ws://h/proto?x=1");
    }

    #[test]
    fn a_token_a_header_cannot_carry_is_an_error_that_does_not_echo_it() {
        let err = dial_request("ws://h/proto", Some("bad\ntoken")).unwrap_err();
        assert!(!format!("{err:?}").contains("bad"), "{err:?}");
    }

    // --- connect_failure_hint (the runtime's `SPACES_MULTIUSER=1` refusal is a
    // bare `socket.destroy()` mid-upgrade — no status, no close frame — so the
    // client sees a raw
    // reset that reads exactly like "nothing is listening"). ---

    fn io_error(kind: std::io::ErrorKind) -> tokio_tungstenite::tungstenite::Error {
        tokio_tungstenite::tungstenite::Error::Io(std::io::Error::new(kind, "test"))
    }

    #[test]
    fn an_abrupt_close_at_handshake_time_names_multiuser_as_a_possible_cause() {
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::UnexpectedEof,
        ] {
            let hint = connect_failure_hint(&io_error(kind));
            assert!(hint.contains("SPACES_MULTIUSER=1"), "{kind:?} -> {hint:?}");
            assert!(
                hint.contains("may not be running"),
                "the hint must stay one possibility among others: {hint:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_connection_failure_gets_no_extra_hint() {
        // "connection refused" and "host not found" already say exactly
        // what went wrong; bolting a multiuser guess onto them would be
        // noise, not help.
        for kind in [
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::TimedOut,
        ] {
            assert_eq!(connect_failure_hint(&io_error(kind)), "", "{kind:?}");
        }
    }
}
