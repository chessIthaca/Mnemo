// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Memory tab's upkeep commands: [`memory_cleanup`], [`memory_rebuild_search`]
//! and [`memory_rebuild_index`].
//!
//! The tab's records are read-only (see [`crate::mnemo2::memory_view`]); these
//! three are the one deliberate exception, because the user asked for them.
//! Each is fire-and-forget: the command validates that an upkeep run can start
//! (project open, memory on, nothing else running) and returns; the run itself
//! streams `started` / `progress` / `done` / `failed` on
//! [`MAINTENANCE_CHANNEL`], the same wire shape Mnemo1's Settings → Memory uses
//! (`mnemo2/src/lib/memoryMaintenance.ts` mirrors those structs).
//!
//! Why the events and not the return value: a cleanup consolidates sessions and
//! may take minutes on a large store. The UI shows a live progress line and a
//! one-line result, so the answer has to arrive out of band.
//!
//! One run at a time: the busy flag lives in [`mnemo::maintenance`] and is
//! claimed here, released by the guard's `Drop` when the spawned task ends
//! (panic included). A second start while one runs is rejected with the flag's
//! own message — that is what the tab shows when a click cannot start.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use mnemo::headless::{Engine, MemoryApi};
use mnemo::memory::consolidation::corpus_digest;
use mnemo::memory::maintenance::MaintenancePhase;

use crate::mnemo2::ipc::Mnemo2State;

/// The Tauri event channel an upkeep run streams its progress on.
pub const MAINTENANCE_CHANNEL: &str = "memory://maintenance";

/// Which operation an event belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaintenanceOp {
    /// Consolidate stale sessions + compact the store.
    Cleanup,
    /// Re-embed every memory + rebuild the text index + compact.
    Rebuild,
    /// Wipe + re-scan the derived index (plans/reviews/backlog digests).
    Index,
}

/// An event on the [`MAINTENANCE_CHANNEL`] stream. Internally tagged (`"type"`,
/// kebab-case) so the frontend switches on a single field.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum MaintenanceEvent {
    /// The operation started (the progress line appears).
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
        /// / "vacuuming" / "indexing").
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

/// Map an engine phase to its wire label (the progress line's caption).
fn phase_label(phase: MaintenancePhase) -> &'static str {
    match phase {
        MaintenancePhase::Consolidating => "consolidating",
        MaintenancePhase::Embedding => "embedding",
        MaintenancePhase::Reindexing => "reindexing",
        MaintenancePhase::Vacuuming => "vacuuming",
    }
}

/// Map a panicked engine task (`JoinError`) to its terminal `Failed` event, so
/// a panic inside an upkeep run can never leave the UI stuck on a running line
/// with every button disabled. The busy guard still frees itself via `Drop` on
/// unwind — this only guarantees the terminal event.
fn join_panic_event(op: MaintenanceOp, e: tokio::task::JoinError) -> MaintenanceEvent {
    MaintenanceEvent::Failed {
        op,
        error: format!("maintenance task panicked: {e}"),
    }
}

/// Best-effort emit (a closed window drops the event; the run still happens).
fn emit(app: &AppHandle, event: &MaintenanceEvent) {
    let _ = app.emit(MAINTENANCE_CHANNEL, event);
}

/// The shared preamble of the three commands: the open project's engine, and
/// its memory API when the project runs with memory on.
///
/// Both failures are returns, not events: a start that cannot run answers the
/// caller with a reason and emits nothing.
async fn prepare(state: &Mnemo2State) -> Result<(Arc<Engine>, MemoryApi), String> {
    let engine = crate::mnemo2::memory_view::engine(state).await?;
    if engine.memory_store_handle().is_none() {
        return Err("this project runs with memory off, so there is nothing to maintain".to_string());
    }
    let api = engine.memory().map_err(|err| err.to_string())?;
    Ok((engine, api))
}

/// Claim the one-run-at-a-time flag and announce the start.
///
/// The caller keeps the guard for the lifetime of the spawned task (and moves
/// it in), so the flag is released when the run ends, panic included.
fn claim(app: &AppHandle, op: MaintenanceOp) -> Result<mnemo::maintenance::BusyGuard, String> {
    mnemo::maintenance::claim_busy().map_err(|busy| busy.to_string())?;
    let guard = mnemo::maintenance::BusyGuard;
    emit(app, &MaintenanceEvent::Started { op });
    Ok(guard)
}

/// Consolidate every non-live session that still holds raw working-tier events
/// into episodic summaries (with semantic/procedural extraction when a
/// summarise model is configured), delete the raw rows, and compact the
/// database.
///
/// Running sessions are left alone: their working memory is still being written
/// and consolidates at their own session end. The skip list is read inside the
/// spawned task, immediately before the engine call, so a session started after
/// this command returned cannot be swept into an early consolidation.
#[tauri::command]
pub async fn memory_cleanup(app: AppHandle, state: State<'_, Mnemo2State>) -> Result<(), String> {
    let (engine, api) = prepare(&state).await?;
    // The corpus digest: the plans dir, and the reviews dir beside it (both
    // under `.coding`). A digest failure degrades to an empty corpus — the
    // cleanup still runs, it just cannot credit unchanged sessions.
    let reviews_dir = engine.reviews_dir();
    let plans_dir = reviews_dir
        .parent()
        .map(|coding| coding.join("plans"))
        .unwrap_or_default();
    let digest_plans = plans_dir.clone();
    let corpus = match tokio::task::spawn_blocking(move || corpus_digest(&digest_plans, &reviews_dir))
        .await
    {
        Ok(corpus) => corpus,
        Err(error) => {
            eprintln!("memory maintenance: corpus digest task failed: {error}");
            String::new()
        }
    };
    let provider = engine.host().factory.summarize_client();
    let guard = claim(&app, MaintenanceOp::Cleanup)?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        // The engine call runs on its own task so a panic inside it (a
        // poisoned connection lock downstream, say) surfaces as a JoinError
        // instead of vanishing: the UI must always get a terminal event.
        let inner_app = app.clone();
        let run = tokio::task::spawn(async move {
            let skip: Vec<String> = engine
                .host()
                .agent_loops
                .lock()
                .await
                .values()
                .filter_map(|agent| agent.session_id())
                .collect();
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
            api.cleanup(&skip, Some(provider.as_ref()), &corpus, &report)
                .await
        });
        match run.await {
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
            Ok(Err(error)) => {
                emit(
                    &app,
                    &MaintenanceEvent::Failed {
                        op: MaintenanceOp::Cleanup,
                        error: error.to_string(),
                    },
                );
            }
            Err(join) => emit(&app, &join_panic_event(MaintenanceOp::Cleanup, join)),
        }
    });
    Ok(())
}

/// Re-embed every memory with the store's active embedder, rebuild the text
/// index and compact the database — the recovery path for stale or mixed
/// embedding fingerprints and a drifted index.
#[tauri::command]
pub async fn memory_rebuild_search(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
) -> Result<(), String> {
    let (_engine, api) = prepare(&state).await?;
    let guard = claim(&app, MaintenanceOp::Rebuild)?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let inner_app = app.clone();
        let run = tokio::task::spawn(async move {
            let report = |phase: MaintenancePhase, done: usize, total: usize| {
                // One event per re-embedded row floods the webview on a large
                // store, so only every `PROGRESS_EVERY`-th tick is forwarded
                // (the label-only tail phases always are).
                if matches!(phase, MaintenancePhase::Embedding)
                    && !mnemo::maintenance::should_forward_progress(
                        done,
                        total,
                        mnemo::maintenance::PROGRESS_EVERY,
                    )
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
            api.rebuild_search(&report).await
        });
        match run.await {
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
            Ok(Err(error)) => {
                emit(
                    &app,
                    &MaintenanceEvent::Failed {
                        op: MaintenanceOp::Rebuild,
                        error: error.to_string(),
                    },
                );
            }
            Err(join) => emit(&app, &join_panic_event(MaintenanceOp::Rebuild, join)),
        }
    });
    Ok(())
}

/// Wipe and re-scan the derived index — budgeted digests of `.coding/plans`,
/// `.coding/reviews`, the knowledge records and the pending backlog. Authored
/// records are never changed, only re-read.
#[tauri::command]
pub async fn memory_rebuild_index(
    app: AppHandle,
    state: State<'_, Mnemo2State>,
) -> Result<(), String> {
    let (engine, api) = prepare(&state).await?;
    let plans_dir = engine
        .reviews_dir()
        .parent()
        .map(|coding| coding.join("plans"))
        .unwrap_or_default();
    let backlog_path = engine.workspace_root().join(".coding").join("backlog.jsonl");
    let guard = claim(&app, MaintenanceOp::Index)?;

    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        let inner_app = app.clone();
        let run = tokio::task::spawn(async move {
            let report = |done: usize, total: usize| {
                if !mnemo::maintenance::should_forward_progress(
                    done,
                    total,
                    mnemo::maintenance::PROGRESS_EVERY,
                ) {
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
            api.rebuild_index(&plans_dir, &backlog_path, &report)
                .await
        });
        match run.await {
            Ok(Ok(report)) => {
                // A rebuild wipes the state table first, so every live source
                // re-indexes (skipped/removed are always 0 on this path).
                let mut summary = format!("{} source(s) re-indexed from .coding", report.indexed);
                if !report.errors.is_empty() {
                    for error in &report.errors {
                        eprintln!("derived index: {error}");
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
            Ok(Err(error)) => {
                emit(
                    &app,
                    &MaintenanceEvent::Failed {
                        op: MaintenanceOp::Index,
                        error: error.to_string(),
                    },
                );
            }
            Err(join) => emit(&app, &join_panic_event(MaintenanceOp::Index, join)),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the wire shape the frontend parses: `"type"`-tagged, kebab-case
    /// variant + field names. If this changes, `memoryMaintenance.ts` and its
    /// tests must change with it.
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

        // The derived-index op serializes kebab-case like the rest.
        let index_started = serde_json::to_value(MaintenanceEvent::Started {
            op: MaintenanceOp::Index,
        })
        .unwrap();
        assert_eq!(
            index_started,
            serde_json::json!({"type": "started", "op": "index"})
        );
    }

    /// Every engine phase maps to a non-empty label (a missing arm would be a
    /// compile error, but an empty label would render an empty caption).
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

    /// A panicked upkeep task must map to a terminal `Failed` event, so the
    /// tab can never get stuck on a running line with its buttons disabled.
    #[tokio::test]
    async fn panicked_engine_task_yields_failed_event() {
        let handle = tokio::task::spawn(async {
            panic!("boom");
        });
        let err = handle
            .await
            .expect_err("a panicking task must join with Err");
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
}
