// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Routed shared memory (plan a040713f, phase 2 step 4).
//!
//! A node keeps its own SQLite store for everything session-local — working
//! memories, the tool-result archive, request/savings/spend rows, sessions,
//! the derived-index state — and routes the *shared knowledge* surface of
//! [`MemoryStoreTrait`] over the link, so the hub's store stays the single
//! shared memory. [`RoutedMemoryStore`] is the node's implementation; the hub
//! answers it through [`MemoryHandler`].
//!
//! Two things make the split invisible to the engine: every routed answer
//! carries the hub store's `version`, which the node mirrors so the agent
//! loop's per-turn recall cache invalidates exactly as it would on the hub;
//! and the hub pushes a `mem.version` note whenever its store changes, so a
//! write made by another node (or the hub itself) invalidates the cache too.
//!
//! Errors are [`Error::Memory`] with the hub's message verbatim; a link that
//! stays down past the outage budget reports the same "hub link down" string
//! the model proxy uses.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::memory::{
    ArchiveSearchHit, ArchivedToolResult, Embedder, Memory, MemoryFilter, MemorySearchConfig,
    MemoryStore, MemoryStoreTrait, MemoryTier, ProjectStats, RequestStats, SavingsEvent,
    SavingsStats, SavingsValuation, ScoredMemory, Session, SessionStats, SessionSummary,
    SpendEvent,
};
use crate::remote::{Link, LinkSlot, RemoteError, CODE_LINK_DOWN};

/// The memory method names on the link.
pub const MEM_WRITE: &str = "mem.write";
/// Recall (bumps the hub store's access counters).
pub const MEM_RECALL: &str = "mem.recall";
/// Recall without bumping counters.
pub const MEM_RECALL_PEEK: &str = "mem.recall_peek";
/// Bump one memory's access counters.
pub const MEM_ACCESS: &str = "mem.access";
/// Bump many memories' access counters.
pub const MEM_BATCH_ACCESS: &str = "mem.batch_access";
/// Update a memory's title/content in place.
pub const MEM_UPDATE: &str = "mem.update";
/// Supersede one memory with another, atomically.
pub const MEM_SUPERSEDE: &str = "mem.supersede";
/// Quarantine a record.
pub const MEM_QUARANTINE: &str = "mem.quarantine";
/// Hard-delete one memory.
pub const MEM_DELETE: &str = "mem.delete";
/// Fetch one memory by id.
pub const MEM_GET: &str = "mem.get";
/// List memories matching a filter.
pub const MEM_LIST_FILTERED: &str = "mem.list_filtered";
/// List memories in one tier.
pub const MEM_LIST_BY_TIER: &str = "mem.list_by_tier";
/// Count memories in one tier.
pub const MEM_COUNT_BY_TIER: &str = "mem.count_by_tier";
/// The standing project-memory primer.
pub const MEM_PRIMER: &str = "mem.primer";
/// Count memories as recalled by the auto-recall path.
pub const MEM_NOTE_RECALLED: &str = "mem.note_recalled";
/// The most-recalled records.
pub const MEM_TOP_RECALLED: &str = "mem.top_recalled";
/// The hub's live [`MemorySearchConfig`].
pub const MEM_CONFIG: &str = "mem.config";
/// Hub store-version push note.
pub const MEM_VERSION: &str = "mem.version";

/// Error code: the hub has no live shared memory store.
pub const CODE_MEMORY: &str = "memory";

/// How often an attached hub link checks its store version for a push.
pub const VERSION_POLL: Duration = Duration::from_millis(500);

/// The message a routed call reports when the link stays down past the
/// outage budget.
fn down_message(budget: Duration) -> String {
    format!(
        "hub link down: no connection to the hub for {}s",
        budget.as_secs_f64()
    )
}

/// The node's memory store: session-local rows stay in `local`, shared
/// knowledge travels over the link.
pub struct RoutedMemoryStore {
    local: Arc<MemoryStore>,
    slot: Arc<LinkSlot>,
    budget: Duration,
    /// The hub store's version as last observed on any routed answer or note.
    hub_version: AtomicU64,
    /// The hub's search knobs, cached because `memory_search_config` is sync.
    search_config: RwLock<MemorySearchConfig>,
}

impl RoutedMemoryStore {
    /// A routed store over the node-local `local` store and `slot`'s link.
    pub fn new(local: Arc<MemoryStore>, slot: Arc<LinkSlot>, budget: Duration) -> Arc<Self> {
        Arc::new(Self {
            local,
            slot,
            budget,
            hub_version: AtomicU64::new(0),
            search_config: RwLock::new(MemorySearchConfig::default()),
        })
    }

    /// The node-local store (tests and the node runtime inspect it).
    pub fn local(&self) -> &Arc<MemoryStore> {
        &self.local
    }

    /// The hub store version currently mirrored.
    pub fn mirrored_version(&self) -> u64 {
        self.hub_version.load(Ordering::Relaxed)
    }

    /// Refresh the cached search config and version mirror from the hub.
    /// The node runtime calls this once each time the link comes up.
    pub async fn refresh(&self) -> Result<()> {
        let answer = self.call(MEM_CONFIG, json!({})).await?;
        let Some(config) = answer.get("config") else {
            return Ok(());
        };
        let config: MemorySearchConfig = serde_json::from_value(config.clone())
            .map_err(|error| Error::Memory(format!("hub: malformed mem.config: {error}")))?;
        *self
            .search_config
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = config;
        Ok(())
    }

    /// Apply a `mem.version` note from the hub.
    pub fn note_version(&self, version: u64) {
        self.hub_version.store(version, Ordering::Relaxed);
    }

    /// Serve one `mem.*` note from the hub. `false` when the note is not a
    /// memory note.
    pub fn note(&self, method: &str, params: Value) -> bool {
        match method {
            MEM_VERSION => {
                if let Some(version) = params.get("version").and_then(Value::as_u64) {
                    self.note_version(version);
                }
                true
            }
            _ => false,
        }
    }

    /// One routed call: wait for the link within the outage budget, call the
    /// hub, and mirror the store version every answer carries.
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let link = self
            .slot
            .wait_up(self.budget)
            .await
            .ok_or_else(|| Error::Memory(down_message(self.budget)))?;
        let answer = link
            .call(method, params, self.budget)
            .await
            .map_err(|error| match error.code.as_str() {
                CODE_LINK_DOWN | crate::remote::CODE_TIMEOUT => {
                    Error::Memory(down_message(self.budget))
                }
                _ => Error::Memory(error.message),
            })?;
        self.observe_version(&answer);
        Ok(answer)
    }

    /// Mirror the `version` every hub answer carries.
    fn observe_version(&self, answer: &Value) {
        if let Some(version) = answer.get("version").and_then(Value::as_u64) {
            self.note_version(version);
        }
    }

    /// Decode a `{hits: [ScoredMemory]}` answer.
    fn hits(answer: Value) -> Result<Vec<ScoredMemory>> {
        answer
            .get("hits")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| Error::Memory(format!("hub: malformed recall hits: {error}")))
            .map(Option::unwrap_or_default)
    }

    /// Decode a `{memories: [Memory]}` answer.
    fn memories(answer: Value) -> Result<Vec<Memory>> {
        answer
            .get("memories")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| Error::Memory(format!("hub: malformed memories: {error}")))
            .map(Option::unwrap_or_default)
    }
}

#[async_trait]
impl MemoryStoreTrait for RoutedMemoryStore {
    async fn write(&self, memory: Memory) -> Result<String> {
        if memory.tier == MemoryTier::Working || memory.record_class == crate::memory::MemoryClass::Derived
        {
            return self.local.write(memory).await;
        }
        let answer = self.call(MEM_WRITE, json!({ "memory": memory })).await?;
        answer
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Memory("hub: mem.write returned no id".to_string()))
    }

    async fn recall(&self, query: &str, filter: &MemoryFilter) -> Result<Vec<ScoredMemory>> {
        if filter.tier == Some(MemoryTier::Working) {
            return self.local.recall(query, filter).await;
        }
        if filter.tier.is_none() && !filter.exclude_working {
            let answer = self
                .call(MEM_RECALL, json!({ "query": query, "filter": filter }))
                .await?;
            let mut hits = Self::hits(answer)?;
            // The node-local working tier is session data: merge it with the
            // hub's shared hits and let the engine see one ranked list.
            let mut local = self.local.recall(query, filter).await?;
            local.append(&mut hits);
            local.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            if let Some(limit) = filter.limit {
                local.truncate(limit);
            }
            return Ok(local);
        }
        let answer = self
            .call(MEM_RECALL, json!({ "query": query, "filter": filter }))
            .await?;
        Self::hits(answer)
    }

    async fn recall_peek(&self, query: &str, filter: &MemoryFilter) -> Result<Vec<ScoredMemory>> {
        if filter.tier == Some(MemoryTier::Working) {
            return self.local.recall_peek(query, filter).await;
        }
        if filter.tier.is_none() && !filter.exclude_working {
            let answer = self
                .call(MEM_RECALL_PEEK, json!({ "query": query, "filter": filter }))
                .await?;
            let mut hits = Self::hits(answer)?;
            let mut local = self.local.recall_peek(query, filter).await?;
            local.append(&mut hits);
            local.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            if let Some(limit) = filter.limit {
                local.truncate(limit);
            }
            return Ok(local);
        }
        let answer = self
            .call(MEM_RECALL_PEEK, json!({ "query": query, "filter": filter }))
            .await?;
        Self::hits(answer)
    }

    async fn access(&self, id: &str) -> Result<()> {
        if self.local.get_memory(id).await?.is_some() {
            return self.local.access(id).await;
        }
        self.call(MEM_ACCESS, json!({ "id": id })).await?;
        Ok(())
    }

    async fn batch_access(&self, ids: &[String]) -> Result<()> {
        let mut local_ids = Vec::new();
        let mut hub_ids = Vec::new();
        for id in ids {
            if self.local.get_memory(id).await?.is_some() {
                local_ids.push(id.clone());
            } else {
                hub_ids.push(id.clone());
            }
        }
        if !local_ids.is_empty() {
            self.local.batch_access(&local_ids).await?;
        }
        if !hub_ids.is_empty() {
            self.call(MEM_BATCH_ACCESS, json!({ "ids": hub_ids })).await?;
        }
        Ok(())
    }

    async fn list_by_tier(&self, tier: MemoryTier) -> Result<Vec<Memory>> {
        if tier == MemoryTier::Working {
            return self.local.list_by_tier(tier).await;
        }
        let answer = self.call(MEM_LIST_BY_TIER, json!({ "tier": tier })).await?;
        Self::memories(answer)
    }

    async fn count_by_tier(&self, tier: MemoryTier) -> Result<usize> {
        if tier == MemoryTier::Working {
            return self.local.count_by_tier(tier).await;
        }
        let answer = self.call(MEM_COUNT_BY_TIER, json!({ "tier": tier })).await?;
        answer
            .get("count")
            .and_then(Value::as_u64)
            .map(|count| count as usize)
            .ok_or_else(|| Error::Memory("hub: mem.count_by_tier returned no count".to_string()))
    }

    fn memory_search_config(&self) -> MemorySearchConfig {
        self.search_config
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn version(&self) -> u64 {
        self.hub_version.load(Ordering::Relaxed) + self.local.version()
    }

    async fn update_memory(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
    ) -> Result<bool> {
        if self.local.get_memory(id).await?.is_some() {
            return self.local.update_memory(id, title, content).await;
        }
        let answer = self
            .call(
                MEM_UPDATE,
                json!({ "id": id, "title": title, "content": content }),
            )
            .await?;
        Ok(answer
            .get("updated")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    async fn supersede_memory(&self, old_id: &str, new_memory: Memory) -> Result<String> {
        let answer = self
            .call(
                MEM_SUPERSEDE,
                json!({ "old_id": old_id, "memory": new_memory }),
            )
            .await?;
        answer
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| Error::Memory("hub: mem.supersede returned no id".to_string()))
    }

    async fn quarantine_memory(&self, id: &str, reason: &str) -> Result<()> {
        self.call(MEM_QUARANTINE, json!({ "id": id, "reason": reason }))
            .await?;
        Ok(())
    }

    async fn delete_memory(&self, id: &str) -> Result<bool> {
        let answer = self.call(MEM_DELETE, json!({ "id": id })).await?;
        Ok(answer
            .get("deleted")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    async fn get_memory(&self, id: &str) -> Result<Option<Memory>> {
        if let Some(memory) = self.local.get_memory(id).await? {
            return Ok(Some(memory));
        }
        let answer = self.call(MEM_GET, json!({ "id": id })).await?;
        match answer.get("memory") {
            Some(Value::Null) | None => Ok(None),
            Some(memory) => serde_json::from_value(memory.clone())
                .map(Some)
                .map_err(|error| Error::Memory(format!("hub: malformed memory: {error}"))),
        }
    }

    async fn delete_derived(&self) -> Result<usize> {
        self.local.delete_derived().await
    }

    async fn index_state_get(&self, source_key: &str) -> Result<Option<(String, String)>> {
        self.local.index_state_get(source_key).await
    }

    async fn index_state_upsert(
        &self,
        source_key: &str,
        content_hash: &str,
        memory_id: &str,
    ) -> Result<()> {
        self.local
            .index_state_upsert(source_key, content_hash, memory_id)
            .await
    }

    async fn index_state_remove(&self, source_key: &str) -> Result<()> {
        self.local.index_state_remove(source_key).await
    }

    async fn index_state_clear(&self) -> Result<()> {
        self.local.index_state_clear().await
    }

    async fn index_state_keys(&self) -> Result<Vec<String>> {
        self.local.index_state_keys().await
    }

    async fn set_derived_metadata(
        &self,
        id: &str,
        superseded_by: Option<&str>,
        data: &Value,
    ) -> Result<bool> {
        self.local
            .set_derived_metadata(id, superseded_by, data)
            .await
    }

    async fn list_filtered(&self, filter: &MemoryFilter) -> Result<Vec<Memory>> {
        if filter.tier == Some(MemoryTier::Working) {
            return self.local.list_filtered(filter).await;
        }
        let answer = self
            .call(MEM_LIST_FILTERED, json!({ "filter": filter }))
            .await?;
        Self::memories(answer)
    }

    async fn primer_records(&self, limit: usize) -> Result<Vec<Memory>> {
        let answer = self.call(MEM_PRIMER, json!({ "limit": limit })).await?;
        Self::memories(answer)
    }

    async fn note_recalled(&self, ids: &[String]) -> Result<()> {
        self.call(MEM_NOTE_RECALLED, json!({ "ids": ids })).await?;
        Ok(())
    }

    async fn top_recalled(&self, limit: usize) -> Result<Vec<(String, u64)>> {
        let answer = self.call(MEM_TOP_RECALLED, json!({ "limit": limit })).await?;
        answer
            .get("top")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| Error::Memory(format!("hub: malformed top_recalled: {error}")))
            .map(Option::unwrap_or_default)
    }

    async fn delete_working_for_session(&self, session_id: &str) -> Result<usize> {
        self.local.delete_working_for_session(session_id).await
    }

    async fn stored_model_fingerprints(&self) -> Result<Vec<(String, usize)>> {
        self.local.stored_model_fingerprints().await
    }

    async fn reembed_all(
        &self,
        embedder: Arc<dyn Embedder>,
        progress: Option<&(dyn Fn(usize, usize) + Send + Sync)>,
    ) -> Result<usize> {
        self.local.reembed_all(embedder, progress).await
    }

    async fn start_session(&self, project: &str) -> Result<Session> {
        self.local.start_session(project).await
    }

    async fn end_session(&self, id: &str) -> Result<()> {
        self.local.end_session(id).await
    }

    async fn record_tool_event(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        tool_input: Value,
        tool_output: &str,
        error: Option<&str>,
    ) -> Result<String> {
        self.local
            .record_tool_event(session_id, tool_name, tool_input, tool_output, error)
            .await
    }

    async fn record_request_stats(&self, stats: &RequestStats) -> Result<()> {
        self.local.record_request_stats(stats).await
    }

    async fn record_savings_event(&self, event: &SavingsEvent) -> Result<()> {
        self.local.record_savings_event(event).await
    }

    async fn savings_events_rows(&self, session_id: Option<&str>) -> Result<Vec<SavingsEvent>> {
        self.local.savings_events_rows(session_id).await
    }

    async fn savings_stats(&self) -> Result<SavingsStats> {
        self.local.savings_stats().await
    }

    async fn savings_valuation(&self) -> Result<SavingsValuation> {
        self.local.savings_valuation().await
    }

    async fn cache_lifetime_secs(&self, model: &str) -> Result<Option<u64>> {
        self.local.cache_lifetime_secs(model).await
    }

    async fn record_spend_event(&self, event: &SpendEvent) -> Result<()> {
        self.local.record_spend_event(event).await
    }

    async fn spend_rows_for_plan(&self, plan_id: &str) -> Result<Vec<SpendEvent>> {
        self.local.spend_rows_for_plan(plan_id).await
    }

    async fn archive_tool_result(
        &self,
        session_id: Option<&str>,
        tool: &str,
        detail: Option<&str>,
        content: &str,
    ) -> Result<String> {
        self.local
            .archive_tool_result(session_id, tool, detail, content)
            .await
    }

    async fn expand_tool_result(&self, id: &str) -> Result<Option<ArchivedToolResult>> {
        self.local.expand_tool_result(id).await
    }

    async fn search_archive(&self, query: &str, limit: usize) -> Result<Vec<ArchiveSearchHit>> {
        self.local.search_archive(query, limit).await
    }

    async fn prune_tool_result_archive(&self, older_than_secs: i64, batch: usize) -> Result<usize> {
        self.local
            .prune_tool_result_archive(older_than_secs, batch)
            .await
    }

    async fn request_stats_rows(&self, session_id: &str) -> Result<Vec<RequestStats>> {
        self.local.request_stats_rows(session_id).await
    }

    async fn session_stats(&self, session_id: &str) -> Result<SessionStats> {
        self.local.session_stats(session_id).await
    }

    async fn project_stats(&self) -> Result<ProjectStats> {
        self.local.project_stats().await
    }

    async fn session_list(&self) -> Result<Vec<SessionSummary>> {
        self.local.session_list().await
    }
}

// ── hub side ───────────────────────────────────────────────────────────────

/// What the hub's `mem.*` handlers need from the app: the shared store.
pub trait MemoryService: Send + Sync + 'static {
    /// The hub's shared memory store, or `None` while it is unavailable.
    fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>>;
}

/// The hub's `mem.*` handlers. Run each op on the live shared store and carry
/// the store version in every answer; [`spawn_version_watch`] pushes changes.
pub struct MemoryHandler {
    service: Arc<dyn MemoryService>,
}

impl MemoryHandler {
    /// Handlers over `service`'s shared store.
    pub fn new(service: Arc<dyn MemoryService>) -> Arc<Self> {
        Arc::new(Self { service })
    }

    /// Serve one `mem.*` call. `None` when the method is not a memory method.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Option<std::result::Result<Value, RemoteError>> {
        if !method.starts_with("mem.") {
            return None;
        }
        let store = match self.service.memory() {
            Some(store) => store,
            None => {
                return Some(Err(RemoteError::new(
                    CODE_MEMORY,
                    "the hub has no live memory store",
                )))
            }
        };
        Some(
            match method {
                MEM_WRITE => {
                    async {
                        let params: WriteParams = decode(&params)?;
                        answer(store.write(params.memory).await, |id| json!({ "id": id }))
                    }
                    .await
                }
                MEM_RECALL => self.recall(&store, params, false).await,
                MEM_RECALL_PEEK => self.recall(&store, params, true).await,
                MEM_ACCESS => {
                    async {
                        let params: IdParams = decode(&params)?;
                        answer(store.access(&params.id).await, |()| json!({}))
                    }
                    .await
                }
                MEM_BATCH_ACCESS => {
                    async {
                        let params: IdsParams = decode(&params)?;
                        answer(store.batch_access(&params.ids).await, |()| json!({}))
                    }
                    .await
                }
                MEM_UPDATE => {
                    async {
                        let params: UpdateParams = decode(&params)?;
                        answer(
                            store
                                .update_memory(
                                    &params.id,
                                    params.title.as_deref(),
                                    params.content.as_deref(),
                                )
                                .await,
                            |updated| json!({ "updated": updated }),
                        )
                    }
                    .await
                }
                MEM_SUPERSEDE => {
                    async {
                        let params: SupersedeParams = decode(&params)?;
                        answer(
                            store.supersede_memory(&params.old_id, params.memory).await,
                            |id| json!({ "id": id }),
                        )
                    }
                    .await
                }
                MEM_QUARANTINE => {
                    async {
                        let params: QuarantineParams = decode(&params)?;
                        answer(
                            store.quarantine_memory(&params.id, &params.reason).await,
                            |()| json!({}),
                        )
                    }
                    .await
                }
                MEM_DELETE => {
                    async {
                        let params: IdParams = decode(&params)?;
                        answer(store.delete_memory(&params.id).await, |deleted| {
                            json!({ "deleted": deleted })
                        })
                    }
                    .await
                }
                MEM_GET => {
                    async {
                        let params: IdParams = decode(&params)?;
                        answer(store.get_memory(&params.id).await, |memory| {
                            json!({ "memory": memory })
                        })
                    }
                    .await
                }
                MEM_LIST_FILTERED => {
                    async {
                        let params: FilterParams = decode(&params)?;
                        answer(store.list_filtered(&params.filter).await, |memories| {
                            json!({ "memories": memories })
                        })
                    }
                    .await
                }
                MEM_LIST_BY_TIER => {
                    async {
                        let params: TierParams = decode(&params)?;
                        answer(store.list_by_tier(params.tier).await, |memories| {
                            json!({ "memories": memories })
                        })
                    }
                    .await
                }
                MEM_COUNT_BY_TIER => {
                    async {
                        let params: TierParams = decode(&params)?;
                        answer(store.count_by_tier(params.tier).await, |count| {
                            json!({ "count": count })
                        })
                    }
                    .await
                }
                MEM_PRIMER => {
                    async {
                        let params: LimitParams = decode(&params)?;
                        answer(store.primer_records(params.limit).await, |records| {
                            json!({ "memories": records })
                        })
                    }
                    .await
                }
                MEM_NOTE_RECALLED => {
                    async {
                        let params: IdsParams = decode(&params)?;
                        answer(store.note_recalled(&params.ids).await, |()| json!({}))
                    }
                    .await
                }
                MEM_TOP_RECALLED => {
                    async {
                        let params: LimitParams = decode(&params)?;
                        answer(store.top_recalled(params.limit).await, |top| {
                            json!({ "top": top })
                        })
                    }
                    .await
                }
                MEM_CONFIG => Ok(json!({ "config": store.memory_search_config() })),
                _ => {
                    return None;
                }
            }
            .map(|mut value| {
                if let Value::Object(map) = &mut value {
                    map.insert("version".to_string(), json!(store.version()));
                }
                value
            }),
        )
    }

    /// `mem.recall` / `mem.recall_peek`.
    async fn recall(
        &self,
        store: &Arc<dyn MemoryStoreTrait>,
        params: Value,
        peek: bool,
    ) -> std::result::Result<Value, RemoteError> {
        let params: RecallParams = decode(&params)?;
        let result = if peek {
            store.recall_peek(&params.query, &params.filter).await
        } else {
            store.recall(&params.query, &params.filter).await
        };
        match result {
            Ok(hits) => Ok(json!({ "hits": hits })),
            Err(error) => Err(RemoteError::new(CODE_MEMORY, error.to_string())),
        }
    }
}

/// Decode `params`, answering a `bad_params` stream error when malformed.
fn decode<T: for<'de> Deserialize<'de>>(params: &Value) -> std::result::Result<T, RemoteError> {
    serde_json::from_value(params.clone())
        .map_err(|error| RemoteError::bad_params(format!("memory params: {error}")))
}

/// Run one store op and shape its answer, mapping store errors to `memory`.
fn answer<T>(
    result: crate::error::Result<T>,
    shape: impl FnOnce(T) -> Value,
) -> std::result::Result<Value, RemoteError> {
    match result {
        Ok(value) => Ok(shape(value)),
        Err(error) => Err(RemoteError::new(CODE_MEMORY, error.to_string())),
    }
}

#[derive(Debug, Deserialize)]
struct WriteParams {
    memory: Memory,
}

#[derive(Debug, Deserialize)]
struct IdParams {
    id: String,
}

#[derive(Debug, Deserialize)]
struct IdsParams {
    ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateParams {
    id: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SupersedeParams {
    old_id: String,
    memory: Memory,
}

#[derive(Debug, Deserialize)]
struct QuarantineParams {
    id: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct RecallParams {
    query: String,
    filter: MemoryFilter,
}

#[derive(Debug, Deserialize)]
struct FilterParams {
    filter: MemoryFilter,
}

#[derive(Debug, Deserialize)]
struct TierParams {
    tier: MemoryTier,
}

#[derive(Debug, Deserialize)]
struct LimitParams {
    limit: usize,
}

/// Push a `mem.version` note on `link` whenever the shared store's version
/// changes, checking every [`VERSION_POLL`]; stops when the link goes down.
pub fn spawn_version_watch(
    service: Arc<dyn MemoryService>,
    link: Link,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last = None;
        loop {
            let closed = link.closed();
            tokio::select! {
                _ = closed => return,
                _ = tokio::time::sleep(VERSION_POLL) => {}
            }
            let Some(store) = service.memory() else {
                continue;
            };
            let version = store.version();
            if last != Some(version) {
                last = Some(version);
                link.notify(MEM_VERSION, json!({ "version": version }));
            }
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::memory::{HashEmbedder, MemoryClass};
    use crate::remote::auth;
    use crate::remote::link::{accept_on, dial_on, HubAuth};
    use crate::remote::pairing::NodeIdentity;

    const NODE_ID: &str = "0123456789abcdef0123456789abcdef";

    fn now_secs() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("a clock after 1970")
            .as_secs() as i64
    }

    /// A store on a temp dir inside the checkout's `target/` (never %TEMP%).
    fn open_store(name: &str) -> (PathBuf, Arc<MemoryStore>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        let dir = root.join(format!(
            "mnemo-remote-memory-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the store dir");
        let store = MemoryStore::open(&dir.join("memory.db"), Arc::new(HashEmbedder::new()))
            .expect("open the store");
        (dir, Arc::new(store))
    }

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

    /// The hub's `mem.*` handlers, composed as step 5's hub will.
    /// The hub's `mem.*` handlers, composed as step 5's hub will.
    struct HubLink {
        memory: Arc<MemoryHandler>,
    }

    #[async_trait]
    impl crate::remote::LinkHandler for HubLink {
        async fn request(
            &self,
            method: &str,
            params: Value,
            _out: crate::remote::StreamTx,
        ) -> std::result::Result<Value, RemoteError> {
            match self.memory.request(method, params).await {
                Some(result) => result,
                None => Err(RemoteError::unknown_method(method)),
            }
        }

        fn note(&self, _method: &str, _params: Value) {}
    }

    struct Pair {
        node: Link,
        hub: Link,
    }

    async fn linked_pair(
        node_handler: Arc<dyn crate::remote::LinkHandler>,
        hub_handler: Arc<dyn crate::remote::LinkHandler>,
    ) -> Pair {
        let node = Node::new();
        let authz = node.authz();
        let (node_io, hub_io) = tokio::io::duplex(1 << 20);
        let hub_task = tokio::spawn(async move { accept_on(hub_io, authz, hub_handler).await });
        let node_link = dial_on(node_io, &node.identity, node_handler)
            .await
            .expect("the node dials and authenticates");
        let hub_link = hub_task.await.expect("hub task joins").expect("accept");
        Pair {
            node: node_link,
            hub: hub_link,
        }
    }

    fn publish(slot: &Arc<LinkSlot>, link: &Link) {
        slot.set(Some(link.clone()));
        let slot = slot.clone();
        let link = link.clone();
        tokio::spawn(async move {
            link.closed().await;
            slot.set(None);
        });
    }

    struct HubMemory {
        store: Arc<MemoryStore>,
    }

    impl MemoryService for HubMemory {
        fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
            Some(self.store.clone() as Arc<dyn MemoryStoreTrait>)
        }
    }

    /// A node routed store over its own local store, plus the hub's store and
    /// service, wired together over an in-memory link pair.
    struct Fixture {
        store: Arc<RoutedMemoryStore>,
        hub_store: Arc<MemoryStore>,
        service: Arc<dyn MemoryService>,
        pair: Pair,
        dirs: Vec<PathBuf>,
    }

    async fn fixture(name: &str) -> Fixture {
        let (local_dir, local) = open_store(&format!("{name}-local"));
        let (hub_dir, hub_store) = open_store(&format!("{name}-hub"));
        let slot = Arc::new(LinkSlot::new());
        let store = RoutedMemoryStore::new(local, slot.clone(), Duration::from_secs(2));
        let service: Arc<dyn MemoryService> = Arc::new(HubMemory {
            store: hub_store.clone(),
        });
        let memory = MemoryHandler::new(service.clone());
        let pair = linked_pair(
            Arc::new(NodeStorage(store.clone())),
            Arc::new(HubLink { memory }),
        )
        .await;
        publish(&slot, &pair.node);
        Fixture {
            store,
            hub_store,
            service,
            pair,
            dirs: vec![local_dir, hub_dir],
        }
    }

    /// The node's link handler keeps the `RoutedMemoryStore` alive for the
    /// link's lifetime and routes `mem.version` notes into it.
    struct NodeStorage(Arc<RoutedMemoryStore>);

    #[async_trait]
    impl crate::remote::LinkHandler for NodeStorage {
        async fn request(
            &self,
            method: &str,
            _params: Value,
            _out: crate::remote::StreamTx,
        ) -> std::result::Result<Value, RemoteError> {
            Err(RemoteError::unknown_method(method))
        }

        fn note(&self, method: &str, params: Value) {
            self.0.note(method, params);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for dir in &self.dirs {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// Wait until `check` holds, failing the test after a few seconds.
    async fn wait_until(mut check: impl FnMut() -> bool, what: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn memory(tier: MemoryTier, title: &str, content: &str) -> Memory {
        Memory::new(tier, title, content, now_secs())
    }

    #[tokio::test]
    async fn routed_write_lands_in_hub_store() {
        let fixture = fixture("write").await;
        let id = fixture
            .store
            .write(memory(
                MemoryTier::Semantic,
                "shared fact",
                "alpha shared fact",
            ))
            .await
            .expect("the hub accepts the write");
        assert!(
            fixture
                .hub_store
                .get_memory(&id)
                .await
                .expect("hub lookup")
                .is_some(),
            "the shared write must land in the hub's store"
        );
        assert!(
            fixture
                .store
                .local()
                .get_memory(&id)
                .await
                .expect("local lookup")
                .is_none(),
            "the shared write must not be duplicated on the node"
        );
    }

    #[tokio::test]
    async fn working_tier_write_stays_local() {
        let fixture = fixture("working").await;
        let id = fixture
            .store
            .write(memory(
                MemoryTier::Working,
                "local event",
                "alpha local event",
            ))
            .await
            .expect("the local write succeeds");
        assert!(fixture
            .store
            .local()
            .get_memory(&id)
            .await
            .expect("local lookup")
            .is_some());
        assert!(
            fixture
                .hub_store
                .get_memory(&id)
                .await
                .expect("hub lookup")
                .is_none(),
            "working-tier rows are session-local"
        );
    }

    #[tokio::test]
    async fn derived_write_stays_local() {
        let fixture = fixture("derived").await;
        let mut row = memory(MemoryTier::Episodic, "derived row", "alpha derived fact");
        row.record_class = MemoryClass::Derived;
        let id = fixture
            .store
            .write(row)
            .await
            .expect("the local write succeeds");
        assert!(fixture
            .store
            .local()
            .get_memory(&id)
            .await
            .expect("local lookup")
            .is_some());
        assert!(
            fixture
                .hub_store
                .get_memory(&id)
                .await
                .expect("hub lookup")
                .is_none(),
            "derived (indexer-built) rows are node-local"
        );
    }

    #[tokio::test]
    async fn recall_goes_to_hub_and_include_working_merges() {
        let fixture = fixture("recall").await;
        fixture
            .hub_store
            .write(memory(
                MemoryTier::Semantic,
                "hub fact",
                "alpha shared fact",
            ))
            .await
            .expect("hub write");
        fixture
            .store
            .local()
            .write(memory(
                MemoryTier::Working,
                "local event",
                "alpha local event",
            ))
            .await
            .expect("local write");

        let filter = MemoryFilter {
            exclude_working: false,
            limit: Some(10),
            ..MemoryFilter::default()
        };
        let hits = fixture.store.recall("alpha", &filter).await.expect("recall");
        let titles: Vec<&str> = hits.iter().map(|hit| hit.memory.title.as_str()).collect();
        assert!(titles.contains(&"hub fact"), "hub hits: {titles:?}");
        assert!(
            titles.contains(&"local event"),
            "the working tier merges into an unfiltered recall: {titles:?}"
        );

        let semantic = MemoryFilter::default().tier(MemoryTier::Semantic);
        let hits = fixture
            .store
            .recall("alpha", &semantic)
            .await
            .expect("tiered recall");
        assert!(!hits.is_empty());
        assert!(
            hits.iter().all(|hit| hit.memory.tier == MemoryTier::Semantic),
            "an explicit tier goes to the hub only"
        );
        assert!(hits.iter().any(|hit| hit.memory.title == "hub fact"));
    }

    #[tokio::test]
    async fn id_ops_prefer_local_rows() {
        let fixture = fixture("id-ops").await;
        let local_id = fixture
            .store
            .write(memory(
                MemoryTier::Working,
                "local event",
                "alpha local event",
            ))
            .await
            .expect("local write");
        // The hub has no such row: success proves each op stayed local.
        assert!(fixture
            .store
            .get_memory(&local_id)
            .await
            .expect("get")
            .is_some());
        fixture.store.access(&local_id).await.expect("local access");
        let row = fixture
            .store
            .local()
            .get_memory(&local_id)
            .await
            .expect("local lookup")
            .expect("the row exists");
        assert_eq!(row.access_count, 1);
        assert!(fixture
            .store
            .update_memory(&local_id, Some("renamed"), None)
            .await
            .expect("local update"));
        assert_eq!(
            fixture
                .store
                .local()
                .get_memory(&local_id)
                .await
                .expect("local lookup")
                .expect("the row exists")
                .title,
            "renamed"
        );

        // A shared row falls through to the hub, and a mixed batch splits.
        let hub_id = fixture
            .store
            .write(memory(
                MemoryTier::Semantic,
                "shared fact",
                "alpha shared fact",
            ))
            .await
            .expect("hub write");
        assert!(fixture
            .store
            .get_memory(&hub_id)
            .await
            .expect("get")
            .is_some());
        fixture
            .store
            .batch_access(&[local_id, hub_id])
            .await
            .expect("mixed batch");
    }

    #[tokio::test]
    async fn version_mirror_follows_hub_writes() {
        let fixture = fixture("version").await;
        fixture.store.refresh().await.expect("mem.config");
        let before = fixture.hub_store.version();
        assert_eq!(fixture.store.mirrored_version(), before);

        let watch = spawn_version_watch(fixture.service.clone(), fixture.pair.hub.clone());
        fixture
            .hub_store
            .write(memory(
                MemoryTier::Semantic,
                "hub fact",
                "alpha shared fact",
            ))
            .await
            .expect("hub write");
        let after = fixture.hub_store.version();
        assert_ne!(after, before, "a hub write bumps its version");
        let store = fixture.store.clone();
        wait_until(
            move || store.mirrored_version() == after,
            "the mem.version push",
        )
        .await;
        assert!(fixture.store.version() >= after);

        let config = MemorySearchConfig {
            decay_half_life_days: 42.0,
            ..MemorySearchConfig::default()
        };
        fixture.hub_store.set_memory_search_config(config);
        fixture.store.refresh().await.expect("mem.config");
        assert_eq!(fixture.store.memory_search_config().decay_half_life_days, 42.0);
        watch.abort();
    }
}
