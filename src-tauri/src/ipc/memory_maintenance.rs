// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Memory maintenance Tauri commands — the backend of Settings → Memory.
//!
//! Three fire-and-forget commands (mirroring `download_bundled_model`'s
//! shape): [`memory_cleanup`], [`memory_rebuild_search`], and
//! [`memory_rebuild_index`]. Each validates that a memory store is wired and
//! no maintenance op is already running, spawns the actual work as a
//! background task, and returns immediately. The task streams
//! `memory://maintenance` events — `started`, then `progress` ticks (phase
//! label + done/total), then a terminal `done` (human summary) or `failed` —
//! which the Memory settings section renders as the progress bar and result
//! line. [`memory_index_status`] is the synchronous read behind the section's
//! derived-index card (counts by class + last-built timestamp).
//!
//! The heavy lifting lives in `mnemo::memory::maintenance` (unit-tested
//! without Tauri); this module only wires IPC state (store handle, live
//! session ids, default LLM provider, corpus digest) and event emission.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use mnemo::memory::consolidation::corpus_digest;
use mnemo::memory::maintenance::{self, MaintenancePhase};
use mnemo::memory::MemoryStore;
use mnemo::provider::client_factory::build_client;
use mnemo::provider::LlmClient;

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The Tauri event channel the maintenance task streams progress on.
const CHANNEL: &str = "memory://maintenance";

/// The Tauri event channel the STARTUP derived-index reconciliation streams
/// on. Distinct from [`CHANNEL`]: the reconcile is not a Settings → Memory
/// operation — it is the startup check that the semantic DB matches the
/// on-disk truth (plans, reviews, knowledge records, backlog), with a wait
/// dialog + progress bar. NO events are emitted when the corpus is already
/// in sync (the dialog stays silent); events appear only when a rebuild
/// actually runs (git merge landed, a record was edited, or `memory.db` was
/// deleted and must be rebuilt from the files).
pub const RECONCILE_CHANNEL: &str = "memory://reconcile";

/// An event on the [`RECONCILE_CHANNEL`] stream — the startup derived-index
/// reconciliation. Internally tagged (`"type"`, kebab-case) like
/// [`MaintenanceEvent`], but simpler (no op field — the channel is
/// reconcile-only).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ReconcileEvent {
    /// The reconcile started (the wait dialog appears).
    Started,
    /// A progress tick: `done`/`total` sources reconciled.
    Progress {
        /// Completed sources.
        done: usize,
        /// Total sources.
        total: usize,
    },
    /// Terminal success with a human-readable summary.
    Done {
        /// One-line summary for the result line.
        summary: String,
    },
    /// Terminal failure.
    Failed {
        /// Error text for the result line.
        error: String,
    },
}

/// Only one maintenance operation runs at a time: a module-level busy flag
/// rejects a second concurrent command (the UI disables both buttons while an
/// op runs, but the guard also covers a deep-linked second invocation).
static BUSY: AtomicBool = AtomicBool::new(false);

/// Drops the busy flag when the spawned task finishes — including on panic,
/// so a crashed task can never wedge maintenance off permanently.
/// `pub(crate)` so the startup derived-index bootstrap (main.rs) can claim
/// the same single-op guard (review LOW 3: the bootstrap must not interleave
/// with a manual Rebuild).
pub(crate) struct BusyGuard;

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// Claim the busy flag, or error when an operation is already running.
/// `pub(crate)` for the startup bootstrap (see [`BusyGuard`]).
pub(crate) fn claim_busy() -> Result<(), IpcError> {
    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        Ok(())
    } else {
        Err(IpcError::msg(
            "a memory maintenance operation is already running",
        ))
    }
}

/// Which operation an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenanceOp {
    /// Consolidate stale sessions + compact the store.
    Cleanup,
    /// Re-embed all memories + rebuild the FTS index + compact.
    Rebuild,
    /// Wipe + re-scan the derived index (plans/reviews/backlog digests).
    Index,
}

/// An event on the [`CHANNEL`] stream. Internally tagged (`"type"`,
/// kebab-case) so the frontend switches on a single field.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum MaintenanceEvent {
    /// The operation started (the bar appears at 0%).
    Started {
        /// Which operation.
        op: MaintenanceOp,
    },
    /// A progress tick. `done`/`total` are `0/0` for label-only phases with
    /// no measurable units ("reindexing", "vacuuming").
    Progress {
        /// Which operation.
        op: MaintenanceOp,
        /// Completed units (0 when the phase has no units).
        done: usize,
        /// Total units (0 when the phase has no units → indeterminate).
        total: usize,
        /// Short stage label ("consolidating" / "embedding" / "reindexing"
        /// / "vacuuming").
        phase: &'static str,
    },
    /// Terminal success with a human-readable summary.
    Done {
        /// Which operation.
        op: MaintenanceOp,
        /// One-line summary for the result line.
        summary: String,
    },
    /// Terminal failure.
    Failed {
        /// Which operation.
        op: MaintenanceOp,
        /// Error text for the result line.
        error: String,
    },
}

/// Map an engine phase to its wire label (the UI's progress-bar caption).
fn phase_label(phase: MaintenancePhase) -> &'static str {
    match phase {
        MaintenancePhase::Consolidating => "consolidating",
        MaintenancePhase::Embedding => "embedding",
        MaintenancePhase::Reindexing => "reindexing",
        MaintenancePhase::Vacuuming => "vacuuming",
    }
}

/// Snapshot the live agents' session ids — the cleanup skip list. Sessions
/// listed here are still being written; they consolidate at their own session
/// end, never by a manual cleanup. Called as late as possible (immediately
/// before the engine call) so a session started mid-cleanup can't be swept
/// into an early consolidation (review B3).
async fn live_sessions(loops: &crate::ipc::state::AgentLoopMap) -> Vec<String> {
    let map = loops.lock().await;
    map.values().filter_map(|l| l.session_id()).collect()
}

/// Map a panicked engine task (`JoinError`) to its terminal `Failed` event,
/// so a panic inside the maintenance engine can never leave the UI stuck on
/// a running bar with both buttons disabled (review B2). The busy guard
/// still frees itself via `Drop` on unwind — this only guarantees the
/// terminal event.
fn join_panic_event(op: MaintenanceOp, e: tokio::task::JoinError) -> MaintenanceEvent {
    MaintenanceEvent::Failed {
        op,
        error: format!("maintenance task panicked: {e}"),
    }
}

/// Minimum rows between forwarded embedding-progress events. One IPC event
/// per re-embedded row floods the webview with thousands of event/setState
/// round-trips on a large store; 1-in-50 keeps the bar smooth at a fraction
/// of the traffic (review perf note). `pub(crate)` so the codegraph reindex
/// command shares the same throttle policy.
pub(crate) const PROGRESS_EVERY: usize = 50;

/// Whether an engine progress tick should be forwarded to the UI: always the
/// first and last tick of a run, otherwise every `every`-th. Label-only
/// phases (`done == total == 0`) forward unconditionally. `pub(crate)` so the
/// codegraph reindex command shares the same throttle policy.
pub(crate) fn should_forward_progress(done: usize, total: usize, every: usize) -> bool {
    done == 1 || done == total || done % every == 0
}

/// Best-effort emit (a dead window drops the event; the op still runs).
fn emit(app: &AppHandle, event: &MaintenanceEvent) {
    let _ = app.emit(CHANNEL, event);
}

/// The shared store-handle + busy-claim preamble of both commands: resolves
/// the memory store, claims the busy flag, emits `started`, and returns the
/// guard the spawned task must own. The lock-ordering rule is respected:
/// `agent_loops`, then `config`, then `root` — each snapshot-and-drop, never
/// two held across an await.
fn start_op(
    app: &AppHandle,
    state: &IpcState,
    op: MaintenanceOp,
) -> Result<(Arc<MemoryStore>, BusyGuard), IpcError> {
    let store = state
        .runtime
        .memory_store
        .clone()
        .ok_or_else(|| IpcError::msg("memory store unavailable"))?;
    claim_busy()?;
    let guard = BusyGuard;
    emit(app, &MaintenanceEvent::Started { op });
    Ok((store, guard))
}

/// Consolidate every non-live session that still holds raw working-tier
/// events into episodic summaries (with semantic/procedural extraction when a
/// default LLM endpoint is configured), delete the raw rows, and VACUUM the
/// database. Fire-and-forget: progress + the terminal summary arrive via
/// `memory://maintenance` events.
#[tauri::command]
pub async fn memory_cleanup(app: AppHandle, state: State<'_, IpcState>) -> Result<(), IpcError> {
    // Live sessions: their working memory is still being written and is
    // consolidated at their own session end, not by a manual cleanup. The
    // skip list is snapshotted INSIDE the spawned task, immediately before
    // the engine call (review B3) — an agent spawned after this command
    // returns must not have its in-flight session swept into an early
    // consolidation.
    let loops = state.runtime.agent_loops.clone();
    // Optional LLM provider for semantic/procedural extraction — the default
    // provider from config, resolved by
    // `crate::startup::resolve_startup_provider` (the same tested helper the
    // startup build uses; minus the dummy fallback: no endpoint →
    // synthetic-only consolidation).
    let provider: Option<Arc<dyn LlmClient>> = {
        let config = state.project.config.lock().await;
        let (endpoint, model) = crate::startup::resolve_startup_provider(&config);
        endpoint.map(|ep| {
            build_client(
                &config,
                ep,
                &model,
                ep.multimodal_for(&model),
                ep.effective_reasoning_effort_for(Some(&model)),
                Some(state.trace.clone()),
            )
        })
    };
    // Corpus digest input: the project's plans dir (reviews dir derived the
    // same way as every other corpus_digest call site).
    let plans_dir = state.project.root.lock().await.plans_dir.clone();

    let (store, guard) = start_op(&app, &state, MaintenanceOp::Cleanup)?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        // The digest reads up to ~20 files synchronously — blocking pool, so
        // it never occupies a tokio worker. A join error (unreachable short
        // of a panic) degrades to an empty corpus — cleanup still runs.
        let corpus = match tokio::task::spawn_blocking(move || {
            let reviews_dir = plans_dir
                .parent()
                .map(|p| p.join("reviews"))
                .unwrap_or_else(|| std::path::PathBuf::from(".coding/reviews"));
            corpus_digest(&plans_dir, &reviews_dir)
        })
        .await
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("memory maintenance: corpus digest task failed: {e}");
                String::new()
            }
        };

        // The engine (skip-list snapshot + run) executes on its own task so a
        // panic anywhere inside it (e.g. a poisoned connection lock
        // downstream in MemoryStore) surfaces below as a JoinError instead of
        // vanishing — the UI must always receive a terminal event (review
        // B2). The skip list is taken here, as late as possible (review B3).
        let inner_app = app.clone();
        let engine = tokio::task::spawn(async move {
            let skip_sessions = live_sessions(&loops).await;
            let report = |phase: MaintenancePhase, done: usize, total: usize| {
                emit(
                    &inner_app,
                    &MaintenanceEvent::Progress {
                        op: MaintenanceOp::Cleanup,
                        done,
                        total,
                        phase: phase_label(phase),
                    },
                );
            };
            maintenance::cleanup(
                &store,
                &skip_sessions,
                provider.as_deref(),
                &corpus,
                &report,
            )
            .await
        });
        match engine.await {
            Ok(Ok(report)) => {
                let summary = format!(
                    "{} session(s) consolidated · {} working row(s) removed · database compacted",
                    report.sessions_consolidated, report.working_rows_removed
                );
                emit(
                    &app,
                    &MaintenanceEvent::Done {
                        op: MaintenanceOp::Cleanup,
                        summary,
                    },
                );
            }
            Ok(Err(e)) => {
                emit(
                    &app,
                    &MaintenanceEvent::Failed {
                        op: MaintenanceOp::Cleanup,
                        error: e.to_string(),
                    },
                );
            }
            Err(join) => {
                emit(&app, &join_panic_event(MaintenanceOp::Cleanup, join));
            }
        }
    });
    Ok(())
}

/// Re-embed every memory with the store's active embedder, rebuild the FTS5
/// full-text index, and VACUUM — the recovery path for stale/mixed embedding
/// fingerprints or a drifted index. Fire-and-forget: progress + the terminal
/// summary arrive via `memory://maintenance` events.
#[tauri::command]
pub async fn memory_rebuild_search(
    app: AppHandle,
    state: State<'_, IpcState>,
) -> Result<(), IpcError> {
    let (store, guard) = start_op(&app, &state, MaintenanceOp::Rebuild)?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        // The engine runs on its own task so a panic inside it surfaces as a
        // JoinError below instead of vanishing — the UI must always receive
        // a terminal event (review B2).
        let inner_app = app.clone();
        let engine = tokio::task::spawn(async move {
            let report = |phase: MaintenancePhase, done: usize, total: usize| {
                // Throttle per-row embedding ticks (review perf note); the
                // label-only tail phases always forward.
                if matches!(phase, MaintenancePhase::Embedding)
                    && !should_forward_progress(done, total, PROGRESS_EVERY)
                {
                    return;
                }
                emit(
                    &inner_app,
                    &MaintenanceEvent::Progress {
                        op: MaintenanceOp::Rebuild,
                        done,
                        total,
                        phase: phase_label(phase),
                    },
                );
            };
            maintenance::rebuild_search(&store, &report).await
        });
        match engine.await {
            Ok(Ok(report)) => {
                let summary = format!(
                    "{} memorie(s) re-embedded · index rebuilt · database compacted",
                    report.memories_reembedded
                );
                emit(
                    &app,
                    &MaintenanceEvent::Done {
                        op: MaintenanceOp::Rebuild,
                        summary,
                    },
                );
            }
            Ok(Err(e)) => {
                emit(
                    &app,
                    &MaintenanceEvent::Failed {
                        op: MaintenanceOp::Rebuild,
                        error: e.to_string(),
                    },
                );
            }
            Err(join) => {
                emit(&app, &join_panic_event(MaintenanceOp::Rebuild, join));
            }
        }
    });
    Ok(())
}

/// Wipe + re-scan the derived index (budgeted digests of `.coding/plans`,
/// `.coding/reviews`, and the pending backlog items) — authored memories are
/// never touched. Fire-and-forget: progress + the terminal summary arrive
/// via `memory://maintenance` events with op `"index"`.
#[tauri::command]
pub async fn memory_rebuild_index(
    app: AppHandle,
    state: State<'_, IpcState>,
) -> Result<(), IpcError> {
    // Snapshot the corpus paths first (snapshot-and-drop — never hold the
    // root lock across an await; the lock-ordering rule in start_op's docs).
    let (plans_dir, backlog_path) = {
        let project = state.project.root.lock().await;
        (
            project.plans_dir.clone(),
            project.coding_dir.join("backlog.jsonl"),
        )
    };
    let (store, guard) = start_op(&app, &state, MaintenanceOp::Index)?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        // The engine runs on its own task so a panic inside it surfaces as a
        // JoinError below instead of vanishing — the UI must always receive
        // a terminal event (review B2).
        let inner_app = app.clone();
        let engine = tokio::task::spawn(async move {
            let report = |done: usize, total: usize| {
                if !should_forward_progress(done, total, PROGRESS_EVERY) {
                    return;
                }
                emit(
                    &inner_app,
                    &MaintenanceEvent::Progress {
                        op: MaintenanceOp::Index,
                        done,
                        total,
                        phase: "indexing",
                    },
                );
            };
            mnemo::memory::indexer::rebuild_derived(
                store.as_ref(),
                &plans_dir,
                &backlog_path,
                &report,
            )
            .await
        });
        match engine.await {
            Ok(Ok(report)) => {
                // A rebuild wipes the state table first, so every live source
                // re-indexes (skipped/removed are always 0 on this path).
                let mut summary = format!(
                    "{} source(s) re-indexed from the .coding corpus",
                    report.indexed
                );
                if !report.errors.is_empty() {
                    for e in &report.errors {
                        eprintln!("derived index: {e}");
                    }
                    summary.push_str(&format!(" · {} source(s) degraded", report.errors.len()));
                }
                emit(
                    &app,
                    &MaintenanceEvent::Done {
                        op: MaintenanceOp::Index,
                        summary,
                    },
                );
            }
            Ok(Err(e)) => {
                emit(
                    &app,
                    &MaintenanceEvent::Failed {
                        op: MaintenanceOp::Index,
                        error: e.to_string(),
                    },
                );
            }
            Err(join) => {
                emit(&app, &join_panic_event(MaintenanceOp::Index, join));
            }
        }
    });
    Ok(())
}

/// The derived-index status behind the Settings → Memory card: record counts
/// by class + the last index-run timestamp.
#[derive(Debug, Clone, Serialize)]
pub struct MemoryIndexStatus {
    /// Authored (agent/user-written) memory count.
    pub authored_count: usize,
    /// Derived (indexer-built digest) memory count.
    pub derived_count: usize,
    /// Unix seconds of the most recent index run; `None` when never indexed.
    pub last_built_at: Option<i64>,
}

/// Read the derived-index status — the Settings → Memory card loads it on
/// open and re-reads it after a rebuild completes.
#[tauri::command]
pub async fn memory_index_status(
    state: State<'_, IpcState>,
) -> Result<MemoryIndexStatus, IpcError> {
    let store = state
        .runtime
        .memory_store
        .clone()
        .ok_or_else(|| IpcError::msg("memory store unavailable"))?;
    let (authored_count, derived_count) = store.count_by_class().await?;
    let last_built_at = store.index_state_last_built().await?;
    Ok(MemoryIndexStatus {
        authored_count,
        derived_count,
        last_built_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire shape the frontend parses: `"type"`-tagged,
    /// kebab-case variant + field names. If this changes, the MemorySection
    /// parser + ipc-contract test must change with it.
    #[test]
    fn maintenance_event_wire_shape() {
        let started = serde_json::to_value(MaintenanceEvent::Started {
            op: MaintenanceOp::Cleanup,
        })
        .unwrap();
        assert_eq!(
            started,
            serde_json::json!({"type": "started", "op": "cleanup"})
        );

        let progress = serde_json::to_value(MaintenanceEvent::Progress {
            op: MaintenanceOp::Rebuild,
            done: 2,
            total: 5,
            phase: "embedding",
        })
        .unwrap();
        assert_eq!(
            progress,
            serde_json::json!({
                "type": "progress", "op": "rebuild",
                "done": 2, "total": 5, "phase": "embedding"
            })
        );

        let done = serde_json::to_value(MaintenanceEvent::Done {
            op: MaintenanceOp::Cleanup,
            summary: "ok".into(),
        })
        .unwrap();
        assert_eq!(
            done,
            serde_json::json!({"type": "done", "op": "cleanup", "summary": "ok"})
        );

        let failed = serde_json::to_value(MaintenanceEvent::Failed {
            op: MaintenanceOp::Rebuild,
            error: "boom".into(),
        })
        .unwrap();
        assert_eq!(
            failed,
            serde_json::json!({"type": "failed", "op": "rebuild", "error": "boom"})
        );

        // The Phase-2 derived-index op serializes kebab-case like the rest.
        let index_started = serde_json::to_value(MaintenanceEvent::Started {
            op: MaintenanceOp::Index,
        })
        .unwrap();
        assert_eq!(
            index_started,
            serde_json::json!({"type": "started", "op": "index"})
        );
    }

    /// Every engine phase maps to a non-empty label (a missing arm would be
    /// a compile error, but an empty label would render an empty caption).
    #[test]
    fn every_phase_has_a_label() {
        for phase in [
            MaintenancePhase::Consolidating,
            MaintenancePhase::Embedding,
            MaintenancePhase::Reindexing,
            MaintenancePhase::Vacuuming,
        ] {
            assert!(!phase_label(phase).is_empty());
        }
    }

    /// Regression pin for review B2: a panicked engine task must map to a
    /// terminal `Failed` event, so the UI can never get stuck on a running
    /// bar with both buttons disabled after a panic.
    #[tokio::test]
    async fn panicked_engine_task_yields_failed_event() {
        let handle = tokio::task::spawn(async {
            panic!("boom");
        });
        let err = handle
            .await
            .err()
            .expect("a panicking task must join with Err");
        assert!(err.is_panic());
        match join_panic_event(MaintenanceOp::Cleanup, err) {
            MaintenanceEvent::Failed { op, error } => {
                assert_eq!(op, MaintenanceOp::Cleanup);
                assert!(
                    error.contains("panicked"),
                    "error text should name the panic, got: {error}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// The embedding-progress throttle forwards the first tick, the last
    /// tick, every 50th in between, and label-only `(0, 0)` phases — and
    /// skips the rest.
    #[test]
    fn progress_forwarding_is_throttled_but_pins_first_and_last() {
        assert!(should_forward_progress(1, 500, 50), "first tick forwards");
        assert!(!should_forward_progress(2, 500, 50));
        assert!(should_forward_progress(50, 500, 50));
        assert!(!should_forward_progress(51, 500, 50));
        assert!(should_forward_progress(500, 500, 50), "last tick forwards");
        // Label-only phases have no units — always forward.
        assert!(should_forward_progress(0, 0, 50));
        // Small runs: first + last cover the whole span.
        assert!(should_forward_progress(1, 3, 50));
        assert!(!should_forward_progress(2, 3, 50));
        assert!(should_forward_progress(3, 3, 50));
    }
}
