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
//! Pricing resolution: an explicit `[[pricing]]` row in the user's
//! `endpoints.toml` always wins; vendor-default rates (see
//! [`vendor_default_rates`]) are only a fallback for models the user did not
//! price. Usage for models with neither configured nor vendor pricing stays
//! in the totals but is excluded from the cost and listed in
//! [`ProjectUsageView::unpriced_models`] so the UI can mark the estimate
//! partial instead of lying.
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

/// The model key for one usage report: the observed id without any
/// `endpoint/` prefix, so spec-shaped ids (`test/glm-5.3`) and bare observed
/// ids (`glm-5.3`) land on the same row. Version tags (`:1017`) are kept —
/// they are part of the observed model identity and are stripped only when
/// resolving prices.
pub fn usage_model_key(model: &str) -> &str {
    model.rsplit('/').next().unwrap_or(model)
}

/// The model id without any `endpoint/` prefix and without an Ollama-style
/// version tag, used only for price lookups.
fn price_lookup_key(model: &str) -> &str {
    let bare = usage_model_key(model);
    bare.split(':').next().unwrap_or(bare)
}

/// Per-model price rates in USD per 1M tokens, matching the
/// `input_per_1m`/`cached_per_1m`/`output_per_1m` fields of a
/// [`PricingEntry`]. A rate of X dollars per 1M tokens is exactly X
/// micro-USD per token, so these values multiply token counts directly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelRates {
    /// USD per 1M uncached input tokens.
    pub input: f64,
    /// USD per 1M cached (cache-hit) input tokens.
    pub cached: f64,
    /// USD per 1M output tokens.
    pub output: f64,
}

impl ModelRates {
    /// The rates of one explicit configuration entry.
    fn from_entry(entry: &PricingEntry) -> Self {
        Self {
            input: entry.input_per_1m,
            cached: entry.cached_per_1m,
            output: entry.output_per_1m,
        }
    }
}

/// Estimated cost in micro-USD for one raw usage report at the given rates.
///
/// Mirrors the per-request estimate used for worker telemetry: uncached input
/// (`prompt - cached`), cached input, and output are priced separately and
/// the result saturates instead of overflowing.
pub fn cost_micros(
    rates: &ModelRates,
    prompt_tokens: u64,
    cached_tokens: u64,
    output_tokens: u64,
) -> u64 {
    let uncached = prompt_tokens.saturating_sub(cached_tokens) as f64;
    let micros = uncached * rates.input
        + cached_tokens as f64 * rates.cached
        + output_tokens as f64 * rates.output;
    if !micros.is_finite() || micros < 0.0 {
        return u64::MAX;
    }
    micros.round().min(u64::MAX as f64) as u64
}

/// Vendor-default fallback prices (USD per 1M tokens), used ONLY when the
/// user's `endpoints.toml` configures no explicit `[[pricing]]` row for the
/// observed model — explicit rows always win (see [`resolve_rates`]).
///
/// Every rate below was read from the vendor's official pricing page.
/// Never add a rate you have not confirmed there, and leave genuinely
/// unavailable models unpriced. Where a vendor differentiates peak and
/// off-peak hours, the peak (conservative, upper) rate is used.
///
/// - Ollama Cloud — https://ollama.com/pricing (per 1M tokens):
///   - `glm-5.3`: 1.40 input / 0.26 cached / 4.40 output
///   - `glm-5.3-flash`: 0.15 / 0.03 / 0.50
///   - `deepseek-v4.1-flash`: 0.30 / 0.006 / 1.20 (also serves the
///     configured `deepseek-v4.1-flash:1017` tag)
/// - Anthropic first-party API —
///   https://platform.claude.com/docs/en/about-claude/pricing:
///   - `claude-fable-5-1`: 10.00 input / 0.25 cache-hit / 50.00 output
///   - `claude-opus-5-5`: 4.00 / 0.20 / 20.00
/// - Google Gemini API — https://ai.google.dev/gemini-api/docs/pricing:
///   - `gemini-3-flash-preview`: 0.50 input / 0.05 cached / 3.00 output
///     (text/image/video rates)
/// - DeepSeek platform — https://api-docs.deepseek.com/quick_start/pricing:
///   - `deepseek-flash`: 0.30 cache-miss / 0.006 cache-hit / 1.20 output
///     (peak-hour rates; off-peak is half)
/// - Kimi (Moonshot) — https://platform.moonshot.ai/docs/pricing/chat:
///   - `kimi-k3.1-pro`: 2.00 input / 0.20 cached / 10.00 output
///   - `kimi-k3.1-pro-highspeed`: 4.00 / 0.20 / 20.00
pub fn vendor_default_rates(model: &str) -> Option<ModelRates> {
    let rates = match price_lookup_key(model) {
        "glm-5.3" => ModelRates {
            input: 1.40,
            cached: 0.26,
            output: 4.40,
        },
        "glm-5.3-flash" => ModelRates {
            input: 0.15,
            cached: 0.03,
            output: 0.50,
        },
        "deepseek-v4.1-flash" | "deepseek-flash" => ModelRates {
            input: 0.30,
            cached: 0.006,
            output: 1.20,
        },
        "claude-fable-5-1" => ModelRates {
            input: 10.00,
            cached: 0.25,
            output: 50.00,
        },
        "claude-opus-5-5" => ModelRates {
            input: 4.00,
            cached: 0.20,
            output: 20.00,
        },
        "gemini-3-flash-preview" => ModelRates {
            input: 0.50,
            cached: 0.05,
            output: 3.00,
        },
        "kimi-k3.1-pro" => ModelRates {
            input: 2.00,
            cached: 0.20,
            output: 10.00,
        },
        "kimi-k3.1-pro-highspeed" => ModelRates {
            input: 4.00,
            cached: 0.20,
            output: 20.00,
        },
        _ => return None,
    };
    Some(rates)
}

/// Resolve the price for one observed model: an explicit `[[pricing]]` row
/// from `endpoints.toml` always wins; vendor defaults are the fallback.
///
/// Explicit rows match the observed model id, its bare form (after any
/// `endpoint/` prefix), and its tag-stripped form, so a row named
/// `deepseek-v4.1-flash` also prices `deepseek-v4.1-flash:1017`.
pub fn resolve_rates(model: &str, pricing: &[PricingEntry]) -> Option<ModelRates> {
    let bare = usage_model_key(model);
    let tagless = price_lookup_key(model);
    if let Some(entry) = pricing
        .iter()
        .find(|entry| entry.model == model || entry.model == bare || entry.model == tagless)
    {
        return Some(ModelRates::from_entry(entry));
    }
    vendor_default_rates(model)
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
    /// [`usage_model_key`]; reports without an observed model are attributed
    /// to the literal `"unknown"` row so they stay visible (and unpriced)
    /// instead of being silently dropped.
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

    fn rates(input: f64, cached: f64, output: f64) -> Option<ModelRates> {
        Some(ModelRates {
            input,
            cached,
            output,
        })
    }

    fn entry(model: &str, input: f64, cached: f64, output: f64) -> PricingEntry {
        PricingEntry {
            model: model.to_string(),
            input_per_1m: input,
            cached_per_1m: cached,
            output_per_1m: output,
        }
    }

    #[test]
    fn vendor_defaults_match_the_official_vendor_pages() {
        assert_eq!(vendor_default_rates("glm-5.3"), rates(1.40, 0.26, 4.40));
        assert_eq!(vendor_default_rates("glm-5.3-flash"), rates(0.15, 0.03, 0.50));
        assert_eq!(
            vendor_default_rates("deepseek-v4.1-flash"),
            rates(0.30, 0.006, 1.20)
        );
        // The configured Ollama tag and the spec-shaped id both resolve.
        assert_eq!(
            vendor_default_rates("deepseek-v4.1-flash:1017"),
            rates(0.30, 0.006, 1.20)
        );
        assert_eq!(
            vendor_default_rates("Ollama Cloud/deepseek-v4.1-flash:1017"),
            rates(0.30, 0.006, 1.20)
        );
        assert_eq!(vendor_default_rates("deepseek-flash"), rates(0.30, 0.006, 1.20));
        assert_eq!(
            vendor_default_rates("claude-fable-5-1"),
            rates(10.00, 0.25, 50.00)
        );
        assert_eq!(
            vendor_default_rates("claude-opus-5-5"),
            rates(4.00, 0.20, 20.00)
        );
        assert_eq!(
            vendor_default_rates("gemini-3-flash-preview"),
            rates(0.50, 0.05, 3.00)
        );
        assert_eq!(vendor_default_rates("kimi-k3.1-pro"), rates(2.00, 0.20, 10.00));
        assert_eq!(
            vendor_default_rates("kimi-k3.1-pro-highspeed"),
            rates(4.00, 0.20, 20.00)
        );
        // Unlisted models stay unpriced — never guess a rate.
        assert_eq!(vendor_default_rates("some-internal-model"), None);
        assert_eq!(vendor_default_rates(""), None);
    }

    #[test]
    fn explicit_pricing_wins_over_vendor_defaults() {
        let pricing = vec![entry("glm-5.3", 9.0, 8.0, 7.0)];
        assert_eq!(
            resolve_rates("glm-5.3", &pricing),
            rates(9.0, 8.0, 7.0),
            "the explicit row must win over the 1.40/0.26/4.40 vendor default"
        );
        assert_eq!(
            resolve_rates("test/glm-5.3", &pricing),
            rates(9.0, 8.0, 7.0),
            "explicit rows match the bare id after any endpoint prefix"
        );
        assert_eq!(
            resolve_rates("glm-5.3:1017", &pricing),
            rates(9.0, 8.0, 7.0),
            "explicit rows match the tag-stripped id"
        );
        // A model with no explicit row still falls back to the vendor table.
        assert_eq!(
            resolve_rates("claude-opus-5-5", &pricing),
            rates(4.00, 0.20, 20.00)
        );
        // Unpriced-by-anyone models stay unpriced.
        assert_eq!(resolve_rates("some-internal-model", &pricing), None);
    }

    #[test]
    fn explicit_pricing_only_matches_its_own_model() {
        let pricing = vec![entry("some-internal-model", 4.0, 0.20, 20.0)];
        assert_eq!(resolve_rates("some-internal-model", &pricing), rates(4.0, 0.20, 20.0));
        assert_eq!(
            resolve_rates("another-model", &pricing),
            None,
            "an explicit row for one model must not price a different model"
        );
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

    #[test]
    fn model_key_strips_the_endpoint_but_keeps_the_tag() {
        assert_eq!(usage_model_key("test/glm-5.3"), "glm-5.3");
        assert_eq!(usage_model_key("glm-5.3:1017"), "glm-5.3:1017");
        assert_eq!(usage_model_key("glm-5.3"), "glm-5.3");
    }
}