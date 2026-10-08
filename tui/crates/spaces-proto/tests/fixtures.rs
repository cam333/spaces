use serde_json::Value;
use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

fn fixture(name: &str) -> Value {
    let p = repo_root()
        .join("tests")
        .join("proto")
        .join("fixtures")
        .join(name);
    let raw = fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
    serde_json::from_str(&raw).unwrap()
}

/// A value that deserializes into `T` and serializes back must be
/// semantically unchanged — this is what proves the Rust types agree with
/// the fixtures the Node test-suite asserts the server produces.
fn round_trips<T>(v: &Value) -> bool
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    match serde_json::from_value::<T>(v.clone()) {
        Ok(parsed) => {
            let back = serde_json::to_value(&parsed).expect("serialize");
            subset_matches(v, &back)
        }
        Err(_) => false,
    }
}

/// Every field in `original` must survive the round trip with the same
/// value. Extra fields the type adds (e.g. `None` written as `null`) are
/// tolerated; dropped or altered fields are not.
fn subset_matches(original: &Value, back: &Value) -> bool {
    match (original, back) {
        (Value::Object(a), Value::Object(b)) => a
            .iter()
            .all(|(k, va)| b.get(k).map(|vb| subset_matches(va, vb)).unwrap_or(false)),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| subset_matches(x, y))
        }
        // `serde_json::Number` distinguishes an integer literal from a
        // float with the same value (`Number::from(1u64)` != `Number::
        // from_f64(1.0)`), so a Rust `f64` field round-tripping an integer
        // JSON literal (e.g. `EventFrame.event.ts: 1`) would otherwise fail
        // this check for a reason that has nothing to do with whether the
        // value was preserved. Compare numbers by numeric value instead.
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => original == back,
    }
}

#[test]
fn hello_fixture_round_trips() {
    let f = fixture("hello.json");
    assert!(round_trips::<spaces_proto::Hello>(&f["send"]), "hello send");
}

#[test]
fn pane_up_frames_match_validity() {
    let f = fixture("pane-frames.json");
    for v in f["validUpFrames"].as_array().unwrap() {
        assert!(
            round_trips::<spaces_proto::PaneUpFrame>(v),
            "should accept {v}"
        );
    }
    for v in f["invalidUpFrames"].as_array().unwrap() {
        assert!(
            serde_json::from_value::<spaces_proto::PaneUpFrame>(v.clone()).is_err(),
            "should reject {v}"
        );
    }
}

#[test]
fn pane_down_frames_match_validity() {
    let f = fixture("pane-down-frames.json");
    for v in f["validDownFrames"].as_array().unwrap() {
        assert!(
            round_trips::<spaces_proto::PaneDownFrame>(v),
            "should accept {v}"
        );
    }
    for v in f["invalidDownFrames"].as_array().unwrap() {
        assert!(
            serde_json::from_value::<spaces_proto::PaneDownFrame>(v.clone()).is_err(),
            "should reject {v}"
        );
    }
}

#[test]
fn event_samples_round_trip() {
    let f = fixture("session-control-frames.json");
    for sample in f["sampleEvents"].as_array().unwrap() {
        let framed = serde_json::json!({ "event": sample });
        assert!(
            round_trips::<spaces_proto::EventFrame>(&framed),
            "event {sample}"
        );
    }
}

#[test]
fn every_documented_event_type_has_a_sample() {
    let f = fixture("session-control-frames.json");
    let samples: Vec<&str> = f["sampleEvents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    for t in f["eventTypes"].as_array().unwrap() {
        let t = t.as_str().unwrap();
        assert!(samples.contains(&t), "no sample event for {t}");
    }
}

#[test]
fn protocol_version_matches_the_schema() {
    let raw = fs::read_to_string(
        repo_root()
            .join("docs")
            .join("protocol")
            .join("spaces-proto.schema.json"),
    )
    .unwrap();
    let doc: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        doc["protocol"].as_u64().unwrap() as u32,
        spaces_proto::PROTOCOL_VERSION
    );
}
