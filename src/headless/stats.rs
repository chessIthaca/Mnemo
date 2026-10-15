// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The stats pull + rates (§5.7, job 3b.5 D5) — the binding-ready data
//! over the engine's observability: the per-request [`UsageRates`]
//! (computed in the headless pump, not the runtime), the session's
//! live running totals ([`SessionStats`] — folded from the live
//! events), the engine-wide pull ([`EngineStats`] — the store's
//! project + savings aggregates + the steering counters), and the LLM
//! request trace ([`TraceApi`] over the shared
//! [`LlmRequestLog`](crate::provider::trace::LlmRequestLog)).
//!
//! Everything here is plain serde data or a thin handle — no I/O of
//! its own: the durable halves are store reads (one implementation
//! with the IPC's `get_*_stats`), the live halves are fed by the pump
//! (session.rs).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::steering_stats::SteeringStatsSnapshot;
use crate::memory::types::{ProjectStats, SavingsStats};
use crate::provider::trace::{LlmRequestDetail, LlmRequestLog, LlmRequestSummary};
use crate::runtime::channels::AgentEvent;

/// The per-request derived rates (§5.7) — attached to a `SessionEvent`
/// whose event is a Usage. The formulas are the ones
/// [`AgentEvent::Usage`](crate::runtime::channels::AgentEvent::Usage)
/// documents (input = prompt/ttft, output = completion/generation),
/// normalized to seconds; `cache_hit_ratio` is
/// [`LlmUsage::cache_hit_ratio`](crate::provider::trace::LlmUsage::cache_hit_ratio)'s
/// formula (cached / prompt, `0.0` when no prompt tokens were
/// reported). Computed in the headless layer — the runtime emits only
/// the raw counts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct UsageRates {
    /// Prompt tokens per second of time-to-first-token
    /// (`prompt_tokens × 1000 / ttft_ms`). `None` when the provider
    /// reported no TTFT (or a zero one — no meaningful rate).
    pub input_tok_per_s: Option<f64>,
    /// Completion tokens per second of generation time
    /// (`completion_tokens × 1000 / generation_ms`). `None` when the
    /// provider reported no generation time.
    pub output_tok_per_s: Option<f64>,
    /// The share of prompt tokens served from the provider's cache
    /// (`cached / prompt`; `0.0` when no prompt tokens were reported).
    pub cache_hit_ratio: f64,
}

impl UsageRates {
    /// Derive from a Usage event's fields — the pump's hook. The
    /// `× 1000 / ms` form is the variant doc's prompt/ttft and
    /// completion/generation normalized to seconds (and it keeps the
    /// round-number cases exact).
    pub fn from_usage(
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: u32,
        ttft_ms: Option<u32>,
        generation_ms: Option<u32>,
    ) -> Self {
        let per_s = |tokens: u32, ms: u32| tokens as f64 * 1000.0 / ms as f64;
        Self {
            input_tok_per_s: ttft_ms.filter(|ms| *ms > 0).map(|ms| per_s(prompt_tokens, ms)),
            output_tok_per_s: generation_ms
                .filter(|ms| *ms > 0)
                .map(|ms| per_s(completion_tokens, ms)),
            cache_hit_ratio: if prompt_tokens == 0 {
                0.0
            } else {
                cached_tokens as f64 / prompt_tokens as f64
            },
        }
    }
}

/// The session's LIVE running totals (§5.7) — the pump folds these from
/// the session's events as they flow (Usage, Savings, ModelChanged,
/// ContextUsage); `AgentSession::stats` reads them. The durable
/// counterpart is the store's `session_stats`
/// (`AgentSession::stored_stats`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionStats {
    /// Completed LLM requests (Usage events seen on the bus).
    pub requests: u64,
    /// Prompt tokens summed.
    pub prompt_tokens: u64,
    /// Completion tokens summed.
    pub completion_tokens: u64,
    /// Reasoning tokens summed.
    pub reasoning_tokens: u64,
    /// Cached prompt tokens summed.
    pub cached_tokens: u64,
    /// Saved tokens per optimization lever (Savings events; a
    /// re-expansion lands negative).
    pub saved_tokens_by_kind: BTreeMap<String, i64>,
    /// Model switches (ModelChanged events).
    pub model_switches: u64,
    /// The model serving now (the last ModelChanged's).
    pub current_model: Option<String>,
    /// The reasoning effort serving now.
    pub current_reasoning_effort: Option<String>,
    /// The last context-usage report's used tokens.
    pub context_used: Option<u32>,
    /// The last context-usage report's window max.
    pub context_max: Option<u32>,
    /// TTFT ms summed over timed requests (the mean's numerator).
    pub ttft_ms_total: u64,
    /// Requests that reported a TTFT.
    pub timed_requests: u64,
    /// Generation ms summed (the mean output rate's denominator).
    pub generation_ms_total: u64,
}

impl SessionStats {
    /// Mean TTFT (ms) — `ttft_ms_total / timed_requests`.
    pub fn mean_ttft_ms(&self) -> Option<f64> {
        (self.timed_requests > 0)
            .then(|| self.ttft_ms_total as f64 / self.timed_requests as f64)
    }

    /// Mean output rate — the aggregate `completion_tokens / total
    /// generation time` (duration-weighted, not a mean of per-request
    /// rates; `None` when nothing timed was served).
    pub fn mean_output_tok_per_s(&self) -> Option<f64> {
        (self.generation_ms_total > 0)
            .then(|| self.completion_tokens as f64 * 1000.0 / self.generation_ms_total as f64)
    }

    /// Fold one live event into the totals — the pump's hook (every
    /// event the bus carries is folded, so `stats()` always equals the
    /// sum of what `events()` delivered). The spend ledger contributes
    /// nothing: its tokens duplicate the Usage event's, and its budget
    /// attribution lives in the store's rows.
    pub(crate) fn tally(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Usage {
                prompt_tokens,
                completion_tokens,
                reasoning_tokens,
                cached_tokens,
                ttft_ms,
                generation_ms,
            } => {
                self.requests += 1;
                self.prompt_tokens += *prompt_tokens as u64;
                self.completion_tokens += *completion_tokens as u64;
                self.reasoning_tokens += *reasoning_tokens as u64;
                self.cached_tokens += *cached_tokens as u64;
                if let Some(ms) = ttft_ms {
                    self.ttft_ms_total += *ms as u64;
                    self.timed_requests += 1;
                }
                if let Some(ms) = generation_ms {
                    self.generation_ms_total += *ms as u64;
                }
            }
            AgentEvent::ModelChanged {
                model,
                reasoning_effort,
                ..
            } => {
                self.model_switches += 1;
                self.current_model = Some(model.clone());
                self.current_reasoning_effort = reasoning_effort.clone();
            }
            AgentEvent::Savings { kind, tokens_saved, .. } => {
                *self.saved_tokens_by_kind.entry(kind.clone()).or_insert(0) += *tokens_saved;
            }
            AgentEvent::ContextUsage { used, max, .. } => {
                self.context_used = Some(*used);
                self.context_max = Some(*max);
            }
            _ => {}
        }
    }
}

/// The engine-wide stats pull (§5.7) — the store's aggregates plus the
/// steering-effectiveness snapshot (the same counters the app's Trace
/// tab reads). Serialize-only: [`SteeringStatsSnapshot`] has no
/// `Deserialize` (a binding receives this pull; it never sends it
/// back).
#[derive(Debug, Clone, Serialize)]
pub struct EngineStats {
    /// The store's project-level aggregate.
    pub project_stats: ProjectStats,
    /// The store's savings-ledger aggregate (`saved_tokens_total`,
    /// per-kind, per-day, recent, the cache efficiency).
    pub savings_stats: SavingsStats,
    /// The steering-effectiveness counters' snapshot.
    pub steering_snapshot: SteeringStatsSnapshot,
}

/// The LLM request trace pull (§5.7) — list/get/clear over the shared
/// [`LlmRequestLog`] (the same log the app's Trace tab reads; one
/// instance per engine). Sync calls: the headless caller already sits
/// off any UI thread. (No `Debug` derive — `LlmRequestLog` has none.)
#[derive(Clone)]
pub struct TraceApi {
    log: Arc<LlmRequestLog>,
}

impl TraceApi {
    /// From the engine's shared log.
    pub(crate) fn from_parts(log: Arc<LlmRequestLog>) -> Self {
        Self { log }
    }

    /// All recorded requests as lightweight summaries, oldest first
    /// (no JSON payloads — [`get`](Self::get) fetches one full
    /// detail).
    pub fn list(&self) -> Vec<LlmRequestSummary> {
        self.log.list()
    }

    /// One full detail (request JSON + raw response + usage +
    /// timing); `None` when the id was never recorded or has been
    /// evicted from the ring buffer.
    pub fn get(&self, id: u64) -> Option<LlmRequestDetail> {
        self.log.get(id)
    }

    /// Drop all recorded requests. Ids keep counting up — no reuse.
    pub fn clear(&self) {
        self.log.clear()
    }
}