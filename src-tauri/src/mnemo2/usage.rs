// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Durable, per-project usage accounting for Mnemo2 teams.
//!
//! The accumulator keeps RAW per-model token counts (persisted inside the
//! project's `team.json` ledger, keyed by the observed model id) so the
//! project's estimated cost survives restarts, project switches, and worker
//! deletion, and can be repriced whenever pricing changes. `prompt_tokens`
//! includes `cached_tokens`, so uncached input is
//! `prompt_tokens.saturating_sub(cached_tokens)`; costs are integer micro-USD
//! computed with saturating arithmetic, consistent with [`WorkerTelemetry`]
//! (`crate::mnemo2::team::WorkerRecord`).
//!
//! Pricing resolution lives in [`crate::mnemo2::pricing`] (extracted by plan
//! c3ba2b58 step B): an explicit `[[pricing]]` row in the user's
//! `endpoints.toml` always wins; vendor-default rates are only a fallback
//! for models the user did not price. Usage for models with neither
//! configured nor vendor pricing stays in the totals but is excluded from
//! the cost and listed in [`ProjectUsageView::unpriced_models`] so the UI can
//! mark the estimate partial instead of lying.
//!
//! Double-count safety: the accumulator folds LIVE usage events only, and
//! each fold is SESSION-FILTERED — the headless engine's event bus is
//! engine-wide (every session's pump sees every session's events), so an
//! unfiltered hook would fold the same report once per listening pump. With
//! [`event_session`] filtering, each report is folded exactly once by the
//! pump of the session that emitted it. The headless engine seeds a resumed
//! session's messages directly and never re-emits historical `Usage`
//! events, and the accumulator is loaded from the ledger once per
//! `TeamRuntime::open` — so restarts, session-snapshot recovery, and worker
//! resume add each usage report exactly once.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use mnemo::config::PricingEntry;
use mnemo::headless::{SessionEvent, SessionId};

use crate::mnemo2::pricing::{cost_micros, resolve_rates};

/// One model's raw (unpriced) token usage as persisted in the ledger.
///
/// `prompt_tokens` includes `cached_tokens`; both are kept raw so the row
/// can be repriced whenever the pricing tables change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Total prompt tokens reported by the provider; INCLUDES cached tokens.
    pub prompt_tokens: u64,
    /// The prompt-token subset served from the provider cache.
    pub cached_tokens: u64,
    /// Completion (output) tokens.
    pub output_tokens: u64,
}

impl ModelUsage {
    /// Fold one live usage report into this row (saturating).
    pub fn record(&mut self, prompt_tokens: u64, cached_tokens: u64, output_tokens: u64) {
        self.prompt_tokens = self.prompt_tokens.saturating_add(prompt_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(cached_tokens);
        self.output_tokens = self.output_tokens.saturating_add(output_tokens);
    }
}

/// The durable per-project usage accumulator: raw token rows keyed by the
/// observed model id (for example `glm-5.3` or `deepseek-v4.1-flash:1017`).
///
/// Rows are independent of [`WorkerRecord`]s, so deleting or pruning worker
/// records never removes their contribution, and the same underlying model
/// observed with different Ollama-style tags is accounted per tag.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectUsage {
    /// Observed model id → raw token usage.
    pub models: BTreeMap<String, ModelUsage>,
}

/// The aggregate usage summary exposed on [`TeamSnapshot`](super::TeamSnapshot).
///
/// `cost_micros` is the non-negative sum over the PRICED models only;
/// `unpriced_models` lists every model with recorded usage but neither a
/// configured nor a vendor-default price, so the UI can mark the estimate
/// partial instead of silently under-reporting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectUsageView {
    /// Uncached input tokens (prompt tokens minus cached tokens).
    pub input_tokens: u64,
    /// Cached prompt tokens (billed at the cheaper cache rate).
    pub cached_tokens: u64,
    /// Completion (output) tokens.
    pub output_tokens: u64,
    /// Estimated cost in micro-USD (millionths of a dollar) for the priced
    /// usage; `0` when nothing is priced.
    pub cost_micros: u64,
    /// Models with recorded usage but no configured or vendor-default price,
    /// sorted by model id (BTreeMap iteration order).
    pub unpriced_models: Vec<String>,
}

/// The session that emitted one bus event, if its tag resolves.
///
/// The headless engine's event bus is ENGINE-wide: every session's pump
/// sees every session's events, tagged only with the raw `AgentId`.
/// `SessionId` is an opaque newtype over that id, so the serde round-trip
/// recovers the comparable session id without widening the engine's public
/// surface. Events tagged with a child agent resolve to a different id than
/// their session's main agent — Mnemo2's lead and worker sessions never run
/// child agents, so per-session filtering is exact for them.
pub(crate) fn event_session(event: &SessionEvent) -> Option<SessionId> {
    serde_json::from_value(serde_json::to_value(event.agent).ok()?).ok()
}

impl ProjectUsage {
    /// Fold one live usage report into the accumulator (saturating).
    ///
    /// The model is the observed id, already keyed by
    /// `crate::mnemo2::pricing::usage_model_key`; reports without an observed
    /// model are attributed to the literal `"unknown"` row so they stay
    /// visible (and unpriced) instead of being silently dropped.
    pub fn record(&mut self, model: &str, prompt_tokens: u64, cached_tokens: u64, output_tokens: u64) {
        let row = self.models.entry(model.to_string()).or_default();
        row.record(prompt_tokens, cached_tokens, output_tokens);
    }

    /// Aggregate the raw rows into the frontend summary at the given prices.
    pub fn view(&self, pricing: &[PricingEntry]) -> ProjectUsageView {
        let mut summary = ProjectUsageView::default();
        for (model, row) in &self.models {
            let uncached = row.prompt_tokens.saturating_sub(row.cached_tokens);
            summary.input_tokens = summary.input_tokens.saturating_add(uncached);
            summary.cached_tokens = summary.cached_tokens.saturating_add(row.cached_tokens);
            summary.output_tokens = summary.output_tokens.saturating_add(row.output_tokens);
            match resolve_rates(model, pricing) {
                Some(rates) => {
                    summary.cost_micros = summary.cost_micros.saturating_add(cost_micros(
                        &rates,
                        row.prompt_tokens,
                        row.cached_tokens,
                        row.output_tokens,
                    ));
                }
                None => summary.unpriced_models.push(model.clone()),
            }
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    fn entry(model: &str, input: f64, cached: f64, output: f64) -> PricingEntry {
        PricingEntry {
            model: model.to_string(),
            input_per_1m: input,
            cached_per_1m: cached,
            output_per_1m: output,
        }
    }


    #[test]
    fn view_splits_cached_from_uncached_and_marks_unpriced_models() {
        let mut usage = ProjectUsage::default();
        usage.record("glm-5.3", 100_000, 30_000, 20_000);
        usage.record("mystery-model", 1_000, 0, 0);
        let view = usage.view(&[]);
        assert_eq!(view.input_tokens, 71_000, "prompt minus cached, both models");
        assert_eq!(view.cached_tokens, 30_000);
        assert_eq!(view.output_tokens, 20_000);
        // glm-5.3: 70k * 1.40 + 30k * 0.26 + 20k * 4.40 = 193,800 micro-USD.
        assert_eq!(view.cost_micros, 193_800);
        assert_eq!(view.unpriced_models, vec!["mystery-model".to_string()]);
    }

    #[test]
    fn view_prices_cached_input_at_the_cheaper_rate() {
        let pricing = vec![entry("scripted-a", 2.0, 1.0, 10.0)];
        let mut usage = ProjectUsage::default();
        usage.record("scripted-a", 100, 25, 10);
        // 75 * 2.0 + 25 * 1.0 + 10 * 10.0 = 275, matching worker telemetry.
        assert_eq!(usage.view(&pricing).cost_micros, 275);
        assert!(usage.view(&pricing).unpriced_models.is_empty());
    }

    #[test]
    fn view_saturates_instead_of_overflowing() {
        let pricing = vec![entry("scripted-a", 2.0, 1.0, 10.0)];
        let mut usage = ProjectUsage::default();
        // Two near-u64::MAX reports: every fold and total must saturate
        // (u64::MAX + u64::MAX, and the f64 cost estimate overflows to
        // non-finite) instead of panicking or wrapping.
        usage.record("scripted-a", u64::MAX, u64::MAX / 2, u64::MAX);
        usage.record("scripted-a", u64::MAX, u64::MAX / 2, u64::MAX);
        let view = usage.view(&pricing);
        assert_eq!(view.cached_tokens, u64::MAX - 1);
        assert_eq!(view.output_tokens, u64::MAX);
        assert_eq!(view.input_tokens, 1, "saturated prompt minus the near-saturated cache");
        assert_eq!(view.cost_micros, u64::MAX);
    } 

}