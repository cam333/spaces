//! Wire types and clients for the Spaces runtime protocol.
//!
//! This crate is a peer client of the protocol documented in
//! `docs/protocol/README.md`. It never reaches into the runtime's
//! internals — everything it knows arrives over a WebSocket.

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u32 = 1;

// Types generated at build time from the committed contract schema
// (`docs/protocol/spaces-proto.schema.json`), which the Node runtime
// generates from its zod definitions. See `build.rs`.
mod generated {
    #![allow(clippy::all, dead_code)]
    include!(concat!(env!("OUT_DIR"), "/proto_types.rs"));
}

mod frames;

pub mod client;

pub mod pane;

// Explicit re-exports take priority over the glob import below for the same
// name, so this replaces the codegen'd `PaneUpFrame`/`PaneDownFrame` (which
// model the wire's discriminated unions too permissively — see
// `frames.rs`) while every other generated type still comes through
// `generated::*` unchanged.
pub use frames::{PaneDownFrame, PaneDownFrameErrorCode, PaneUpFrame};

// `client::EventFrameInner` is likewise hand-written rather than reusing the
// generated `EventFrameEvent` — see the doc comment on `client` for why —
// and is re-exported at the crate root so callers can reach it either as
// `spaces_proto::EventFrameInner` or `spaces_proto::client::EventFrameInner`.
pub use client::{ControlClient, EventFrameInner, ProtoError, SubscribeAck};

pub use generated::*;
