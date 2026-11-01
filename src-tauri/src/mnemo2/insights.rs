// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Cost and savings insight commands for the Mnemo2 Cost tab.
//!
//! [`insights_cost`] prices Mnemo2's durable per-model usage ledger — the same
//! [`ProjectUsage`] rows the team snapshot aggregates — against the configured
//! `[[pricing]]` rows, with the shared vendor-default fallback. The ledger has
//! no day dimension, so the command also returns a per-day spend series: the
//! engine's daily token totals ([`DayBreakdown`]) priced at the blended rates
//! the project's priced ledger rows imply. That split is an ESTIMATE — a day
//! whose model mix differed from the project's overall mix is priced at the
//! blend — and it stays empty when no ledger row is priced.
//!
//! [`insights_savings`] reads the shared engine's `EngineStats` and prices the
//! savings-event ledger and the prompt-cache hit at the project's realized
//! input rates, mirroring Mnemo1's Dashboard `rateEstimate`.
//!
//! Dollar figures are micro-USD (millionths of a dollar), rounded once, and
//! `None` when no price covers the traffic so the UI can flag the estimate as
//! partial instead of showing a misleading `$0.00`.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use mnemo::config::PricingEntry;
use mnemo::memory::{DayBreakdown, ProjectStats, SavingsStats};

use crate::mnemo2::ipc::Mnemo2State;
use crate::mnemo2::runtime::LeadRuntime;
use crate::mnemo2::usage::{cost_micros, resolve_rates, ModelRates, ProjectUsage};

/// One model's usage and estimated spend in the Cost tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelCost {
    /// The observed model id as recorded in the ledger.
    pub model: String,
    /// Uncached input tokens (prompt tokens minus cached tokens).
    pub input_tokens: u64,
    /// Cached prompt tokens.
    pub cached_tokens: u64,
    /// Completion (output) tokens.
    pub output_tokens: u64,
    /// Estimated cost in micro-USD; `None` when neither a configured nor a
    /// vendor-default price covers the model.
    pub cost_micros: Option<u64>,
}

/// The project's total tokens and estimated spend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CostTotal {
    /// Uncached input tokens across every model.
    pub input_tokens: u64,
    /// Cached prompt tokens across every model.
    pub cached_tokens: u64,
    /// Completion tokens across every model.
    pub output_tokens: u64,
    /// Estimated cost in micro-USD for the PRICED usage only.
    pub cost_micros: u64,
}

/// One day of estimated project spend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CostDay {
    /// Start of the UTC day, in Unix seconds (the engine's bucket value).
    pub day: i64,
    /// Estimated cost in micro-USD for that day.
    pub cost_micros: u64,
}

/// The Cost tab's header, per-model rows, and 14-day spend series.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CostReport {
    /// Project totals (token categories and priced cost).
    pub total: CostTotal,
    /// Per-model rows, priced first (largest cost first), unpriced last.
    pub models: Vec<ModelCost>,
    /// Models with recorded usage but no price, sorted by model id.
    pub unpriced_models: Vec<String>,
    /// Estimated spend for the last 14 UTC days; empty when nothing is priced.
    pub per_day: Vec<CostDay>,
}

/// One savings lever's tokens and estimated dollars.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SavingsLever {
    /// The ledger's lever kind (the frontend maps it to a friendly name).
    pub kind: String,
    /// Number of ledger events of this kind.
    pub events: u64,
    /// Tokens saved by this kind (signed; re-expansions subtract).
    pub tokens_saved: i64,
    /// Estimated dollars saved in micro-USD; `None` when no price is known.
    pub estimated_saved_micros: Option<u64>,
}

/// Prompt-cache totals and what the cache hits saved.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SavingsCache {
    /// Total prompt tokens across the project's recorded requests.
    pub prompt_tokens: u64,
    /// Prompt tokens served from the provider's cache.
    pub cached_tokens: u64,
    /// `cached_tokens / prompt_tokens` (`0.0` when nothing was requested).
    pub hit_rate: f64,
    /// Estimated dollars the cache saved versus paying the input rate for
    /// every cached token; `None` when no price is known.
    pub estimated_saved_micros: Option<u64>,
}

/// One day of tokens saved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SavingsDay {
    /// Start of the UTC day, in Unix seconds (normalized from the ledger's
    /// epoch-day bucket so both insight series share one unit).
    pub day: i64,
    /// Tokens saved that day (signed).
    pub tokens_saved: i64,
}

/// The Cost tab's savings section.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SavingsReport {
    /// Total tokens saved across every lever.
    pub tokens_saved: i64,
    /// Estimated dollars saved in micro-USD; `None` when no price is known.
    pub estimated_saved_micros: Option<u64>,
    /// Per-lever rows, largest saving first (the ledger's order).
    pub levers: Vec<SavingsLever>,
    /// Prompt-cache totals and their estimated dollars saved.
    pub cache: SavingsCache,
    /// Tokens saved for the last 14 UTC days.
    pub per_day: Vec<SavingsDay>,
}

/// The Cost tab's spend report: Mnemo2's durable per-model ledger priced with
/// the open project's `[[pricing]]` configuration.
///
/// Returns a clear error when no project (and therefore no lead runtime) is
/// open, matching the other Mnemo2 commands.
#[tauri::command]
pub async fn insights_cost(state: State<'_, Mnemo2State>) -> Result<CostReport, String> {
    let lead = open_lead(&state).await?;
    let team = lead.team_handle().await?;
    let usage = team.usage().await;
    let pricing = lead.pricing()?;
    let engine = lead.engine_handle().await?;
    let stats = engine
        .stats()
        .await
        .map_err(|error| format!("could not read project stats: {error}"))?;
    Ok(cost_report(
        &usage,
        &pricing,
        &stats.project_stats.per_day,
        now_epoch_secs(),
    ))
}

/// The Cost tab's savings report: the shared engine's savings-event ledger
/// and prompt-cache totals, priced at the project's realized input rates.
///
/// Returns a clear error when no project (and therefore no lead runtime) is
/// open, matching the other Mnemo2 commands.
#[tauri::command]
pub async fn insights_savings(state: State<'_, Mnemo2State>) -> Result<SavingsReport, String> {
    let lead = open_lead(&state).await?;
    let engine = lead.engine_handle().await?;
    let pricing = lead.pricing()?;
    let stats = engine
        .stats()
        .await
        .map_err(|error| format!("could not read engine stats: {error}"))?;
    Ok(savings_report(
        &stats.savings_stats,
        &stats.project_stats,
        &pricing,
        now_epoch_secs(),
    ))
}

/// The open project's lead runtime, or the same clear error the other Mnemo2
/// commands return.
async fn open_lead(state: &Mnemo2State) -> Result<Arc<LeadRuntime>, String> {
    state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())
}

/// The current Unix time in seconds (clamped to `0` before the epoch, which
/// only matters for a system clock that has not been set).
fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// Build the Cost tab's spend report from Mnemo2's durable ledger.
///
/// `per_day` is the engine's daily token history and `now_secs` the current
/// Unix time; both are parameters so the 14-day window is testable.
pub fn cost_report(
    usage: &ProjectUsage,
    pricing: &[PricingEntry],
    per_day: &[DayBreakdown],
    now_secs: i64,
) -> CostReport {
    let mut report = CostReport::default();
    for (model, row) in &usage.models {
        let input_tokens = row.prompt_tokens.saturating_sub(row.cached_tokens);
        report.total.input_tokens = report.total.input_tokens.saturating_add(input_tokens);
        report.total.cached_tokens = report.total.cached_tokens.saturating_add(row.cached_tokens);
        report.total.output_tokens = report.total.output_tokens.saturating_add(row.output_tokens);
        let cost = match resolve_rates(model, pricing) {
            Some(rates) => {
                let cost = cost_micros(
                    &rates,
                    row.prompt_tokens,
                    row.cached_tokens,
                    row.output_tokens,
                );
                report.total.cost_micros = report.total.cost_micros.saturating_add(cost);
                Some(cost)
            }
            None => {
                report.unpriced_models.push(model.clone());
                None
            }
        };
        report.models.push(ModelCost {
            model: model.clone(),
            input_tokens,
            cached_tokens: row.cached_tokens,
            output_tokens: row.output_tokens,
            cost_micros: cost,
        });
    }
    report.models.sort_by(model_cost_order);
    report.per_day = spend_per_day(usage, pricing, per_day, now_secs);
    report
}

/// Priced rows first, most expensive first; ties and the unpriced tail order
/// by model id so repeated reads are stable.
fn model_cost_order(left: &ModelCost, right: &ModelCost) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (left.cost_micros, right.cost_micros) {
        (Some(left_cost), Some(right_cost)) => right_cost
            .cmp(&left_cost)
            .then_with(|| left.model.cmp(&right.model)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => left.model.cmp(&right.model),
    }
}

/// The last 14 UTC days of engine token history, priced at the blended rates
/// the project's priced ledger rows imply.
///
/// The ledger keeps no day dimension and the engine's daily rows carry no
/// model breakdown, so this is the best available estimate: each day's
/// prompt/cached/completion totals are priced at the project-wide blend. It is
/// empty when no ledger row is priced (there is no rate to apply).
fn spend_per_day(
    usage: &ProjectUsage,
    pricing: &[PricingEntry],
    per_day: &[DayBreakdown],
    now_secs: i64,
) -> Vec<CostDay> {
    let Some(blend) = blended_rates(usage, pricing) else {
        return Vec::new();
    };
    let today = now_secs.div_euclid(86_400);
    let oldest = today - 13;
    per_day
        .iter()
        .filter(|day| (oldest..=today).contains(&day.day.div_euclid(86_400)))
        .map(|day| {
            let uncached = day.prompt_tokens.saturating_sub(day.cached_tokens) as f64;
            let micros = uncached * blend.input
                + day.cached_tokens as f64 * blend.cached
                + day.completion_tokens as f64 * blend.output;
            CostDay {
                day: day.day,
                cost_micros: round_micros(micros),
            }
        })
        .collect()
}

/// The project's realized price per token category, blended over its priced
/// ledger rows (micro-USD per token; a rate of X dollars per 1M tokens is
/// exactly X micro-USD per token).
///
/// A category the ledger never recorded falls back to the unweighted mean of
/// the priced models' rates, so a day carrying such tokens is still priced.
/// `None` when no row is priced at all.
fn blended_rates(usage: &ProjectUsage, pricing: &[PricingEntry]) -> Option<ModelRates> {
    #[derive(Default)]
    struct Blend {
        uncached_tokens: f64,
        uncached_micros: f64,
        cached_tokens: f64,
        cached_micros: f64,
        output_tokens: f64,
        output_micros: f64,
        input_rate_sum: f64,
        cached_rate_sum: f64,
        output_rate_sum: f64,
        priced_models: u64,
    }

    let mut blend = Blend::default();
    for (model, row) in &usage.models {
        let Some(rates) = resolve_rates(model, pricing) else {
            continue;
        };
        let uncached = row.prompt_tokens.saturating_sub(row.cached_tokens) as f64;
        blend.uncached_tokens += uncached;
        blend.uncached_micros += uncached * rates.input;
        blend.cached_tokens += row.cached_tokens as f64;
        blend.cached_micros += row.cached_tokens as f64 * rates.cached;
        blend.output_tokens += row.output_tokens as f64;
        blend.output_micros += row.output_tokens as f64 * rates.output;
        blend.input_rate_sum += rates.input;
        blend.cached_rate_sum += rates.cached;
        blend.output_rate_sum += rates.output;
        blend.priced_models += 1;
    }
    if blend.priced_models == 0 {
        return None;
    }
    let models = blend.priced_models as f64;
    Some(ModelRates {
        input: weighted_rate(
            blend.uncached_tokens,
            blend.uncached_micros,
            blend.input_rate_sum,
            models,
        ),
        cached: weighted_rate(
            blend.cached_tokens,
            blend.cached_micros,
            blend.cached_rate_sum,
            models,
        ),
        output: weighted_rate(
            blend.output_tokens,
            blend.output_micros,
            blend.output_rate_sum,
            models,
        ),
    })
}

/// The token-weighted rate for one category, or the unweighted mean when the
/// category has no recorded tokens.
fn weighted_rate(tokens: f64, micros: f64, rate_sum: f64, models: f64) -> f64 {
    if tokens > 0.0 {
        micros / tokens
    } else {
        rate_sum / models
    }
}

/// Round micro-USD to a whole number, clamping negatives and non-finite
/// values to `0` (a savings estimate never renders as a negative dollar).
fn round_micros(micros: f64) -> u64 {
    if !micros.is_finite() || micros <= 0.0 {
        return 0;
    }
    micros.round().min(u64::MAX as f64) as u64
}

/// Build the Cost tab's savings report from the engine's stats pull.
///
/// Savings are priced at the project's realized uncached-input rate (the same
/// derivation as Mnemo1's Dashboard `rateEstimate`): the savings ledger does
/// not record which model saved the tokens, so the project's blended rate is
/// the honest estimate. `now_secs` bounds the 14-day series and exists so the
/// window is testable.
pub fn savings_report(
    savings: &SavingsStats,
    project: &ProjectStats,
    pricing: &[PricingEntry],
    now_secs: i64,
) -> SavingsReport {
    let rates = realized_rates(project, pricing);
    let price_tokens = |tokens: i64| {
        rates.map(|rates| round_micros(tokens.max(0) as f64 * rates.input))
    };

    let levers = savings
        .per_kind
        .iter()
        .map(|lever| SavingsLever {
            kind: lever.kind.clone(),
            events: lever.event_count,
            tokens_saved: lever.saved_tokens,
            estimated_saved_micros: price_tokens(lever.saved_tokens),
        })
        .collect();

    let cache = &savings.cache;
    let hit_rate = if cache.prompt_tokens > 0 {
        cache.cached_tokens as f64 / cache.prompt_tokens as f64
    } else {
        0.0
    };
    let cache_saved = rates.map(|rates| {
        // The cache saved the difference between the input rate and the
        // cached rate on every cached token. A misconfigured cached rate above
        // the input rate would be nonsense, so clamp the difference at zero.
        round_micros(
            cache.cached_tokens as f64 * (rates.input - rates.cached).max(0.0),
        )
    });

    let today = now_secs.div_euclid(86_400);
    let oldest = today - 13;
    let per_day = savings
        .per_day
        .iter()
        .filter(|day| (oldest..=today).contains(&day.day))
        .map(|day| SavingsDay {
            day: day.day * 86_400,
            tokens_saved: day.saved_tokens,
        })
        .collect();

    SavingsReport {
        tokens_saved: savings.saved_tokens_total,
        estimated_saved_micros: price_tokens(savings.saved_tokens_total),
        levers,
        cache: SavingsCache {
            prompt_tokens: cache.prompt_tokens,
            cached_tokens: cache.cached_tokens,
            hit_rate,
            estimated_saved_micros: cache_saved,
        },
        per_day,
    }
}

/// The project's realized prompt prices in micro-USD per token, derived from
/// its per-model request totals joined to the pricing table.
///
/// Only priced rows contribute; `None` when no row matched a price at all.
fn realized_rates(project: &ProjectStats, pricing: &[PricingEntry]) -> Option<ModelRates> {
    let mut uncached_tokens = 0u64;
    let mut cached_tokens = 0u64;
    let mut uncached_micros = 0.0f64;
    let mut cached_micros = 0.0f64;
    let mut output_micros = 0.0f64;
    let mut output_tokens = 0u64;
    let mut matched = 0u64;
    for row in &project.per_model {
        let Some(rates) = resolve_rates(&row.model, pricing) else {
            continue;
        };
        let uncached = row.prompt_tokens.saturating_sub(row.cached_tokens);
        uncached_tokens = uncached_tokens.saturating_add(uncached);
        cached_tokens = cached_tokens.saturating_add(row.cached_tokens);
        output_tokens = output_tokens.saturating_add(row.completion_tokens);
        uncached_micros += uncached as f64 * rates.input;
        cached_micros += row.cached_tokens as f64 * rates.cached;
        output_micros += row.completion_tokens as f64 * rates.output;
        matched += 1;
    }
    if matched == 0 {
        return None;
    }
    Some(ModelRates {
        input: if uncached_tokens > 0 {
            uncached_micros / uncached_tokens as f64
        } else {
            0.0
        },
        cached: if cached_tokens > 0 {
            cached_micros / cached_tokens as f64
        } else {
            0.0
        },
        output: if output_tokens > 0 {
            output_micros / output_tokens as f64
        } else {
            0.0
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::memory::{
        CacheEfficiency, ModelBreakdown, SavingsDayBreakdown, SavingsKindBreakdown,
    };

    fn entry(model: &str, input: f64, cached: f64, output: f64) -> PricingEntry {
        PricingEntry {
            model: model.to_string(),
            input_per_1m: input,
            cached_per_1m: cached,
            output_per_1m: output,
        }
    }

    fn ledger(rows: &[(&str, u64, u64, u64)]) -> ProjectUsage {
        let mut usage = ProjectUsage::default();
        for (model, prompt, cached, output) in rows {
            usage.record(model, *prompt, *cached, *output);
        }
        usage
    }

    fn day(day: i64, prompt: u64, cached: u64, completion: u64) -> DayBreakdown {
        DayBreakdown {
            day,
            prompt_tokens: prompt,
            completion_tokens: completion,
            reasoning_tokens: 0,
            cached_tokens: cached,
            request_count: 1,
        }
    }

    fn model_row(model: &str, prompt: u64, cached: u64, completion: u64) -> ModelBreakdown {
        ModelBreakdown {
            model: model.to_string(),
            endpoint: Some("test".to_string()),
            prompt_tokens: prompt,
            completion_tokens: completion,
            reasoning_tokens: 0,
            cached_tokens: cached,
            ttft_ms_total: 0,
            generation_ms_total: 0,
            timed_requests: 0,
            request_count: 1,
        }
    }

    fn savings_stats(
        kinds: &[(&str, u64, i64)],
        days: &[(i64, i64)],
        prompt: u64,
        cached: u64,
    ) -> SavingsStats {
        SavingsStats {
            saved_tokens_total: kinds.iter().map(|(_, _, tokens)| *tokens).sum(),
            event_count: kinds.iter().map(|(_, events, _)| *events).sum(),
            per_kind: kinds
                .iter()
                .map(|(kind, events, tokens)| SavingsKindBreakdown {
                    kind: (*kind).to_string(),
                    event_count: *events,
                    saved_tokens: *tokens,
                })
                .collect(),
            per_day: days
                .iter()
                .map(|(day, tokens)| SavingsDayBreakdown {
                    day: *day,
                    event_count: 1,
                    saved_tokens: *tokens,
                })
                .collect(),
            recent: Vec::new(),
            cache: CacheEfficiency {
                prompt_tokens: prompt,
                cached_tokens: cached,
                cached_not_null_requests: 0,
                request_count: 0,
            },
        }
    }

    #[test]
    fn cost_report_prices_every_model_row() {
        let pricing = vec![entry("alpha", 2.0, 0.5, 4.0)];
        let usage = ledger(&[("alpha", 1_000_000, 250_000, 100_000)]);

        let report = cost_report(&usage, &pricing, &[], 0);

        assert_eq!(
            report.total,
            CostTotal {
                input_tokens: 750_000,
                cached_tokens: 250_000,
                output_tokens: 100_000,
                // 750k uncached x 2 + 250k cached x 0.5 + 100k output x 4.
                cost_micros: 2_025_000,
            }
        );
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].model, "alpha");
        assert_eq!(report.models[0].cost_micros, Some(2_025_000));
        assert!(report.unpriced_models.is_empty());
        assert!(report.per_day.is_empty());
    }

    #[test]
    fn cost_report_marks_unpriced_models_and_keeps_them_last() {
        let pricing = vec![entry("alpha", 1.0, 0.0, 2.0)];
        // `mystery` has no configured row and no vendor default.
        let usage = ledger(&[("alpha", 10, 0, 5), ("mystery", 100, 20, 7)]);

        let report = cost_report(&usage, &pricing, &[], 0);

        assert_eq!(report.unpriced_models, vec!["mystery".to_string()]);
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].model, "alpha");
        assert_eq!(report.models[0].cost_micros, Some(20));
        assert_eq!(report.models[1].model, "mystery");
        assert_eq!(report.models[1].cost_micros, None);
        assert_eq!(report.models[1].input_tokens, 80);
        assert_eq!(report.total.input_tokens, 90);
        // The total is the PRICED part only.
        assert_eq!(report.total.cost_micros, 20);
    }

    #[test]
    fn cost_report_is_empty_without_usage() {
        let pricing = vec![entry("alpha", 1.0, 0.0, 1.0)];
        let report = cost_report(
            &ProjectUsage::default(),
            &pricing,
            &[day(1_756_684_800, 1_000, 0, 10)],
            1_756_684_800,
        );

        assert_eq!(report, CostReport::default());
    }

    #[test]
    fn cost_report_prices_the_last_fourteen_days_at_the_blend() {
        let now: i64 = 1_799_000_000;
        let today = now.div_euclid(86_400) * 86_400;
        let pricing = vec![entry("alpha", 2.0, 0.5, 4.0)];
        let usage = ledger(&[("alpha", 1_000_000, 0, 0)]);
        let days = [
            day(today, 1_000_000, 0, 0),
            day(today - 13 * 86_400, 500_000, 0, 0),
            day(today - 14 * 86_400, 999_999, 0, 0),
        ];

        let report = cost_report(&usage, &pricing, &days, now);

        assert_eq!(report.per_day.len(), 2);
        assert_eq!(report.per_day[0].day, today);
        assert_eq!(report.per_day[0].cost_micros, 2_000_000);
        assert_eq!(report.per_day[1].cost_micros, 1_000_000);
    }

    #[test]
    fn cost_report_falls_back_to_vendor_defaults() {
        // glm-5.3 has a vendor-default rate, so it prices without a config row.
        let report = cost_report(&ledger(&[("glm-5.3", 1_000_000, 0, 0)]), &[], &[], 0);

        assert_eq!(report.total.cost_micros, 1_400_000);
        assert!(report.unpriced_models.is_empty());
    }

    #[test]
    fn savings_report_prices_levers_and_the_cache_at_the_realized_rate() {
        let now: i64 = 1_799_000_000;
        let today = now.div_euclid(86_400);
        let pricing = vec![entry("alpha", 2.0, 0.5, 4.0)];
        let project = ProjectStats {
            per_model: vec![model_row("alpha", 1_000_000, 250_000, 0)],
            ..ProjectStats::default()
        };
        let savings = savings_stats(
            &[("truncation", 2, 100_000), ("archive_expand", 1, -50_000)],
            &[(today, 30_000), (today - 20, 5_000)],
            1_000_000,
            250_000,
        );

        let report = savings_report(&savings, &project, &pricing, now);

        // The realized uncached input rate is 2.0 micro-USD per token.
        assert_eq!(report.tokens_saved, 50_000);
        assert_eq!(report.estimated_saved_micros, Some(100_000));
        assert_eq!(report.levers.len(), 2);
        assert_eq!(report.levers[0].kind, "truncation");
        assert_eq!(report.levers[0].estimated_saved_micros, Some(200_000));
        // Negative savings clamp to zero dollars rather than going negative.
        assert_eq!(report.levers[1].estimated_saved_micros, Some(0));
        assert_eq!(report.cache.hit_rate, 0.25);
        assert_eq!(report.cache.estimated_saved_micros, Some(375_000));
        // Only the day inside the 14-day window survives, normalized to
        // day-start seconds.
        assert_eq!(
            report.per_day,
            vec![SavingsDay {
                day: today * 86_400,
                tokens_saved: 30_000,
            }]
        );
    }

    #[test]
    fn savings_report_is_unpriced_without_pricing() {
        let now: i64 = 1_799_000_000;
        let today = now.div_euclid(86_400);
        let savings = savings_stats(&[("truncation", 1, 1_000)], &[(today, 1_000)], 100, 50);

        let report = savings_report(&savings, &ProjectStats::default(), &[], now);

        assert_eq!(report.tokens_saved, 1_000);
        assert_eq!(report.estimated_saved_micros, None);
        assert_eq!(report.levers[0].estimated_saved_micros, None);
        assert_eq!(report.cache.estimated_saved_micros, None);
        assert_eq!(report.cache.hit_rate, 0.5);
        assert_eq!(report.per_day.len(), 1);
    }

    #[test]
    fn reports_serialize_with_the_documented_wire_shape() {
        let cost = CostReport {
            total: CostTotal {
                input_tokens: 1,
                cached_tokens: 2,
                output_tokens: 3,
                cost_micros: 4,
            },
            models: vec![ModelCost {
                model: "alpha".to_string(),
                input_tokens: 1,
                cached_tokens: 2,
                output_tokens: 3,
                cost_micros: Some(4),
            }],
            unpriced_models: Vec::new(),
            per_day: vec![CostDay {
                day: 0,
                cost_micros: 4,
            }],
        };
        assert_eq!(
            serde_json::to_value(&cost).expect("cost report serializes"),
            serde_json::json!({
                "total": {
                    "input_tokens": 1,
                    "cached_tokens": 2,
                    "output_tokens": 3,
                    "cost_micros": 4
                },
                "models": [{
                    "model": "alpha",
                    "input_tokens": 1,
                    "cached_tokens": 2,
                    "output_tokens": 3,
                    "cost_micros": 4
                }],
                "unpriced_models": [],
                "per_day": [{ "day": 0, "cost_micros": 4 }]
            })
        );

        let savings = SavingsReport {
            tokens_saved: 10,
            estimated_saved_micros: None,
            levers: vec![SavingsLever {
                kind: "delta_read".to_string(),
                events: 1,
                tokens_saved: 10,
                estimated_saved_micros: Some(5),
            }],
            cache: SavingsCache {
                prompt_tokens: 10,
                cached_tokens: 5,
                hit_rate: 0.5,
                estimated_saved_micros: Some(2),
            },
            per_day: vec![SavingsDay {
                day: 0,
                tokens_saved: 10,
            }],
        };
        assert_eq!(
            serde_json::to_value(&savings).expect("savings report serializes"),
            serde_json::json!({
                "tokens_saved": 10,
                "estimated_saved_micros": null,
                "levers": [{
                    "kind": "delta_read",
                    "events": 1,
                    "tokens_saved": 10,
                    "estimated_saved_micros": 5
                }],
                "cache": {
                    "prompt_tokens": 10,
                    "cached_tokens": 5,
                    "hit_rate": 0.5,
                    "estimated_saved_micros": 2
                },
                "per_day": [{ "day": 0, "tokens_saved": 10 }]
            })
        );
    }
}
