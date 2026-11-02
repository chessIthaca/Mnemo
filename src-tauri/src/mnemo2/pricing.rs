// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's one model-pricing rule (plan c3ba2b58 step B).
//!
//! Extracted from [`crate::mnemo2::usage`] so every dollar figure in Mnemo2 —
//! the Cost tab's aggregates ([`crate::mnemo2::insights`]), the team ledger's
//! [`crate::mnemo2::usage`] rows, and the Trace view's per-call estimates
//! ([`crate::mnemo2::trace_view`]) — is computed by the SAME function and can
//! never drift: uncached input (`prompt - cached`), cached input, and output
//! are priced separately in integer micro-USD with saturating arithmetic.
//!
//! Pricing resolution: an explicit `[[pricing]]` row in the user's
//! `endpoints.toml` always wins; vendor-default rates (see
//! [`vendor_default_rates`]) are only a fallback for models the user did not
//! price. Models with neither configured nor vendor pricing are unpriced —
//! never guessed — and the callers surface that (`unpriced_models` on the
//! Cost tab, a dash instead of a `$0` on trace rows).

use serde::Serialize;

use mnemo::config::PricingEntry;

/// The model id without any `endpoint/` prefix, used to key usage rows. The
/// bare form keeps Ollama-style version tags (`:1017`) — they are part of
/// the observed model identity and are stripped only when resolving prices.
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
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
    fn model_key_strips_the_endpoint_but_keeps_the_tag() {
        assert_eq!(usage_model_key("test/glm-5.3"), "glm-5.3");
        assert_eq!(usage_model_key("glm-5.3:1017"), "glm-5.3:1017");
        assert_eq!(usage_model_key("glm-5.3"), "glm-5.3");
    }
}