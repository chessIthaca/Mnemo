// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Backlog + auto-feed Tauri commands.
//!
//! CRUD on the persistent backlog (`.coding/backlog.jsonl`), the auto-feed
//! toggle, single-item dispatch, and the Run-All start/stop commands. The
//! Run-All loop itself (dispatch-next, turn resolution, approval halt)
//! lives in `run_all.rs`; this module owns the command surface + the shared
//! `emit_backlog_changed` event helper.

use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{Emitter, State};

use mnemo::runtime::channels::AgentCommand;

use crate::ipc::error::IpcError;
use crate::ipc::events::emit_prompt_dispatched;
use crate::ipc::run_all::{extract_checkpoint_sha, run_all_dispatch_next};
use crate::ipc::state::{IpcState, RunAllState};
use mnemo::backlog::{
    BacklogItem, BacklogPosition, BacklogStatus, BacklogStore, normalize_new_item_text,
};

// ── Backlog ─────────────────────────────────────────────────────────────────
//
// The backlog is a persistent list of prompts waiting to be dispatched to the
// main agent. Two execution modes share one dispatch path:
// - **Auto-feed**: when the main agent goes idle and auto-feed is on, the top
//   pending item is dispatched (no git checkpoint — the user is watching).
// - **Run-All**: the unattended loop. Each item gets a git checkpoint, an
//   unattended-mode preamble embedded in its dispatched prompt (see
//   `run_all::run_all_prompt`), and a commit on success — a turn that ends
//   without the plan closing keeps the work in the tree (no rollback; see
//   run_all's plan-tied status contract).
//
// RULE: prompts only ever go to the MAIN agent (`manager.main_agent_id()`);
// subagents accept steers, never prompts.

/// The Tauri event channel for backlog changes (emitted on every mutation).
pub const BACKLOG_CHANGED_CHANNEL: &str = "backlog://changed";

/// The serializable payload for `backlog://changed`.
#[derive(Debug, Clone, Serialize)]
pub struct BacklogChangedPayload {
    /// All backlog items, in display order.
    pub items: Vec<BacklogItemView>,
    /// Whether auto-feed is currently enabled.
    pub auto_feed: bool,
    /// Whether PARALLEL run-all is currently enabled (plan ffd7a86f) —
    /// the checkbox next to auto-feed. Gates run-all concurrency only;
    /// auto-feed stays sequential.
    pub parallel: bool,
    /// Run-All progress.
    pub run_all: RunAllProgress,
}

/// A backlog item as sent to the frontend: the stored item plus the run-all
/// checkpoint sha parsed out of its note head.
///
/// The wire shape is the stored item's fields (flattened) plus
/// `checkpoint_sha` — the git checkpoint a Run-All turn took before
/// dispatching this item, i.e. the manual resume/rollback anchor
/// (`git reset --hard <sha>`), machine-readable instead of buried in the
/// note text. Always serialized (null when the note carries no sha) so the
/// payload shape is stable.
#[derive(Debug, Clone, Serialize)]
pub struct BacklogItemView {
    #[serde(flatten)]
    pub item: BacklogItem,
    /// Parsed from the note head by [`BacklogItemView::new`] — `None` for
    /// items that were never checkpointed (manual adds, auto-feed).
    pub checkpoint_sha: Option<String>,
}

impl BacklogItemView {
    /// Wrap a stored item for the frontend, parsing the run-all checkpoint
    /// sha out of its note head (if any).
    pub(crate) fn new(item: BacklogItem) -> Self {
        let checkpoint_sha = item.note.as_deref().and_then(extract_checkpoint_sha);
        Self { checkpoint_sha, item }
    }
}

/// Run-All progress for the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct RunAllProgress {
    /// Whether a Run-All loop is active.
    pub active: bool,
    /// Items resolved so far this run.
    pub done: u64,
    /// Items that were pending when the run started.
    pub total: u64,
    /// `true` while the Run-All auto-compact (`auto_compact_on_plan_complete`)
    /// is compacting the main agent's context between items — surfaces the
    /// brief "compacting…" state in the progress line. Always `false` when
    /// no run is active.
    pub compacting: bool,
    /// The run's dispatch concurrency (plan ffd7a86f): 1 = sequential
    /// (today's behavior), N > 1 = items beyond the first dispatch
    /// concurrently to spawned worktree agents. The UI renders the lane
    /// count alongside the in-flight set.
    pub concurrency: usize,
    /// The concurrently dispatched items (plan ffd7a86f) — one view per
    /// spawned worktree agent. Empty on the sequential path (concurrency 1).
    pub spawned: Vec<SpawnedRunView>,
    /// The last run's completion note — "wrote N knowledge record(s) during
    /// this run — uncommitted in the main tree" when the run's agents wrote
    /// knowledge files; persists until the next run starts (memory review
    /// 2026-09-08, suggestion 3).
    pub note: Option<String>,
}

/// One concurrently dispatched run-all item, for the frontend (plan
/// ffd7a86f).
#[derive(Debug, Clone, Serialize)]
pub struct SpawnedRunView {
    /// The spawned agent's id.
    pub agent_id: u64,
    /// The backlog item id.
    pub item_id: String,
    /// The item's branch (`wt/runall-<item8>`).
    pub branch: String,
}

/// Build the frontend-facing view of a stored item: image PATHS resolved back
/// into base64 data URLs (the store keeps paths in the git-tracked JSONL for
/// small diffs; the IPC boundary resolves them so the frontend + dispatch see
/// data URLs unchanged), plus the checkpoint sha parsed from the note head.
pub(crate) fn resolve_item_view(store: &BacklogStore, item: &BacklogItem) -> BacklogItemView {
    let mut resolved = item.clone();
    resolved.images = store.resolve_images(item);
    BacklogItemView::new(resolved)
}

/// Emit the current backlog state to the frontend.
pub async fn emit_backlog_changed(app: &tauri::AppHandle, state: &IpcState) {
    let items = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .map(|i| resolve_item_view(&store, i))
            .collect::<Vec<_>>()
    };
    let auto_feed = state.backlog.auto_feed.load(Ordering::Relaxed);
    let parallel = state.backlog.parallel_run_all.load(Ordering::Relaxed);
    let run_note = state
        .backlog
        .run_completion_note
        .lock()
        .unwrap()
        .clone();
    let run_all = {
        let guard = state.backlog.run_all.lock().await;
        match guard.as_ref() {
            Some(r) => RunAllProgress {
                active: true,
                done: r.done.load(Ordering::Relaxed),
                total: r.total.load(Ordering::Relaxed),
                compacting: state.backlog.compacting.load(Ordering::Relaxed),
                concurrency: r.concurrency,
                spawned: spawned_views(r),
                note: run_note.clone(),
            },
            None => RunAllProgress {
                active: false,
                done: 0,
                total: 0,
                compacting: false,
                concurrency: 1,
                spawned: Vec::new(),
                note: run_note,
            },
        }
    };
    let _ = app.emit(
        BACKLOG_CHANGED_CHANNEL,
        BacklogChangedPayload {
            items,
            auto_feed,
            parallel,
            run_all,
        },
    );
}

/// Add a prompt (+ optional image attachments) to the backlog. Returns the
/// stored item (with its allocated id), as the frontend-facing view.
///
/// Enforces the same headline+body shape contract as the `backlog_add`
/// agent tool (backlog 45a4eb88): first line a short headline (≤ 100
/// chars), then the body — the Backlog tab renders that split (backlog
/// 40763a24). The validator's error names the required shape, so the UI can
/// surface it directly. Image-only items (empty text + attachments) are
/// exempt — the UI's add path explicitly supports pasting a screenshot with
/// no caption, and there is no headline to validate. `backlog_edit`
/// deliberately does NOT validate: editing an existing (possibly
/// grandfathered, single-line) item must not force restructuring.
///
/// Optional `position` ("top" | "end", backlog 06a31736) — symmetry with
/// the agent tool: "top" inserts at the FRONT of the queue (dispatched
/// first; urgent items only), absent (or "end") appends. The UI composer
/// does not pass it — it has the send-to-top button — so the default is
/// the unchanged append every existing caller relies on.
#[tauri::command]
pub async fn backlog_add(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    text: String,
    images: Vec<String>,
    position: Option<BacklogPosition>,
) -> Result<BacklogItemView, IpcError> {
    // Shared shape contract — the image-aware validator (the agent tool
    // takes no images and uses the text-only normalize_item_text directly);
    // also trims and caps length (this command previously validated
    // nothing).
    let text = normalize_new_item_text(&text, &images).map_err(IpcError::from)?;
    // Absent (or explicit null) position = append — the pre-position
    // behavior the frontend caller (which passes no position) relies on.
    let position = position.unwrap_or(BacklogPosition::End);
    let item = {
        let mut store = state.backlog.store.lock().await;
        let item = store.add_at(position, text, images);
        resolve_item_view(&store, &item)
    };
    emit_backlog_changed(&app, &state).await;
    Ok(item)
}

/// List all backlog items, in display order — the frontend-facing views,
/// with each item's run-all checkpoint sha (if any) parsed from its note.
#[tauri::command]
pub async fn backlog_list(state: State<'_, IpcState>) -> Result<Vec<BacklogItemView>, IpcError> {
    let items = {
        let store = state.backlog.store.lock().await;
        store
            .items()
            .iter()
            .map(|i| resolve_item_view(&store, i))
            .collect::<Vec<_>>()
    };
    Ok(items)
}

/// Remove a backlog item by id.
#[tauri::command]
pub async fn backlog_remove(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    id: String,
) -> Result<(), IpcError> {
    state.backlog.store.lock().await.remove(&id);
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

/// Reorder the backlog to the given id order (unlisted ids keep their
/// relative order and go last).
#[tauri::command]
pub async fn backlog_reorder(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    ids: Vec<String>,
) -> Result<(), IpcError> {
    state.backlog.store.lock().await.reorder(&ids);
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

/// Drop all finished items (done/failed/cant-resolve), keeping pending and
/// in-flight ones.
#[tauri::command]
pub async fn backlog_clear_finished(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
) -> Result<(), IpcError> {
    state.backlog.store.lock().await.clear_finished();
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

/// Re-queue a finished item back to pending — the "retry" action for
/// failed/cant-resolve items AND the user's reset for items the agent marked
/// `Done` (2026-08-20). Routes through [`BacklogStore::requeue`], which only
/// accepts terminal statuses (done/failed/cant-resolve) and clears the note:
/// a stale click on a pending/in-flight card is a safe no-op — critically, an
/// item halted mid-run keeps its checkpoint-sha note (the manual
/// resume/rollback anchor) instead of being flipped to pending under a
/// running turn.
#[tauri::command]
pub async fn backlog_retry(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    id: String,
) -> Result<(), IpcError> {
    // The returned bool (was it re-queued?) is deliberately ignored: a stale
    // click on a non-terminal item must be a silent no-op, not an error.
    let _ = state.backlog.store.lock().await.requeue(&id);
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

/// Edit the text + images of an existing backlog item (the "edit" action).
/// The item's status, note, and creation time are preserved. No-op (returns
/// `Ok(false)`) when the id doesn't exist.
#[tauri::command]
pub async fn backlog_edit(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    id: String,
    text: String,
    images: Vec<String>,
) -> Result<bool, IpcError> {
    let updated = state.backlog.store.lock().await.edit(&id, text, images);
    emit_backlog_changed(&app, &state).await;
    Ok(updated)
}

/// Set or clear the deferred (skip Run-All) flag on a backlog item — the
/// "keep but do not auto-run" state (user request 2027-01-07). The item
/// stays in the backlog with its current status (typically `Pending`) and
/// remains manually dispatchable (the per-item ▶ button); only Run-All
/// selection skips deferred items. No-op (returns `Ok(false)`) when the id
/// doesn't exist.
#[tauri::command]
pub async fn backlog_set_deferred(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    id: String,
    deferred: bool,
) -> Result<bool, IpcError> {
    let updated = state.backlog.store.lock().await.set_deferred(&id, deferred);
    emit_backlog_changed(&app, &state).await;
    Ok(updated)
}

/// Set the auto-feed flag (session-only; never persisted).
#[tauri::command]
pub async fn backlog_set_auto_feed(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    enabled: bool,
) -> Result<(), IpcError> {
    state.backlog.auto_feed.store(enabled, Ordering::Relaxed);
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

/// Map a run's spawned entries to the frontend views (plan ffd7a86f).
fn spawned_views(run: &RunAllState) -> Vec<SpawnedRunView> {
    run.spawned
        .lock()
        .unwrap()
        .iter()
        .map(|s| SpawnedRunView {
            agent_id: s.agent_id,
            item_id: s.item_id.clone(),
            branch: s.branch.clone(),
        })
        .collect()
}

/// Resolve a run's dispatch concurrency (plan ffd7a86f): an explicit
/// param wins (tests, advanced use); otherwise the parallel checkbox
/// decides (3 lanes when on, sequential when off). Clamped to 1..=8 —
/// each extra lane is a git worktree + a parentless agent + eventually
/// a reviewer subagent.
fn resolve_run_all_concurrency(param: Option<usize>, parallel: bool) -> usize {
    param.unwrap_or(if parallel { 3 } else { 1 }).clamp(1, 8)
}

/// Set the parallel run-all flag (session-only; never persisted; plan
/// ffd7a86f). Gates run-all CONCURRENCY only — auto-feed stays sequential
/// (the single-item conveyor).
#[tauri::command]
pub async fn backlog_set_parallel_run_all(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    enabled: bool,
) -> Result<(), IpcError> {
    state
        .backlog
        .parallel_run_all
        .store(enabled, Ordering::Relaxed);
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

/// Dispatch the top pending backlog item to the main agent, if it's idle.
///
/// This is the shared dispatch path used by the ▶ button, auto-feed, and the
/// Run-All loop. It's a no-op (returns `Ok(false)`) when the main agent is
/// busy, has running descendants, or there's nothing pending. With
/// `with_checkpoint: false` it dispatches directly (auto-feed / manual); the
/// Run-All loop calls `run_all_dispatch_next` instead (which checkpoints).
///
/// Returns `Ok(true)` when an item was dispatched.
#[tauri::command]
pub async fn backlog_dispatch_next(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
) -> Result<bool, IpcError> {
    dispatch_next_impl(&app, state.inner())
        .await
        .map_err(IpcError::from)
}

/// Dispatch a specific pending backlog item (by id) to the main agent, if it's
/// idle. Backs the ▶ button on an individual backlog card — unlike
/// `backlog_dispatch_next` (which always takes the top pending item), this
/// targets the exact id the user clicked. No-op (`Ok(false)`) when the item
/// isn't `Pending`, the main agent is busy, or has running descendants.
///
/// Returns `Ok(true)` when an item was dispatched.
#[tauri::command]
pub async fn backlog_dispatch_item(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    id: String,
) -> Result<bool, IpcError> {
    dispatch_item_impl(&app, state.inner(), &id)
        .await
        .map_err(IpcError::from)
}

/// Shared dispatch implementation (also called by the event forwarder for
/// auto-feed, which has no Tauri `State`). Returns `true` when dispatched.
pub(crate) async fn dispatch_next_impl(
    app: &tauri::AppHandle,
    state: &IpcState,
) -> Result<bool, String> {
    // Read the next pending item + resolve the main agent without holding
    // either lock across the send.
    let item = state.backlog.store.lock().await.next_pending();
    let Some(item) = item else { return Ok(false) };
    dispatch_item(app, state, &item).await
}

/// Dispatch a specific pending backlog item (by id) to the main agent, if it's
/// idle. This is the per-item dispatch path used by the ▶ button on a specific
/// backlog card — unlike `dispatch_next_impl` it targets an exact id and is a
/// no-op (`Ok(false)`) when that item isn't `Pending` (so a stale click on an
/// already-dispatched card does nothing). Returns `true` when dispatched.
pub(crate) async fn dispatch_item_impl(
    app: &tauri::AppHandle,
    state: &IpcState,
    id: &str,
) -> Result<bool, String> {
    let item = state.backlog.store.lock().await.pending_item(id);
    let Some(item) = item else { return Ok(false) };
    dispatch_item(app, state, &item).await
}

/// Send a backlog item's prompt to the main agent (when idle), mark it
/// in-flight, and notify the frontend. Shared by `dispatch_next_impl`
/// (top-pending) and `dispatch_item_impl` (specific id). Returns `true` when
/// dispatched; `false` when the main agent is busy.
async fn dispatch_item(
    app: &tauri::AppHandle,
    state: &IpcState,
    item: &BacklogItem,
) -> Result<bool, String> {
    // Resolve image paths → data URLs for the agent dispatch + the
    // prompt-dispatched event (the store keeps paths in the JSONL; the agent
    // loop + frontend need data URLs).
    let images = state.backlog.store.lock().await.resolve_images(item);
    let manager = state.runtime.manager.lock().await;
    let Some(main_id) = manager.main_agent_id() else {
        return Ok(false);
    };
    // Only dispatch when the main agent is fully idle: not running and no
    // subagents still out (a turn isn't resolved until its descendants are).
    let busy = manager
        .get(main_id)
        .map(|h| h.is_running())
        .unwrap_or(false)
        || manager.has_running_descendants(main_id);
    if busy {
        return Ok(false);
    }
    // Record which item is in flight on the single-dispatch path BEFORE
    // sending so a hyper-fast turn can never resolve an unset id. The item
    // itself stays `Pending` — it is stamped `InFlight` only when the
    // workflow enters `Executing` (forwarder → `stamp_backlog_in_flight`):
    // a pre-planning steer/interrupt must leave the item queued, not
    // resolved or failed.
    *state
        .backlog
        .single_in_flight
        .lock()
        .expect("single_in_flight lock poisoned") = Some(item.id.clone());
    if let Err(e) = manager.send(
        main_id,
        AgentCommand::Prompt {
            text: item.text.clone(),
            images: images.clone(),
        },
    ) {
        // Don't leak the in-flight pointer: a later unrelated turn's
        // resolution would take the stale id and resolve a never-dispatched
        // item through the plan gate.
        *state
            .backlog
            .single_in_flight
            .lock()
            .expect("single_in_flight lock poisoned") = None;
        return Err(format!("failed to dispatch backlog item to main agent: {e:?}").into());
    }
    drop(manager);

    // Tell the frontend to show the dispatched prompt as the goal at the top
    // of the transcript — mirroring how the main input optimistically appends
    // the user message before the agent starts streaming. Emitted right after
    // the Prompt command so it lands before the agent's `Started` event.
    emit_prompt_dispatched(app, main_id, &item.text, &images);

    emit_backlog_changed(app, state).await;
    Ok(true)
}

/// Start the Run-All loop: work through every pending backlog item, one
/// plan/execute turn each, until the list is drained or the loop is stopped.
///
/// Never overrides the user's safety mode — startable in any mode. If an
/// approval request arrives mid-run, the loop halts and waits for the user
/// (the approval is the guard, not a precondition).
#[tauri::command]
pub async fn backlog_run_all(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    concurrency: Option<usize>,
) -> Result<(), IpcError> {
    // Already running?
    if state.backlog.run_all.lock().await.is_some() {
        return Err("run-all is already active".into());
    }
    // Adopt orphaned InFlight items BEFORE counting (run-all orphans,
    // 2027-01-07): an InFlight item whose owner is gone (an app crash
    // killed the in-memory run state; a legacy drain left it in flight) is
    // skipped by every future run-all — requeue orphans to Pending (a
    // landed one auto-resolves Done first — the done-orphan guard,
    // 6c6966b9) so the dispatch order picks them up and they count toward
    // the run's total.
    crate::ipc::run_all::adopt_orphaned_in_flight(state.inner()).await;
    // Backlog 64662ef2 (review L4 of the b52b041a landing): sweep
    // wt/runall-* branches whose PRs the human has merged — their
    // commits are in origin/main, so the local refs are redundant.
    // Best-effort + conservative (unmerged/worktree-held branches are
    // never touched); runs before the pending-count check so a run
    // that finds no eligible items still sweeps.
    let swept = mnemo::project::worktrees::sweep_merged_runall_branches(
        state.inner().project.root.lock().await.root.clone(),
    )
    .await;
    if !swept.is_empty() {
        eprintln!(
            "backlog: swept {} merged runall branch(es): {}",
            swept.len(),
            swept.join(", ")
        );
    }
    // The run's total counts only ELIGIBLE items — pending and not
    // deferred (user request 2027-01-07: deferred items are excluded from
    // Run-All). A backlog whose pending items are ALL deferred still starts
    // the run (total 0): the first dispatch finds nothing eligible and ends
    // the run cleanly via `end_run` — no spin, no error. The error below
    // stays reserved for a backlog with no pending items at all.
    let items = state.backlog.store.lock().await.items().clone();
    let total = items
        .iter()
        .filter(|i| i.status == BacklogStatus::Pending && !i.deferred)
        .count() as u64;
    if total == 0 && !items.iter().any(|i| i.status == BacklogStatus::Pending) {
        return Err("no pending backlog items to run".into());
    }
    // Run-All owns the dispatch loop — turn auto-feed off so they never fight.
    state.backlog.auto_feed.store(false, Ordering::Relaxed);
    // Parallel run-all (plan ffd7a86f) — see resolve_run_all_concurrency.
    let parallel = state.backlog.parallel_run_all.load(Ordering::Relaxed);
    let concurrency = resolve_run_all_concurrency(concurrency, parallel);
    // A new run resets the completion note (the previous run's note is stale).
    *state.backlog.run_completion_note.lock().unwrap() = None;
    *state.backlog.run_all.lock().await = Some(RunAllState {
        stop: std::sync::atomic::AtomicBool::new(false),
        current_item: std::sync::Mutex::new(None),
        done: std::sync::atomic::AtomicU64::new(0),
        total: std::sync::atomic::AtomicU64::new(total),
        concurrency,
        spawned: std::sync::Mutex::new(Vec::new()),
        knowledge_writes_at_start: std::sync::atomic::AtomicU64::new(
            mnemo::tool::memory::KNOWLEDGE_WRITES.load(std::sync::atomic::Ordering::Relaxed),
        ),
    });
    // The run-all heartbeat (backlog 0ed9d24f): heal stalled runs for the
    // run's whole lifetime — see `run_all_heartbeat`. Bound to THIS run's
    // generation so a previous run's heartbeat cannot survive into it
    // (review 2026-09-09 LOW-4).
    let generation = state
        .backlog
        .run_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        + 1;
    tokio::spawn(crate::ipc::run_all::run_all_heartbeat(
        app.clone(),
        generation,
    ));
    emit_backlog_changed(&app, &state).await;
    // Kick off the first item. Subsequent items are dispatched from the event
    // forwarder as each turn resolves (see `on_main_turn_resolved`).
    run_all_dispatch_next(&app, state.inner())
        .await
        .map_err(IpcError::from)
}

/// Stop the Run-All loop after the current in-flight item resolves. (A hard
/// stop is the existing `interrupt` command, which cancels the agent's turn.)
#[tauri::command]
pub async fn backlog_stop_all(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
) -> Result<(), IpcError> {
    if let Some(r) = state.backlog.run_all.lock().await.as_ref() {
        r.stop.store(true, Ordering::Relaxed);
    }
    emit_backlog_changed(&app, &state).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{resolve_run_all_concurrency, spawned_views, BacklogItemView};
    use crate::ipc::state::{RunAllState, SpawnedRun};
    use mnemo::backlog::{BacklogItem, BacklogStatus};

    /// Parallel run-all (plan ffd7a86f): an explicit concurrency param
    /// wins; the parallel checkbox decides when absent (3 lanes); always
    /// clamped to 1..=8.
    #[test]
    fn resolve_run_all_concurrency_clamps_and_defaults() {
        assert_eq!(
            resolve_run_all_concurrency(None, false),
            1,
            "sequential by default (today's behavior)"
        );
        assert_eq!(
            resolve_run_all_concurrency(None, true),
            3,
            "the parallel checkbox gives 3 lanes"
        );
        assert_eq!(
            resolve_run_all_concurrency(Some(2), false),
            2,
            "an explicit param wins over the checkbox"
        );
        assert_eq!(resolve_run_all_concurrency(Some(99), true), 8, "clamped to 8");
        assert_eq!(resolve_run_all_concurrency(Some(0), true), 1, "clamped to 1");
    }

    /// Parallel run-all (plan ffd7a86f): the payload carries the
    /// concurrent in-flight set (agent, item, branch) — the UI renders
    /// "N in flight" from it.
    #[test]
    fn spawned_views_map_agent_item_and_branch() {
        let run = RunAllState {
            stop: std::sync::atomic::AtomicBool::new(false),
            current_item: std::sync::Mutex::new(Some("item-1".into())),
            done: std::sync::atomic::AtomicU64::new(0),
            total: std::sync::atomic::AtomicU64::new(2),
            concurrency: 2,
            spawned: std::sync::Mutex::new(vec![SpawnedRun {
                agent_id: 7,
                item_id: "item-2".into(),
                worktree: std::path::PathBuf::from("/tmp/wt"),
                branch: "wt/runall-item2xx".into(),
            }]),
            knowledge_writes_at_start: std::sync::atomic::AtomicU64::new(0),
        };
        let views = spawned_views(&run);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].agent_id, 7);
        assert_eq!(views[0].item_id, "item-2");
        assert_eq!(views[0].branch, "wt/runall-item2xx");
    }

    /// Regression (review finding 1, 2026-08-20): `backlog_retry` must route
    /// through the guarded `BacklogStore::requeue` (terminal statuses only) —
    /// NOT the raw `set_status(id, Pending, None)`, which let a stale UI
    /// click flip an IN-FLIGHT item back to pending and destroy its
    /// checkpoint-sha note (the halt-for-approval resume anchor), or
    /// double-queue an item whose turn was still running. The command needs a
    /// Tauri `AppHandle`, so the wiring is pinned as a source contract; the
    /// guard behavior itself is unit-tested on the store
    /// (`backlog::tests::requeue_refuses_pending_in_flight_and_unknown`).
    #[test]
    fn backlog_retry_routes_through_guarded_requeue() {
        let src = include_str!("backlog_cmds.rs");
        // Slice out the backlog_retry command body so the assertions inspect
        // the real call site — asserting against the whole file would be
        // self-referential (this test's own literals live in `src` too).
        let start = src
            .find("pub async fn backlog_retry")
            .expect("backlog_retry command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains(".requeue(&id)"),
            "backlog_retry must call BacklogStore::requeue"
        );
        assert!(
            !body.contains("set_status"),
            "backlog_retry must not use the raw set_status setter"
        );
    }

    /// Backlog 45a4eb88: `backlog_add` must route text through the shared
    /// shape validator (headline+body, ≤ 4000 chars, trimmed) — the same
    /// contract the agent-facing `backlog_add` tool enforces, so the UI
    /// path can't write shape-less items the Backlog tab renders badly
    /// (bare headline / oversized headline). The command uses the
    /// image-AWARE `normalize_new_item_text` (image-only items — a pasted
    /// screenshot with no caption — are exempt); the assertion pins the
    /// `&images` argument so a refactor can't silently drop the exemption
    /// back to the text-only validator, which rejected them. The command
    /// needs Tauri state, so the wiring is pinned as a source contract;
    /// the validators themselves are unit-tested in
    /// `mnemo::backlog::tests`.
    #[test]
    fn backlog_add_routes_through_the_shape_validator() {
        let src = include_str!("backlog_cmds.rs");
        // Slice out the backlog_add command body so the assertions inspect
        // the real call site — asserting against the whole file would be
        // self-referential (this test's own literals live in `src` too).
        let start = src
            .find("pub async fn backlog_add")
            .expect("backlog_add command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("normalize_new_item_text(&text, &images)"),
            "backlog_add must validate through the image-aware \
             normalize_new_item_text, passing the images"
        );
        assert!(
            body.contains("map_err(IpcError::from)"),
            "the validator's error must reach the frontend as an IpcError"
        );
    }

    /// Backlog 06a31736: `backlog_add` takes an optional `position`
    /// ("top" | "end") — symmetry with the agent tool. "top" must route
    /// through `BacklogStore::add_at` (the store-level front insert), and
    /// an absent position must default to End (append) — the frontend
    /// caller passes no position, so its behavior is unchanged. The
    /// command needs Tauri state, so the wiring is pinned as a source
    /// contract; the position semantics themselves are unit-tested on
    /// the store (`backlog::tests::
    /// add_at_top_lands_first_in_store_and_file_order`).
    #[test]
    fn backlog_add_position_routes_through_add_at() {
        let src = include_str!("backlog_cmds.rs");
        // Slice out the backlog_add command body so the assertions inspect
        // the real call site — asserting against the whole file would be
        // self-referential (this test's own literals live in `src` too).
        let start = src
            .find("pub async fn backlog_add")
            .expect("backlog_add command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("position: Option<BacklogPosition>"),
            "backlog_add must take an optional position"
        );
        assert!(
            body.contains(".add_at(position, text, images)"),
            "backlog_add must route through BacklogStore::add_at"
        );
        assert!(
            body.contains("unwrap_or(BacklogPosition::End)"),
            "an absent position must default to End (append)"
        );
    }

    /// Regression (review finding LOW-1, 2026-12-05): a failed `manager.send`
    /// must clear the `single_in_flight` pointer before returning — a stale
    /// id would let a later unrelated turn's resolution resolve a
    /// never-dispatched item through the plan gate. Source-contract:
    /// dispatch_item needs the Tauri state.
    #[test]
    fn dispatch_item_clears_in_flight_pointer_on_send_failure() {
        let src = include_str!("backlog_cmds.rs");
        let start = src
            .find("async fn dispatch_item(")
            .expect("dispatch_item present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        let set = body
            .find("= Some(item.id.clone());")
            .expect("the pointer is set before send");
        let err_block = body
            .find("if let Err(e) = manager")
            .expect("send errors are handled inline");
        let clear = body[err_block..]
            .find("= None;")
            .expect("the pointer is cleared on send failure");
        let ret = body[err_block..]
            .find("return Err(")
            .expect("send failure returns Err");
        assert!(set < err_block, "the pointer is set before the send");
        assert!(
            clear < ret,
            "clear the stale pointer BEFORE returning the error"
        );
    }

    /// `BacklogItemView::new` parses the run-all checkpoint sha out of an
    /// item's note head — the machine-readable resume/rollback anchor
    /// surfaced by `backlog_list` / `backlog_add` / `backlog://changed`
    /// (backlog 212c14af: the restored production caller that removed the
    /// tree's first `#[allow(dead_code)]`). The note itself is preserved
    /// verbatim — it stays the human-readable record.
    #[test]
    fn backlog_item_view_parses_checkpoint_sha() {
        let item = |note: Option<&str>| BacklogItem {
            id: "test-item".into(),
            text: "task".into(),
            images: vec![],
            status: BacklogStatus::InFlight,
            created_at: 1_700_000_000,
            note: note.map(str::to_string),
            deferred: false,
            plan_id: None,
            plan_title: None,
            deleted_at: None,
        };
        // A checkpointed item: the note head is the sha, " | reason"
        // appended on halt — the sha is recovered, the note preserved.
        let view = BacklogItemView::new(item(Some(
            "a1b2c3d4e5f6789012345678901234567890abcd | approval requested — halted",
        )));
        assert_eq!(
            view.checkpoint_sha.as_deref(),
            Some("a1b2c3d4e5f6789012345678901234567890abcd")
        );
        assert_eq!(
            view.item.note.as_deref(),
            Some("a1b2c3d4e5f6789012345678901234567890abcd | approval requested — halted")
        );
        // A note without a sha head (manual dispatch) parses to None.
        assert_eq!(
            BacklogItemView::new(item(Some("approval requested"))).checkpoint_sha,
            None
        );
        // No note at all → None.
        assert_eq!(BacklogItemView::new(item(None)).checkpoint_sha, None);
    }

    /// The three IPC read paths (`backlog_list`, `backlog_add`,
    /// `emit_backlog_changed`) must route through `resolve_item_view` so the
    /// parsed `checkpoint_sha` actually reaches the frontend payload — the
    /// production caller that keeps `extract_checkpoint_sha` alive (backlog
    /// 212c14af). The commands need Tauri state, so the wiring is pinned as a
    /// source contract; the parsing itself is unit-tested above.
    #[test]
    fn backlog_read_paths_surface_checkpoint_sha() {
        let src = include_str!("backlog_cmds.rs");
        for command in [
            "async fn backlog_list(",
            "async fn backlog_add(",
            "async fn emit_backlog_changed(",
        ] {
            let start = src
                .find(command)
                .unwrap_or_else(|| panic!("command present: {command}"));
            let end = src[start..]
                .find("\n}\n")
                .map(|i| start + i)
                .unwrap_or(src.len());
            let body = &src[start..end];
            assert!(
                body.contains("resolve_item_view("),
                "{command} must route through resolve_item_view so \
                 checkpoint_sha reaches the payload"
            );
        }
    }

    /// Backlog f2d2809b (user request 2027-01-07): `backlog_set_deferred`
    /// must route through the guarded store setter (reload → find live
    /// item → persist) and emit the change event — the command needs Tauri
    /// state, so the wiring is pinned as a source contract; the setter
    /// itself is unit-tested in `mnemo::backlog::tests`.
    #[test]
    fn backlog_set_deferred_routes_through_the_store_setter() {
        let src = include_str!("backlog_cmds.rs");
        let start = src
            .find("pub async fn backlog_set_deferred")
            .expect("backlog_set_deferred command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("set_deferred(&id, deferred)"),
            "backlog_set_deferred must route through BacklogStore::set_deferred"
        );
        assert!(
            body.contains("emit_backlog_changed(&app, &state)"),
            "the toggle must emit the backlog change event"
        );
    }

    /// Backlog f2d2809b: the Run-All total must count only ELIGIBLE items
    /// (pending and not deferred) — deferred items are excluded from the
    /// run's progress denominator, and the start error stays reserved for
    /// a backlog with no pending items at all (an all-deferred backlog
    /// starts the run and the first dispatch ends it cleanly).
    #[test]
    fn backlog_run_all_counts_only_eligible_items() {
        let src = include_str!("backlog_cmds.rs");
        let start = src
            .find("pub async fn backlog_run_all")
            .expect("backlog_run_all command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("i.status == BacklogStatus::Pending && !i.deferred"),
            "the run's total must exclude deferred items"
        );
    }

    /// Backlog 998f85fc (user ask 2027-01-07): `backlog_clear_finished` must
    /// touch ONLY the store + the change event — the run's state (its
    /// in-flight pointer, done/total counters) is run-scoped and must be
    /// unreachable from clearing, so mid-run clearing can never corrupt a
    /// run. The command needs Tauri state, so the isolation is pinned as a
    /// source contract; the clear-finished store semantics (soft-delete
    /// exactly done/failed/cant-resolve, keep pending+in-flight) are
    /// unit-tested in `mnemo::backlog::tests`.
    #[test]
    fn backlog_clear_finished_touches_only_the_store_and_the_event() {
        let src = include_str!("backlog_cmds.rs");
        let start = src
            .find("pub async fn backlog_clear_finished")
            .expect("backlog_clear_finished command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("clear_finished()"),
            "backlog_clear_finished must route through BacklogStore::clear_finished"
        );
        assert!(
            body.contains("emit_backlog_changed(&app, &state)"),
            "clearing must emit the backlog change event"
        );
        assert!(
            !body.contains("run_all"),
            "clearing must not touch the run-all state (the run-scoped counters/pointer are unreachable from a clear)"
        );
    }
}
