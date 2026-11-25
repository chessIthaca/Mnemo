// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pairing: the hub's one-time codes and node registry, the node's identity
//! file, and the one handshake that exchanges a code for a long-lived token.
//!
//! The operator asks the hub for a code ([`PairingRegistry::issue_code`]), reads
//! it off the hub's screen, and types it on the node. The node redeems it over
//! a plain WebSocket ([`pair_with`]); the hub answers with a node id and a
//! 32-byte token ([`serve_pairing`]) which the node stores in `<config>/node.json`
//! and the hub keeps as `<state_dir>/<node_id>.json` ([`NodeRegistry`]). Both
//! files are created, restricted to the current user, written and atomically
//! renamed into place, so a crash never leaves a secret readable by others.
//! Deleting the hub's file revokes the node.
//!
//! A code is single use, expires after [`PAIRING_TTL`], and burns after
//! [`PAIRING_ATTEMPTS`] failed tries in its window (a wrong guess counts
//! against every live code of the registry, so an attacker cannot grind codes
//! indefinitely; the honest operator simply issues a new one).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::auth::{self, Token};
use super::{RemoteError, PAIRING_ATTEMPTS, PAIRING_TTL};

/// The node identity file inside the node's config dir.
pub const NODE_FILE: &str = "node.json";

/// Crockford base32: no `I`, `L`, `O` or `U` (the characters a human voice
/// channel or a handwritten note confuses).
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A clock returning Unix milliseconds; the registry and the registries'
/// tests can replace it.
type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// How long a pairing code lives, in milliseconds.
fn pairing_ttl_ms() -> u64 {
    PAIRING_TTL.as_millis() as u64
}

// ── pairing codes ─────────────────────────────────────────────────────────

/// A one-time pairing code the hub shows the operator: eight Crockford base32
/// characters, displayed `XXXX-XXXX`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    /// The display form, `XXXX-XXXX`.
    pub code: String,
    /// Unix milliseconds after which the code is dead.
    pub expires_ms: u64,
}

/// Why the hub refused a pairing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingRefusal {
    /// No live code matches.
    BadCode,
    /// The code was already used, burned, or outlived its window.
    Expired,
    /// The node speaks another protocol version.
    Proto,
}

impl PairingRefusal {
    /// The refusal reason as it appears on the wire (`bad_code`, `expired`,
    /// `proto`).
    pub fn reason(self) -> &'static str {
        match self {
            PairingRefusal::BadCode => "bad_code",
            PairingRefusal::Expired => "expired",
            PairingRefusal::Proto => "proto",
        }
    }
}

impl std::fmt::Display for PairingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.reason())
    }
}

struct PairingSlot {
    /// The eight normalized characters.
    normalized: String,
    name_hint: String,
    expires_ms: u64,
    attempts: u32,
    used: bool,
}

/// The hub's live pairing codes (in memory only — a hub restart invalidates
/// them, which is fine: pairing happens while the hub is running).
pub struct PairingRegistry {
    slots: Mutex<Vec<PairingSlot>>,
    clock: Clock,
}

impl Default for PairingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PairingRegistry {
    /// A registry on the system clock.
    pub fn new() -> Self {
        Self::with_clock(system_now_ms)
    }

    /// A registry on a caller-supplied Unix-ms clock (tests pin time with it).
    pub fn with_clock(clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            clock: Box::new(clock),
        }
    }

    /// Mint a fresh single-use code for a node the operator is about to pair.
    /// `name_hint` is what the hub suggests as the node's name.
    pub fn issue_code(&self, name_hint: &str) -> PairingCode {
        let now = (self.clock)();
        let expires_ms = now.saturating_add(pairing_ttl_ms());
        let mut slots = self.slots.lock().unwrap_or_else(|poison| poison.into_inner());
        // Drop what can no longer be redeemed before adding the new code.
        slots.retain(|slot| !slot.used && slot.expires_ms > now);
        let mut slot = PairingSlot {
            normalized: String::new(),
            name_hint: name_hint.to_string(),
            expires_ms,
            attempts: 0,
            used: false,
        };
        loop {
            slot.normalized = random_code();
            if !slots.iter().any(|live| live.normalized == slot.normalized) {
                break;
            }
        }
        let display = format!("{}-{}", &slot.normalized[..4], &slot.normalized[4..]);
        slots.push(slot);
        PairingCode {
            code: display,
            expires_ms,
        }
    }

    /// Redeem a code (case- and hyphen-insensitive). A success consumes the
    /// code and returns the name hint; every failure is a failed try, and a
    /// code that exhausted [`PAIRING_ATTEMPTS`] of them is dead.
    pub fn redeem(&self, code: &str) -> Result<String, PairingRefusal> {
        let now = (self.clock)();
        let mut slots = self.slots.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(normalized) = normalize_pairing_code(code) {
            if let Some(index) = slots
                .iter()
                .position(|slot| auth::constant_time_eq(slot.normalized.as_bytes(), normalized.as_bytes()))
            {
                let slot = &mut slots[index];
                if slot.used
                    || slot.attempts >= PAIRING_ATTEMPTS
                    || now >= slot.expires_ms
                {
                    slot.used = true;
                    return Err(PairingRefusal::Expired);
                }
                slot.used = true;
                return Ok(slot.name_hint.clone());
            }
        }
        // A miss spends one try of every live code in this window.
        for slot in slots.iter_mut() {
            if !slot.used && slot.expires_ms > now {
                slot.attempts = slot.attempts.saturating_add(1);
            }
        }
        Err(PairingRefusal::BadCode)
    }
}

/// Eight random Crockford base32 characters (40 bits from the CSPRNG).
fn random_code() -> String {
    let raw = auth::random_32();
    raw[..8]
        .iter()
        .map(|byte| CROCKFORD[(byte & 0x1f) as usize] as char)
        .collect()
}

/// Normalize a typed pairing code to its eight canonical characters: drop
/// hyphens and whitespace, upper-case, and apply Crockford's look-alike
/// mapping (`O`→`0`, `I`/`L`→`1`). `None` when the input is not eight code
/// characters.
pub fn normalize_pairing_code(code: &str) -> Option<String> {
    let mut normalized = String::with_capacity(8);
    for ch in code.chars() {
        let ch = match ch {
            '-' | ' ' | '_' => continue,
            other => other.to_ascii_uppercase(),
        };
        let mapped = match ch {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        };
        if !CROCKFORD.contains(&(mapped as u8)) {
            return None;
        }
        normalized.push(mapped);
    }
    (normalized.len() == 8).then_some(normalized)
}

// ── the hub's node registry ───────────────────────────────────────────────

/// One paired node as the hub stores it in `<state_dir>/<node_id>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRecord {
    /// The node's 32-hex id (16 random bytes).
    pub node_id: String,
    /// The operator-visible name.
    pub name: String,
    /// `windows`, `macos` or `linux`.
    pub os: String,
    /// The node's 64-hex token.
    pub token: String,
    /// Unix milliseconds of pairing.
    pub created_ms: u64,
    /// Unix milliseconds of the last accepted connection.
    pub last_seen_ms: u64,
}

impl NodeRecord {
    /// The token as raw bytes, when the file carries a well-formed one.
    pub fn token_bytes(&self) -> Option<Token> {
        auth::parse_32_hex(&self.token)
    }
}

/// The hub's directory of paired nodes. One file per node; deleting the file
/// revokes the node.
pub struct NodeRegistry {
    dir: PathBuf,
    clock: Clock,
}

impl NodeRegistry {
    /// A registry over `state_dir` (created on first write).
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Self::with_clock(state_dir, system_now_ms)
    }

    /// A registry on a caller-supplied Unix-ms clock (tests pin time with it).
    pub fn with_clock(state_dir: impl Into<PathBuf>, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        Self {
            dir: state_dir.into(),
            clock: Box::new(clock),
        }
    }

    /// The registry directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file of one node.
    pub fn path(&self, node_id: &str) -> PathBuf {
        self.dir.join(format!("{node_id}.json"))
    }

    /// Mint a node id and token, and write the record for a node that just
    /// redeemed a pairing code.
    pub fn register(&self, name: &str, os: &str) -> Result<NodeRecord, String> {
        let now = (self.clock)();
        let record = NodeRecord {
            node_id: auth::hex_encode(&auth::random_32()[..16]),
            name: name.to_string(),
            os: os.to_string(),
            token: auth::hex_encode(&auth::random_token()),
            created_ms: now,
            last_seen_ms: now,
        };
        self.write(&record)?;
        Ok(record)
    }

    /// Read one node record; `Ok(None)` when the node is unknown or revoked.
    pub fn get(&self, node_id: &str) -> Result<Option<NodeRecord>, String> {
        if !is_node_id(node_id) {
            return Ok(None);
        }
        match std::fs::read_to_string(self.path(node_id)) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|error| format!("could not decode {}: {error}", self.path(node_id).display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("could not read {}: {error}", self.path(node_id).display())),
        }
    }

    /// The token of a known node, for the link handshake's token lookup.
    pub fn token(&self, node_id: &str) -> Option<Token> {
        self.get(node_id).ok().flatten().and_then(|record| record.token_bytes())
    }

    /// Note that the node just connected successfully.
    pub fn touch(&self, node_id: &str) -> Result<(), String> {
        let Some(mut record) = self.get(node_id)? else {
            return Err(format!("node {node_id} is not registered"));
        };
        record.last_seen_ms = (self.clock)();
        self.write(&record)
    }

    /// Revoke a node: delete its record. A missing file is not an error.
    pub fn revoke(&self, node_id: &str) -> Result<(), String> {
        if !is_node_id(node_id) {
            return Err(format!("not a node id: {node_id:?}"));
        }
        match std::fs::remove_file(self.path(node_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("could not remove {}: {error}", self.path(node_id).display())),
        }
    }

    /// Every registered node.
    pub fn list(&self) -> Result<Vec<NodeRecord>, String> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("could not read {}: {error}", self.dir.display())),
        };
        let mut records = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(node_id) = name.strip_suffix(".json") else {
                continue;
            };
            if !is_node_id(node_id) {
                continue;
            }
            if let Ok(mut record) = serde_json::from_str::<NodeRecord>(
                &std::fs::read_to_string(&path).unwrap_or_default(),
            ) {
                record.node_id = node_id.to_string();
                records.push(record);
            }
        }
        records.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(records)
    }

    fn write(&self, record: &NodeRecord) -> Result<(), String> {
        let path = self.path(&record.node_id);
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|error| format!("could not encode the node record: {error}"))?;
        write_private(&path, &bytes)
    }
}

/// True for a 32-lowercase-hex node id (16 bytes) — also the file-name guard
/// that keeps a crafted id from escaping the registry directory.
pub fn is_node_id(node_id: &str) -> bool {
    node_id.len() == 32
        && node_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// ── the node's identity file ──────────────────────────────────────────────

/// Everything a paired node needs to dial its hub, stored at
/// `<config dir>/node.json`. The only secret in it is the node token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeIdentity {
    /// The hub's `host:port`.
    pub hub: String,
    /// The node's 32-hex id.
    pub node_id: String,
    /// The node's 64-hex token.
    pub token: String,
    /// The hub's stable id (pinned at pairing; a different one is a different
    /// hub).
    pub hub_id: String,
    /// Unix milliseconds of pairing.
    pub paired_ms: u64,
}

impl NodeIdentity {
    /// The identity file inside a config dir.
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join(NODE_FILE)
    }

    /// Read the identity; `Ok(None)` when this node is not paired yet.
    pub fn load(config_dir: &Path) -> Result<Option<Self>, String> {
        let path = Self::path(config_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|error| format!("could not decode {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("could not read {}: {error}", path.display())),
        }
    }

    /// Write the identity user-only and atomically.
    pub fn save(&self, config_dir: &Path) -> Result<(), String> {
        let path = Self::path(config_dir);
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| format!("could not encode the node identity: {error}"))?;
        write_private(&path, &bytes)
    }

    /// The token as raw bytes.
    pub fn token_bytes(&self) -> Option<Token> {
        auth::parse_32_hex(&self.token)
    }
}

// ── the pairing handshake ──────────────────────────────────────────────────

/// Node side: redeem `code` with the hub at `hub` (`host:port`) and return the
/// identity to save. The returned identity's `hub_id` is the hub's, not the
/// hub address.
pub async fn pair_tcp(hub: &str, code: &str, name: &str) -> Result<NodeIdentity, RemoteError> {
    let stream = super::link::within_handshake(
        async {
            tokio::net::TcpStream::connect(hub).await.map_err(|error| {
                RemoteError::link_down(format!("could not reach {hub}: {error}"))
            })
        },
        super::link::HANDSHAKE_TIMEOUT,
    )
    .await?;
    pair_with(stream, hub, code, name).await
}

/// Node side: the same over an already-connected stream (the tests use
/// `tokio::io::duplex`).
pub async fn pair_with<S>(
    stream: S,
    hub: &str,
    code: &str,
    name: &str,
) -> Result<NodeIdentity, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    super::link::within_handshake(
        pair_within(stream, hub, code, name),
        super::link::HANDSHAKE_TIMEOUT,
    )
    .await
}

/// The upgrade and the `pair`/`paired` exchange, without the outer budget.
async fn pair_within<S>(
    stream: S,
    hub: &str,
    code: &str,
    name: &str,
) -> Result<NodeIdentity, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let url = format!("ws://{hub}{}", super::PATH);
    let (mut ws, _response) =
        async_tungstenite::tokio::client_async_with_config(url, stream, Some(super::link::ws_config()))
            .await
            .map_err(|error| RemoteError::link_down(format!("websocket handshake failed: {error}")))?;
    use async_tungstenite::tungstenite::Message;

    let pair = super::wire::Handshake::Pair {
        proto: super::PROTO,
        code: code.to_string(),
        name: name.to_string(),
        os: super::wire::os_name().to_string(),
        version: super::wire::version().to_string(),
    };
    let text = serde_json::to_string(&pair)
        .map_err(|error| RemoteError::internal(format!("could not encode the pair request: {error}")))?;
    ws.send(Message::text(text))
        .await
        .map_err(|error| RemoteError::link_down(format!("could not send the pair request: {error}")))?;
    let reply = super::link::read_handshake_reply(&mut ws).await?;
    match reply {
        super::wire::Handshake::Paired {
            node_id,
            token,
            hub_id,
        } => {
            if !is_node_id(&node_id) || auth::parse_32_hex(&token).is_none() {
                return Err(RemoteError::internal(
                    "the hub's pairing answer carries a malformed node id or token",
                ));
            }
            Ok(NodeIdentity {
                hub: hub.to_string(),
                node_id,
                token,
                hub_id,
                paired_ms: system_now_ms(),
            })
        }
        super::wire::Handshake::Refused { reason, .. } => Err(RemoteError::refused(reason)),
        other => Err(RemoteError::internal(format!(
            "unexpected answer to a pair request: {}",
            other.kind()
        ))),
    }
}

/// Hub side: accept one pairing connection on `stream`, redeem its code
/// against `registry`, and answer with a fresh node record. `Ok(None)` means
/// the code was refused (the refusal was sent); an `Err` means the transport
/// failed.
pub async fn serve_pairing<S>(
    stream: S,
    pairing: &PairingRegistry,
    nodes: &NodeRegistry,
    hub_id: &str,
) -> Result<Option<NodeRecord>, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    super::link::within_handshake(
        serve_pairing_within(stream, pairing, nodes, hub_id),
        super::link::HANDSHAKE_TIMEOUT,
    )
    .await
}

/// The upgrade and the `pair`/`paired` exchange, without the outer budget.
async fn serve_pairing_within<S>(
    stream: S,
    pairing: &PairingRegistry,
    nodes: &NodeRegistry,
    hub_id: &str,
) -> Result<Option<NodeRecord>, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut ws =
        async_tungstenite::tokio::accept_async_with_config(stream, Some(super::link::ws_config()))
            .await
            .map_err(|error| RemoteError::link_down(format!("websocket handshake failed: {error}")))?;
    let request = super::link::read_handshake_reply(&mut ws).await?;
    let result = match request {
        super::wire::Handshake::Pair {
            proto,
            code,
            name,
            os,
            ..
        } => {
            if proto != super::PROTO {
                super::link::send_handshake_refusal(
                    &mut ws,
                    PairingRefusal::Proto.reason(),
                    Some(super::PROTO),
                )
                .await?;
                return Ok(None);
            }
            match pairing.redeem(&code) {
                Ok(name_hint) => {
                    let name = if name.trim().is_empty() {
                        name_hint
                    } else {
                        name
                    };
                    let record = nodes.register(&name, &os).map_err(|error| {
                        RemoteError::internal(format!("could not register the node: {error}"))
                    })?;
                    let answer = super::wire::Handshake::Paired {
                        node_id: record.node_id.clone(),
                        token: record.token.clone(),
                        hub_id: hub_id.to_string(),
                    };
                    super::link::send_handshake(&mut ws, &answer).await?;
                    Ok(Some(record))
                }
                Err(refusal) => {
                    super::link::send_handshake_refusal(&mut ws, refusal.reason(), None).await?;
                    Ok(None)
                }
            }
        }
        other => Err(RemoteError::internal(format!(
            "unexpected pairing request: {}",
            other.kind()
        ))),
    };
    let _ = ws.close(None).await;
    result
}

// ── private file helpers ───────────────────────────────────────────────────

/// Create the file, restrict it to the current user, and only then write the
/// secret, so the bytes are never readable by anyone else; then rename the
/// temp file over the target so a reader never sees a half-written file. The
/// ordering mirrors the app's run-file writer
/// (`src-tauri/src/mnemo2/run_file.rs`), reimplemented here because the library
/// may not depend on the app crate.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;

    // Distinguishes concurrent writers of the same process (e.g. a `touch`
    // during a `register`) so their temp files never collide.
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    if let Err(error) = crate::config::keys::restrict_dir_permissions(dir) {
        eprintln!(
            "mnemo remote: could not restrict {}: {error}",
            dir.display()
        );
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|error| format!("could not create {}: {error}", temp.display()))?;
    if let Err(error) = crate::config::keys::restrict_permissions(&temp) {
        let _ = std::fs::remove_file(&temp);
        return Err(format!(
            "could not restrict {}: {error}",
            temp.display()
        ));
    }
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", temp.display()))?;
    drop(file);
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("could not replace {}: {error}", path.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn test_clock(start: u64) -> (Arc<AtomicU64>, impl Fn() -> u64 + Send + Sync + 'static) {
        let clock = Arc::new(AtomicU64::new(start));
        let reader = clock.clone();
        (clock, move || reader.load(Ordering::SeqCst))
    }

    /// A fresh directory under the checkout's `target/` — never `%TEMP%`.
    fn test_dir(name: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&root).expect("target dir");
        let dir = root.join(format!(
            "mnemo-remote-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    /// The file belongs to the current user only.
    fn assert_user_only(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{} is user-only", path.display());
        }
        // Windows: `crate::config::keys::restrict_permissions` owns the DACL
        // rewrite (and its own tests). Pin that it applies to exactly this file,
        // so a change in `write_private` that drops the call fails here.
        assert!(
            crate::config::keys::restrict_permissions(path).is_ok(),
            "{} accepts the user-only restriction",
            path.display()
        );
    }

    #[test]
    fn pairing_code_single_use() {
        let (_clock, now) = test_clock(1_000);
        let registry = PairingRegistry::with_clock(now);
        let issued = registry.issue_code("laptop");
        assert_eq!(issued.code.len(), 9, "XXXX-XXXX: {}", issued.code);
        assert_eq!(issued.expires_ms, 1_000 + 600_000);
        assert_eq!(registry.redeem(&issued.code), Ok("laptop".to_string()));
        // The second use is refused; case and hyphens do not matter.
        assert_eq!(
            registry.redeem(&issued.code.to_lowercase()),
            Err(PairingRefusal::Expired)
        );
        assert_eq!(
            registry.redeem(&issued.code.replace('-', "")),
            Err(PairingRefusal::Expired)
        );
    }

    #[test]
    fn pairing_code_expires() {
        let (clock, now) = test_clock(1_000);
        let registry = PairingRegistry::with_clock(now);
        let issued = registry.issue_code("laptop");
        clock.store(1_000 + 600_000, Ordering::SeqCst);
        assert_eq!(registry.redeem(&issued.code), Err(PairingRefusal::Expired));

        let fresh = registry.issue_code("desktop");
        // An unknown code is bad, not expired.
        assert_eq!(registry.redeem("ZZZZ-ZZZZ"), Err(PairingRefusal::BadCode));
        // The fresh code still works after the miss.
        assert_eq!(registry.redeem(&fresh.code), Ok("desktop".to_string()));
    }

    #[test]
    fn pairing_attempts_burn_code() {
        let (_clock, now) = test_clock(1_000);
        let registry = PairingRegistry::with_clock(now);
        let issued = registry.issue_code("laptop");
        for attempt in 1..=crate::remote::PAIRING_ATTEMPTS {
            assert_eq!(
                registry.redeem("ZZZZ-ZZZZ"),
                Err(PairingRefusal::BadCode),
                "wrong try {attempt}"
            );
        }
        // The right code is burned now.
        assert_eq!(registry.redeem(&issued.code), Err(PairingRefusal::Expired));
        // A newly issued code is unaffected.
        let fresh = registry.issue_code("laptop");
        assert_eq!(registry.redeem(&fresh.code), Ok("laptop".to_string()));
    }

    #[test]
    fn pairing_writes_user_only_files() {
        let dir = test_dir("pairing-files");
        let registry = NodeRegistry::new(&dir);
        let record = registry.register("worker", "windows").expect("registers");
        let file = registry.path(&record.node_id);
        assert!(file.is_file(), "{} exists", file.display());
        assert_user_only(&file);
        let stored = registry.get(&record.node_id).expect("reads").expect("found");
        assert_eq!(stored, record);
        assert_eq!(registry.token(&record.node_id), record.token_bytes());

        let identity = NodeIdentity {
            hub: "hub.example:8765".to_string(),
            node_id: record.node_id.clone(),
            token: record.token.clone(),
            hub_id: "hub-test".to_string(),
            paired_ms: 5,
        };
        identity.save(&dir).expect("saves the identity");
        assert_user_only(&NodeIdentity::path(&dir));
        assert_eq!(
            NodeIdentity::load(&dir).expect("loads"),
            Some(identity.clone())
        );
        assert_eq!(identity.token_bytes(), record.token_bytes());

        // Revoking deletes the record; the identity file stays the node's.
        registry.revoke(&record.node_id).expect("revokes");
        assert_eq!(registry.get(&record.node_id), Ok(None));
        assert_eq!(registry.list().expect("lists"), Vec::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pairing_code_normalization() {
        assert_eq!(
            normalize_pairing_code("abcd-efgh"),
            Some("ABCDEFGH".to_string())
        );
        assert_eq!(
            normalize_pairing_code("ABCD EFGH"),
            Some("ABCDEFGH".to_string())
        );
        // Crockford look-alikes.
        assert_eq!(
            normalize_pairing_code("o1il-2345"),
            Some("01112345".to_string())
        );
        assert_eq!(normalize_pairing_code("short"), None);
        assert_eq!(normalize_pairing_code("ABCD-EFGU"), None);
    }

    #[tokio::test]
    async fn pairing_round_trip_registers_the_node() {
        let dir = test_dir("pair-round-trip");
        let pairing = Arc::new(PairingRegistry::new());
        let nodes = Arc::new(NodeRegistry::new(&dir));

        // A wrong code is refused with bad_code.
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = {
            let pairing = pairing.clone();
            let nodes = nodes.clone();
            tokio::spawn(async move { serve_pairing(hub_io, &pairing, &nodes, "hub-test").await })
        };
        let error = pair_with(node_io, "duplex", "ZZZZ-ZZZZ", "node-a")
            .await
            .expect_err("a wrong code is refused");
        assert_eq!(error.code, "bad_code");
        let refused = hub.await.expect("hub task joins").expect("transport ok");
        assert_eq!(refused, None);

        // The real code pairs and registers the node.
        let code = pairing.issue_code("hint");
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = {
            let pairing = pairing.clone();
            let nodes = nodes.clone();
            tokio::spawn(async move { serve_pairing(hub_io, &pairing, &nodes, "hub-test").await })
        };
        let identity = pair_with(node_io, "duplex", &code.code, "node-a")
            .await
            .expect("pairs");
        let record = hub
            .await
            .expect("hub task joins")
            .expect("transport ok")
            .expect("paired");
        assert_eq!(identity.node_id, record.node_id);
        assert_eq!(identity.token, record.token);
        assert_eq!(identity.hub_id, "hub-test");
        assert_eq!(identity.hub, "duplex");
        assert_eq!(record.name, "node-a");
        assert_eq!(record.os, crate::remote::wire::os_name());
        assert_eq!(nodes.get(&record.node_id).expect("reads"), Some(record.clone()));
        assert_eq!(nodes.token(&record.node_id), identity.token_bytes());

        // The identity lands on disk user-only and loads back.
        identity.save(&dir).expect("saves");
        assert_user_only(&NodeIdentity::path(&dir));
        assert_eq!(NodeIdentity::load(&dir).expect("loads"), Some(identity));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
