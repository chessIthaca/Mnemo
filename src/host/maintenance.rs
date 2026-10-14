// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The memory-maintenance single-operation guard, the reconcile event the
//! startup derived-index pass reports, and the progress-forwarding throttle.
//! Moved from the app crate so the brain builder can live in the lib.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct BusyGuard;

impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// Returned by [`claim_busy`] when another maintenance operation holds the
/// flag. Carries the user-facing message the app surfaces as an IPC error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaintenanceBusy {
    /// The message to show.
    pub message: String,
}

impl std::fmt::Display for MaintenanceBusy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MaintenanceBusy {}

/// Claim the busy flag, or error when an operation is already running.
/// `pub(crate)` for the startup bootstrap (see [`BusyGuard`]).
pub fn claim_busy() -> Result<(), MaintenanceBusy> {
    if BUSY
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        Ok(())
    } else {
        Err(MaintenanceBusy { message: String::from(
            "a memory maintenance operation is already running",
        ) })
    }
}

/// Minimum rows between forwarded embedding-progress events. One IPC event
/// per re-embedded row floods the webview with thousands of event/setState
/// round-trips on a large store; 1-in-50 keeps the bar smooth at a fraction
/// of the traffic (review perf note). `pub(crate)` so the codegraph reindex
/// command shares the same throttle policy.
pub const PROGRESS_EVERY: usize = 50;

/// Whether an engine progress tick should be forwarded to the UI: always the
/// first and last tick of a run, otherwise every `every`-th. Label-only
/// phases (`done == total == 0`) forward unconditionally. `pub(crate)` so the
/// codegraph reindex command shares the same throttle policy.
pub fn should_forward_progress(done: usize, total: usize, every: usize) -> bool {
    done == 1 || done == total || done.is_multiple_of(every)
}