// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Startup code-graph index progress: the event the host forwards to its UI
//! and the process-wide snapshot the `get_index_progress` command reads.
//! Moved from the app crate so the brain builder can live in the lib.

use serde::{Deserialize, Serialize};

use crate::host::maintenance::{should_forward_progress, PROGRESS_EVERY};

/// The Tauri event channel the STARTUP indexing pass (main.rs) and the
/// create-project seed pass (ipc/projects.rs) stream file progress on, so the
/// open-project overlay can show a progress bar + "N/M files indexed"
/// counter. Distinct from [`REINDEX_CHANNEL`] (the Settings card's manual
/// rebuild) so the two UIs never fold each other's traffic.
pub const INDEX_PROGRESS_CHANNEL: &str = "codegraph://index-progress";

/// An event on the [`INDEX_PROGRESS_CHANNEL`] stream — the indexing progress
/// behind the open-project overlay. The same `"type"`-tagged, kebab-case
/// shape as [`ReconcileEvent`] (memory_maintenance.rs), but with NO `phase`
/// field: a code index has exactly one phase. Pinned by
/// `index_progress_event_wire_shape`; the frontend folds it via
/// `applyIndexProgress` in IndexingOverlay.tsx.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum IndexProgressEvent {
    /// The pass started (the overlay appears, counter "…").
    Started,
    /// A progress tick after a walked file (throttled — first, last, and
    /// every `PROGRESS_EVERY`-th tick forward; the startup pass additionally
    /// gates on 1s elapsed so a sub-second pass emits nothing).
    Progress {
        /// Files processed so far.
        done: usize,
        /// Total source files to walk.
        total: usize,
    },
    /// Terminal success with a human-readable summary.
    Done {
        /// One-line summary for the overlay's result line.
        summary: String,
    },
    /// Terminal failure (engine error or a panicked task).
    Failed {
        /// Error text for the overlay's result line.
        error: String,
    },
}

/// Whether a STARTUP-pass progress tick should be forwarded to the overlay.
///
/// Cold start (`switched = false`): only after a full second has elapsed (an
/// already-indexed project's sub-second pass emits nothing, so the overlay
/// never flashes) AND the shared throttle accepts the tick (first /
/// every-50th / last).
///
/// Post-switch startup (`switched = true`, a pending-project marker was
/// consumed — this launch is the reload half of a `switch_project`): the
/// 1-second gate is skipped. The user explicitly opened this project
/// (mirroring the create-project seed pass, which never gates), and the UI
/// they clicked from was just torn down for the restart — delaying feedback
/// would only hide the dialog they expect to see. The shared throttle still
/// applies.
///
/// `pub(crate)`: the startup pass (main.rs) gates its ticks through it.
pub fn startup_should_forward(
    elapsed: std::time::Duration,
    done: usize,
    total: usize,
    switched: bool,
) -> bool {
    (switched || elapsed >= std::time::Duration::from_secs(1))
        && should_forward_progress(done, total, PROGRESS_EVERY)
}

// ── Startup-pass snapshot (late-overlay catch-up) ────────────────────────────

/// Whether a startup indexing pass is (possibly) running right now.
static STARTUP_INDEX_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Files processed so far by the startup pass (meaningful while active).
static STARTUP_INDEX_DONE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Total files the startup pass will walk (meaningful while active).
static STARTUP_INDEX_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mark the STARTUP indexing pass as begun and reset its counters. The
/// snapshot becomes fetchable immediately only when `switched` is true (a
/// post-switch startup always shows the overlay). On a COLD start the
/// snapshot stays invisible until the first tick actually clears the 1s gate
/// ([`ensure_startup_index_visible`]) — a sub-second cold pass on an
/// already-indexed project must never expose the snapshot, or a late
/// overlay's catch-up fetch would flash a dialog the cold-start contract
/// says must not appear.
///
/// WHY a snapshot: the webview listening on `codegraph://index-progress` is
/// (re)created on every app start AND every project switch; React mounts and
/// subscribes some time after the startup pass has already begun, so events
/// emitted before the subscription are lost. The pass mirrors its live
/// progress here so a late-mounting overlay can fetch a snapshot via
/// [`get_index_progress`] and catch up. Only the startup pass records — the
/// Settings-card maintenance stream and the Graph-tab refresh have their own
/// always-mounted UIs and never touch it.
pub fn mark_startup_index_active(switched: bool) {
    STARTUP_INDEX_DONE.store(0, std::sync::atomic::Ordering::Relaxed);
    STARTUP_INDEX_TOTAL.store(0, std::sync::atomic::Ordering::Relaxed);
    STARTUP_INDEX_ACTIVE.store(switched, std::sync::atomic::Ordering::Relaxed);
}

/// Make the startup-pass snapshot fetchable (idempotent). Called by the
/// progress callback on the first FORWARDED tick, so a cold start whose
/// 1s gate opens (a genuinely slow pass worth showing) becomes catch-up
/// visible exactly when the overlay stream becomes active — while gated
/// sub-second cold passes never publish anything.
pub fn ensure_startup_index_visible() {
    STARTUP_INDEX_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Record one startup-pass progress tick. Called on EVERY tick, before any
/// gate, so the snapshot stays complete even for ticks the gate drops.
pub fn record_startup_index_progress(done: usize, total: usize) {
    STARTUP_INDEX_DONE.store(done as u64, std::sync::atomic::Ordering::Relaxed);
    STARTUP_INDEX_TOTAL.store(total as u64, std::sync::atomic::Ordering::Relaxed);
}

/// Clear the snapshot when the startup pass ends (before its terminal event
/// is emitted), so a fetch after the pass reports nothing rather than stale
/// final counts.
pub fn clear_startup_index() {
    STARTUP_INDEX_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// The startup pass's live progress, or `None` when no pass is running (or it
/// already ended — the clear and the terminal event race harmlessly: an
/// overlay that misses both sees nothing, which is correct for a finished
/// pass).
pub fn startup_index_snapshot() -> Option<(usize, usize)> {
    if STARTUP_INDEX_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
        let done = STARTUP_INDEX_DONE.load(std::sync::atomic::Ordering::Relaxed) as usize;
        let total = STARTUP_INDEX_TOTAL.load(std::sync::atomic::Ordering::Relaxed) as usize;
        // The two counters are independent Relaxed atomics, so a concurrent
        // read can transiently pair a new `done` with the previous tick's
        // `total` (done > total for a nanosecond). Clamping keeps the
        // frontend's percentage arithmetic sane; the tear is otherwise
        // harmless and never persists (the next tick re-pairs them).
        Some((done.min(total), total))
    } else {
        None
    }
}