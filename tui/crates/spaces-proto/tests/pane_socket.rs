use futures_util::{SinkExt, StreamExt};
use spaces_proto::pane::{PaneEvent, PaneMode, PaneSocket};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Mock pane socket: records the request path, then plays a fixed script.
///
/// `accept_hdr_async`'s callback signature is fixed by `tokio-tungstenite`
/// (`Result<Response, ErrorResponse>`, where `ErrorResponse` is a full HTTP
/// response type) — the large-`Err` lint below is inherent to that API, not
/// anything this closure does, so it's suppressed rather than restructured.
#[allow(clippy::result_large_err)]
async fn mock_pane(
    script: Vec<Message>,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let path = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let p2 = path.clone();
        let mut ws = tokio_tungstenite::accept_hdr_async(
            stream,
            |req: &tokio_tungstenite::tungstenite::handshake::server::Request, res| {
                let auth = req
                    .headers()
                    .get("authorization")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default();
                *p2.lock().unwrap() = format!("{} auth={auth}", req.uri());
                Ok(res)
            },
        )
        .await
        .unwrap();
        seen2.lock().unwrap().push(path.lock().unwrap().clone());
        for m in script {
            ws.send(m).await.unwrap();
        }
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(t) = msg {
                seen2.lock().unwrap().push(t.to_string());
            }
        }
    });
    (format!("ws://{addr}"), seen)
}

#[tokio::test]
async fn attach_builds_the_documented_url() {
    let (base, seen) = mock_pane(vec![]).await;
    let _s = PaneSocket::attach(
        &base,
        "pane 1",
        PaneMode::Control { takeover: true },
        Some("tok"),
    )
    .await
    .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let path = seen.lock().unwrap()[0].clone();
    assert!(path.starts_with("/proto/pane/pane%201?"), "got {path}");
    assert!(path.contains("mode=control"));
    assert!(path.contains("takeover=1"));
    assert!(!path.contains("terminalToken"), "never in the url: {path}");
    assert!(path.ends_with("auth=Bearer tok"), "got {path}");
}

#[tokio::test]
async fn observe_mode_omits_takeover() {
    let (base, seen) = mock_pane(vec![]).await;
    let _s = PaneSocket::attach(&base, "p1", PaneMode::Observe, None)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let path = seen.lock().unwrap()[0].clone();
    assert!(path.contains("mode=observe"), "got {path}");
    assert!(!path.contains("takeover"), "got {path}");
    assert!(!path.contains("terminalToken"), "got {path}");
    assert!(path.ends_with("auth="), "no token, no header: {path}");
}

#[tokio::test]
async fn the_replay_sequence_arrives_in_order() {
    let (base, _) = mock_pane(vec![
        Message::Text(r#"{"type":"replay-start"}"#.into()),
        Message::Binary(b"restored screen".to_vec()),
        Message::Text(r#"{"type":"replay-end"}"#.into()),
        Message::Binary(b"live output".to_vec()),
    ])
    .await;
    let mut s = PaneSocket::attach(&base, "p1", PaneMode::Observe, None)
        .await
        .unwrap();
    let mut rx = s.events();
    let mut got = Vec::new();
    for _ in 0..4 {
        got.push(
            tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(matches!(got[0], PaneEvent::ReplayStart));
    assert!(matches!(&got[1], PaneEvent::Bytes(b) if b == b"restored screen"));
    assert!(matches!(got[2], PaneEvent::ReplayEnd));
    assert!(matches!(&got[3], PaneEvent::Bytes(b) if b == b"live output"));
}

#[tokio::test]
async fn exit_and_error_frames_are_typed() {
    let (base, _) = mock_pane(vec![
        Message::Text(
            r#"{"type":"error","code":"conflict","message":"pane already controlled"}"#.into(),
        ),
        Message::Text(r#"{"type":"exit","exitCode":3}"#.into()),
    ])
    .await;
    let mut s = PaneSocket::attach(&base, "p1", PaneMode::Observe, None)
        .await
        .unwrap();
    let mut rx = s.events();
    let a = rx.recv().await.unwrap();
    let b = rx.recv().await.unwrap();
    assert!(matches!(&a, PaneEvent::Error { code, .. } if code == "conflict"));
    assert!(matches!(b, PaneEvent::Exit { exit_code: 3 }));
}

#[tokio::test]
async fn input_and_resize_send_the_documented_frames() {
    let (base, seen) = mock_pane(vec![]).await;
    let mut s = PaneSocket::attach(&base, "p1", PaneMode::Control { takeover: false }, None)
        .await
        .unwrap();
    s.send_input("ls\r").await.unwrap();
    s.send_resize(120, 40).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let frames = seen.lock().unwrap().clone();
    assert!(
        frames
            .iter()
            .any(|f| f.contains(r#""t":"i""#) && f.contains("ls")),
        "{frames:?}"
    );
    assert!(
        frames
            .iter()
            .any(|f| f.contains(r#""t":"r""#) && f.contains("120")),
        "{frames:?}"
    );
}

#[tokio::test]
async fn an_unknown_text_frame_is_ignored_not_fatal() {
    let (base, _) = mock_pane(vec![
        Message::Text(r#"{"type":"who-knows"}"#.into()),
        Message::Text(r#"{"type":"replay-start"}"#.into()),
    ])
    .await;
    let mut s = PaneSocket::attach(&base, "p1", PaneMode::Observe, None)
        .await
        .unwrap();
    let mut rx = s.events();
    let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(ev, PaneEvent::ReplayStart),
        "unknown frames must be skipped"
    );
}

/// Regression test for a socket leak found by connecting, dropping, and
/// checking `netstat`: `PaneSocket` has its own `Drop` impl and field layout
/// independent of `ControlClient`'s, so `control_channel.rs`'s equivalent test
/// does not guard this type. Mirrors
/// `dropping_the_client_closes_the_connection_and_reader_task` in that file --
/// the mock server only signals once it observes the client side of the socket
/// actually close (EOF/read error), not on a timer.
#[tokio::test]
async fn dropping_the_pane_socket_closes_the_connection_and_reader_task() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(msg) = ws.next().await {
            if msg.is_err() {
                break;
            }
        }
        // The loop above only ends once the client's side of the socket has
        // actually closed (EOF or a read error) -- signal that back to the
        // test rather than the test having to poke at client internals.
        let _ = closed_tx.send(());
    });
    let url = format!("ws://{addr}");

    let socket = PaneSocket::attach(&url, "p1", PaneMode::Observe, None)
        .await
        .unwrap();
    drop(socket);

    tokio::time::timeout(std::time::Duration::from_secs(3), closed_rx)
        .await
        .expect("server did not observe the connection closing within 3s -- dropping PaneSocket leaked the socket/reader task")
        .expect("the mock server task ended without signalling (it panicked?)");
}
