// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Hub-proxied model calls (plan a040713f, phase 2 step 4).
//!
//! A node holds no API keys. The node's [`RemoteTransport`] gives the engine a
//! client that looks like the endpoint's real shape (capabilities, kind, model
//! id) but forwards every request over the link; the hub answers it from its
//! own [`LlmService`] and streams the [`LlmEvent`]s back. Two properties make
//! the proxy survive the link:
//!
//! * the hub buffers each call in a [`CallLog`] keyed by node and call id while
//!   a background task keeps draining the model stream — a dropped link does
//!   not cancel the model call, and `llm.resume {call, from}` replays exactly
//!   the events the node missed before re-attaching live;
//! * the node streams every event onward as it arrives (never collecting the
//!   response first), and every wait for the link is bounded by the caller's
//!   outage budget.
//!
//! Error strings cross the link verbatim: a provider's `401 …` stays a
//! non-retryable [`Error::Provider`] on the node, so its retry ladder behaves
//! exactly as it would have on the hub.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex, Notify};

use crate::config::{Config, Endpoint, ModelRef};
use crate::error::{Error, Result};
use crate::provider::client_factory::{build_client, ModelTransport};
use crate::provider::trace::LlmRequestContext;
use crate::provider::{
    Capabilities, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice, ToolSchema,
};
use crate::remote::{
    Link, LinkSlot, RemoteError, StreamRx, StreamTx, CODE_LINK_DOWN, CODE_MODEL_NOT_CONFIGURED,
    CODE_PROVIDER, CODE_UNKNOWN_CALL,
};

/// The model method names on the link.
pub const LLM_COMPLETE: &str = "llm.complete";
/// Replay a call's events above `from` and re-attach live.
pub const LLM_RESUME: &str = "llm.resume";
/// Drop the hub's buffered call after the node consumed it.
pub const LLM_ACK: &str = "llm.ack";
/// Stop the hub's model stream because the node abandoned it.
pub const LLM_CANCEL: &str = "llm.cancel";
/// Attribute a tools phase or the delivered raw tool calls to the trace.
pub const LLM_RECORD: &str = "llm.record";

/// How long the hub keeps an ended call around for a resume/ack.
pub const CALL_TTL: Duration = Duration::from_secs(600);

/// The node-side transport: a [`ModelTransport`] that asks the hub for every
/// model call.
///
/// It returns a client for every endpoint in the mirrored catalog — only an
/// unknown endpoint yields `None` — so the engine's fallback path can never
/// silently build a direct (keyless) client on a node. The shape client built
/// here is never used for network I/O; it only answers capabilities, kind and
/// provider name.
pub struct RemoteTransport {
    slot: Arc<LinkSlot>,
    budget: Duration,
}

impl RemoteTransport {
    /// A transport over `slot`, waiting at most `budget` for each outage.
    pub fn new(slot: Arc<LinkSlot>, budget: Duration) -> Self {
        Self { slot, budget }
    }
}

impl ModelTransport for RemoteTransport {
    fn client_for(&self, config: &Config, model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
        let endpoint = config.endpoint(&model.endpoint)?;
        let shape = build_client(
            config,
            endpoint,
            &model.model,
            endpoint.multimodal_for(&model.model),
            effective_effort(endpoint, model),
            None,
            None,
        );
        Some(Arc::new(RemoteLlmClient {
            slot: self.slot.clone(),
            model: model.clone(),
            shape,
            parked: Mutex::new(ParkedTimings::default()),
            budget: self.budget,
        }))
    }
}

/// The effective reasoning effort for `model`, mirroring
/// `client_factory::resolve_effort` (private to that module): the per-context
/// override wins, normalized for the endpoint; otherwise the endpoint/model
/// default chain applies. The value only shapes the (non-networked) shape
/// client.
fn effective_effort(endpoint: &Endpoint, model: &ModelRef) -> Option<String> {
    match model.reasoning_effort.as_deref() {
        Some(effort) => endpoint.normalize_reasoning_effort_for(&model.model, effort),
        None => endpoint.effective_reasoning_effort_for(Some(&model.model)),
    }
}

/// The message the node reports when it cannot reach the hub within the
/// outage budget.
fn down_message(budget: Duration) -> String {
    format!(
        "hub link down: no connection to the hub for {}s",
        budget.as_secs_f64()
    )
}

/// Timings parked by the turn loop before a request, to ride the next one.
#[derive(Debug, Default, Clone, Copy)]
struct ParkedTimings {
    prep_ms: Option<u32>,
    compact_ms: Option<u32>,
    backoff_ms: Option<u32>,
}

/// The node's model client: same shape as the endpoint, every call proxied.
pub struct RemoteLlmClient {
    slot: Arc<LinkSlot>,
    model: ModelRef,
    shape: Arc<dyn LlmClient>,
    parked: Mutex<ParkedTimings>,
    budget: Duration,
}

impl RemoteLlmClient {
    /// The model reference this client serves.
    pub fn model_ref(&self) -> &ModelRef {
        &self.model
    }

    fn take_parked(&self) -> ParkedTimings {
        let mut parked = self
            .parked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        std::mem::take(&mut *parked)
    }

    /// Map a failed `llm.complete` answer onto the engine's error surface.
    fn map_call_error(&self, error: RemoteError) -> Error {
        match error.code.as_str() {
            CODE_PROVIDER => Error::Provider(error.message),
            CODE_MODEL_NOT_CONFIGURED => Error::Provider(format!(
                "hub: model not found: {}/{} is not configured on the hub",
                self.model.endpoint, self.model.model
            )),
            _ => Error::Provider(error.message),
        }
    }

    /// Send a best-effort trace note on the current link.
    fn note_record(&self, payload: Value) {
        if let Some(link) = self.slot.current() {
            link.notify(LLM_RECORD, payload);
        }
    }
}

#[async_trait]
impl LlmClient for RemoteLlmClient {
    fn capabilities(&self) -> &Capabilities {
        self.shape.capabilities()
    }

    fn kind(&self) -> ProviderKind {
        self.shape.kind()
    }

    fn model(&self) -> &str {
        &self.model.model
    }

    fn provider_name(&self) -> &str {
        self.shape.provider_name()
    }

    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<BoxStream<'_, LlmEvent>> {
        self.complete_with_context(
            messages,
            tools,
            tool_choice,
            &LlmRequestContext::unattributed(),
        )
        .await
    }

    async fn complete_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
        context: &LlmRequestContext,
    ) -> Result<BoxStream<'_, LlmEvent>> {
        let parked = self.take_parked();
        let call = uuid::Uuid::new_v4().to_string();
        let mut timings = serde_json::Map::new();
        if let Some(ms) = parked.prep_ms {
            timings.insert("prep_ms".to_string(), json!(ms));
        }
        if let Some(ms) = parked.compact_ms {
            timings.insert("compact_ms".to_string(), json!(ms));
        }
        if let Some(ms) = parked.backoff_ms {
            timings.insert("backoff_ms".to_string(), json!(ms));
        }
        let params = json!({
            "call": call,
            "model": self.model,
            "messages": messages,
            "tools": tools,
            "tool_choice": tool_choice,
            "context": context,
            "timings": Value::Object(timings),
        });
        let link = self
            .slot
            .wait_up(self.budget)
            .await
            .ok_or_else(|| Error::Provider(down_message(self.budget)))?;
        let (answer, rx) =
            match tokio::time::timeout(self.budget, link.call_stream(LLM_COMPLETE, params.clone()))
                .await
            {
                Ok(Ok(pair)) => pair,
                Ok(Err(error)) => return Err(self.map_call_error(error)),
                Err(_) => return Err(Error::Provider(down_message(self.budget))),
            };
        if answer.get("accepted").and_then(Value::as_bool) != Some(true) {
            return Err(Error::Provider(format!(
                "hub: unexpected llm.complete answer: {answer}"
            )));
        }
        let state = StreamState {
            client: self,
            call,
            params,
            link,
            rx,
            received: 0,
            finished: false,
        };
        Ok(Box::pin(futures::stream::unfold(
            state,
            |mut state| async move {
                let event = state.next_event().await?;
                Some((event, state))
            },
        )))
    }

    fn record_tools_phase_ms(&self, ms: u32) {
        self.note_record(json!({ "model": self.model, "tools_phase_ms": ms }));
    }

    fn record_prep_ms(&self, ms: u32) {
        self.parked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .prep_ms = Some(ms);
    }

    fn record_compact_ms(&self, ms: u32) {
        self.parked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .compact_ms = Some(ms);
    }

    fn record_backoff_ms(&self, ms: u32) {
        self.parked
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .backoff_ms = Some(ms);
    }

    fn record_raw_tool_calls(&self, calls: Vec<(String, String, String)>) {
        self.note_record(json!({ "model": self.model, "raw_tool_calls": calls }));
    }
}

/// The state of one proxied stream: the current link + stream, the retained
/// request for a re-issue after a hub restart, and how many events the caller
/// has already seen.
struct StreamState<'a> {
    client: &'a RemoteLlmClient,
    call: String,
    params: Value,
    link: Link,
    rx: StreamRx,
    received: usize,
    finished: bool,
}

impl Drop for StreamState<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // An early drop tells the hub to stop the model stream and forget the
        // call. The link-level cancel already fires from `StreamRx`'s drop;
        // these notes make the CallLog drop the buffered events too.
        let link = self
            .client
            .slot
            .current()
            .unwrap_or_else(|| self.link.clone());
        link.notify(LLM_CANCEL, json!({ "call": self.call }));
        link.notify(LLM_ACK, json!({ "call": self.call }));
    }
}

impl StreamState<'_> {
    /// The next event to yield: normal items, resume after a link drop, or a
    /// terminal [`LlmEvent::Error`].
    async fn next_event(&mut self) -> Option<LlmEvent> {
        loop {
            match self.rx.next().await {
                Some(Ok(item)) => {
                    self.received += 1;
                    match serde_json::from_value::<LlmEvent>(item.d) {
                        Ok(event) => return Some(event),
                        Err(error) => {
                            self.finished = true;
                            return Some(LlmEvent::Error {
                                error: format!("hub: malformed model event: {error}"),
                            });
                        }
                    }
                }
                Some(Err(error)) => {
                    if error.code != CODE_LINK_DOWN {
                        self.finished = true;
                        return Some(LlmEvent::Error {
                            error: error.message,
                        });
                    }
                    if let Err(event) = self.resume().await {
                        self.finished = true;
                        return Some(event);
                    }
                }
                None => {
                    self.finished = true;
                    let link = self
                        .client
                        .slot
                        .current()
                        .unwrap_or_else(|| self.link.clone());
                    link.notify(LLM_ACK, json!({ "call": self.call }));
                    return None;
                }
            }
        }
    }

    /// Wait out the outage and resume the call, or re-issue it when the hub
    /// forgot it before a single item arrived.
    async fn resume(&mut self) -> std::result::Result<(), LlmEvent> {
        loop {
            let Some(link) = self.client.slot.wait_up(self.client.budget).await else {
                return Err(LlmEvent::Error {
                    error: down_message(self.client.budget),
                });
            };
            let resume = link.call_stream(
                LLM_RESUME,
                json!({ "call": self.call, "from": self.received }),
            );
            match tokio::time::timeout(self.client.budget, resume).await {
                Err(_) => {
                    return Err(LlmEvent::Error {
                        error: down_message(self.client.budget),
                    });
                }
                Ok(Ok((answer, rx))) => {
                    if answer.get("accepted").and_then(Value::as_bool) != Some(true) {
                        return Err(LlmEvent::Error {
                            error: format!("hub: unexpected llm.resume answer: {answer}"),
                        });
                    }
                    self.link = link;
                    self.rx = rx;
                    return Ok(());
                }
                Ok(Err(error)) if error.code == CODE_UNKNOWN_CALL => {
                    if self.received > 0 {
                        return Err(LlmEvent::Error {
                            error: "remote link lost mid-response: the hub restarted; \
                                    the partial response was kept"
                                .to_string(),
                        });
                    }
                    // Nothing was consumed yet, so re-issuing the retained
                    // request is exactly equivalent — under a fresh call id.
                    let call = uuid::Uuid::new_v4().to_string();
                    self.call = call.clone();
                    self.params["call"] = json!(call);
                    match tokio::time::timeout(
                        self.client.budget,
                        link.call_stream(LLM_COMPLETE, self.params.clone()),
                    )
                    .await
                    {
                        Ok(Ok((answer, rx)))
                            if answer.get("accepted").and_then(Value::as_bool) == Some(true) =>
                        {
                            self.link = link;
                            self.rx = rx;
                            return Ok(());
                        }
                        Ok(Ok((answer, _))) => {
                            return Err(LlmEvent::Error {
                                error: format!("hub: unexpected llm.complete answer: {answer}"),
                            });
                        }
                        Ok(Err(error)) => {
                            return Err(LlmEvent::Error {
                                error: error.message,
                            });
                        }
                        Err(_) => {
                            return Err(LlmEvent::Error {
                                error: down_message(self.client.budget),
                            });
                        }
                    }
                }
                Ok(Err(error)) if error.code == CODE_LINK_DOWN => continue,
                Ok(Err(error)) => {
                    return Err(LlmEvent::Error {
                        error: error.message,
                    });
                }
            }
        }
    }
}

// ── hub side ───────────────────────────────────────────────────────────────

/// What the hub's `llm.*` handlers need from the app: the live model resolver.
pub trait LlmService: Send + Sync + 'static {
    /// The client for `model`, or `None` when the hub has no such
    /// endpoint/model configured (the node then gets `model_not_configured`).
    fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>>;
}

/// Parameters of `llm.complete`.
#[derive(Debug, Deserialize)]
struct CompleteParams {
    call: String,
    model: ModelRef,
    messages: Vec<Message>,
    tools: Vec<ToolSchema>,
    #[serde(default)]
    tool_choice: Option<ToolChoice>,
    #[serde(default)]
    context: LlmRequestContext,
    #[serde(default)]
    timings: Timings,
}

/// The pre-request timings the turn loop parked on the node.
#[derive(Debug, Default, Deserialize)]
struct Timings {
    #[serde(default)]
    prep_ms: Option<u32>,
    #[serde(default)]
    compact_ms: Option<u32>,
    #[serde(default)]
    backoff_ms: Option<u32>,
}

/// Parameters of `llm.resume`.
#[derive(Debug, Deserialize)]
struct ResumeParams {
    call: String,
    #[serde(default)]
    from: usize,
}

/// Parameters of the `llm.record` note.
#[derive(Debug, Deserialize)]
struct RecordParams {
    model: ModelRef,
    #[serde(default)]
    tools_phase_ms: Option<u32>,
    #[serde(default)]
    raw_tool_calls: Option<Vec<(String, String, String)>>,
}

/// Progress of one buffered call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Progress {
    Running,
    Ended,
    Canceled,
}

/// One call's buffered stream: every event produced so far plus the stream
/// currently attached to the node, if any.
struct Call {
    state: AsyncMutex<CallState>,
    /// Notified on every appended event and on any [`Progress`] change.
    changed: Notify,
    progress: watch::Sender<Progress>,
}

impl Call {
    fn new() -> Self {
        let (progress, _rx) = watch::channel(Progress::Running);
        Self {
            state: AsyncMutex::new(CallState::default()),
            changed: Notify::new(),
            progress,
        }
    }

    fn set_progress(&self, progress: Progress) {
        self.progress.send_replace(progress);
        self.changed.notify_waiters();
    }

    /// Wait until the model stream ended or the call was canceled.
    async fn wait_done(&self) {
        let mut progress = self.progress.subscribe();
        loop {
            if *progress.borrow_and_update() != Progress::Running {
                return;
            }
            if progress.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The call's mutable state, never held across an unrelated await.
#[derive(Default)]
struct CallState {
    /// Every event produced so far, in emission order (`LlmEvent` JSON).
    events: Vec<Value>,
    /// The stream attached to the node, replaced on every resume.
    attached: Option<StreamTx>,
    /// Set once the model stream finished (cleanly or with a final error
    /// event).
    ended: bool,
    /// When the call ended — the [`CALL_TTL`] sweep keys off this.
    ended_at: Option<Instant>,
    /// Set when the hub rejected the request before accepting it, so a resume
    /// reports the same provider error instead of `unknown_call`.
    rejected: Option<RemoteError>,
}

/// The hub's per-node call registry: the events of every in-flight call,
/// kept alive across link drops until the node acks or cancels it, or
/// [`CALL_TTL`] passes after the call ended.
pub struct CallLog {
    calls: Mutex<HashMap<(String, String), Arc<Call>>>,
    ttl: Duration,
}

impl Default for CallLog {
    fn default() -> Self {
        Self::new()
    }
}

impl CallLog {
    /// An empty log with the default call TTL.
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(HashMap::new()),
            ttl: CALL_TTL,
        }
    }

    /// An empty log with a caller-supplied TTL (tests use a short one so the
    /// sweep runs without waiting [`CALL_TTL`]).
    #[cfg(test)]
    fn with_ttl(ttl: Duration) -> Self {
        Self {
            calls: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Arc<Call>>> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Register a call whose stream the node has just been told was accepted.
    fn accept(&self, node_id: &str, call: &str, out: StreamTx) -> Arc<Call> {
        let entry = Arc::new(Call::new());
        entry
            .state
            .try_lock()
            .expect("a fresh call state is unlocked")
            .attached = Some(out);
        self.map()
            .insert((node_id.to_string(), call.to_string()), entry.clone());
        entry
    }

    /// Register a call the hub rejected before accepting it.
    fn reject(&self, node_id: &str, call: &str, error: RemoteError) {
        let entry = Arc::new(Call::new());
        {
            let mut state = entry
                .state
                .try_lock()
                .expect("a fresh call state is unlocked");
            state.rejected = Some(error);
            state.ended = true;
            state.ended_at = Some(Instant::now());
        }
        entry.set_progress(Progress::Ended);
        self.map()
            .insert((node_id.to_string(), call.to_string()), entry);
    }

    /// Look one call up.
    fn get(&self, node_id: &str, call: &str) -> Option<Arc<Call>> {
        self.map()
            .get(&(node_id.to_string(), call.to_string()))
            .cloned()
    }

    /// Drop a call: `cancel` also stops the hub-side model stream, `ack`
    /// forgets a call the node has fully consumed.
    fn drop_call(&self, node_id: &str, call: &str, cancel: bool) {
        if let Some(entry) = self
            .map()
            .remove(&(node_id.to_string(), call.to_string()))
        {
            if cancel {
                entry.set_progress(Progress::Canceled);
            }
        }
    }

    /// Forget every call that ended longer than the TTL ago.
    fn sweep(&self) {
        let now = Instant::now();
        let ttl = self.ttl;
        self.map().retain(|_, call| {
            let state = call.state.try_lock();
            match state {
                Ok(state) => match state.ended_at {
                    Some(ended_at) => now.duration_since(ended_at) < ttl,
                    None => true,
                },
                // The driving task holds the lock; keep the call.
                Err(_) => true,
            }
        });
    }
}

/// The hub's `llm.*` handlers for one node link. Step 5's hub composes this
/// with its session and host-tool handlers: [`LlmHandler::request`] returns
/// `None` for a method it does not own, [`LlmHandler::note`] returns `false`.
pub struct LlmHandler {
    service: Arc<dyn LlmService>,
    calls: Arc<CallLog>,
}

impl LlmHandler {
    /// Handlers over `service`'s model resolver.
    pub fn new(service: Arc<dyn LlmService>) -> Arc<Self> {
        Arc::new(Self {
            service,
            calls: Arc::new(CallLog::new()),
        })
    }

    /// The shared call log (tests inspect it; the hub needs no direct access).
    pub fn calls(&self) -> &Arc<CallLog> {
        &self.calls
    }

    /// Forget every call that ended longer than the log's TTL ago. The hub's
    /// housekeeping runs this every [`super::hub::CALL_SWEEP`] and whenever a
    /// node's link attaches, so a call whose `llm.ack` never arrived (a node
    /// that vanished mid-call) does not stay in the log forever.
    pub fn sweep(&self) {
        self.calls.sweep();
    }

    /// Serve one `llm.*` call for `node_id`. `None` when the method is not a
    /// model method (the caller routes it elsewhere).
    pub async fn request(
        &self,
        node_id: &str,
        method: &str,
        params: Value,
        out: StreamTx,
    ) -> Option<std::result::Result<Value, RemoteError>> {
        match method {
            LLM_COMPLETE => Some(self.complete(node_id, params, out).await),
            LLM_RESUME => Some(self.resume(node_id, params, out).await),
            _ => None,
        }
    }

    /// Serve one `llm.*` note for `node_id`. `false` when the note is not a
    /// model note.
    pub fn note(&self, node_id: &str, method: &str, params: Value) -> bool {
        match method {
            LLM_ACK => {
                if let Some(call) = params.get("call").and_then(Value::as_str) {
                    self.calls.drop_call(node_id, call, false);
                }
                true
            }
            LLM_CANCEL => {
                if let Some(call) = params.get("call").and_then(Value::as_str) {
                    self.calls.drop_call(node_id, call, true);
                }
                true
            }
            LLM_RECORD => {
                if let Ok(params) = serde_json::from_value::<RecordParams>(params) {
                    if let Some(client) = self.service.model_client(&params.model) {
                        if let Some(ms) = params.tools_phase_ms {
                            client.record_tools_phase_ms(ms);
                        }
                        if let Some(calls) = params.raw_tool_calls {
                            client.record_raw_tool_calls(calls);
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// `llm.complete`: resolve the model, start the model stream, answer
    /// `accepted`, then keep the call open until the stream ends.
    async fn complete(
        &self,
        node_id: &str,
        params: Value,
        out: StreamTx,
    ) -> std::result::Result<Value, RemoteError> {
        let params: CompleteParams = serde_json::from_value(params)
            .map_err(|error| RemoteError::bad_params(format!("llm.complete: {error}")))?;
        self.calls.sweep();
        let client = self.service.model_client(&params.model).ok_or_else(|| {
            RemoteError::model_not_configured(format!(
                "{}/{} is not configured on the hub",
                params.model.endpoint, params.model.model
            ))
        })?;
        if let Some(ms) = params.timings.prep_ms {
            client.record_prep_ms(ms);
        }
        if let Some(ms) = params.timings.compact_ms {
            client.record_compact_ms(ms);
        }
        if let Some(ms) = params.timings.backoff_ms {
            client.record_backoff_ms(ms);
        }

        let calls = self.calls.clone();
        let node = node_id.to_string();
        let call_id = params.call.clone();
        let (init_tx, init_rx) = oneshot::channel();
        tokio::spawn(async move {
            let stream = match client
                .complete_with_context(
                    &params.messages,
                    &params.tools,
                    params.tool_choice.clone(),
                    &params.context,
                )
                .await
            {
                Ok(stream) => stream,
                Err(Error::Provider(message)) => {
                    let error = RemoteError::provider(message);
                    calls.reject(&node, &params.call, error.clone());
                    let _ = init_tx.send(Err(error));
                    return;
                }
                Err(error) => {
                    let error = RemoteError::internal(error.to_string());
                    calls.reject(&node, &params.call, error.clone());
                    let _ = init_tx.send(Err(error));
                    return;
                }
            };
            if let Err(error) = out.start(json!({ "accepted": true })).await {
                let _ = init_tx.send(Err(error));
                return;
            }
            let call = calls.accept(&node, &params.call, out);
            let _ = init_tx.send(Ok(()));
            drive(stream, call).await;
        });

        match init_rx.await {
            Ok(Ok(())) => {
                if let Some(call) = self.calls.get(node_id, &call_id) {
                    call.wait_done().await;
                }
                Ok(Value::Null)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(RemoteError::internal(
                "the model task ended before it answered",
            )),
        }
    }

    /// `llm.resume`: replay the events above `from`, then re-attach live.
    async fn resume(
        &self,
        node_id: &str,
        params: Value,
        out: StreamTx,
    ) -> std::result::Result<Value, RemoteError> {
        let params: ResumeParams = serde_json::from_value(params)
            .map_err(|error| RemoteError::bad_params(format!("llm.resume: {error}")))?;
        let call = self.calls.get(node_id, &params.call).ok_or_else(|| {
            RemoteError::new(CODE_UNKNOWN_CALL, format!("unknown call: {}", params.call))
        })?;
        // A call the hub rejected before accepting reports the same provider
        // error to the resume (no stream was ever opened).
        {
            let state = call.state.lock().await;
            if let Some(error) = &state.rejected {
                return Err(error.clone());
            }
        }
        let mut state = call.state.lock().await;
        out.start(json!({ "accepted": true })).await?;
        let start = params.from.min(state.events.len());
        for event in state.events[start..].iter() {
            out.item(event.clone()).await?;
        }
        state.attached = Some(out);
        let ended = state.ended;
        drop(state);
        if !ended {
            call.wait_done().await;
        }
        Ok(Value::Null)
    }
}

/// Drain one model stream into its call log, pushing each event live while a
/// stream is attached. Stops early when the node cancels or acks the call.
async fn drive(mut stream: BoxStream<'_, LlmEvent>, call: Arc<Call>) {
    let mut progress = call.progress.subscribe();
    loop {
        tokio::select! {
            _ = progress.changed() => {
                if *progress.borrow_and_update() != Progress::Running {
                    return;
                }
            }
            next = stream.next() => {
                match next {
                    Some(event) => {
                        let data = match serde_json::to_value(&event) {
                            Ok(data) => data,
                            // `LlmEvent` always serialises; keep a hostile
                            // custom event from killing the call.
                            Err(_) => continue,
                        };
                        let mut state = call.state.lock().await;
                        state.events.push(data.clone());
                        if let Some(tx) = state.attached.clone() {
                            if tx.item(data).await.is_err() {
                                state.attached = None;
                            }
                        }
                        drop(state);
                        call.changed.notify_waiters();
                    }
                    None => {
                        let mut state = call.state.lock().await;
                        state.ended = true;
                        state.ended_at = Some(Instant::now());
                        state.attached = None;
                        drop(state);
                        call.set_progress(Progress::Ended);
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex as StdMutex;

    use futures::stream;

    use crate::config::Endpoint;
    use crate::provider::{Capabilities, Message};
    use crate::remote::auth;
    use crate::remote::link::{accept_on, dial_on, HubAuth};
    use crate::remote::pairing::NodeIdentity;

    const NODE_ID: &str = "0123456789abcdef0123456789abcdef";

    /// A node identity with a fresh token.
    struct Node {
        identity: NodeIdentity,
        token: auth::Token,
    }

    impl Node {
        fn new() -> Self {
            let token = auth::random_token();
            Self {
                identity: NodeIdentity {
                    hub: "duplex".to_string(),
                    node_id: NODE_ID.to_string(),
                    token: auth::hex_encode(&token),
                    hub_id: "hub-test".to_string(),
                    paired_ms: 0,
                },
                token,
            }
        }

        fn authz(&self) -> HubAuth {
            let node_id = self.identity.node_id.clone();
            let token = self.token;
            HubAuth::new(
                "hub-test",
                "epoch-test",
                Arc::new(move |id: &str| (id == node_id).then_some(token)),
            )
        }
    }

    /// The node accepts no inbound calls in these tests.
    struct NoopHandler;

    #[async_trait]
    impl crate::remote::LinkHandler for NoopHandler {
        async fn request(
            &self,
            method: &str,
            _params: Value,
            _out: StreamTx,
        ) -> std::result::Result<Value, RemoteError> {
            Err(RemoteError::unknown_method(method))
        }

        fn note(&self, _method: &str, _params: Value) {}
    }

    /// The hub's model handlers, composed as step 5's hub will.
    struct HubLink {
        llm: Arc<LlmHandler>,
    }

    #[async_trait]
    impl crate::remote::LinkHandler for HubLink {
        async fn request(
            &self,
            method: &str,
            params: Value,
            out: StreamTx,
        ) -> std::result::Result<Value, RemoteError> {
            match self.llm.request(NODE_ID, method, params, out).await {
                Some(result) => result,
                None => Err(RemoteError::unknown_method(method)),
            }
        }

        fn note(&self, method: &str, params: Value) {
            self.llm.note(NODE_ID, method, params);
        }
    }

    /// A dialed link pair; keep both handles alive.
    struct Pair {
        node: Link,
        /// Held for its lifetime: dropping the hub handle closes the socket.
        _hub: Link,
    }

    async fn linked_pair(hub: Arc<dyn crate::remote::LinkHandler>) -> Pair {
        let node = Node::new();
        let authz = node.authz();
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub_task = tokio::spawn(async move { accept_on(hub_io, authz, hub).await });
        let node_link = dial_on(node_io, &node.identity, Arc::new(NoopHandler))
            .await
            .expect("the node dials and authenticates");
        let hub_link = hub_task.await.expect("hub task joins").expect("accept");
        Pair {
            node: node_link,
            _hub: hub_link,
        }
    }

    fn hub_link(handler: &Arc<LlmHandler>) -> Arc<dyn crate::remote::LinkHandler> {
        Arc::new(HubLink {
            llm: handler.clone(),
        })
    }

    /// Publish a link into the slot and clear it when the link dies, the way
    /// the node runtime does.
    fn publish(slot: &Arc<LinkSlot>, link: &Link) {
        slot.set(Some(link.clone()));
        let slot = slot.clone();
        let link = link.clone();
        tokio::spawn(async move {
            link.closed().await;
            slot.set(None);
        });
    }

    /// What the scripted hub client serves for one `complete` call.
    enum Script {
        /// Yield these events, then end.
        Items(Vec<LlmEvent>),
        /// Yield `(delay, event)` steps, sleeping before each, then end.
        Timed(Vec<(Duration, LlmEvent)>),
        /// Yield these events, then hang until the stream is dropped; the
        /// flag proves the hub stopped the model stream.
        Pending(Vec<LlmEvent>, Arc<AtomicBool>),
        /// Reject the request.
        Fail(Error),
    }

    /// Records the `record_*` calls the hub applies from timings and notes.
    #[derive(Default)]
    struct Recorded(StdMutex<Vec<String>>);

    impl Recorded {
        fn entries(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }

        fn push(&self, entry: String) {
            self.0
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(entry);
        }
    }

    struct ScriptedClient {
        scripts: StdMutex<VecDeque<Script>>,
        caps: Capabilities,
        recorded: Arc<Recorded>,
        calls: AtomicUsize,
    }

    impl ScriptedClient {
        fn new(scripts: Vec<Script>) -> Arc<Self> {
            Arc::new(Self {
                scripts: StdMutex::new(scripts.into()),
                caps: Capabilities::openai(),
                recorded: Arc::new(Recorded::default()),
                calls: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(AtomicOrdering::SeqCst)
        }

        fn next_script(&self) -> Result<BoxStream<'_, LlmEvent>> {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            let script = self
                .scripts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .pop_front()
                .expect("a script for every hub model call");
            match script {
                Script::Items(items) => Ok(Box::pin(stream::iter(items))),
                Script::Timed(steps) => Ok(Box::pin(stream::unfold(
                    steps.into_iter(),
                    |mut steps| async move {
                        let (delay, event) = steps.next()?;
                        tokio::time::sleep(delay).await;
                        Some((event, steps))
                    },
                ))),
                Script::Pending(items, stopped) => {
                    struct Stop(Arc<AtomicBool>);
                    impl Drop for Stop {
                        fn drop(&mut self) {
                            self.0.store(true, AtomicOrdering::SeqCst);
                        }
                    }
                    let stop = Stop(stopped);
                    Ok(Box::pin(stream::unfold(
                        (items.into_iter(), stop),
                        |(mut items, stop)| async move {
                            if let Some(event) = items.next() {
                                return Some((event, (items, stop)));
                            }
                            std::future::pending::<()>().await;
                            unreachable!("pending never resolves")
                        },
                    )))
                }
                Script::Fail(error) => Err(error),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }

        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }

        fn model(&self) -> &str {
            "m"
        }

        fn provider_name(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.next_script()
        }

        async fn complete_with_context(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
            _context: &LlmRequestContext,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.next_script()
        }

        fn record_tools_phase_ms(&self, ms: u32) {
            self.recorded.push(format!("tools:{ms}"));
        }

        fn record_prep_ms(&self, ms: u32) {
            self.recorded.push(format!("prep:{ms}"));
        }

        fn record_compact_ms(&self, ms: u32) {
            self.recorded.push(format!("compact:{ms}"));
        }

        fn record_backoff_ms(&self, ms: u32) {
            self.recorded.push(format!("backoff:{ms}"));
        }

        fn record_raw_tool_calls(&self, calls: Vec<(String, String, String)>) {
            self.recorded.push(format!("raw:{}", calls.len()));
        }
    }

    struct ScriptedService {
        client: Arc<ScriptedClient>,
        known: bool,
    }

    impl LlmService for ScriptedService {
        fn model_client(&self, _model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
            if self.known {
                Some(self.client.clone() as Arc<dyn LlmClient>)
            } else {
                None
            }
        }
    }

    /// One hub process: its handler and the scripted model client behind it.
    struct Hub {
        handler: Arc<LlmHandler>,
        client: Arc<ScriptedClient>,
    }

    fn hub(scripts: Vec<Script>) -> Hub {
        let client = ScriptedClient::new(scripts);
        let handler = LlmHandler::new(Arc::new(ScriptedService {
            client: client.clone(),
            known: true,
        }));
        Hub { handler, client }
    }

    fn unknown_model_hub() -> Hub {
        let client = ScriptedClient::new(Vec::new());
        let handler = LlmHandler::new(Arc::new(ScriptedService {
            client: client.clone(),
            known: false,
        }));
        Hub { handler, client }
    }

    fn test_config() -> Config {
        let mut cfg = Config::default();
        let mut endpoint = Endpoint::test_default();
        endpoint.name = "hub-ep".to_string();
        cfg.endpoints = vec![endpoint];
        cfg
    }

    fn model_ref() -> ModelRef {
        ModelRef {
            endpoint: "hub-ep".to_string(),
            model: "m".to_string(),
            reasoning_effort: None,
        }
    }

    fn remote_client(slot: Arc<LinkSlot>, budget: Duration) -> Arc<dyn LlmClient> {
        RemoteTransport::new(slot, budget)
            .client_for(&test_config(), &model_ref())
            .expect("every mirrored endpoint yields a client")
    }

    fn user() -> Vec<Message> {
        vec![Message::user_text("hi")]
    }

    fn delta(text: &str) -> LlmEvent {
        LlmEvent::TextDelta {
            text: text.to_string(),
        }
    }

    async fn next_event(stream: &mut BoxStream<'_, LlmEvent>, wait: Duration) -> LlmEvent {
        tokio::time::timeout(wait, stream.next())
            .await
            .expect("an event arrives")
            .expect("the stream yields an event")
    }

    async fn expect_end(stream: &mut BoxStream<'_, LlmEvent>) {
        assert!(
            tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .expect("the stream ends")
                .is_none(),
            "the stream must end cleanly"
        );
    }

    async fn text_of(event: LlmEvent) -> String {
        match event {
            LlmEvent::TextDelta { text } => text,
            other => panic!("expected a text delta, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn remote_client_streams_hub_events_in_order() {
        let hub = hub(vec![Script::Items(vec![
            delta("a"),
            delta("b"),
            delta("c"),
        ])]);
        let slot = Arc::new(LinkSlot::new());
        let pair = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &pair.node);
        let client = remote_client(slot.clone(), Duration::from_secs(2));
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("the hub accepts");
        let mut texts = Vec::new();
        while let Some(event) = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("an event arrives")
        {
            texts.push(text_of(event).await);
        }
        assert_eq!(texts, vec!["a", "b", "c"]);
        assert_eq!(hub.client.calls(), 1);
        drop((pair, slot));
    }

    #[tokio::test]
    async fn remote_client_maps_provider_error_verbatim() {
        let hub = hub(vec![Script::Fail(Error::Provider(
            "401 unauthorized: bad key".to_string(),
        ))]);
        let slot = Arc::new(LinkSlot::new());
        let pair = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &pair.node);
        let client = remote_client(slot.clone(), Duration::from_secs(2));
        let error = client
            .complete(&user(), &[], None)
            .await
            .err()
            .expect("the provider error must fail the call");
        match error {
            Error::Provider(ref message) => assert_eq!(message, "401 unauthorized: bad key"),
            other => panic!("expected Error::Provider, got {other:?}"),
        }
        assert!(
            error_is_non_retryable(&error),
            "a 401 stays non-retryable through the proxy"
        );
        drop((pair, slot));
    }

    fn error_is_non_retryable(error: &Error) -> bool {
        error.is_non_retryable()
    }

    #[tokio::test]
    async fn unknown_model_is_non_retryable() {
        let hub = unknown_model_hub();
        let slot = Arc::new(LinkSlot::new());
        let pair = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &pair.node);
        let client = remote_client(slot.clone(), Duration::from_secs(2));
        let error = client
            .complete(&user(), &[], None)
            .await
            .err()
            .expect("an unknown model must fail the call");
        match &error {
            Error::Provider(message) => assert_eq!(
                message,
                "hub: model not found: hub-ep/m is not configured on the hub"
            ),
            other => panic!("expected Error::Provider, got {other:?}"),
        }
        assert!(error_is_non_retryable(&error));
        assert_eq!(hub.client.calls(), 0);
        drop((pair, slot));
    }

    #[tokio::test]
    async fn link_drop_mid_stream_resumes_without_duplicates() {
        let hub = hub(vec![Script::Timed(vec![
            (Duration::from_millis(10), delta("a")),
            (Duration::from_millis(120), delta("b")),
            (Duration::from_millis(60), delta("c")),
        ])]);
        let slot = Arc::new(LinkSlot::new());
        let first = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &first.node);
        let client = remote_client(slot.clone(), Duration::from_millis(400));
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("the hub accepts");

        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(2)).await).await, "a");

        // The connection dies mid-stream; the hub keeps draining the model.
        drop(first);
        let connector = {
            let handler = hub.handler.clone();
            let slot = slot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                let pair = linked_pair(hub_link(&handler)).await;
                publish(&slot, &pair.node);
                pair
            })
        };

        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(3)).await).await, "b");
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(3)).await).await, "c");
        expect_end(&mut stream).await;
        assert_eq!(hub.client.calls(), 1, "a resume never re-issues the call");
        drop(connector.await.expect("the connector joins"));
    }

    #[tokio::test]
    async fn hub_restart_before_first_item_reissues_call() {
        let hub_a = hub(vec![Script::Timed(vec![(
            Duration::from_millis(300),
            delta("a"),
        )])]);
        let slot = Arc::new(LinkSlot::new());
        let first = linked_pair(hub_link(&hub_a.handler)).await;
        publish(&slot, &first.node);
        let client = remote_client(slot.clone(), Duration::from_millis(500));
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("hub A accepts");
        // Let hub A start the call, then kill it before any item arrives.
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(first);

        // A restarted hub has a fresh call log: the resume is `unknown_call`,
        // and with nothing consumed the node re-issues the request.
        let hub_b = hub(vec![Script::Items(vec![delta("a")])]);
        let connector = {
            let handler = hub_b.handler.clone();
            let slot = slot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                let pair = linked_pair(hub_link(&handler)).await;
                publish(&slot, &pair.node);
                pair
            })
        };
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(3)).await).await, "a");
        expect_end(&mut stream).await;
        assert_eq!(hub_b.client.calls(), 1, "the request was re-issued once");
        drop(connector.await.expect("the connector joins"));
    }

    #[tokio::test]
    async fn hub_restart_after_items_yields_error_and_ends() {
        let hub_a = hub(vec![Script::Timed(vec![
            (Duration::from_millis(10), delta("a")),
            (Duration::from_millis(300), delta("b")),
        ])]);
        let slot = Arc::new(LinkSlot::new());
        let first = linked_pair(hub_link(&hub_a.handler)).await;
        publish(&slot, &first.node);
        let client = remote_client(slot.clone(), Duration::from_millis(500));
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("hub A accepts");
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(2)).await).await, "a");
        drop(first);

        // A restarted hub has no call to resume: the partial response is kept
        // (the node already saw `a`) and the turn gets a terminal error.
        let hub_b = hub(Vec::new());
        let connector = {
            let handler = hub_b.handler.clone();
            let slot = slot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                let pair = linked_pair(hub_link(&handler)).await;
                publish(&slot, &pair.node);
                pair
            })
        };
        match next_event(&mut stream, Duration::from_secs(3)).await {
            LlmEvent::Error { error } => assert_eq!(
                error,
                "remote link lost mid-response: the hub restarted; the partial response was kept"
            ),
            other => panic!("expected a terminal error event, got {other:?}"),
        }
        expect_end(&mut stream).await;
        assert_eq!(hub_b.client.calls(), 0, "a partial response is never re-issued");
        drop(connector.await.expect("the connector joins"));
    }

    #[tokio::test]
    async fn outage_budget_exceeded_fails_complete_fast() {
        let slot = Arc::new(LinkSlot::new());
        let client = remote_client(slot, Duration::from_millis(300));
        let started = Instant::now();
        let error = client
            .complete(&user(), &[], None)
            .await
            .err()
            .expect("no link means no call");
        match error {
            Error::Provider(message) => assert_eq!(
                message,
                "hub link down: no connection to the hub for 0.3s"
            ),
            other => panic!("expected Error::Provider, got {other:?}"),
        }
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(250), "waited {elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "did not fail fast: {elapsed:?}");
    }

    #[tokio::test]
    async fn budget_is_per_outage_not_per_call() {
        // `b` is buffered during the first outage, `c` during the second; each
        // outage is under the 500 ms budget, the two together well over it.
        let hub = hub(vec![Script::Timed(vec![
            (Duration::from_millis(10), delta("a")),
            (Duration::from_millis(250), delta("b")),
            (Duration::from_millis(400), delta("c")),
        ])]);
        let slot = Arc::new(LinkSlot::new());
        let first = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &first.node);
        let client = remote_client(slot.clone(), Duration::from_millis(500));
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("the hub accepts");
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(2)).await).await, "a");

        drop(first);
        let connector = {
            let handler = hub.handler.clone();
            let slot = slot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let pair = linked_pair(hub_link(&handler)).await;
                publish(&slot, &pair.node);
                pair
            })
        };
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(3)).await).await, "b");
        drop(connector.await.expect("the first connector joins"));

        let connector = {
            let handler = hub.handler.clone();
            let slot = slot.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let pair = linked_pair(hub_link(&handler)).await;
                publish(&slot, &pair.node);
                pair
            })
        };
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(3)).await).await, "c");
        expect_end(&mut stream).await;
        drop(connector.await.expect("the second connector joins"));
    }

    #[tokio::test]
    async fn cancel_on_drop_stops_hub_stream() {
        let stopped = Arc::new(AtomicBool::new(false));
        let hub = hub(vec![Script::Pending(vec![delta("a")], stopped.clone())]);
        let slot = Arc::new(LinkSlot::new());
        let pair = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &pair.node);
        let client = remote_client(slot.clone(), Duration::from_secs(2));
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("the hub accepts");
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(2)).await).await, "a");
        drop(stream);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !stopped.load(AtomicOrdering::SeqCst) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the hub never stopped the abandoned model stream"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop((pair, slot));
    }

    #[tokio::test]
    async fn timings_ride_next_request() {
        let hub = hub(vec![Script::Items(vec![delta("a")])]);
        let slot = Arc::new(LinkSlot::new());
        let pair = linked_pair(hub_link(&hub.handler)).await;
        publish(&slot, &pair.node);
        let client = remote_client(slot.clone(), Duration::from_secs(2));
        client.record_prep_ms(3);
        client.record_compact_ms(4);
        client.record_backoff_ms(5);
        let mut stream = client
            .complete(&user(), &[], None)
            .await
            .expect("the hub accepts");
        client.record_tools_phase_ms(7);
        client.record_raw_tool_calls(vec![("id".to_string(), "tool".to_string(), "{}".to_string())]);
        assert_eq!(text_of(next_event(&mut stream, Duration::from_secs(2)).await).await, "a");
        expect_end(&mut stream).await;
        // Notes are best effort; give the link a moment to deliver them.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let recorded = hub.client.recorded.entries();
        for expected in ["prep:3", "compact:4", "backoff:5", "tools:7", "raw:1"] {
            assert!(
                recorded.iter().any(|entry| entry == expected),
                "missing {expected} in {recorded:?}"
            );
        }
        drop((pair, slot));
    }
    #[tokio::test]
    async fn calllog_sweep_drops_unacked_ended_calls() {
        let log = CallLog::with_ttl(Duration::from_millis(20));
        let ended = Arc::new(Call::new());
        {
            let mut state = ended
                .state
                .try_lock()
                .expect("a fresh call state is unlocked");
            state.ended = true;
            state.ended_at = Some(Instant::now());
        }
        ended.set_progress(Progress::Ended);
        log.map()
            .insert(("node".to_string(), "ended".to_string()), ended);
        let running = Arc::new(Call::new());
        log.map()
            .insert(("node".to_string(), "running".to_string()), running);
        tokio::time::sleep(Duration::from_millis(40)).await;
        log.sweep();
        assert!(
            log.get("node", "ended").is_none(),
            "an ended call whose ack never arrived is swept after the TTL"
        );
        assert!(
            log.get("node", "running").is_some(),
            "a running call survives the sweep"
        );
    }
}
