// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Memory debug Tauri commands — the backend of the right-panel "Memory" tab.
//!
//! Surfaces the memory system's internals for debugging: the embedder status +
//! model, the stored vector fingerprints (so a model mismatch / cross-machine
//! drift is visible), per-tier counts, a recall test box, and a browsable
//! memory list with detail.
//!
//! 3b.2 (D2, §5.6): the command LOGIC lives in the kernel lib's
//! [`mnemo::headless::MemoryApi`] — ONE implementation shared with the
//! headless Engine's `Engine::memory()`. These commands are thin callers:
//! resolve the state handles, construct the API, forward. The wire types
//! moved to the lib VERBATIM — their serde shapes are pinned by the row-19
//! contract fixtures (`stats_memory_codegraph_ipc_unchanged`), so the IPC
//! bytes cannot drift.
//!
//! Also serves the Graph tab's "Memory access" section: the
//! [`memory_access_log`] command returns the store's rolling read/write
//! activity log (see `MemoryStore::access_log`).

use tauri::State;

use mnemo::headless::{MemoryApi, MemoryDebugOverview, MemoryDebugWire, ScoredMemoryDebugWire};
use mnemo::memory::{MemoryAccessEntry, MemoryTier};

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// Construct the shared API over the state's handles (ONE place; every
/// command forwards through it — §5.6's "ONE implementation").
pub(crate) fn memory_api(state: &State<'_, IpcState>, configured_model: Option<String>) -> MemoryApi {
    MemoryApi::from_parts(
        state.runtime.memory_store.clone(),
        state.runtime.embedder_status.clone(),
        configured_model,
    )
}

/// The Memory tab's overview card: the embedder status + active model/dim,
/// the configured model, the stored vector fingerprints (for mismatch
/// detection), and per-tier counts. When no memory store is wired (the
/// brain failed to build at startup), returns zeroed counts + an empty
/// fingerprint list + `model_id = "<none>"` so the frontend can show a
/// "not initialized" state.
#[tauri::command]
pub async fn memory_debug_overview(
    state: State<'_, IpcState>,
) -> Result<MemoryDebugOverview, IpcError> {
    let configured_model = {
        let config = state.project.config.lock().await;
        config
            .general
            .general
            .bundled_embedding_model
            .clone()
            // The "hash" sentinel (keyword-only opt-out) is not a real model
            // id — map it to None so the debug overview shows "<none>" instead
            // of leaking the sentinel string. Case-insensitive, matching the
            // sentinel contract everywhere else.
            .filter(|m| !m.eq_ignore_ascii_case(mnemo::config::EMBEDDING_MODEL_SENTINEL_HASH))
    };
    Ok(memory_api(&state, configured_model).overview().await?)
}

/// List memories, optionally filtered by tier, with content truncated to
/// [`MAX_CONTENT_CHARS`](mnemo::headless::MAX_CONTENT_CHARS) chars.
/// `tier = None` lists across all four tiers; `limit` caps the result
/// count per tier (so each tier is fairly represented). Returns `Err`
/// when no memory store is wired.
#[tauri::command]
pub async fn memory_debug_list(
    state: State<'_, IpcState>,
    tier: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<MemoryDebugWire>, IpcError> {
    let tier = tier
        .as_deref()
        .map(|s| {
            MemoryTier::from_str_opt(s).ok_or_else(|| IpcError::msg(format!("unknown tier '{s}'")))
        })
        .transpose()?;
    Ok(memory_api(&state, None).list(tier, limit).await?)
}

/// Run a recall test against the memory store and return the scored results
/// (memory + relevance score). Includes working-tier memories (unlike the
/// production auto-recall, which excludes them by default) so the debug view
/// can surface raw tool-event snapshots too. Returns `Err` when no memory
/// store is wired.
///
/// **Read-only:** goes through `MemoryStore::recall_peek` — the same
/// non-bumping path production auto-recall takes — so a debug recall test
/// never freshens the memories it returns (no `access_count` /
/// `last_accessed_at` update, no decayed-strength boost) and previewing
/// recall ranking cannot change future production rankings.
#[tauri::command]
pub async fn memory_debug_recall(
    state: State<'_, IpcState>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<ScoredMemoryDebugWire>, IpcError> {
    Ok(memory_api(&state, None).search(&query, limit).await?)
}

/// The Graph tab's "Memory access" log: the rolling last-100 memory reads +
/// writes (newest first) from the store's in-memory ring. Degrades to an
/// empty log when no memory store is wired — the Graph tab's section shows
/// "no accesses yet" instead of an error banner.
///
/// Deliberately NOT `spawn_blocking` (unlike the SQLite-backed paths): the
/// log is an in-memory `VecDeque` snapshot behind a short std `Mutex` — no
/// SQLite connection is touched, so there is no sync lock that could park
/// a tokio worker.
#[tauri::command]
pub async fn memory_access_log(
    state: State<'_, IpcState>,
) -> Result<Vec<MemoryAccessEntry>, IpcError> {
    Ok(memory_api(&state, None).access_log(None).await)
}

/// Resolve one `[[wiki-link]]` target to its current record — the
/// knowledge-links surface of the Memory debug tab. Deterministic (id math
/// + one point lookup); a missing target resolves to a path-only entry.
#[tauri::command]
pub async fn memory_debug_resolve_link(
    state: State<'_, IpcState>,
    target: String,
) -> Result<Option<mnemo::memory::indexer::ResolvedLink>, IpcError> {
    Ok(memory_api(&state, None).resolve_link(&target).await?)
}

/// Every live memory whose `data.links` points at `rel` (the "referenced
/// by" reverse surface for the knowledge UI). Degrades to an empty list when
/// no memory store is wired.
#[tauri::command]
pub async fn memory_debug_backlinks(
    state: State<'_, IpcState>,
    rel: String,
) -> Result<Vec<MemoryDebugWire>, IpcError> {
    Ok(memory_api(&state, None).backlinks(&rel).await?)
}