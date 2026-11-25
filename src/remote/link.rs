// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The multiplexed, authenticated link: one WebSocket, calls and event
//! streams in both directions, MAC and sequence enforcement on every frame.
//!
//! A [`Link`] is cheap to clone and `Send + Sync`; the socket is owned by a
//! reader task and a writer task. Calls allocate an id per originator, send a
//! `req` envelope and wait for the matching `res` (and, for a streaming method,
//! numbered `item` frames until `end`). Inbound requests run as their own
//! tasks, so a slow handler never stalls the reader; an inbound `cancel`
//! aborts the handler task.
//!
//! The handshake ([`dial`] on the node, [`accept`] on the hub) is plain JSON:
//! `hello` → `challenge` → `auth` → `welcome`, with mutual HMAC proofs of the
//! per-node token and the session key derived from the two nonces. A bad MAC,
//! a replayed or skipped sequence number, a malformed frame, or a dead peer
//! (no inbound frame for [`super::DEAD_AFTER`]) closes the link and fails every
//! pending call with `link_down`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use async_tungstenite::tungstenite::protocol::WebSocketConfig;
use async_tungstenite::tungstenite::{Bytes, Error as WsError, Message};
use futures::stream::{SplitSink, SplitStream};
use futures::{Sink, SinkExt, Stream, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::AbortHandle;

use super::auth;
use super::pairing::NodeIdentity;
use super::wire::{self, Envelope, Frame, Handshake};
use super::{RemoteError, DEAD_AFTER, MAX_FRAME, MAX_INFLIGHT, MAX_MESSAGE, PATH, PING, PROTO};

/// How long the plain pre-authentication handshake — the WebSocket upgrade
/// plus every JSON frame of it — may take before the peer is declared dead.
/// Not a wire constant; a stuck socket must not pin a task.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Capacity of the outbound envelope queue. A caller that fills it waits (a
/// live link drains it); `note` is best effort and drops instead.
const OUTBOX: usize = 1024;

/// Capacity of one stream's item queue. A consumer that stops reading gets
/// the stream failed and cancelled rather than stalling the reader task.
const STREAM_ITEMS: usize = 1024;

/// One streamed item: `n` numbers the items from 1 per call.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// The item number, starting at 1.
    pub n: u64,
    /// The item payload.
    pub d: Value,
}

/// The terminal state of one stream, recorded by the reader before the item
/// channel closes. A channel that closes without one is reported as
/// `link_down`, never as a clean end.
enum StreamTerminal {
    Clean,
    Error(RemoteError),
}

/// The state of one stream that the reader and its [`StreamRx`] share: the
/// terminal outcome and the next item number the peer must send.
struct StreamState {
    terminal: Mutex<Option<StreamTerminal>>,
    /// The number of the last item accepted (0 before the first).
    next_n: AtomicU64,
}

impl StreamState {
    fn new() -> Self {
        Self {
            terminal: Mutex::new(None),
            next_n: AtomicU64::new(0),
        }
    }

    /// Record the terminal outcome. The first one wins; a later one is a
    /// no-op, so a race between a clean `end` and a link failure keeps the
    /// outcome that was seen first.
    fn finish(&self, terminal: StreamTerminal) {
        let mut slot = self
            .terminal
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if slot.is_none() {
            *slot = Some(terminal);
        }
    }

    fn take(&self) -> Option<StreamTerminal> {
        self.terminal
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }
}

/// The queue half of one stream: the item channel plus the shared state, so the
/// reader can hand out items and later terminate the stream through the same
/// entry.
#[derive(Clone)]
struct StreamQueue {
    items: mpsc::Sender<Item>,
    state: Arc<StreamState>,
}

/// The receiver half of a streaming call. Yields `Some(Ok(item))` per item,
/// then either `None` (the peer ended the stream cleanly) or one final
/// `Some(Err(..))` (the peer ended with an error, the stream overflowed, the
/// peer violated the stream protocol, or the link went down). A channel that
/// closes without an `end` is always an error, never a clean end. Dropping it
/// before the end cancels the call on the peer.
pub struct StreamRx {
    inner: Arc<LinkInner>,
    id: u64,
    rx: mpsc::Receiver<Item>,
    state: Arc<StreamState>,
    ended: bool,
}

impl StreamRx {
    /// The call id this stream answers.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The next item, the end of the stream, or the terminal error.
    pub async fn next(&mut self) -> Option<Result<Item, RemoteError>> {
        if self.ended {
            return None;
        }
        match self.rx.recv().await {
            Some(item) => Some(Ok(item)),
            None => {
                self.ended = true;
                match self.state.take() {
                    Some(StreamTerminal::Clean) => None,
                    Some(StreamTerminal::Error(error)) => Some(Err(error)),
                    None => Some(Err(RemoteError::link_down(
                        "the stream ended without an end frame",
                    ))),
                }
            }
        }
    }
}

impl Drop for StreamRx {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&self.id);
        let _ = self
            .inner
            .outbound
            .try_send(Outbound::Envelope(Envelope::cancel(self.id)));
    }
}

/// The handler-side half of a streamed request, handed to a
/// [`LinkHandler::request`].
///
/// A streaming handler calls [`StreamTx::start`] once with the `res` payload,
/// then [`StreamTx::item`] per element; the link sends `end` when the handler
/// returns. A handler that never calls `start` is an ordinary call: its return
/// value becomes the `res` payload.
#[derive(Clone)]
pub struct StreamTx {
    inner: Arc<LinkInner>,
    id: u64,
    next_n: Arc<AtomicU64>,
    started: Arc<AtomicBool>,
}

impl StreamTx {
    fn new(inner: Arc<LinkInner>, id: u64) -> Self {
        Self {
            inner,
            id,
            next_n: Arc::new(AtomicU64::new(0)),
            started: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The inbound call id.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Send the `res` payload that opens the stream. Exactly once; a second
    /// call fails with `internal`.
    pub async fn start(&self, value: Value) -> Result<(), RemoteError> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(RemoteError::internal("the stream was already started"));
        }
        self.inner
            .send(Envelope::res_ok(self.id, value))
            .await
    }

    /// Send the next numbered item. A stream must be [`StreamTx::start`]ed
    /// before its first item.
    pub async fn item(&self, data: Value) -> Result<(), RemoteError> {
        if !self.started.load(Ordering::SeqCst) {
            return Err(RemoteError::internal(
                "a stream must be started before its first item",
            ));
        }
        let n = self.next_n.fetch_add(1, Ordering::SeqCst) + 1;
        self.inner.send(Envelope::item(self.id, n, data)).await
    }
}

/// The answering side of a link: one handler per link, called for every
/// inbound request (in its own task) and every inbound note (inline, so it
/// must be quick).
#[async_trait]
pub trait LinkHandler: Send + Sync + 'static {
    /// Answer an inbound call. Return the `res` payload; a streaming handler
    /// calls [`StreamTx::start`] first and then streams items.
    async fn request(&self, m: &str, p: Value, out: StreamTx) -> Result<Value, RemoteError>;

    /// Handle a best-effort inbound note. Never answered; must not block.
    fn note(&self, m: &str, p: Value);
}

/// One outbound frame awaiting the writer task.
enum Outbound {
    Envelope(Envelope),
}

/// A pending call or stream on the requester's side.
enum Pending {
    /// A plain call waiting for its `res`.
    Call(oneshot::Sender<Result<Value, RemoteError>>),
    /// A streaming call waiting for its `res`; the item queue is already open.
    StreamInit(oneshot::Sender<Result<Value, RemoteError>>, StreamQueue),
    /// A streaming call whose `res` arrived; items flow until `end`.
    Stream(StreamQueue),
}

/// The hub's credentials accepted by [`accept`]: its id, its current epoch,
/// and the token lookup backed by the node registry.
pub struct HubAuth {
    /// The hub's stable id.
    pub hub_id: String,
    /// The hub server's epoch — fresh per `HubServer` start, so a node can
    /// tell a restarted hub from the old one.
    pub hub_epoch: String,
    /// Node id → token, `None` for an unknown (or revoked) node.
    pub tokens: TokenLookup,
}

/// Node id → token lookup used by the hub handshake.
pub type TokenLookup = Arc<dyn Fn(&str) -> Option<auth::Token> + Send + Sync>;

impl HubAuth {
    /// Build the hub-side handshake credentials.
    pub fn new(
        hub_id: impl Into<String>,
        hub_epoch: impl Into<String>,
        tokens: TokenLookup,
    ) -> Self {
        Self {
            hub_id: hub_id.into(),
            hub_epoch: hub_epoch.into(),
            tokens,
        }
    }

    /// Build them over a [`super::pairing::NodeRegistry`].
    pub fn with_registry(
        hub_id: impl Into<String>,
        hub_epoch: impl Into<String>,
        registry: Arc<super::pairing::NodeRegistry>,
    ) -> Self {
        Self::new(hub_id, hub_epoch, Arc::new(move |node_id| registry.token(node_id)))
    }

    /// The token of `node_id`, when the hub knows it.
    pub fn token(&self, node_id: &str) -> Option<auth::Token> {
        (self.tokens)(node_id)
    }
}

/// The shared state of one established link.
struct LinkInner {
    session_key: [u8; 32],
    /// Direction byte stamped on outgoing frames.
    out_dir: u8,
    /// Direction byte expected on incoming frames.
    in_dir: u8,
    peer_epoch: String,
    handler: Arc<dyn LinkHandler>,
    outbound: mpsc::Sender<Outbound>,
    pending: Mutex<HashMap<u64, Pending>>,
    inbound: Mutex<HashMap<u64, AbortHandle>>,
    next_id: AtomicU64,
    /// Number of live [`Link`] handles; the link shuts down when the last one
    /// drops.
    refs: AtomicUsize,
    /// Set once when the link failed; lets [`Link::closed`] resolve.
    failed: AtomicBool,
    closed: watch::Sender<bool>,
    shutdown: watch::Sender<bool>,
}

impl LinkInner {
    /// Queue one envelope for the writer. Fails with `link_down` when the link
    /// is gone.
    async fn send(&self, envelope: Envelope) -> Result<(), RemoteError> {
        self.outbound
            .send(Outbound::Envelope(envelope))
            .await
            .map_err(|_| RemoteError::link_down("the link is closed"))
    }

    /// Fail the link once: wake `closed()` waiters, fail every pending call
    /// and stream, abort the inbound handlers, and stop the tasks.
    fn fail(&self, error: RemoteError) {
        if self.failed.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.closed.send(true);
        let _ = self.shutdown.send(true);
        let pending: Vec<Pending> = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            pending.drain().map(|(_, entry)| entry).collect()
        };
        for entry in pending {
            match entry {
                Pending::Call(tx) => {
                    let _ = tx.send(Err(error.clone()));
                }
                Pending::StreamInit(tx, _queue) => {
                    let _ = tx.send(Err(error.clone()));
                }
                Pending::Stream(queue) => {
                    queue.state.finish(StreamTerminal::Error(error.clone()));
                    drop(queue.items);
                }
            }
        }
        let inbound: Vec<AbortHandle> = {
            let mut inbound = self
                .inbound
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            inbound.drain().map(|(_, handle)| handle).collect()
        };
        for handle in inbound {
            handle.abort();
        }
    }
}

/// One established link. Clone it to share, drop the last clone to close it.
pub struct Link {
    inner: Arc<LinkInner>,
}

impl Clone for Link {
    fn clone(&self) -> Self {
        self.inner.refs.fetch_add(1, Ordering::SeqCst);
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if self.inner.refs.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.inner
                .fail(RemoteError::link_down("the link handle was dropped"));
        }
    }
}

impl Link {
    /// Call `m` and wait for the answer, at most `timeout`. The timeout starts
    /// the moment the request is queued.
    pub async fn call(
        &self,
        m: &str,
        p: Value,
        timeout: Duration,
    ) -> Result<Value, RemoteError> {
        if *self.inner.closed.borrow() {
            return Err(RemoteError::link_down("the link is closed"));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id, Pending::Call(tx));
        // The link can fail between the `closed` check above and the insert:
        // `fail` drains the map first, so an entry inserted after that drain
        // would never be answered. Re-check the flag now that the entry is
        // visible to a concurrent drain.
        if self.inner.failed.load(Ordering::SeqCst) {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            return Err(RemoteError::link_down("the link is closed"));
        }
        if let Err(error) = self.inner.send(Envelope::req(id, m, p)).await {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RemoteError::link_down(
                "the link dropped the pending call",
            )),
            Err(_) => {
                self.inner
                    .pending
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&id);
                let _ = self
                    .inner
                    .outbound
                    .try_send(Outbound::Envelope(Envelope::cancel(id)));
                Err(RemoteError::timeout(format!(
                    "no answer to {m:?} within {timeout:?}"
                )))
            }
        }
    }

    /// Open a streaming call: the returned value is the method's `res`
    /// payload, and the [`StreamRx`] yields the items until `end`.
    pub async fn call_stream(
        &self,
        m: &str,
        p: Value,
    ) -> Result<(Value, StreamRx), RemoteError> {
        if *self.inner.closed.borrow() {
            return Err(RemoteError::link_down("the link is closed"));
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (res_tx, res_rx) = oneshot::channel();
        let (items_tx, items_rx) = mpsc::channel(STREAM_ITEMS);
        let state = Arc::new(StreamState::new());
        self.inner
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                id,
                Pending::StreamInit(
                    res_tx,
                    StreamQueue {
                        items: items_tx,
                        state: state.clone(),
                    },
                ),
            );
        // Same window as `call`: a failure between the `closed` check and the
        // insert would otherwise leave the stream waiting forever.
        if self.inner.failed.load(Ordering::SeqCst) {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            return Err(RemoteError::link_down("the link is closed"));
        }
        if let Err(error) = self.inner.send(Envelope::req(id, m, p)).await {
            self.inner
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            return Err(error);
        }
        let result = match res_rx.await {
            Ok(result) => result,
            Err(_) => return Err(RemoteError::link_down("the link dropped the stream")),
        };
        match result {
            Ok(value) => Ok((
                value,
                StreamRx {
                    inner: self.inner.clone(),
                    id,
                    rx: items_rx,
                    state,
                    ended: false,
                },
            )),
            Err(error) => Err(error),
        }
    }

    /// Push a best-effort one-way note. Dropped silently when the outbound
    /// queue is full or the link is gone.
    pub fn notify(&self, m: &str, p: Value) {
        let _ = self
            .inner
            .outbound
            .try_send(Outbound::Envelope(Envelope::note(m, p)));
    }

    /// Resolves when the link is down (peer gone, protocol violation, or the
    /// last handle dropped).
    pub fn closed(&self) -> impl Future<Output = ()> + '_ {
        let mut closed = self.inner.closed.subscribe();
        async move {
            loop {
                if *closed.borrow_and_update() {
                    return;
                }
                if closed.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    /// The hub epoch announced on this link (on the node: the peer's; on the
    /// hub: its own, recorded for symmetry).
    pub fn peer_epoch(&self) -> &str {
        &self.inner.peer_epoch
    }
}

/// Tracks whether a node currently has a live link, and since when it is down.
/// [`LinkSlot::wait_up`] gives each outage one budget, measured from when the
/// link went down rather than from when the wait started.
pub struct LinkSlot {
    tx: watch::Sender<Option<Link>>,
    down_since: Mutex<Option<tokio::time::Instant>>,
}

impl Default for LinkSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkSlot {
    /// An empty slot (no link, considered down since creation).
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(None);
        Self {
            tx,
            down_since: Mutex::new(Some(tokio::time::Instant::now())),
        }
    }

    /// Publish the current link, or `None` when it dropped. Going down records
    /// the outage start once; a redundant `None` does not restart the budget.
    pub fn set(&self, link: Option<Link>) {
        {
            let mut down_since = self
                .down_since
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match link {
                Some(_) => *down_since = None,
                None => {
                    if down_since.is_none() {
                        *down_since = Some(tokio::time::Instant::now());
                    }
                }
            }
        }
        self.tx.send_replace(link);
    }

    /// The live link, if any.
    pub fn current(&self) -> Option<Link> {
        self.tx.borrow().clone()
    }

    /// Whether a link is currently published.
    pub fn is_up(&self) -> bool {
        self.tx.borrow().is_some()
    }

    /// Wait for a link, at most until `down_since + budget`. Returns `None`
    /// when the outage outlasted the budget (immediately, if it already did).
    pub async fn wait_up(&self, budget: Duration) -> Option<Link> {
        let mut rx = self.tx.subscribe();
        let current = rx.borrow_and_update().clone();
        if let Some(link) = current {
            return Some(link);
        }
        let deadline = self
            .down_since
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .map(|since| since + budget)
            .unwrap_or_else(|| tokio::time::Instant::now() + budget);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, rx.changed()).await {
                Ok(Ok(())) => {
                    let current = rx.borrow_and_update().clone();
                    if let Some(link) = current {
                        return Some(link);
                    }
                }
                // The sender is gone (the slot was dropped) or the budget ran
                // out.
                _ => return None,
            }
        }
    }
}

// ── handshake ──────────────────────────────────────────────────────────────

/// The WebSocket limits every link and pairing connection is created with.
pub(crate) fn ws_config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_MESSAGE);
    config.max_frame_size = Some(MAX_FRAME);
    config
}

/// Bound one whole handshake — the WebSocket upgrade plus the plain-JSON
/// exchange — by `budget`.
pub(crate) async fn within_handshake<F, T>(future: F, budget: Duration) -> Result<T, RemoteError>
where
    F: Future<Output = Result<T, RemoteError>>,
{
    tokio::time::timeout(budget, future).await.map_err(|_| {
        RemoteError::link_down(format!("the handshake did not finish within {budget:?}"))
    })?
}

/// Node side: dial the hub in `identity.hub` and authenticate.
pub async fn dial(
    identity: &NodeIdentity,
    handler: Arc<dyn LinkHandler>,
) -> Result<Link, RemoteError> {
    let address = identity.hub.clone();
    let stream = within_handshake(
        async move {
            tokio::net::TcpStream::connect(&address)
                .await
                .map_err(|error| {
                    RemoteError::link_down(format!("could not reach {address}: {error}"))
                })
        },
        HANDSHAKE_TIMEOUT,
    )
    .await?;
    dial_on(stream, identity, handler).await
}

/// Node side over an already-connected stream (the tests use
/// `tokio::io::duplex`).
pub async fn dial_on<S>(
    stream: S,
    identity: &NodeIdentity,
    handler: Arc<dyn LinkHandler>,
) -> Result<Link, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    dial_on_within(stream, identity, handler, HANDSHAKE_TIMEOUT).await
}

/// [`dial_on`] with an explicit whole-handshake budget (the tests shrink it).
async fn dial_on_within<S>(
    stream: S,
    identity: &NodeIdentity,
    handler: Arc<dyn LinkHandler>,
    budget: Duration,
) -> Result<Link, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    within_handshake(dial_handshake(stream, identity, handler), budget).await
}

/// The upgrade and the `hello`/`challenge`/`auth`/`welcome` exchange.
async fn dial_handshake<S>(
    stream: S,
    identity: &NodeIdentity,
    handler: Arc<dyn LinkHandler>,
) -> Result<Link, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let token = identity
        .token_bytes()
        .ok_or_else(|| RemoteError::internal("the node identity carries a malformed token"))?;
    let url = format!("ws://{}{PATH}", identity.hub);
    let (mut ws, _response) =
        async_tungstenite::tokio::client_async_with_config(url, stream, Some(ws_config()))
            .await
            .map_err(|error| {
                RemoteError::link_down(format!("websocket handshake failed: {error}"))
            })?;

    let node_nonce = auth::random_nonce();
    let hello = Handshake::Hello {
        proto: PROTO,
        node_id: identity.node_id.clone(),
        nonce: auth::hex_encode(&node_nonce),
        version: wire::version().to_string(),
        os: wire::os_name().to_string(),
    };
    send_handshake(&mut ws, &hello).await?;

    let hub_nonce = match read_handshake_reply(&mut ws).await? {
        Handshake::Challenge { nonce } => auth::parse_32_hex(&nonce)
            .ok_or_else(|| RemoteError::internal("the hub's challenge nonce is malformed"))?,
        Handshake::Refused { reason, .. } => return Err(RemoteError::refused(reason)),
        other => {
            return Err(RemoteError::internal(format!(
                "unexpected handshake message: {}",
                other.kind()
            )))
        }
    };

    let auth_message = Handshake::Auth {
        proof: auth::node_proof(&token, &node_nonce, &hub_nonce),
    };
    send_handshake(&mut ws, &auth_message).await?;

    let (proof, hub_epoch) = match read_handshake_reply(&mut ws).await? {
        Handshake::Welcome {
            proof,
            hub_id: _hub_id,
            hub_epoch,
        } => (proof, hub_epoch),
        Handshake::Refused { reason, .. } => return Err(RemoteError::refused(reason)),
        other => {
            return Err(RemoteError::internal(format!(
                "unexpected handshake message: {}",
                other.kind()
            )))
        }
    };
    if !auth::verify_proof(
        &proof,
        &auth::hub_proof(&token, &node_nonce, &hub_nonce),
    ) {
        let _ = ws.close(None).await;
        return Err(RemoteError::new(
            "bad_proof",
            "the hub could not prove the node token",
        ));
    }

    let session_key = auth::session_key(&token, &node_nonce, &hub_nonce);
    Ok(start_tasks(
        ws,
        session_key,
        auth::DIR_NODE_TO_HUB,
        auth::DIR_HUB_TO_NODE,
        hub_epoch,
        handler,
    ))
}

/// Hub side: accept one node connection on `stream` and authenticate it.
pub async fn accept<S>(
    stream: S,
    authz: HubAuth,
    handler: Arc<dyn LinkHandler>,
) -> Result<Link, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    accept_on(stream, authz, handler).await
}

/// Hub side over an already-connected stream (the tests use
/// `tokio::io::duplex`).
pub async fn accept_on<S>(
    stream: S,
    authz: HubAuth,
    handler: Arc<dyn LinkHandler>,
) -> Result<Link, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    within_handshake(accept_handshake(stream, authz, handler), HANDSHAKE_TIMEOUT).await
}

/// The upgrade and the `hello`/`challenge`/`auth`/`welcome` exchange.
async fn accept_handshake<S>(
    stream: S,
    authz: HubAuth,
    handler: Arc<dyn LinkHandler>,
) -> Result<Link, RemoteError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let mut ws =
        async_tungstenite::tokio::accept_async_with_config(stream, Some(ws_config()))
            .await
            .map_err(|error| {
                RemoteError::link_down(format!("websocket handshake failed: {error}"))
            })?;

    let (node_id, node_nonce) = match read_handshake_reply(&mut ws).await? {
        Handshake::Hello {
            proto,
            node_id,
            nonce,
            ..
        } => {
            if proto != PROTO {
                send_handshake_refusal(&mut ws, "proto", Some(PROTO)).await?;
                let _ = ws.close(None).await;
                return Err(RemoteError::refused("proto"));
            }
            let node_nonce = auth::parse_32_hex(&nonce)
                .ok_or_else(|| RemoteError::internal("the node's nonce is malformed"))?;
            (node_id, node_nonce)
        }
        other => {
            return Err(RemoteError::internal(format!(
                "unexpected handshake message: {}",
                other.kind()
            )))
        }
    };

    let Some(token) = authz.token(&node_id) else {
        send_handshake_refusal(&mut ws, "unknown_node", None).await?;
        let _ = ws.close(None).await;
        return Err(RemoteError::refused("unknown_node"));
    };

    let hub_nonce = auth::random_nonce();
    let challenge = Handshake::Challenge {
        nonce: auth::hex_encode(&hub_nonce),
    };
    send_handshake(&mut ws, &challenge).await?;

    let proof = match read_handshake_reply(&mut ws).await? {
        Handshake::Auth { proof } => proof,
        other => {
            return Err(RemoteError::internal(format!(
                "unexpected handshake message: {}",
                other.kind()
            )))
        }
    };
    if !auth::verify_proof(
        &proof,
        &auth::node_proof(&token, &node_nonce, &hub_nonce),
    ) {
        send_handshake_refusal(&mut ws, "bad_proof", None).await?;
        let _ = ws.close(None).await;
        return Err(RemoteError::refused("bad_proof"));
    }

    let welcome = Handshake::Welcome {
        proof: auth::hub_proof(&token, &node_nonce, &hub_nonce),
        hub_id: authz.hub_id.clone(),
        hub_epoch: authz.hub_epoch.clone(),
    };
    send_handshake(&mut ws, &welcome).await?;

    let session_key = auth::session_key(&token, &node_nonce, &hub_nonce);
    Ok(start_tasks(
        ws,
        session_key,
        auth::DIR_HUB_TO_NODE,
        auth::DIR_NODE_TO_HUB,
        authz.hub_epoch,
        handler,
    ))
}

/// Read one plain-JSON handshake message, bounded by [`HANDSHAKE_TIMEOUT`].
pub(crate) async fn read_handshake_reply<W>(ws: &mut W) -> Result<Handshake, RemoteError>
where
    W: Stream<Item = Result<Message, WsError>> + Unpin,
{
    let message = tokio::time::timeout(HANDSHAKE_TIMEOUT, ws.next())
        .await
        .map_err(|_| RemoteError::link_down("the peer did not answer the handshake in time"))?
        .ok_or_else(|| {
            RemoteError::link_down("the peer closed the connection during the handshake")
        })?
        .map_err(|error| {
            RemoteError::link_down(format!("websocket error during the handshake: {error}"))
        })?;
    let Message::Text(text) = message else {
        return Err(RemoteError::internal(format!(
            "a handshake must be a text message, got a {}",
            message_summary(&message)
        )));
    };
    wire::parse_handshake(&text).map_err(RemoteError::internal)
}

/// A message's kind and byte length — never its payload, so a stray binary
/// message cannot put attacker-chosen bytes into a log line or an error.
fn message_summary(message: &Message) -> String {
    match message {
        Message::Text(text) => format!("text message ({} bytes)", text.len()),
        Message::Binary(bytes) => format!("binary message ({} bytes)", bytes.len()),
        Message::Ping(bytes) => format!("ping ({} bytes)", bytes.len()),
        Message::Pong(bytes) => format!("pong ({} bytes)", bytes.len()),
        Message::Close(_) => "close message".to_string(),
        Message::Frame(_) => "websocket frame".to_string(),
    }
}

/// Send one plain-JSON handshake message.
pub(crate) async fn send_handshake<W>(ws: &mut W, message: &Handshake) -> Result<(), RemoteError>
where
    W: Sink<Message, Error = WsError> + Unpin,
{
    let text = serde_json::to_string(message)
        .map_err(|error| RemoteError::internal(format!("could not encode a handshake: {error}")))?;
    if text.len() > MAX_MESSAGE {
        return Err(RemoteError::internal("a handshake message is too large"));
    }
    ws.send(Message::text(text))
        .await
        .map_err(|error| RemoteError::link_down(format!("could not send a handshake: {error}")))
}

/// Send a `refused` handshake message and nothing else.
pub(crate) async fn send_handshake_refusal<W>(
    ws: &mut W,
    reason: &str,
    proto: Option<u32>,
) -> Result<(), RemoteError>
where
    W: Sink<Message, Error = WsError> + Unpin,
{
    send_handshake(
        ws,
        &Handshake::Refused {
            reason: reason.to_string(),
            proto,
        },
    )
    .await
}

/// Split an authenticated socket into its reader and writer tasks and hand
/// back the [`Link`].
fn start_tasks<W>(
    ws: W,
    session_key: [u8; 32],
    out_dir: u8,
    in_dir: u8,
    peer_epoch: String,
    handler: Arc<dyn LinkHandler>,
) -> Link
where
    W: Stream<Item = Result<Message, WsError>>
        + Sink<Message, Error = WsError>
        + Unpin
        + Send
        + 'static,
{
    let (sink, stream) = ws.split();
    let (outbound, outbound_rx) = mpsc::channel(OUTBOX);
    let (closed, _) = watch::channel(false);
    let (shutdown, shutdown_rx) = watch::channel(false);
    let inner = Arc::new(LinkInner {
        session_key,
        out_dir,
        in_dir,
        peer_epoch,
        handler,
        outbound,
        pending: Mutex::new(HashMap::new()),
        inbound: Mutex::new(HashMap::new()),
        next_id: AtomicU64::new(1),
        refs: AtomicUsize::new(1),
        failed: AtomicBool::new(false),
        closed,
        shutdown,
    });
    tokio::spawn(writer_task(
        sink,
        inner.clone(),
        outbound_rx,
        shutdown_rx.clone(),
    ));
    tokio::spawn(reader_task(stream, inner.clone(), shutdown_rx));
    Link { inner }
}

/// Own the socket's write half: MAC and send envelopes, ping on idle, and send
/// the close frame when the link shuts down.
async fn writer_task<W>(
    mut sink: SplitSink<W, Message>,
    inner: Arc<LinkInner>,
    mut outbound: mpsc::Receiver<Outbound>,
    mut shutdown: watch::Receiver<bool>,
) where
    W: Sink<Message, Error = WsError> + Unpin,
{
    let mut seq = 1u64;
    let mut ping = tokio::time::interval(PING);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => {
                let _ = sink.send(Message::Close(None)).await;
                break;
            }
            outgoing = outbound.recv() => match outgoing {
                Some(Outbound::Envelope(envelope)) => {
                    match encode_frame(&inner, seq, &envelope) {
                        Ok(text) => {
                            seq += 1;
                            if let Err(error) = sink.send(Message::text(text)).await {
                                inner.fail(RemoteError::link_down(format!(
                                    "could not write to the link: {error}"
                                )));
                                break;
                            }
                        }
                        Err(error) => {
                            inner.fail(error);
                            break;
                        }
                    }
                }
                None => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            },
            _ = ping.tick() => {
                if let Err(error) = sink.send(Message::Ping(Bytes::new())).await {
                    inner.fail(RemoteError::link_down(format!(
                        "could not ping the link: {error}"
                    )));
                    break;
                }
            }
        }
    }
}

/// Build one MACed text frame.
fn encode_frame(inner: &LinkInner, seq: u64, envelope: &Envelope) -> Result<String, RemoteError> {
    let json = serde_json::to_string(envelope)
        .map_err(|error| RemoteError::internal(format!("could not encode an envelope: {error}")))?;
    if json.len() > MAX_MESSAGE {
        return Err(RemoteError::internal(format!(
            "an envelope of {} bytes exceeds the {MAX_MESSAGE}-byte message limit",
            json.len()
        )));
    }
    let mac = auth::frame_mac(&inner.session_key, inner.out_dir, seq, json.as_bytes());
    Ok(Frame::new(seq, mac, json).encode())
}

/// Own the socket's read half: verify and dispatch frames, watch peer
/// liveness.
async fn reader_task<W>(
    mut stream: SplitStream<W>,
    inner: Arc<LinkInner>,
    mut shutdown: watch::Receiver<bool>,
) where
    W: Stream<Item = Result<Message, WsError>> + Unpin,
{
    let mut expected_seq = 1u64;
    let mut last_rx = tokio::time::Instant::now();
    let mut watchdog = tokio::time::interval(PING);
    watchdog.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            _ = watchdog.tick() => {
                if last_rx.elapsed() > DEAD_AFTER {
                    inner.fail(RemoteError::link_down(format!(
                        "the peer sent nothing for {DEAD_AFTER:?}"
                    )));
                    return;
                }
            }
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => {
                    last_rx = tokio::time::Instant::now();
                    if let Err(error) = dispatch_frame(&inner, &mut expected_seq, &text).await {
                        eprintln!("mnemo remote link closed: {error}");
                        inner.fail(error);
                        return;
                    }
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {
                    last_rx = tokio::time::Instant::now();
                }
                Some(Ok(Message::Close(_))) => {
                    inner.fail(RemoteError::link_down("the peer closed the link"));
                    return;
                }
                Some(Ok(Message::Binary(bytes))) => {
                    inner.fail(RemoteError::link_down(format!(
                        "the link expects text frames only, got a binary message ({} bytes)",
                        bytes.len()
                    )));
                    return;
                }
                Some(Ok(Message::Frame(_))) => {}
                Some(Err(error)) => {
                    inner.fail(RemoteError::link_down(format!("the link failed: {error}")));
                    return;
                }
                None => {
                    inner.fail(RemoteError::link_down("the connection ended"));
                    return;
                }
            },
        }
    }
}

/// Verify one frame's sequence number and MAC, then dispatch its envelope.
async fn dispatch_frame(
    inner: &Arc<LinkInner>,
    expected_seq: &mut u64,
    text: &str,
) -> Result<(), RemoteError> {
    let frame = Frame::parse(text)
        .map_err(|reason| RemoteError::link_down(format!("malformed frame: {reason}")))?;
    if frame.seq != *expected_seq {
        return Err(RemoteError::link_down(format!(
            "frame sequence {} where {expected_seq} was expected (replay or gap)",
            frame.seq
        )));
    }
    if !auth::verify_frame_mac(
        &inner.session_key,
        inner.in_dir,
        frame.seq,
        frame.json.as_bytes(),
        &frame.mac,
    ) {
        return Err(RemoteError::link_down(
            "a frame MAC does not verify — refusing the link",
        ));
    }
    *expected_seq = frame
        .seq
        .checked_add(1)
        .ok_or_else(|| RemoteError::link_down("the frame sequence overflowed"))?;
    let envelope = wire::parse_envelope(&frame.json)
        .map_err(|reason| RemoteError::link_down(format!("malformed envelope: {reason}")))?;
    dispatch(inner, envelope).await;
    Ok(())
}

/// Route one authenticated envelope.
async fn dispatch(inner: &Arc<LinkInner>, envelope: Envelope) {
    match envelope {
        Envelope::Res { id, ok, err } => {
            let entry = inner
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            match entry {
                Some(Pending::Call(tx)) => {
                    let _ = tx.send(match err {
                        Some(error) => Err(error),
                        None => Ok(ok.unwrap_or(Value::Null)),
                    });
                }
                Some(Pending::StreamInit(tx, queue)) => match err {
                    Some(error) => {
                        let _ = tx.send(Err(error));
                    }
                    None => {
                        inner
                            .pending
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .insert(id, Pending::Stream(queue));
                        // The requester may have given up while the response
                        // was in flight; drop the stream rather than leave an
                        // entry nobody will read.
                        if tx.send(Ok(ok.unwrap_or(Value::Null))).is_err() {
                            inner
                                .pending
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .remove(&id);
                            let _ = inner
                                .outbound
                                .try_send(Outbound::Envelope(Envelope::cancel(id)));
                        }
                    }
                },
                Some(Pending::Stream(_)) => {
                    inner.fail(RemoteError::link_down(
                        "the peer answered a stream with a second response",
                    ));
                }
                // A late response to a cancelled call: harmless.
                None => {}
            }
        }
        Envelope::Item { id, n, d } => {
            // Clone the queue out of the map so the map is not borrowed while
            // the item is offered to it.
            let queue = {
                let pending = inner
                    .pending
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                match pending.get(&id) {
                    Some(Pending::Stream(queue)) => Some(queue.clone()),
                    _ => None,
                }
            };
            let Some(queue) = queue else { return };
            let expected = queue.state.next_n.load(Ordering::SeqCst) + 1;
            if n != expected {
                // An item sequence gap is the peer's protocol violation: end
                // this stream (only), and stop it on the peer. The link stays
                // up for every other call.
                inner.end_stream(
                    id,
                    &queue,
                    StreamTerminal::Error(RemoteError::protocol(format!(
                        "stream {id} carried item {n} where {expected} was expected"
                    ))),
                );
                return;
            }
            queue.state.next_n.store(n, Ordering::SeqCst);
            match queue.items.try_send(Item { n, d }) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // A consumer that stopped reading loses this stream, never
                    // the link.
                    inner.end_stream(
                        id,
                        &queue,
                        StreamTerminal::Error(RemoteError::overflow(format!(
                            "stream {id} outran its {STREAM_ITEMS}-item buffer"
                        ))),
                    );
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    inner.end_stream(
                        id,
                        &queue,
                        StreamTerminal::Error(RemoteError::link_down(
                            "the stream consumer went away",
                        )),
                    );
                }
            }
        }
        Envelope::End { id, err } => {
            let entry = inner
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            match entry {
                Some(Pending::Stream(queue)) => {
                    match err {
                        Some(error) => queue.state.finish(StreamTerminal::Error(error)),
                        None => queue.state.finish(StreamTerminal::Clean),
                    }
                    drop(queue.items);
                }
                Some(Pending::StreamInit(_, _)) => {
                    inner.fail(RemoteError::link_down(
                        "the peer ended a stream before its first response",
                    ));
                }
                Some(Pending::Call(tx)) => {
                    let _ = tx.send(Err(RemoteError::unknown_call(id)));
                }
                None => {}
            }
        }
        Envelope::Cancel { id } => {
            let handle = inner
                .inbound
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
            if let Some(handle) = handle {
                handle.abort();
            }
        }
        Envelope::Note { m, p } => inner.handler.note(&m, p),
        Envelope::Req { id, m, p } => {
            let (duplicate, in_flight) = {
                let inbound = inner
                    .inbound
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                (inbound.contains_key(&id), inbound.len())
            };
            if duplicate {
                // The peer reused an id whose handler still runs: a protocol
                // violation that would corrupt the call's answer stream.
                inner.fail(RemoteError::link_down(format!(
                    "the peer reused the in-flight request id {id}"
                )));
                return;
            }
            if in_flight >= MAX_INFLIGHT {
                // Answer without running a handler; the link stays healthy.
                let _ = inner
                    .send(Envelope::res_err(id, RemoteError::busy()))
                    .await;
                return;
            }
            let handler = inner.handler.clone();
            let task_inner = inner.clone();
            let out = StreamTx::new(inner.clone(), id);
            let task = tokio::spawn(async move {
                let result = handler.request(&m, p, out.clone()).await;
                task_inner.finish_request(id, &out, result).await;
                task_inner
                    .inbound
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&id);
            });
            inner
                .inbound
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(id, task.abort_handle());
        }
    }
}

impl LinkInner {
    /// Terminate one stream on the requester's side and ask the peer to stop
    /// it, without touching the link.
    fn end_stream(&self, id: u64, queue: &StreamQueue, terminal: StreamTerminal) {
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id);
        queue.state.finish(terminal);
        let _ = self
            .outbound
            .try_send(Outbound::Envelope(Envelope::cancel(id)));
    }

    /// Answer an inbound request that its handler just finished: unless the
    /// handler already started a stream with `res`, send its outcome as `res`,
    /// then always close with `end`.
    async fn finish_request(
        &self,
        id: u64,
        out: &StreamTx,
        result: Result<Value, RemoteError>,
    ) {
        if !out.started.load(Ordering::SeqCst) {
            let response = match &result {
                Ok(value) => Envelope::res_ok(id, value.clone()),
                Err(error) => Envelope::res_err(id, error.clone()),
            };
            if self.send(response).await.is_err() {
                return;
            }
        }
        let _ = self.send(Envelope::end(id, result.err())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::pairing::NodeRegistry;
    use crate::remote::CODE_LINK_DOWN;
    use futures::FutureExt as _;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};

    // ── helpers ────────────────────────────────────────────────────────────

    /// A handler that answers a fixed set of methods.
    struct TestHandler {
        methods: Vec<&'static str>,
        seen: Mutex<Vec<(String, Value)>>,
    }

    impl TestHandler {
        fn new(methods: &[&'static str]) -> Self {
            Self {
                methods: methods.to_vec(),
                seen: Mutex::new(Vec::new()),
            }
        }

        fn seen(&self) -> Vec<(String, Value)> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    #[async_trait]
    impl LinkHandler for TestHandler {
        async fn request(&self, m: &str, p: Value, out: StreamTx) -> Result<Value, RemoteError> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push((m.to_string(), p.clone()));
            if !self.methods.contains(&m) {
                return Err(RemoteError::unknown_method(m));
            }
            match m {
                "echo" | "node.echo" | "hub.echo" => Ok(json!({ "echo": p, "side": "test" })),
                "fail" => Err(RemoteError::bad_params("deliberately failed")),
                "stream" => {
                    out.start(json!({ "started": true })).await?;
                    for n in 1..=3 {
                        out.item(json!({ "n": n })).await?;
                    }
                    Ok(Value::Null)
                }
                "never" => {
                    std::future::pending::<()>().await;
                    unreachable!("pending never resolves")
                }
                _ => Ok(Value::Null),
            }
        }

        fn note(&self, _m: &str, _p: Value) {}
    }

    struct AbortFlagHandler {
        flag: Arc<AtomicBool>,
    }

    struct AbortFlag(Arc<AtomicBool>);

    impl Drop for AbortFlag {
        fn drop(&mut self) {
            self.0.store(true, AtomicOrdering::SeqCst);
        }
    }

    #[async_trait]
    impl LinkHandler for AbortFlagHandler {
        async fn request(&self, _m: &str, _p: Value, out: StreamTx) -> Result<Value, RemoteError> {
            let _guard = AbortFlag(self.flag.clone());
            out.start(json!({ "started": true })).await?;
            loop {
                out.item(json!({ "tick": true })).await?;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }

        fn note(&self, _m: &str, _p: Value) {}
    }

    /// Parks every inbound request until released, counting the ones that
    /// arrived (used to fill the in-flight cap).
    struct ParkHandler {
        started: Arc<AtomicUsize>,
        release: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl LinkHandler for ParkHandler {
        async fn request(&self, _m: &str, _p: Value, _out: StreamTx) -> Result<Value, RemoteError> {
            self.started.fetch_add(1, AtomicOrdering::SeqCst);
            self.release.notified().await;
            Ok(Value::Null)
        }

        fn note(&self, _m: &str, _p: Value) {}
    }

    /// Streams numbered items as fast as the link accepts them, forever; also
    /// answers `echo` so a test can prove the link still works afterwards.
    struct FirehoseHandler;

    #[async_trait]
    impl LinkHandler for FirehoseHandler {
        async fn request(&self, m: &str, p: Value, out: StreamTx) -> Result<Value, RemoteError> {
            if m == "echo" {
                return Ok(json!({ "echo": p, "side": "firehose" }));
            }
            out.start(json!({ "started": true })).await?;
            let mut n = 0u64;
            loop {
                n += 1;
                out.item(json!({ "i": n })).await?;
            }
        }

        fn note(&self, _m: &str, _p: Value) {}
    }

    /// A node identity with a fresh token.
    struct Node {
        identity: NodeIdentity,
        token: auth::Token,
    }

    impl Node {
        fn new(node_id: &str, hub: &str) -> Self {
            let token = auth::random_token();
            Self {
                identity: NodeIdentity {
                    hub: hub.to_string(),
                    node_id: node_id.to_string(),
                    token: auth::hex_encode(&token),
                    hub_id: "hub-test".to_string(),
                    paired_ms: 0,
                },
                token,
            }
        }

        fn authz(&self, hub_epoch: &str) -> HubAuth {
            let node_id = self.identity.node_id.clone();
            let token = self.token;
            HubAuth::new(
                "hub-test",
                hub_epoch,
                Arc::new(move |id: &str| (id == node_id).then_some(token)),
            )
        }
    }

    const NODE_ID: &str = "0123456789abcdef0123456789abcdef";

    /// Two links over one in-memory duplex socket.
    async fn linked_pair(
        node_handler: Arc<dyn LinkHandler>,
        hub_handler: Arc<dyn LinkHandler>,
    ) -> (Link, Link) {
        let node = Node::new(NODE_ID, "duplex");
        let authz = node.authz("epoch-1");
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move { accept_on(hub_io, authz, hub_handler).await });
        let node_link = dial_on(node_io, &node.identity, node_handler)
            .await
            .expect("the node dials and authenticates");
        let hub_link = hub
            .await
            .expect("hub task joins")
            .expect("the hub accepts and authenticates");
        (node_link, hub_link)
    }

    /// Handshake a raw hub-side WebSocket (no Link) against a dialing node, so
    /// a test can then send hand-built frames. Returns the hub-side socket and
    /// the session key.
    async fn raw_hub_handshake<W>(mut ws: W, token: auth::Token) -> (W, [u8; 32], auth::Nonce)
    where
        W: Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin,
    {
        let hello = read_handshake_reply(&mut ws).await.expect("hello");
        let Handshake::Hello { node_id, nonce, .. } = hello else {
            panic!("expected a hello, got {hello:?}");
        };
        assert_eq!(node_id, NODE_ID);
        let node_nonce = auth::parse_32_hex(&nonce).expect("node nonce");
        let hub_nonce = auth::random_nonce();
        send_handshake(
            &mut ws,
            &Handshake::Challenge {
                nonce: auth::hex_encode(&hub_nonce),
            },
        )
        .await
        .expect("challenge");
        let _ = read_handshake_reply(&mut ws).await.expect("auth");
        send_handshake(
            &mut ws,
            &Handshake::Welcome {
                proof: auth::hub_proof(&token, &node_nonce, &hub_nonce),
                hub_id: "hub-test".to_string(),
                hub_epoch: "epoch-raw".to_string(),
            },
        )
        .await
        .expect("welcome");
        let session_key = auth::session_key(&token, &node_nonce, &hub_nonce);
        (ws, session_key, hub_nonce)
    }

    /// Build the WS server side of a raw hub over `hub_io`.
    async fn raw_hub_ws(
        hub_io: tokio::io::DuplexStream,
    ) -> impl Stream<Item = Result<Message, WsError>> + Sink<Message, Error = WsError> + Unpin
    {
        async_tungstenite::tokio::accept_async_with_config(hub_io, Some(ws_config()))
            .await
            .expect("raw hub websocket handshake")
    }

    /// Send one MACed frame from a raw hub-side socket.
    async fn send_frame<W>(ws: &mut W, key: &[u8; 32], seq: u64, envelope: &Envelope)
    where
        W: Sink<Message, Error = WsError> + Unpin,
    {
        let json = serde_json::to_string(envelope).expect("envelope json");
        let mac = auth::frame_mac(key, auth::DIR_HUB_TO_NODE, seq, json.as_bytes());
        ws.send(Message::text(Frame::new(seq, mac, json).encode()))
            .await
            .expect("frame");
    }

    // ── handshake ──────────────────��────────────────────────────────────────

    #[tokio::test]
    async fn handshake_mutual_auth_succeeds() {
        let node = Arc::new(TestHandler::new(&["echo"]));
        let hub = Arc::new(TestHandler::new(&["hub.echo"]));
        let (node_link, hub_link) = linked_pair(node.clone(), hub.clone()).await;
        assert_eq!(node_link.peer_epoch(), "epoch-1");
        assert_eq!(hub_link.peer_epoch(), "epoch-1");
        // A call in each direction proves both ends derived the same key.
        let answer = hub_link
            .call("echo", json!({ "from": "hub" }), Duration::from_secs(5))
            .await
            .expect("hub calls the node");
        assert_eq!(answer, json!({ "echo": { "from": "hub" }, "side": "test" }));
        let answer = node_link
            .call("hub.echo", json!({ "from": "node" }), Duration::from_secs(5))
            .await
            .expect("node calls the hub");
        assert_eq!(answer, json!({ "echo": { "from": "node" }, "side": "test" }));
    }

    #[tokio::test]
    async fn wrong_token_is_refused_bad_proof() {
        let node = Node::new(NODE_ID, "duplex");
        // The hub knows a *different* token for this node id.
        let hub_token = auth::random_token();
        let node_id = node.identity.node_id.clone();
        let authz = HubAuth::new(
            "hub-test",
            "epoch-1",
            Arc::new(move |id: &str| (id == node_id).then_some(hub_token)),
        );
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move { accept_on(hub_io, authz, Arc::new(TestHandler::new(&[]))).await });
        let result = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[]))).await;
        let error = result.err().expect("the node is refused");
        assert_eq!(error.code, "bad_proof");
        let hub_error = hub
            .await
            .expect("hub task joins")
            .err()
            .expect("the hub refuses");
        assert_eq!(hub_error.code, "bad_proof");
    }

    #[tokio::test]
    async fn fake_hub_without_token_is_rejected_by_node() {
        let node = Node::new(NODE_ID, "duplex");
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let mut ws = raw_hub_ws(hub_io).await;
            let hello = read_handshake_reply(&mut ws).await.expect("hello");
            let Handshake::Hello { nonce, .. } = hello else {
                panic!("expected hello");
            };
            let node_nonce = auth::parse_32_hex(&nonce).expect("nonce");
            let hub_nonce = auth::random_nonce();
            send_handshake(
                &mut ws,
                &Handshake::Challenge {
                    nonce: auth::hex_encode(&hub_nonce),
                },
            )
            .await
            .expect("challenge");
            let _ = read_handshake_reply(&mut ws).await.expect("auth");
            // The hub does not know the token: it guesses one.
            let wrong_token = auth::random_token();
            send_handshake(
                &mut ws,
                &Handshake::Welcome {
                    proof: auth::hub_proof(&wrong_token, &node_nonce, &hub_nonce),
                    hub_id: "fake".to_string(),
                    hub_epoch: "epoch-fake".to_string(),
                },
            )
            .await
            .expect("welcome");
            // Hold the socket open so the node's rejection is its own.
            let _ = ws.next().await;
        });
        let error = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .err()
            .expect("the node rejects the fake hub");
        assert_eq!(error.code, "bad_proof");
        hub.await.expect("hub task joins");
    }

    #[tokio::test]
    async fn revoked_node_is_refused_unknown_node() {
        let dir = test_dir("revoked-node");
        let registry = Arc::new(NodeRegistry::new(&dir));
        let record = registry.register("worker", "linux").expect("registers");
        let identity = NodeIdentity {
            hub: "duplex".to_string(),
            node_id: record.node_id.clone(),
            token: record.token.clone(),
            hub_id: "hub-test".to_string(),
            paired_ms: 0,
        };

        // While the record exists, the node is accepted.
        {
            let (node_io, hub_io) = tokio::io::duplex(1 << 20);
            let authz = HubAuth::with_registry("hub-test", "epoch-1", registry.clone());
            let hub = tokio::spawn(async move {
                accept_on(hub_io, authz, Arc::new(TestHandler::new(&[]))).await
            });
            dial_on(node_io, &identity, Arc::new(TestHandler::new(&[])))
                .await
                .expect("a registered node is accepted");
            hub.await.expect("hub task joins").expect("hub link");
        }

        // Deleting the file revokes it.
        registry.revoke(&record.node_id).expect("revokes");
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let authz = HubAuth::with_registry("hub-test", "epoch-1", registry.clone());
        let hub = tokio::spawn(async move {
            accept_on(hub_io, authz, Arc::new(TestHandler::new(&[]))).await
        });
        let error = dial_on(node_io, &identity, Arc::new(TestHandler::new(&[])))
            .await
            .err()
            .expect("a revoked node is refused");
        assert_eq!(error.code, "unknown_node");
        let hub_error = hub
            .await
            .expect("hub task joins")
            .err()
            .expect("the hub refuses");
        assert_eq!(hub_error.code, "unknown_node");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── frame integrity ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn tampered_frame_closes_link() {
        let node = Node::new(NODE_ID, "duplex");
        let token = node.token;
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let ws = raw_hub_ws(hub_io).await;
            let (mut ws, _session_key, _nonce) = raw_hub_handshake(ws, token).await;
            let json = serde_json::to_string(&Envelope::note("hi", json!({}))).expect("note");
            ws.send(Message::text(format!("1 {} {json}", "0".repeat(64))))
                .await
                .expect("tampered frame");
            // Keep the socket open; the node must close on its own.
            let _ = ws.next().await;
        });
        let link = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .expect("link established");
        tokio::time::timeout(Duration::from_secs(5), link.closed())
            .await
            .expect("the tampered frame closes the link");
        hub.await.expect("hub task joins");
    }

    #[tokio::test]
    async fn replayed_or_skipped_seq_closes_link() {
        // Skipped: the first frame is sequence 2.
        let node = Node::new(NODE_ID, "duplex");
        let token = node.token;
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let ws = raw_hub_ws(hub_io).await;
            let (mut ws, session_key, _nonce) = raw_hub_handshake(ws, token).await;
            let json = serde_json::to_string(&Envelope::note("hi", json!({}))).expect("note");
            let mac = auth::frame_mac(&session_key, auth::DIR_HUB_TO_NODE, 2, json.as_bytes());
            ws.send(Message::text(format!("2 {mac} {json}")))
                .await
                .expect("skipped frame");
            let _ = ws.next().await;
        });
        let link = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .expect("link established");
        tokio::time::timeout(Duration::from_secs(5), link.closed())
            .await
            .expect("a sequence gap closes the link");
        hub.await.expect("hub task joins");

        // Replayed: the same valid frame twice.
        let node = Node::new(NODE_ID, "duplex");
        let token = node.token;
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let ws = raw_hub_ws(hub_io).await;
            let (mut ws, session_key, _nonce) = raw_hub_handshake(ws, token).await;
            let json = serde_json::to_string(&Envelope::note("hi", json!({}))).expect("note");
            let mac = auth::frame_mac(&session_key, auth::DIR_HUB_TO_NODE, 1, json.as_bytes());
            let frame = format!("1 {mac} {json}");
            ws.send(Message::text(frame.clone())).await.expect("frame");
            ws.send(Message::text(frame)).await.expect("replay");
            let _ = ws.next().await;
        });
        let link = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .expect("link established");
        tokio::time::timeout(Duration::from_secs(5), link.closed())
            .await
            .expect("a replayed frame closes the link");
        hub.await.expect("hub task joins");
    }

    // ── calls and streams ───────────────────────────────────────────────────

    #[tokio::test]
    async fn call_round_trip_both_directions() {
        let node = Arc::new(TestHandler::new(&["node.echo"]));
        let hub = Arc::new(TestHandler::new(&["hub.echo"]));
        let (node_link, hub_link) = linked_pair(node.clone(), hub.clone()).await;
        let from_node = node_link
            .call("hub.echo", json!({ "n": 1 }), Duration::from_secs(5))
            .await
            .expect("node → hub");
        assert_eq!(from_node, json!({ "echo": { "n": 1 }, "side": "test" }));
        let from_hub = hub_link
            .call("node.echo", json!({ "n": 2 }), Duration::from_secs(5))
            .await
            .expect("hub → node");
        assert_eq!(from_hub, json!({ "echo": { "n": 2 }, "side": "test" }));
        assert_eq!(hub.seen(), vec![("hub.echo".to_string(), json!({ "n": 1 }))]);
        assert_eq!(node.seen(), vec![("node.echo".to_string(), json!({ "n": 2 }))]);
        // A method the peer does not know comes back as its error.
        let error = node_link
            .call("nope", json!({}), Duration::from_secs(5))
            .await
            .expect_err("unknown method");
        assert_eq!(error.code, crate::remote::CODE_UNKNOWN_METHOD);
    }

    #[tokio::test]
    async fn stream_items_then_end_in_order() {
        let (node_link, _hub_link) = linked_pair(
            Arc::new(TestHandler::new(&[])),
            Arc::new(TestHandler::new(&["stream"])),
        )
        .await;
        let (res, mut stream) = node_link
            .call_stream("stream", json!({}))
            .await
            .expect("stream opens");
        assert_eq!(res, json!({ "started": true }));
        let mut numbered = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item.expect("item");
            numbered.push((item.n, item.d));
        }
        assert_eq!(
            numbered,
            vec![
                (1, json!({ "n": 1 })),
                (2, json!({ "n": 2 })),
                (3, json!({ "n": 3 })),
            ]
        );
    }

    #[tokio::test]
    async fn dropping_stream_sends_cancel_and_aborts_handler() {
        let flag = Arc::new(AtomicBool::new(false));
        let hub = Arc::new(AbortFlagHandler { flag: flag.clone() });
        let (node_link, _hub_link) = linked_pair(Arc::new(TestHandler::new(&[])), hub).await;
        let (res, mut stream) = node_link
            .call_stream("forever", json!({}))
            .await
            .expect("stream opens");
        assert_eq!(res, json!({ "started": true }));
        let first = stream.next().await.expect("first item").expect("item");
        assert_eq!(first.n, 1);
        drop(stream);
        for _ in 0..200 {
            if flag.load(AtomicOrdering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            flag.load(AtomicOrdering::SeqCst),
            "the hub aborted the handler after the cancel"
        );
    }

    #[tokio::test]
    async fn link_close_fails_pending_calls_with_link_down() {
        let (node_link, hub_link) = linked_pair(
            Arc::new(TestHandler::new(&["never"])),
            Arc::new(TestHandler::new(&[])),
        )
        .await;
        let call = tokio::spawn(async move {
            hub_link
                .call("never", json!({}), Duration::from_secs(30))
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(node_link);
        let result = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .expect("the pending call fails")
            .expect("task joins");
        assert_eq!(result.expect_err("link down").code, CODE_LINK_DOWN);
    }

    #[tokio::test]
    async fn timed_out_call_returns_timeout_and_cancels() {
        let (_node_link, hub_link) = linked_pair(
            Arc::new(TestHandler::new(&["never"])),
            Arc::new(TestHandler::new(&[])),
        )
        .await;
        let error = hub_link
            .call("never", json!({}), Duration::from_millis(100))
            .await
            .expect_err("times out");
        assert_eq!(error.code, crate::remote::CODE_TIMEOUT);
    }


    /// A fresh directory under the checkout's `target/` — never `%TEMP%`.
    fn test_dir(name: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&root).expect("target dir");
        let dir = root.join(format!(
            "mnemo-remote-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, AtomicOrdering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }



    // ── the slot ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn slot_wait_up_budget_is_per_outage() {
        let slot = LinkSlot::new();
        // Down since creation, 150 ms ago; a 200 ms budget leaves 50 ms.
        tokio::time::sleep(Duration::from_millis(150)).await;
        let started = std::time::Instant::now();
        assert!(slot.wait_up(Duration::from_millis(200)).await.is_none());
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(20) && waited < Duration::from_millis(150),
            "the deadline is measured from the outage start, waited {waited:?}"
        );

        // A redundant `set(None)` does not restart the outage.
        slot.set(None);
        let started = std::time::Instant::now();
        assert!(slot.wait_up(Duration::from_millis(200)).await.is_none());
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "the outage was not restarted"
        );

        // A live link is returned at once, and losing it starts a fresh outage.
        let (link, _other) = linked_pair(
            Arc::new(TestHandler::new(&[])),
            Arc::new(TestHandler::new(&[])),
        )
        .await;
        slot.set(Some(link));
        assert!(slot.is_up());
        let up = slot.wait_up(Duration::from_millis(10)).await.expect("up");
        assert!(up.closed().now_or_never().is_none(), "the link is alive");
        slot.set(None);
        let started = std::time::Instant::now();
        assert!(slot.wait_up(Duration::from_millis(100)).await.is_none());
        assert!(
            started.elapsed() >= Duration::from_millis(80),
            "a fresh outage gets the whole budget"
        );
    }

    // ── audit regressions ──────────────────────────────────────────────

    #[tokio::test]
    async fn call_stream_fails_link_down_when_link_fails_during_setup() {
        let (node_link, _hub_link) = linked_pair(
            Arc::new(TestHandler::new(&[])),
            Arc::new(TestHandler::new(&[])),
        )
        .await;
        // Model the window between the first `closed` check and the insert into
        // the pending map: `fail` has set the flag but its drain has not seen
        // the new entry yet, so only the post-insert check can catch it.
        node_link.inner.failed.store(true, Ordering::SeqCst);
        let stream = tokio::time::timeout(
            Duration::from_secs(5),
            node_link.call_stream("anything", json!({})),
        )
        .await
        .expect("call_stream must not hang");
        assert_eq!(stream.err().expect("link down").code, CODE_LINK_DOWN);
        let call = tokio::time::timeout(
            Duration::from_secs(5),
            node_link.call("anything", json!({}), Duration::from_secs(5)),
        )
        .await
        .expect("call must not hang");
        assert_eq!(call.expect_err("link down").code, CODE_LINK_DOWN);
    }

    #[tokio::test]
    async fn stalled_upgrade_times_out() {
        let node = Node::new(NODE_ID, "duplex");
        // The peer never answers the WebSocket upgrade; the holding value keeps
        // the socket open so only the handshake budget can end the wait.
        let (node_io, _stalled_peer) = tokio::io::duplex(8 * 1024);
        let started = std::time::Instant::now();
        let error = dial_on_within(
            node_io,
            &node.identity,
            Arc::new(TestHandler::new(&[])),
            Duration::from_millis(100),
        )
        .await
        .err()
        .expect("a stalled upgrade times out");
        assert_eq!(error.code, CODE_LINK_DOWN);
        assert!(
            error.message.contains("handshake did not finish"),
            "the error names the handshake budget: {}",
            error.message
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "bounded by the handshake budget"
        );
    }

    #[tokio::test]
    async fn duplicate_request_id_closes_link() {
        let node = Node::new(NODE_ID, "duplex");
        let token = node.token;
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let ws = raw_hub_ws(hub_io).await;
            let (mut ws, key, _nonce) = raw_hub_handshake(ws, token).await;
            send_frame(&mut ws, &key, 1, &Envelope::req(9, "never", json!({}))).await;
            // The same id again while its handler is still running.
            send_frame(&mut ws, &key, 2, &Envelope::req(9, "never", json!({}))).await;
            let _ = ws.next().await;
        });
        let link = dial_on(
            node_io,
            &node.identity,
            Arc::new(TestHandler::new(&["never"])),
        )
        .await
        .expect("link established");
        tokio::time::timeout(Duration::from_secs(5), link.closed())
            .await
            .expect("a reused in-flight request id closes the link");
        hub.await.expect("hub task joins");
    }

    #[tokio::test]
    async fn inflight_cap_answers_busy() {
        let park = Arc::new(ParkHandler {
            started: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(tokio::sync::Notify::new()),
        });
        let (node_link, _hub_link) = linked_pair(
            Arc::new(TestHandler::new(&[])),
            park.clone(),
        )
        .await;
        let mut tasks = Vec::new();
        for _ in 0..MAX_INFLIGHT {
            let link = node_link.clone();
            tasks.push(tokio::spawn(async move {
                link.call("park", json!({}), Duration::from_secs(30)).await
            }));
        }
        for _ in 0..1000 {
            if park.started.load(AtomicOrdering::SeqCst) >= MAX_INFLIGHT {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            park.started.load(AtomicOrdering::SeqCst),
            MAX_INFLIGHT,
            "the whole cap is in flight"
        );
        let error = node_link
            .call("park", json!({}), Duration::from_secs(5))
            .await
            .expect_err("the cap answers busy");
        assert_eq!(error.code, crate::remote::CODE_BUSY);
        assert!(
            node_link.closed().now_or_never().is_none(),
            "a full cap does not hurt the link"
        );
        for task in tasks {
            task.abort();
        }
        park.release.notify_waiters();
    }

    #[tokio::test]
    async fn overflowing_stream_fails_alone() {
        let (node_link, _hub_link) = linked_pair(
            Arc::new(TestHandler::new(&["echo"])),
            Arc::new(FirehoseHandler),
        )
        .await;
        let (res, mut stream) = node_link
            .call_stream("hose", json!({}))
            .await
            .expect("stream opens");
        assert_eq!(res, json!({ "started": true }));
        assert_eq!(stream.next().await.expect("first").expect("item").n, 1);
        // Stop reading long enough for the item queue to fill.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut items = 1u64;
        let mut terminal = None;
        while let Some(next) = stream.next().await {
            match next {
                Ok(_) => items += 1,
                Err(error) => {
                    terminal = Some(error);
                    break;
                }
            }
        }
        let terminal = terminal.expect("the overflow ends the stream");
        assert_eq!(terminal.code, crate::remote::CODE_OVERFLOW);
        assert!(
            items <= STREAM_ITEMS as u64 + 1,
            "bounded by the item buffer, drained {items}"
        );
        // Only that stream failed: the link still carries calls.
        assert!(
            node_link.closed().now_or_never().is_none(),
            "the link stays up"
        );
        let answer = node_link
            .call("echo", json!({ "ok": true }), Duration::from_secs(5))
            .await
            .expect("the link still answers");
        assert_eq!(answer, json!({ "echo": { "ok": true }, "side": "firehose" }));
    }

    #[tokio::test]
    async fn truncated_stream_never_ends_clean() {
        let (node_link, _hub_link) = linked_pair(
            Arc::new(TestHandler::new(&[])),
            Arc::new(FirehoseHandler),
        )
        .await;
        let (_res, mut stream) = node_link
            .call_stream("hose", json!({}))
            .await
            .expect("stream opens");
        assert_eq!(stream.next().await.expect("first").expect("item").n, 1);
        // Simulate the abnormal close: the pending entry is dropped without an
        // `end`, as when the link vanishes between the last item and the end.
        node_link
            .inner
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&stream.id());
        let mut terminal = None;
        while let Some(next) = stream.next().await {
            if let Err(error) = next {
                terminal = Some(error);
                break;
            }
        }
        let error = terminal.expect("a truncated stream is never a clean end");
        assert_eq!(error.code, CODE_LINK_DOWN);
        assert!(
            error.message.contains("without an end frame"),
            "the error names the truncation: {}",
            error.message
        );
    }

    #[tokio::test]
    async fn stream_item_gap_fails_stream() {
        let node = Node::new(NODE_ID, "duplex");
        let token = node.token;
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let ws = raw_hub_ws(hub_io).await;
            let (mut ws, key, _nonce) = raw_hub_handshake(ws, token).await;
            send_frame(
                &mut ws,
                &key,
                1,
                &Envelope::res_ok(1, json!({ "opened": true })),
            )
            .await;
            send_frame(&mut ws, &key, 2, &Envelope::item(1, 1, json!({ "i": 1 }))).await;
            // Item 2 is missing.
            send_frame(&mut ws, &key, 3, &Envelope::item(1, 3, json!({ "i": 3 }))).await;
            let _ = ws.next().await;
        });
        let link = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .expect("link established");
        let (res, mut stream) = link
            .call_stream("hose", json!({}))
            .await
            .expect("stream opens");
        assert_eq!(res, json!({ "opened": true }));
        assert_eq!(stream.next().await.expect("item").expect("item").n, 1);
        let error = stream
            .next()
            .await
            .expect("the gap ends the stream")
            .expect_err("a gap is an error");
        assert_eq!(error.code, crate::remote::CODE_PROTOCOL);
        assert!(
            link.closed().now_or_never().is_none(),
            "an item gap fails the stream, not the link"
        );
        hub.await.expect("hub task joins");
    }

    #[tokio::test]
    async fn unexpected_binary_reports_kind_and_length() {
        // On a live link: the failure reaches the pending call by kind and
        // length, never by the payload bytes.
        let node = Node::new(NODE_ID, "duplex");
        let token = node.token;
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let ws = raw_hub_ws(hub_io).await;
            let (mut ws, _key, _nonce) = raw_hub_handshake(ws, token).await;
            // Wait for the node's call, then answer it with binary noise.
            let _request = ws.next().await;
            ws.send(Message::binary(vec![0xde, 0xad, 0xbe, 0xef]))
                .await
                .expect("binary frame");
            let _ = ws.next().await;
        });
        let link = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .expect("link established");
        let error = link
            .call("anything", json!({}), Duration::from_secs(5))
            .await
            .expect_err("the binary closes the link");
        assert!(
            error.message.contains("binary message (4 bytes)"),
            "kind and length only: {}",
            error.message
        );
        assert!(
            !error.message.contains("222") && !error.message.contains("dead"),
            "the payload is not echoed: {}",
            error.message
        );
        hub.await.expect("hub task joins");

        // During the handshake too.
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub = tokio::spawn(async move {
            let mut ws = raw_hub_ws(hub_io).await;
            let _hello = ws.next().await;
            ws.send(Message::binary(vec![0xde, 0xad, 0xbe, 0xef]))
                .await
                .expect("binary frame");
            let _ = ws.next().await;
        });
        let error = dial_on(node_io, &node.identity, Arc::new(TestHandler::new(&[])))
            .await
            .err()
            .expect("a binary handshake is refused");
        assert!(
            error.message.contains("binary message (4 bytes)"),
            "kind and length only: {}",
            error.message
        );
        assert!(!error.message.contains("222"), "{}", error.message);
        hub.await.expect("hub task joins");
    }
}
