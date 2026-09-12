// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! LLM request/response trace Tauri commands — the backend of the right-panel
//! "Trace" tab.
//!
//! Five commands over the shared [`LlmRequestLog`](mnemo::provider::trace::LlmRequestLog)
//! held in [`IpcState`]: list lightweight request summaries (no JSON payloads),
//! fetch one full detail (request JSON + raw response), clear the log, and
//! get/set the file-logging toggle.
//!
//! Every command is async + `tokio::task::spawn_blocking`: Tauri v2 runs SYNC
//! commands on the main thread, and these touch the shared trace locks the
//! background writer also takes (an up-to-8 MiB mirror rewrite under the
//! drain). Keeping every trace command on a blocking worker keeps a lock wait
//! off the UI thread (the 2026-08-22 freeze fix, review L2).

use tauri::State;

use mnemo::provider::trace::{LlmRequestDetail, LlmRequestSummary};

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// List all recorded requests as lightweight summaries, oldest first.
/// Payloads (request JSON + raw response) are excluded — fetch one via
/// [`get_llm_request`] when a row is selected.
///
/// Async + `spawn_blocking`: the summaries map runs under the shared
/// `records` lock, which the file writer also takes on every mirror — and
/// Tauri runs sync commands on the main thread. A sync `list` polling while
/// the writer holds `records` (or, before the 2026-08-22 fix, `log_path`
/// across a mirror) would jank the UI; run it off the main thread like the
/// other trace commands (review N1, 2026-06-14).
#[tauri::command]
pub async fn list_llm_requests(
    state: State<'_, IpcState>,
) -> Result<Vec<LlmRequestSummary>, IpcError> {
    let trace = state.trace.clone();
    tokio::task::spawn_blocking(move || trace.list())
        .await
        .map_err(|e| format!("trace list task failed: {e}"))
        .map_err(IpcError::from)
}

/// The full detail for one recorded request (exact request JSON + raw
/// response text + usage + timing). Returns `None` when the id was never
/// recorded or has been evicted from the ring buffer.
///
/// Async + `spawn_blocking`: the record clone can carry up to ~2 MiB of raw
/// response (plus the request body), and Tauri runs sync commands on the
/// main thread — the poll loop's clone + serde must not jank the UI
/// (review N1, 2026-06-14).
#[tauri::command]
pub async fn get_llm_request(
    state: State<'_, IpcState>,
    id: u64,
) -> Result<Option<LlmRequestDetail>, IpcError> {
    let trace = state.trace.clone();
    tokio::task::spawn_blocking(move || trace.get(id))
        .await
        .map_err(|e| format!("trace detail task failed: {e}"))
        .map_err(IpcError::from)
}

/// Drop all recorded requests. Ids keep counting up — no reuse.
///
/// Async + `spawn_blocking` like [`list_llm_requests`]: `clear` takes the
/// shared `records` lock and Tauri runs sync commands on the main thread —
/// it must not wait on that lock on the UI thread.
#[tauri::command]
pub async fn clear_llm_requests(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let trace = state.trace.clone();
    tokio::task::spawn_blocking(move || trace.clear())
        .await
        .map_err(|e| format!("trace clear task failed: {e}"))
        .map_err(IpcError::from)
}

/// Whether trace records are being mirrored to the log file
/// (`<coding_dir>/logs/traces.jsonl`). The Trace tab's "Log to file" checkbox
/// reads this on mount so it reflects the current state across tab switches.
///
/// Async + `spawn_blocking` like the rest of the trace commands (the read
/// itself is a cheap AtomicBool, but keeping every trace command on a worker
/// thread keeps the UI-thread rule uniform).
#[tauri::command]
pub async fn get_trace_logging(state: State<'_, IpcState>) -> Result<bool, IpcError> {
    let trace = state.trace.clone();
    tokio::task::spawn_blocking(move || trace.logging_enabled())
        .await
        .map_err(|e| format!("trace logging query task failed: {e}"))
        .map_err(IpcError::from)
}

/// Turn trace file logging on/off. On enable, the log directory is created
/// if needed; records are then mirrored to disk as they stream in.
///
/// Async + `spawn_blocking`: the DISABLE path drains the writer queue first
/// (`flush_file_writes` blocks on a channel ack while the writer finishes up
/// to an ~8 MiB rewrite), and Tauri runs sync commands on the main thread —
/// unchecking the box must not hitch the UI (review N6, 2026-06-14). The
/// `RunEvent::Exit` caller keeps the direct blocking method: at process exit
/// there is no UI to keep responsive and the drain must complete before the
/// process dies.
#[tauri::command]
pub async fn set_trace_logging(state: State<'_, IpcState>, enabled: bool) -> Result<(), IpcError> {
    let trace = state.trace.clone();
    tokio::task::spawn_blocking(move || trace.set_logging_enabled(enabled))
        .await
        .map_err(|e| format!("trace logging toggle task failed: {e}"))
        .map_err(IpcError::from)
}

/// Steering-effectiveness counters: how often the advisory nudges (search
/// symbol nudge, shell grep TIP, graph miss hint, RECALLED CONTEXT rider,
/// read_files SYMBOL NUDGE) fired transitively through tool results, and how
/// often the immediately-following tool call from the same agent named one of
/// the marker's target tools (the nudge changed the agent's next choice).
///
/// Mirrors the read-only stats commands (`get_index_progress`,
/// `get_trace_logging`): stateless over the process-wide
/// [`mnemo::agent::steering_stats::SteeringStats::shared`] counters — no
/// `spawn_blocking` needed (a single mutex snapshot, no I/O, no shared-lock
/// contention with the request path beyond one counter lock).
#[tauri::command]
pub fn get_steering_stats() -> Result<mnemo::agent::steering_stats::SteeringStatsSnapshot, IpcError>
{
    Ok(mnemo::agent::steering_stats::SteeringStats::shared().snapshot())
}
