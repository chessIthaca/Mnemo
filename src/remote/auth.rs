// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The link's cryptography: HMAC-SHA256 proofs, the session key, frame MACs,
//! and the hex/nonce helpers around them.
//!
//! The pairing handshake establishes a 32-byte per-node token. Everything
//! after it is derived from that one secret:
//!
//! - node proof = `HMAC(token, "mnemo-node/v1/node-proof" || nN || nH)`
//! - hub proof  = `HMAC(token, "mnemo-node/v1/hub-proof"  || nN || nH)`
//! - session key = `HMAC(token, "mnemo-node/v1/session-key" || nN || nH)`
//! - frame MAC  = `HMAC(session_key, dir || seq_be8 || json)`, `dir` being
//!   `b'N'` node→hub and `b'H'` hub→node, `seq_be8` the sequence number as 8
//!   big-endian bytes
//!
//! where `nN`/`nH` are the node's and the hub's 32-byte nonces. The role
//! label differs between the two proofs, so a captured proof can never be
//! reflected back as the other side's proof, and the direction byte keeps a
//! frame from being replayed the other way. All comparisons are constant
//! time (the fold pattern of `mcp_server::token_matches`, copied rather than
//! shared — the app crate owns that one).

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// The frame-MAC direction byte of a node→hub frame.
pub const DIR_NODE_TO_HUB: u8 = b'N';
/// The frame-MAC direction byte of a hub→node frame.
pub const DIR_HUB_TO_NODE: u8 = b'H';

/// HMAC label of the node's token proof.
pub const NODE_PROOF_LABEL: &[u8] = b"mnemo-node/v1/node-proof";
/// HMAC label of the hub's token proof.
pub const HUB_PROOF_LABEL: &[u8] = b"mnemo-node/v1/hub-proof";
/// HMAC label of the per-link session key.
pub const SESSION_KEY_LABEL: &[u8] = b"mnemo-node/v1/session-key";

/// A 32-byte secret or nonce.
pub type Bytes32 = [u8; 32];

/// A 32-byte per-node token (established at pairing, stored hex).
pub type Token = Bytes32;

/// A 32-byte handshake nonce.
pub type Nonce = Bytes32;

type HmacSha256 = Hmac<Sha256>;

/// HMAC-SHA256 over the concatenation of `parts`. The key may have any
/// length; HMAC hashes a long key before use.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> Bytes32 {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key)
        .expect("HMAC accepts a key of any length");
    for part in parts {
        mac.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// Compare two byte strings in constant time (length leaks, content does not).
/// The fold is the one `src-tauri/src/mnemo2/mcp_server.rs` uses for bearer
/// tokens; it is copied here because the library may not depend on the app
/// crate.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .fold(0u8, |diff, (x, y)| diff | (x ^ y))
            == 0
}

/// Lowercase hex encoding.
pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Decode lowercase or uppercase hex. Odd lengths and non-hex characters are
/// rejected.
pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

/// Parse a 64-hex string into its 32 bytes.
pub fn parse_32_hex(text: &str) -> Option<Bytes32> {
    let bytes = hex_decode(text)?;
    let mut out = [0u8; 32];
    if bytes.len() != 32 {
        return None;
    }
    out.copy_from_slice(&bytes);
    Some(out)
}

/// 32 random bytes for a token or nonce: two UUID v4 values (each carries 122
/// bits from the OS CSPRNG through `getrandom`).
pub fn random_32() -> Bytes32 {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    out[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    out
}

/// A fresh 32-byte handshake nonce.
pub fn random_nonce() -> Nonce {
    random_32()
}

/// A fresh 32-byte node token.
pub fn random_token() -> Token {
    random_32()
}

/// The node's token proof, hex encoded.
pub fn node_proof(token: &Token, node_nonce: &Nonce, hub_nonce: &Nonce) -> String {
    hex_encode(&proof(token, NODE_PROOF_LABEL, node_nonce, hub_nonce))
}

/// The hub's token proof, hex encoded.
pub fn hub_proof(token: &Token, node_nonce: &Nonce, hub_nonce: &Nonce) -> String {
    hex_encode(&proof(token, HUB_PROOF_LABEL, node_nonce, hub_nonce))
}

/// Verify a proof string against the expected one, in constant time.
pub fn verify_proof(given: &str, expected: &str) -> bool {
    match (hex_decode(given), hex_decode(expected)) {
        (Some(given), Some(expected)) => constant_time_eq(&given, &expected),
        _ => false,
    }
}

fn proof(token: &Token, label: &[u8], node_nonce: &Nonce, hub_nonce: &Nonce) -> Bytes32 {
    hmac_sha256(token, &[label, node_nonce, hub_nonce])
}

/// The per-link session key both ends derive after a successful handshake.
pub fn session_key(token: &Token, node_nonce: &Nonce, hub_nonce: &Nonce) -> Bytes32 {
    hmac_sha256(token, &[SESSION_KEY_LABEL, node_nonce, hub_nonce])
}

/// The MAC of one frame, hex encoded: `HMAC(session_key, dir || seq_be8 ||
/// json)`.
pub fn frame_mac(session_key: &[u8], dir: u8, seq: u64, json: &[u8]) -> String {
    hex_encode(&frame_mac_bytes(session_key, dir, seq, json))
}

/// Verify a frame MAC string in constant time.
pub fn verify_frame_mac(session_key: &[u8], dir: u8, seq: u64, json: &[u8], mac_hex: &str) -> bool {
    let Some(given) = hex_decode(mac_hex) else {
        return false;
    };
    constant_time_eq(&given, &frame_mac_bytes(session_key, dir, seq, json))
}

fn frame_mac_bytes(session_key: &[u8], dir: u8, seq: u64, json: &[u8]) -> Bytes32 {
    hmac_sha256(
        session_key,
        &[&[dir][..], &seq.to_be_bytes(), json],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_rfc_4231_case_1() {
        // HMAC-SHA-256(key=0x0b * 20, "Hi There").
        let mac = hmac_sha256(&[0x0b; 20], &[b"Hi There"]);
        assert_eq!(
            hex_encode(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn hex_round_trips_and_rejects_junk() {
        let bytes = [0x00u8, 0x0f, 0xa5, 0xff];
        assert_eq!(hex_encode(&bytes), "000fa5ff");
        assert_eq!(hex_decode("000fa5ff"), Some(bytes.to_vec()));
        assert_eq!(hex_decode("000FA5FF"), Some(bytes.to_vec()));
        assert_eq!(hex_decode("abc"), None);
        assert_eq!(hex_decode("zz"), None);
        assert!(parse_32_hex(&"a".repeat(62)).is_none());
        assert!(parse_32_hex(&"a".repeat(64)).is_some());
    }

    #[test]
    fn constant_time_eq_is_an_equality() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(b"secret", b"secre"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn proofs_are_role_separated_and_nonce_bound() {
        let token = [7u8; 32];
        let node_nonce = [1u8; 32];
        let hub_nonce = [2u8; 32];
        let node = node_proof(&token, &node_nonce, &hub_nonce);
        let hub = hub_proof(&token, &node_nonce, &hub_nonce);
        assert_ne!(node, hub);
        assert_eq!(node.len(), 64);
        assert!(verify_proof(&node, &node));
        assert!(!verify_proof(&node, &hub));
        // A reflected proof is not accepted as the other role, and the nonce
        // order is part of the transcript.
        assert!(!verify_proof(
            &node,
            &node_proof(&token, &hub_nonce, &node_nonce)
        ));
        assert!(!verify_proof(
            &node,
            &node_proof(&[8u8; 32], &node_nonce, &hub_nonce)
        ));
        assert!(!verify_proof("zz", &node));
    }

    #[test]
    fn frame_macs_bind_direction_and_sequence() {
        let key = [3u8; 32];
        let json = br#"{"t":"cancel","id":1}"#;
        let mac = frame_mac(&key, DIR_NODE_TO_HUB, 1, json);
        assert!(verify_frame_mac(&key, DIR_NODE_TO_HUB, 1, json, &mac));
        // Wrong direction, sequence or body: no match.
        assert!(!verify_frame_mac(&key, DIR_HUB_TO_NODE, 1, json, &mac));
        assert!(!verify_frame_mac(&key, DIR_NODE_TO_HUB, 2, json, &mac));
        assert!(!verify_frame_mac(
            &key,
            DIR_NODE_TO_HUB,
            1,
            br#"{"t":"cancel","id":2}"#,
            &mac
        ));
        assert!(!verify_frame_mac(&key, DIR_NODE_TO_HUB, 1, json, "nope"));
    }

    #[test]
    fn session_key_differs_from_the_proofs() {
        let token = [9u8; 32];
        let pair = ([4u8; 32], [5u8; 32]);
        let key = session_key(&token, &pair.0, &pair.1);
        assert_ne!(key, [0u8; 32]);
        assert_ne!(hex_encode(&key), node_proof(&token, &pair.0, &pair.1));
        assert_ne!(hex_encode(&key), hub_proof(&token, &pair.0, &pair.1));
        assert_eq!(key, session_key(&token, &pair.0, &pair.1));
    }

    #[test]
    fn random_32_draws_distinct_values() {
        assert_ne!(random_32(), random_32());
    }
}
