// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The link wire format: MACed text frames and the JSON envelopes they carry.
//!
//! A frame is exactly `"<seq> <mac> <json>"`: the per-direction sequence
//! number in decimal, the 64 lowercase hex HMAC-SHA256 tag, then the JSON
//! envelope — one space between the three parts, the JSON itself unescaped
//! (it may contain spaces). The handshake frames ([`Handshake`]) travel as
//! plain JSON text before the first MACed frame; nothing in them is secret
//! except the token proof and the pairing token, both of which are always
//! HMAC outputs or the token itself and never logged.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RemoteError, MAX_MESSAGE};

/// One MACed link frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Per-direction sequence number, starting at 1 and strictly +1.
    pub seq: u64,
    /// The 64 lowercase hex HMAC-SHA256 tag over the frame.
    pub mac: String,
    /// The JSON envelope text exactly as authenticated.
    pub json: String,
}

impl Frame {
    /// Build a frame from its parts.
    pub fn new(seq: u64, mac: impl Into<String>, json: impl Into<String>) -> Self {
        Self {
            seq,
            mac: mac.into(),
            json: json.into(),
        }
    }

    /// The text to put on the wire.
    pub fn encode(&self) -> String {
        format!("{} {} {}", self.seq, self.mac, self.json)
    }

    /// Split a received text message into its three parts and validate their
    /// shape. Returns the reason for a rejected frame as the error string; the
    /// link maps it to a close.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_MESSAGE {
            return Err(format!(
                "frame of {} bytes exceeds the {MAX_MESSAGE}-byte message limit",
                text.len()
            ));
        }
        let mut parts = text.splitn(3, ' ');
        let seq = parts.next().unwrap_or_default();
        let mac = parts.next().unwrap_or_default();
        let json = parts.next().unwrap_or_default();
        if seq.is_empty() || mac.is_empty() || json.is_empty() {
            return Err("frame is not \"<seq> <mac> <json>\"".to_string());
        }
        let seq: u64 = seq
            .parse()
            .map_err(|_| format!("frame sequence is not a u64: {seq:?}"))?;
        if seq == 0 {
            return Err("frame sequence numbers start at 1".to_string());
        }
        if mac.len() != 64 || !mac.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("frame MAC is not 64 lowercase hex digits".to_string());
        }
        Ok(Self {
            seq,
            mac: mac.to_string(),
            json: json.to_string(),
        })
    }
}

/// Parse a MACed frame's JSON text into an [`Envelope`].
pub fn parse_envelope(json: &str) -> Result<Envelope, String> {
    serde_json::from_str(json).map_err(|error| format!("malformed envelope: {error}"))
}

/// One multiplexed link message.
///
/// - `req` opens a call; `id` is unique per originator.
/// - `res` answers it first: exactly one of `ok` (the method's result) or
///   `err` (a [`RemoteError`]).
/// - `item` is one streamed element, numbered from 1 per call.
/// - `end` closes the stream: `err` is null on success.
/// - `cancel` is sent by the requester when it stops caring.
/// - `note` is a best-effort one-way hint; it is never answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Envelope {
    /// Open a call.
    Req {
        /// The originator's call id.
        id: u64,
        /// The method name.
        m: String,
        /// The method parameters.
        #[serde(default)]
        p: Value,
    },
    /// The first answer of a call: the result or the error.
    Res {
        /// The call id being answered.
        id: u64,
        /// The result payload (absent on an error).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Value>,
        /// The failure (absent on success).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<RemoteError>,
    },
    /// One streamed item, numbered from 1 per call.
    Item {
        /// The call id.
        id: u64,
        /// The item number.
        n: u64,
        /// The item payload.
        d: Value,
    },
    /// The end of a stream: `err` is null on success.
    End {
        /// The call id.
        id: u64,
        /// The terminal failure, if any.
        #[serde(default)]
        err: Option<RemoteError>,
    },
    /// The requester dropped the stream.
    Cancel {
        /// The call id.
        id: u64,
    },
    /// A best-effort one-way hint.
    Note {
        /// The note kind.
        m: String,
        /// The note payload.
        #[serde(default)]
        p: Value,
    },
}

impl Envelope {
    /// A request envelope.
    pub fn req(id: u64, m: impl Into<String>, p: Value) -> Self {
        Envelope::Req {
            id,
            m: m.into(),
            p,
        }
    }

    /// A successful response envelope.
    pub fn res_ok(id: u64, ok: Value) -> Self {
        Envelope::Res {
            id,
            ok: Some(ok),
            err: None,
        }
    }

    /// A failed response envelope.
    pub fn res_err(id: u64, err: RemoteError) -> Self {
        Envelope::Res {
            id,
            ok: None,
            err: Some(err),
        }
    }

    /// One stream item.
    pub fn item(id: u64, n: u64, d: Value) -> Self {
        Envelope::Item { id, n, d }
    }

    /// The end of a stream.
    pub fn end(id: u64, err: Option<RemoteError>) -> Self {
        Envelope::End { id, err }
    }

    /// A cancellation of a stream.
    pub fn cancel(id: u64) -> Self {
        Envelope::Cancel { id }
    }

    /// A best-effort note.
    pub fn note(m: impl Into<String>, p: Value) -> Self {
        Envelope::Note { m: m.into(), p }
    }

    /// The call id this envelope refers to (`None` for a note).
    pub fn call_id(&self) -> Option<u64> {
        match self {
            Envelope::Req { id, .. }
            | Envelope::Res { id, .. }
            | Envelope::Item { id, .. }
            | Envelope::End { id, .. }
            | Envelope::Cancel { id } => Some(*id),
            Envelope::Note { .. } => None,
        }
    }
}

/// A pre-authentication handshake message. These travel as plain JSON text
/// (no MAC, no sequence number); the token itself never appears in one —
/// only HMAC proofs and, on pairing, the freshly minted node token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Handshake {
    /// Node → hub: open a link.
    Hello {
        /// The protocol version the node speaks.
        proto: u32,
        /// The node's 32-hex id.
        node_id: String,
        /// The node's 64-hex nonce.
        nonce: String,
        /// The node build version.
        version: String,
        /// `windows`, `macos` or `linux`.
        os: String,
    },
    /// Hub → node: the hub's nonce.
    Challenge {
        /// The hub's 64-hex nonce.
        nonce: String,
    },
    /// Node → hub: the node's proof of the token.
    Auth {
        /// 64-hex HMAC(token, "mnemo-node/v1/node-proof" || nN || nH).
        proof: String,
    },
    /// Hub → node: the hub's proof of the token, plus its identity and epoch.
    Welcome {
        /// 64-hex HMAC(token, "mnemo-node/v1/hub-proof" || nN || nH).
        proof: String,
        /// The hub's stable id.
        hub_id: String,
        /// The hub server's epoch — fresh on every HubServer start.
        hub_epoch: String,
    },
    /// Hub → node: the link is refused, then the socket closes.
    Refused {
        /// `unknown_node`, `proto`, `bad_proof`, `bad_code` or `expired`.
        reason: String,
        /// The protocol version the hub speaks (on a `proto` refusal).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proto: Option<u32>,
    },
    /// Node → hub: redeem a pairing code for a node token.
    Pair {
        /// The protocol version the node speaks.
        proto: u32,
        /// The code the operator read off the hub, `XXXX-XXXX`.
        code: String,
        /// The requested node name.
        name: String,
        /// `windows`, `macos` or `linux`.
        os: String,
        /// The node build version.
        version: String,
    },
    /// Hub → node: the pairing succeeded; these are the node's long-lived
    /// credentials.
    Paired {
        /// The node's 32-hex id.
        node_id: String,
        /// The node's 64-hex token.
        token: String,
        /// The hub's stable id.
        hub_id: String,
    },
}

impl Handshake {
    /// The message kind (`hello`, `challenge`, `auth`, `welcome`, `refused`,
    /// `pair`, `paired`) — for diagnostics that must never echo a proof or a
    /// token.
    pub fn kind(&self) -> &'static str {
        match self {
            Handshake::Hello { .. } => "hello",
            Handshake::Challenge { .. } => "challenge",
            Handshake::Auth { .. } => "auth",
            Handshake::Welcome { .. } => "welcome",
            Handshake::Refused { .. } => "refused",
            Handshake::Pair { .. } => "pair",
            Handshake::Paired { .. } => "paired",
        }
    }
}

/// Parse a handshake text message.
pub fn parse_handshake(text: &str) -> Result<Handshake, String> {
    if text.len() > MAX_MESSAGE {
        return Err("handshake message exceeds the message limit".to_string());
    }
    serde_json::from_str(text).map_err(|error| format!("malformed handshake: {error}"))
}

/// The OS name this build reports in a handshake: `windows`, `macos` or
/// `linux`.
pub fn os_name() -> &'static str {
    std::env::consts::OS
}

/// The build version this binary reports in a handshake.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_and_keeps_json_spaces() {
        let json = r#"{"t":"req","id":1,"m":"x","p":{"a":"b c"}}"#;
        let frame = Frame::new(7, "a".repeat(64), json);
        let parsed = Frame::parse(&frame.encode()).expect("frame parses");
        assert_eq!(parsed, frame);
        assert_eq!(parsed.json, json);
    }

    #[test]
    fn frame_rejects_bad_shape() {
        assert!(Frame::parse("1 abc").is_err());
        assert!(Frame::parse("0 aaaa {}" ).is_err());
        assert!(Frame::parse("1 AAAA {}").is_err());
        assert!(Frame::parse("1 aaaa ").is_err());
    }

    #[test]
    fn res_carries_exactly_one_of_ok_and_err() {
        let ok = serde_json::to_string(&Envelope::res_ok(3, serde_json::json!({ "a": 1 })))
            .expect("serializes");
        assert!(ok.contains("\"ok\""), "{ok}");
        assert!(!ok.contains("\"err\""), "{ok}");

        let err = serde_json::to_string(&Envelope::res_err(3, RemoteError::internal("boom")))
            .expect("serializes");
        assert!(err.contains("\"err\""), "{err}");
        assert!(!err.contains("\"ok\""), "{err}");
    }

    #[test]
    fn end_always_spells_out_null_error() {
        let text = serde_json::to_string(&Envelope::end(4, None)).expect("serializes");
        assert_eq!(text, r#"{"t":"end","id":4,"err":null}"#);
    }

    #[test]
    fn envelope_round_trips_through_json() {
        let cases = vec![
            Envelope::req(1, "session.send", serde_json::json!({"text": "hi"})),
            Envelope::res_ok(2, serde_json::json!("ok")),
            Envelope::res_err(3, RemoteError::bad_params("nope")),
            Envelope::item(4, 2, serde_json::json!({ "d": true })),
            Envelope::end(4, Some(RemoteError::cancelled())),
            Envelope::end(4, None),
            Envelope::cancel(5),
            Envelope::note("progress", serde_json::json!({ "pct": 40 })),
        ];
        for envelope in cases {
            let text = serde_json::to_string(&envelope).expect("serializes");
            assert_eq!(parse_envelope(&text), Ok(envelope));
        }
    }

    #[test]
    fn handshake_round_trips_and_proto_refusal_carries_the_version() {
        let hello = Handshake::Hello {
            proto: 1,
            node_id: "a".repeat(32),
            nonce: "b".repeat(64),
            version: "2.9.0".to_string(),
            os: "windows".to_string(),
        };
        let text = serde_json::to_string(&hello).expect("serializes");
        assert_eq!(parse_handshake(&text), Ok(hello));

        let refused = Handshake::Refused {
            reason: "proto".to_string(),
            proto: Some(1),
        };
        let text = serde_json::to_string(&refused).expect("serializes");
        assert!(text.contains(r#""proto":1"#), "{text}");
        assert_eq!(parse_handshake(&text), Ok(refused));
    }
}
