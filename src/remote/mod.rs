// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The authenticated node link (plan a040713f, phase 2 step 3).
//!
//! One long-lived WebSocket between the hub (the user's Mnemo2 instance) and a
//! headless worker node on another machine. The node always dials; the hub
//! listens, so a node never needs an inbound port. The transport is run inside
//! Tailscale or an SSH tunnel chosen by the user — Mnemo does not implement
//! TLS itself.
//!
//! After a plain-JSON handshake ([`link::dial`] / [`link::accept`]) every frame
//! is authenticated: `"<seq> <mac> <json>"`, where `mac` is
//! `HMAC-SHA256(session_key, dir || seq_be8 || json)` and `seq` is a strictly
//! increasing per-direction counter. The session key is derived from the
//! per-node token established once at pairing ([`pairing`]); neither the token
//! nor any proof is ever logged. A bad MAC or a replayed/skipped sequence
//! number closes the link.
//!
//! The link is multiplexed in both directions: a call carries a request id,
//! answers `res` first (for a streaming method), then numbered `item` frames,
//! then `end`; either side may push best-effort `note` frames, and the caller
//! may `cancel` a stream it no longer wants.
//!
//! Phase 2 step 3 ships the transport, the wire format, the pairing registry
//! and the node identity file. The model/memory relays (step 4) and the hub
//! server / node runtime (step 5) build on [`Link`].

pub mod auth;
pub mod catalog;
pub mod hub;
pub mod link;
pub mod llm;
pub mod memory;
pub mod node;
pub mod pairing;
pub mod repo;
pub mod wire;

pub use link::{HubAuth, Item, Link, LinkHandler, LinkSlot, StreamRx, StreamTx, TokenLookup};
pub use pairing::{
    NodeIdentity, NodeRecord, NodeRegistry, PairingCode, PairingRefusal, PairingRegistry,
};
pub use wire::Envelope;

use serde::{Deserialize, Serialize};

use crate::headless::EngineError;

/// Wire protocol version. Both ends refuse a peer that speaks another one.
pub const PROTO: u32 = 1;

/// The single WebSocket path of the node link.
pub const PATH: &str = "/mnemo-node/v1";

/// Idle interval between keepalive pings (or `pong` probes) on a live link.
pub const PING: std::time::Duration = std::time::Duration::from_secs(15);

/// A link that saw no inbound frame for this long is dead (the peer process
/// vanished, the tunnel dropped). Larger than [`PING`] by three intervals.
pub const DEAD_AFTER: std::time::Duration = std::time::Duration::from_secs(45);

/// How long a node may wait for the hub to come back after the link dropped
/// before it gives up on the outage (a hub restart, plan a040713f scenario 2).
pub const HUB_OUTAGE_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// A pairing code stays usable for this long after the hub issues it.
pub const PAIRING_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Failed pairing attempts a code survives inside its window before it burns.
pub const PAIRING_ATTEMPTS: u32 = 5;

/// Upper bound of the encoded JSON payload of one link message (the WebSocket
/// `max_message_size`; a message may span several frames).
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Upper bound of one WebSocket frame (the WebSocket `max_frame_size`).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Inbound requests a link runs concurrently. A peer that exceeds the cap is
/// answered with a **best-effort** `err` carrying code `busy` instead of a
/// handler task: the answer is dropped when the outbound queue is full, and
/// the requester then times out rather than seeing `busy`.
pub const MAX_INFLIGHT: usize = 256;

// The stable error codes of an RPC failure (the handshake additionally reports
// the hub's refusal reason verbatim, see [`RemoteError::refused`]).
/// The link closed (peer gone, MAC/sequence violation, or local shutdown)
/// before the call finished.
pub const CODE_LINK_DOWN: &str = "link_down";
/// The caller's own timeout expired before the answer arrived.
pub const CODE_TIMEOUT: &str = "timeout";
/// The peer has no handler for this method.
pub const CODE_UNKNOWN_METHOD: &str = "unknown_method";
/// The peer rejected the parameters of the call.
pub const CODE_BAD_PARAMS: &str = "bad_params";
/// The call id is not pending on the answering side.
pub const CODE_UNKNOWN_CALL: &str = "unknown_call";
/// A node session/engine failure; see [`RemoteError::engine`].
pub const CODE_ENGINE: &str = "engine";
/// A model/provider call failed.
pub const CODE_PROVIDER: &str = "provider";
/// A memory operation failed.
pub const CODE_MEMORY: &str = "memory";
/// No model or tier is configured for the request.
pub const CODE_MODEL_NOT_CONFIGURED: &str = "model_not_configured";
/// The requester cancelled the call.
pub const CODE_CANCELLED: &str = "cancelled";
/// The peer already has the cap of inbound requests in flight ([`MAX_INFLIGHT`]);
/// retry once one finishes. Best effort: the `busy` answer itself may be
/// dropped when the peer's outbound queue is full, in which case the call
/// times out instead.
pub const CODE_BUSY: &str = "busy";
/// A stream's item buffer overflowed; that stream (only) was ended with this
/// code and cancelled on the peer. The link itself stays up.
pub const CODE_OVERFLOW: &str = "overflow";
/// The peer violated the stream protocol (an item sequence gap); that stream
/// (only) was ended with this code.
pub const CODE_PROTOCOL: &str = "protocol";
/// Anything else (serialization, internal invariant, protocol violation).
pub const CODE_INTERNAL: &str = "internal";

/// A failed link call — either raised locally (timeout, link down, bad
/// parameters) or relayed from the peer's handler.
///
/// The wire form is `{"code","message","engine"?}`; `engine` carries the
/// node's own [`EngineError`] when the failure came from the headless engine,
/// so the hub can match on the engine code the same way it does locally.
///
/// The stable codes are `link_down`, `timeout`, `unknown_method`, `bad_params`,
/// `unknown_call`, `engine`, `provider`, `memory`, `model_not_configured`,
/// `cancelled`, `busy`, `overflow`, `protocol` and `internal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteError {
    /// The stable machine code (one of the `CODE_*` constants, or the hub's
    /// handshake refusal reason).
    pub code: String,
    /// Human-readable detail.
    pub message: String,
    /// The originating engine error, when the node's engine produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<EngineError>,
}

impl RemoteError {
    /// Build an error with a stable code and a message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            engine: None,
        }
    }

    /// The link closed before the call finished.
    pub fn link_down(message: impl Into<String>) -> Self {
        Self::new(CODE_LINK_DOWN, message)
    }

    /// The caller's timeout expired.
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(CODE_TIMEOUT, message)
    }

    /// No handler is registered for the method on the peer.
    pub fn unknown_method(method: &str) -> Self {
        Self::new(CODE_UNKNOWN_METHOD, format!("unknown method: {method}"))
    }

    /// The peer rejected the call parameters.
    pub fn bad_params(message: impl Into<String>) -> Self {
        Self::new(CODE_BAD_PARAMS, message)
    }

    /// The call id is not pending.
    pub fn unknown_call(id: u64) -> Self {
        Self::new(CODE_UNKNOWN_CALL, format!("unknown call id: {id}"))
    }

    /// The requester cancelled the call.
    pub fn cancelled() -> Self {
        Self::new(CODE_CANCELLED, "the call was cancelled")
    }

    /// The peer has too many inbound requests in flight.
    pub fn busy() -> Self {
        Self::new(
            CODE_BUSY,
            format!("more than {MAX_INFLIGHT} inbound requests are in flight"),
        )
    }

    /// A stream's item buffer overflowed; only that stream ended.
    pub fn overflow(message: impl Into<String>) -> Self {
        Self::new(CODE_OVERFLOW, message)
    }

    /// The peer violated the stream protocol; only that stream ended.
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new(CODE_PROTOCOL, message)
    }

    /// An engine failure on the node.
    pub fn engine(error: EngineError) -> Self {
        Self {
            code: CODE_ENGINE.to_string(),
            message: error.message().to_string(),
            engine: Some(error),
        }
    }

    /// A model/provider failure.
    pub fn provider(message: impl Into<String>) -> Self {
        Self::new(CODE_PROVIDER, message)
    }

    /// A memory failure.
    pub fn memory(message: impl Into<String>) -> Self {
        Self::new(CODE_MEMORY, message)
    }

    /// No model/tier is configured for the request.
    pub fn model_not_configured(message: impl Into<String>) -> Self {
        Self::new(CODE_MODEL_NOT_CONFIGURED, message)
    }

    /// An internal/protocol failure.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(CODE_INTERNAL, message)
    }

    /// The hub refused the connection during the handshake; the refusal reason
    /// (`unknown_node`, `proto`, `bad_proof`, `bad_code`, `expired`) becomes
    /// the error code so the caller can act on it.
    pub fn refused(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self::new(reason.clone(), format!("hub refused the link: {reason}"))
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RemoteError {}
