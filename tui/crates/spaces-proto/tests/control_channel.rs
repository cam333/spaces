use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use spaces_proto::client::{ControlClient, ProtoError};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// Spawn a mock control channel. `script` receives each inbound text frame
/// and returns the frames to send back.
async fn mock_server<F>(script: F) -> String
where
    F: Fn(serde_json::Value) -> Vec<serde_json::Value> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(msg)) = ws.next().await {
            if let Message::Text(t) = msg {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                for out in script(v) {
                    ws.send(Message::Text(out.to_string())).await.unwrap();
                }
            }
        }
    });
    format!("ws://{addr}/proto")
}

#[tokio::test]
async fn connect_completes_the_hello_handshake() {
    let url = mock_server(|inbound| {
        assert_eq!(inbound["type"], "hello");
        assert_eq!(inbound["protocolVersion"], 1);
        vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"test","seq":7})]
    })
    .await;
    let client = ControlClient::connect(&url, None).await.unwrap();
    assert_eq!(client.last_seq(), 7);
}

#[tokio::test]
async fn version_mismatch_is_a_typed_error() {
    let url = mock_server(|_| {
        vec![json!({"id":"hello","error":{"code":"version_mismatch","message":"runtime speaks protocol 2"}})]
    })
    .await;
    match ControlClient::connect(&url, None).await {
        Err(ProtoError::VersionMismatch { runtime }) => assert_eq!(runtime, 2),
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[tokio::test]
async fn request_returns_the_result() {
    let url = mock_server(|inbound| {
        if inbound["type"] == "hello" {
            return vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":0})];
        }
        vec![json!({"id": inbound["id"], "result": {"nodeId":"n","protocolVersion":1,"seq":0}})]
    })
    .await;
    let mut c = ControlClient::connect(&url, None).await.unwrap();
    let r = c.request("node.info", json!({})).await.unwrap();
    assert_eq!(r["nodeId"], "n");
}

#[tokio::test]
async fn a_remote_error_becomes_a_typed_error() {
    let url = mock_server(|inbound| {
        if inbound["type"] == "hello" {
            return vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":0})];
        }
        vec![json!({"id": inbound["id"], "error": {"code":"not_found","message":"no pane p9"}})]
    })
    .await;
    let mut c = ControlClient::connect(&url, None).await.unwrap();
    match c.request("pane.read", json!({"paneId":"p9"})).await {
        Err(ProtoError::Remote { code, message }) => {
            assert_eq!(code, "not_found");
            assert!(message.contains("p9"));
        }
        other => panic!("expected Remote, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribe_returns_the_backlog_and_reports_a_gap() {
    let url = mock_server(|inbound| {
        if inbound["type"] == "hello" {
            return vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":3})];
        }
        vec![json!({"id": inbound["id"], "result": {"gap": true, "events": [
            {"seq":3,"type":"pane.state","payload":{"paneId":"p1","agentState":"blocked","seen":true},"ts":1}
        ]}})]
    })
    .await;
    let mut c = ControlClient::connect(&url, None).await.unwrap();
    let ack = c.subscribe(0).await.unwrap();
    assert!(ack.gap);
    assert_eq!(ack.events.len(), 1);
    assert_eq!(ack.events[0].seq, 3);
}

#[tokio::test]
async fn live_events_reach_the_receiver() {
    let url = mock_server(|inbound| {
        if inbound["type"] == "hello" {
            return vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":0})];
        }
        vec![
            json!({"id": inbound["id"], "result": {"gap": false, "events": []}}),
            json!({"event": {"seq":1,"type":"pane.opened","payload":{"paneId":"p1"},"ts":9}}),
        ]
    })
    .await;
    let mut c = ControlClient::connect(&url, None).await.unwrap();
    c.subscribe(0).await.unwrap();
    let mut rx = c.events();
    let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out")
        .expect("channel closed");
    assert_eq!(ev.seq, 1);
    assert_eq!(ev.r#type, "pane.opened");
}

#[tokio::test]
async fn a_malformed_frame_does_not_panic() {
    let url = mock_server(|inbound| {
        if inbound["type"] == "hello" {
            return vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":0})];
        }
        vec![
            json!({"totally":"unexpected"}),
            json!({"id": inbound["id"], "result": {}}),
        ]
    })
    .await;
    let mut c = ControlClient::connect(&url, None).await.unwrap();
    // The junk frame is ignored; the real reply still arrives.
    let r = c.request("node.info", json!({})).await;
    assert!(r.is_ok(), "unexpected frames must not break correlation");
}

#[tokio::test]
async fn dropping_the_client_closes_the_connection_and_reader_task() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Text(t)) => {
                    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == "hello" {
                        ws.send(Message::Text(
                            json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":0})
                                .to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // The loop above only ends once the client's side of the socket has
        // actually closed (EOF or a read error) -- signal that back to the
        // test rather than the test having to poke at client internals.
        let _ = closed_tx.send(());
    });
    let url = format!("ws://{addr}/proto");

    let client = ControlClient::connect(&url, None).await.unwrap();
    drop(client);

    tokio::time::timeout(std::time::Duration::from_secs(3), closed_rx)
        .await
        .expect("server did not observe the connection closing within 3s -- dropping ControlClient leaked the socket/reader task")
        .expect("the mock server task ended without signalling (it panicked?)");
}

#[tokio::test]
async fn a_dropped_connection_fails_in_flight_requests_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        if let Some(Ok(Message::Text(t))) = ws.next().await {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            assert_eq!(v["type"], "hello");
            ws.send(Message::Text(
                json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":0}).to_string(),
            ))
            .await
            .unwrap();
        }
        // Consume the request frame, then close without ever replying to it.
        let _ = ws.next().await;
        drop(ws);
    });
    let url = format!("ws://{addr}/proto");
    let mut c = ControlClient::connect(&url, None).await.unwrap();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        c.request("node.info", json!({})),
    )
    .await
    .expect("request did not resolve within 5s of the server closing the connection");

    assert!(
        matches!(result, Err(ProtoError::Transport(_))),
        "expected a Transport error, got {result:?}"
    );
}

#[tokio::test]
async fn malformed_backlog_events_are_counted_not_silently_dropped() {
    let url = mock_server(|inbound| {
        if inbound["type"] == "hello" {
            return vec![json!({"type":"hello-ok","protocolVersion":1,"nodeId":"n","seq":5})];
        }
        vec![
            json!({"id": inbound["id"], "result": {"gap": false, "events": [
                {"seq":4,"type":"pane.opened","payload":{"paneId":"p1"},"ts":1},
                {"seq":"not-a-number","type":"pane.opened","payload":{"paneId":"p2"},"ts":2}
            ]}}),
        ]
    })
    .await;
    let mut c = ControlClient::connect(&url, None).await.unwrap();
    let ack = c.subscribe(0).await.unwrap();
    assert_eq!(ack.events.len(), 1);
    assert_eq!(ack.dropped_malformed_events, 1);
}
