// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Trace tab: the engine's shared LLM request log — list, one detail, clear.
//!
//! One [`LlmRequestLog`](mnemo::provider::trace::LlmRequestLog) lives on the
//! engine's brain: `src/host/brain.rs` builds the model resolver with
//! `trace.clone()`, every provider that resolver builds records into it
//! (`src/model_resolver.rs:490`), and `Engine::trace()` exposes that same log
//! (`src/headless/engine.rs:703`). Mnemo2 runs ONE engine for the whole app —
//! the team runtime is opened on the lead's handle
//! (`src-tauri/src/mnemo2/runtime.rs:370`) and worker sessions spawn on it
//! (`src-tauri/src/mnemo2/team.rs:2751`) — so the lead's requests and every
//! worker's requests land in the same ring. The tab therefore covers the whole
//! app, not just the lead's own turns.
//!
//! A record carries no agent/session attribution, so rows are told apart by
//! model and endpoint, not by which session sent them. Payloads (the POST body
//! and the raw response) can be hundreds of kilobytes each, so the list returns
//! summaries only and [`trace_detail`] fetches one record on demand.

use mnemo::headless::TraceApi;
use mnemo::provider::trace::{LlmRequestDetail, LlmRequestSummary, MAX_RECORDS};
use serde::Serialize;
use tauri::State;

use crate::mnemo2::ipc::Mnemo2State;

/// One poll of the Trace tab: the recorded requests plus the ring's size.
///
/// The ring keeps the newest [`MAX_RECORDS`] requests; the capacity travels
/// with the rows so the tab can say plainly how much history it is showing
/// instead of duplicating the constant in the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct TracePage {
    /// The recorded requests, oldest first (the ring's own order).
    pub requests: Vec<LlmRequestSummary>,
    /// How many records the log keeps before evicting the oldest.
    pub capacity: usize,
}

/// The Trace tab's list: every recorded request, payloads omitted.
///
/// Workers run on the lead's engine, so their requests are in this list too;
/// there is no per-session field to group them by.
#[tauri::command]
pub async fn trace_list(state: State<'_, Mnemo2State>) -> Result<TracePage, String> {
    let trace = open_trace(&state).await?;
    tokio::task::spawn_blocking(move || trace_page(trace.list()))
        .await
        .map_err(|error| format!("the trace list task failed: {error}"))
}

/// One recorded request in full: the exact POSTed JSON body and the raw
/// response, plus usage and timing.
///
/// `None` when the id was never recorded or has since been evicted from the
/// ring — the tab treats that as "this row is gone" and returns to the list.
#[tauri::command]
pub async fn trace_detail(
    state: State<'_, Mnemo2State>,
    id: u64,
) -> Result<Option<LlmRequestDetail>, String> {
    let trace = open_trace(&state).await?;
    tokio::task::spawn_blocking(move || trace.get(id))
        .await
        .map_err(|error| format!("the trace detail task failed: {error}"))
}

/// Drop every recorded request. Ids keep counting up; they are never reused.
#[tauri::command]
pub async fn trace_clear(state: State<'_, Mnemo2State>) -> Result<(), String> {
    let trace = open_trace(&state).await?;
    tokio::task::spawn_blocking(move || trace.clear())
        .await
        .map_err(|error| format!("the trace clear task failed: {error}"))
}

/// Pair the log's rows with the ring's capacity.
///
/// Split from [`trace_list`] so the wire shape stays testable without a Tauri
/// state: the command only fetches and delegates here.
fn trace_page(requests: Vec<LlmRequestSummary>) -> TracePage {
    TracePage {
        requests,
        capacity: MAX_RECORDS,
    }
}

/// The open project's trace handle, over the lead's shared engine.
async fn open_trace(state: &Mnemo2State) -> Result<TraceApi, String> {
    let lead = state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())?;
    let engine = lead.engine_handle().await?;
    Ok(engine.trace())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::provider::trace::{LlmRequestLog, LlmUsage};
    use serde_json::json;

    /// One log with a lead-style and a worker-style request, as the engine's
    /// shared log would hold them.
    fn logged() -> (LlmRequestLog, u64, u64) {
        let log = LlmRequestLog::new();
        let lead = log.start(
            "lead-model",
            "https://lead.invalid/v1",
            "lead-endpoint",
            json!({
                "model": "lead-model",
                "messages": [{ "role": "system", "content": "You are the lead." }],
            }),
        );
        log.set_status(lead, 200);
        log.set_usage(
            lead,
            LlmUsage {
                prompt: 1_200,
                completion: 340,
                reasoning: 0,
                cached: 900,
            },
            Some(180),
            Some(2_400),
            None,
            Some(12),
        );
        log.append_response(
            lead,
            "data: {\"choices\":[{\"delta\":{}}]}\n\ndata: [DONE]\n",
        );
        log.finish(lead, "stop");
        let worker = log.start(
            "worker-model",
            "https://worker.invalid/v1",
            "worker-endpoint",
            json!({
                "model": "worker-model",
                "messages": [{ "role": "user", "content": "fix the tests" }],
            }),
        );
        log.set_status(worker, 400);
        log.fail(worker, 400, "unexpected keyword 'strict' in tool schema");
        (log, lead, worker)
    }

    #[test]
    fn trace_page_keeps_the_log_order_and_carries_the_ring_capacity() {
        let (log, lead, worker) = logged();
        let page = trace_page(log.list());

        assert_eq!(page.capacity, MAX_RECORDS);
        assert_eq!(
            page.requests.len(),
            2,
            "the lead's and the worker's requests"
        );
        assert_eq!(page.requests[0].id, lead);
        assert_eq!(page.requests[1].id, worker);
        assert_eq!(page.requests[0].model, "lead-model");
        assert_eq!(page.requests[1].model, "worker-model");
        // The list row carries what the tab renders: status, tokens, duration.
        let usage = page.requests[0].usage.expect("the lead's usage");
        assert_eq!((usage.prompt, usage.completion), (1_200, 340));
        assert_eq!(page.requests[0].generation_ms, Some(2_400));
        assert_eq!(page.requests[0].http_status, Some(200));
        assert!(page.requests[0].is_complete);
        assert_eq!(page.requests[1].http_status, Some(400));
        assert!(
            page.requests[1].error.is_some(),
            "the refusal is on the row"
        );
    }

    #[test]
    fn trace_detail_is_the_exact_request_and_response_that_were_recorded() {
        let (log, lead, worker) = logged();

        let detail = log.get(lead).expect("the lead's record");
        assert_eq!(detail.request_json["model"], json!("lead-model"));
        assert_eq!(
            detail.request_json["messages"][0]["content"],
            json!("You are the lead."),
            "the POSTed body comes back verbatim"
        );
        assert_eq!(
            detail.response_raw.as_deref(),
            Some("data: {\"choices\":[{\"delta\":{}}]}\n\ndata: [DONE]\n"),
            "the raw response comes back verbatim"
        );
        assert_eq!(detail.finish_reason.as_deref(), Some("stop"));

        let refused = log.get(worker).expect("the worker's record");
        assert_eq!(
            refused.error.as_deref(),
            Some("unexpected keyword 'strict' in tool schema"),
            "a failed request keeps its error body"
        );

        assert!(log.get(9_999).is_none(), "an evicted id is simply absent");
    }

    #[test]
    fn trace_clear_empties_the_page() {
        let (log, _lead, _worker) = logged();
        log.clear();

        let page = trace_page(log.list());
        assert!(page.requests.is_empty());
        assert_eq!(page.capacity, MAX_RECORDS, "clearing keeps the ring size");
        assert!(
            log.get(0).is_none(),
            "clearing drops the payloads as well as the rows"
        );
    }
}
