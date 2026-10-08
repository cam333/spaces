//! Hand-written overrides for `PaneUpFrame` and `PaneDownFrame`.
//!
//! Both are discriminated unions on the wire (`z.discriminatedUnion` in
//! `bin/lib/runtime/protocol.js`), but the codegen'd versions (see
//! `build.rs`) model a JSON-Schema `anyOf` as a plain `#[serde(untagged)]`
//! enum with a bare `String` tag field and no `deny_unknown_fields`.
//! Untagged variant matching in serde tries each variant *structurally*, in
//! declaration order, and accepts the first one that deserializes without
//! error — it never actually checks the tag's value. That produced two
//! confirmed bugs against the committed fixtures
//! (`tests/proto/fixtures/pane-down-frames.json`):
//!
//! - `{"type":"exit","exitCode":0}` incorrectly matched the field-less
//!   `replay-start` variant (which has no fields to conflict with the
//!   unrecognized `exitCode`), silently dropping `exitCode` instead of
//!   matching the `exit` variant.
//! - `{"type":"unknown"}` was accepted instead of rejected, because nothing
//!   validates that the tag is one of the known literals.
//!
//! `#[serde(tag = "...")]` (internally tagged) fixes both: it dispatches on
//! the tag's literal value first, matching the discriminated-union
//! semantics the zod schema actually specifies, and produces a real error
//! for a variant whose fields don't match or a tag that isn't recognized.
//!
//! Deliberately not `deny_unknown_fields`: the server tolerates and
//! silently strips unknown keys on every frame (see the comment in
//! `scripts/export-protocol-schema.mjs`), and these types should stay
//! forward-compatible with that.

use serde::{Deserialize, Serialize};

/// Upward wire frames sent by the client on the pane data-socket
/// (`/proto/pane/<id>`), discriminated by `t`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum PaneUpFrame {
    #[serde(rename = "i")]
    Input { data: String },
    #[serde(rename = "r")]
    Resize {
        cols: std::num::NonZeroU32,
        rows: std::num::NonZeroU32,
    },
}

/// Downward wire frames sent by the server on the pane data-socket,
/// discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PaneDownFrame {
    #[serde(rename = "replay-start")]
    ReplayStart,
    #[serde(rename = "replay-end")]
    ReplayEnd,
    #[serde(rename = "exit")]
    Exit {
        #[serde(rename = "exitCode")]
        exit_code: i64,
    },
    #[serde(rename = "error")]
    Error {
        code: PaneDownFrameErrorCode,
        message: String,
    },
}

/// Error codes the server may report on the pane data-socket. Mirrors
/// `ERROR_CODES` in `bin/lib/runtime/protocol.js`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneDownFrameErrorCode {
    BadRequest,
    Unauthorized,
    NotFound,
    Conflict,
    VersionMismatch,
    Gap,
    Internal,
}
