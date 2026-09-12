// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-context model resolution — pick an endpoint + model for a turn based on
//! the workflow state, active skill, and whether the agent is a subagent.
//!
//! The resolver is the seam between the `[models]` config section and the
//! agent loop: at the start of each turn the loop asks the resolver "given
//! where I am right now, should I run on a different model than the default?"
//! and, if so, builds a throwaway provider for that turn.
//!
//! Resolution priority (first match wins):
//! 1. **Skill override** — `[models.skill.<name>]` for the active skill.
//! 2. **Subagent override** — `[models.subagent]` (only for subagents).
//! 3. **Bug-fixing plan-kind override** — `[models.bug_fixing]` while the
//!    active plan's kind is `bug_fixing` and the workflow is in that plan's
//!    active lifecycle (Executing or Reviewing). Unset/dangling falls through.
//! 4. **Workflow-state override** — `[models.planning|executing|complete]`.
//!    The Reviewing state resolves the EXECUTING slot: the main agent keeps
//!    its executing model through the review closing sequence.
//! 5. **None** — fall back to the main agent's default model.
//!
//! `[models.reviewing]` is **reviewer-spawn-only** (2026-12-06 decision): it
//! is never resolved by the per-turn state chain above — only
//! [`ModelResolver::resolve_reviewer_model`] (the `spawn_agent`
//! `role:"reviewer"` spawn-time pin) consumes it.
//!
//! The resolver holds a shared handle to the live [`Config`] so a Settings save
//! (which reloads config) takes effect on the next turn without rebuilding the
//! factory. It is `Send + Sync` and cheap to clone (an `Arc`).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Serialize;

use crate::agent::context::ContextManager;
use crate::config::{Config, ModelRef};
use crate::provider::LlmClient;
use crate::workflow::{PlanKind, WorkflowState};

/// A configured endpoint and the model ids it serves — the payload of
/// [`ModelResolver::list_models`].
///
/// Returned to the agent (via the `list_models` tool) so it can discover which
/// models are available and pick one for `spawn_agent`'s `model` parameter.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    /// The endpoint name (unique key in `endpoints.toml`).
    pub endpoint: String,
    /// Model ids served by this endpoint.
    pub models: Vec<String>,
}

/// The context a turn runs in — what the resolver keys off.
///
/// Built by the agent loop at turn start from its workflow state + whether it
/// is a subagent. `skill_name` is `Some` only while a skill is active
/// (`WorkflowState::Skill`). `plan_kind` is the kind of the active
/// (top-of-stack) plan — it engages the `[models.bug_fixing]` slot while the
/// active plan is a bug-fixing plan in its active lifecycle (see
/// [`ModelResolver::resolve`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelContext<'a> {
    /// The current workflow state (Planning / Executing / Reviewing (which
    /// resolves the executing slot — `[models.reviewing]` is
    /// reviewer-spawn-only) / Complete / Skill).
    pub workflow_state: WorkflowState,
    /// The active skill's name, when the workflow is in the Skill state.
    pub skill_name: Option<&'a str>,
    /// Whether this agent is a subagent (spawned via `spawn_agent` or the UI
    /// spawn button with a parent). Subagents get the `[models.subagent]`
    /// override considered before the state override.
    pub is_subagent: bool,
    /// The kind of the active (top-of-stack) plan, if any. `Some(BugFixing)`
    /// while a bug-fixing plan is executing (or reviewing) makes the resolver
    /// consult `[models.bug_fixing]` before the state slot; `None` (no plan)
    /// and `Some(Implementation)`/`Some(Research)` keep today's behavior.
    pub plan_kind: Option<PlanKind>,
}

impl<'a> ModelContext<'a> {
    /// Build a context from its parts.
    pub fn new(
        workflow_state: WorkflowState,
        skill_name: Option<&'a str>,
        is_subagent: bool,
        plan_kind: Option<PlanKind>,
    ) -> Self {
        Self {
            workflow_state,
            skill_name,
            is_subagent,
            plan_kind,
        }
    }
}

/// Resolves a per-context model override for a turn.
///
/// Implemented by [`ConfigModelResolver`] in production; the trait lets the
/// agent loop depend on an abstraction (and tests inject a canned resolver).
pub trait ModelResolver: Send + Sync {
    /// Resolve the model to use for the given turn context, or `None` to fall
    /// back to the main agent's default model.
    fn resolve(&self, context: ModelContext<'_>) -> Option<ModelRef>;

    /// Resolve the model a spawned `role:"reviewer"` agent runs on.
    ///
    /// `[models.reviewing]` is **reviewer-spawn-only** (2026-12-06 decision):
    /// the per-turn state chain in [`resolve`](Self::resolve) never consults
    /// it (the main agent keeps `[models.executing]` through the Reviewing
    /// state) — this method is its sole consumer, called once at spawn time
    /// by `spawn_agent` to pin the reviewer's model. Precedence:
    /// `[models.reviewing]` → `[models.executing]` (back-compat with
    /// pre-reviewing-slot configs) → `[models.subagent]` → `None` (the
    /// spawned reviewer falls back to the default model). Each link is
    /// validated independently, so a dangling reference falls through to the
    /// next link. The default returns `None` (no config access) so test and
    /// mocked resolvers stay simple.
    fn resolve_reviewer_model(&self) -> Option<ModelRef> {
        None
    }

    /// Resolve the model compaction summaries run on.
    ///
    /// `[models.summarize]` routes the auto-compaction summary call (and the
    /// run-all between-items compact, which funnels through the same
    /// `compact_context` path) to a dedicated — often cheaper — model. The
    /// per-turn state chain in [`resolve`](Self::resolve) never consults it;
    /// the sole consumers are the compaction summary call sites, which fall
    /// back to the turn's provider when this returns `None` (unset — today's
    /// behavior — or a dangling reference). The default returns `None` (no
    /// config access) so test and mocked resolvers stay simple.
    fn resolve_summarize_model(&self) -> Option<ModelRef> {
        None
    }

    /// Build a throwaway provider + context manager for a resolved
    /// [`ModelRef`], sized to the model's context window at `fill_rate`.
    ///
    /// This is the config-backed half of resolution: the loop calls
    /// [`resolve`](Self::resolve) to pick *which* model, then this to actually
    /// build it. Kept on the trait (rather than the loop reaching into config)
    /// so the loop never depends on `Config` directly. Returns `None` when the
    /// referenced endpoint no longer exists (the caller falls back to the
    /// default provider).
    fn build_turn_provider(
        &self,
        model: &ModelRef,
        fill_rate: f64,
    ) -> Option<(Arc<dyn LlmClient>, ContextManager)>;

    /// List the configured endpoints and the model ids each serves.
    ///
    /// Backs the `list_models` tool: the agent calls this to discover which
    /// models are available before picking one for `spawn_agent`'s `model`
    /// parameter. The default returns empty (no config access) so test/mocked
    /// resolvers stay simple.
    fn list_models(&self) -> Vec<ModelInfo> {
        Vec::new()
    }

    /// Resolve a bare model id (e.g. `"deepseek-v4-flash-gcp"`) to a
    /// [`ModelRef`] naming the endpoint that serves it.
    ///
    /// Used by `spawn_agent`'s `model` parameter: the agent passes a model id
    /// (discovered via [`list_models`](Self::list_models)), and this finds the
    /// endpoint that serves it (first match wins if a model id is served by
    /// more than one endpoint). Returns `None` when no endpoint serves the id
    /// (the caller surfaces an "unknown model id" error rather than silently
    /// falling back). The default returns `None` (no config access).
    fn resolve_model_id(&self, _model_id: &str) -> Option<ModelRef> {
        None
    }

    /// Find an ALTERNATE endpoint serving the same model id, excluding the
    /// given endpoint (the one that just failed).
    ///
    /// Used by the 429-fallback path: when a provider returns HTTP 429 (out
    /// of quota), the turn-level retry layer asks the resolver "is this same
    /// model id served by a DIFFERENT endpoint?" — if so, it switches to that
    /// endpoint and retries once, instead of failing or retrying the same
    /// rate-limited provider.
    ///
    /// Returns the first endpoint (in config order, after the excluded one)
    /// that lists `model_id`. Returns `None` when no other endpoint serves the
    /// id (the caller surfaces an actionable error / asks the user). The
    /// default returns `None` (no config access) so test/mocked resolvers
    /// stay simple.
    fn find_alternate_endpoint(
        &self,
        _model_id: &str,
        _exclude_endpoint: &str,
    ) -> Option<ModelRef> {
        None
    }

    /// The DISPLAY-space effective reasoning effort for a resolved
    /// [`ModelRef`] — what the status bar shows: the ref's
    /// `reasoning_effort` override when set, else the model's own default
    /// chain (`ModelSpec::reasoning_effort` → the endpoint's value →
    /// `"max"`), gated by `supports_reasoning_effort` and clamped into the
    /// model's allow-list, with `"off"` kept verbatim (the off-wire
    /// encoding is a wire concern; the UI vocabulary is always
    /// `"off" | "low" | "medium" | "high" | "max"`). Returns `None` when
    /// the ref's endpoint isn't configured (the caller falls back). The
    /// default returns `None` (no config access) so test/mocked resolvers
    /// stay simple.
    fn display_effort_for(&self, _model: &ModelRef) -> Option<String> {
        None
    }
}

/// A [`ModelResolver`] backed by the live [`Config`] (shared handle).
///
/// Reads the `[models]` section each call, so a Settings save takes effect on
/// the next turn. The fill rate for context-manager sizing is passed by the
/// caller (the agent loop) at turn time — the resolver itself doesn't store
/// it, since the loop already owns the authoritative fill rate.
///
/// Built providers are cached by `(endpoint, model)` so the `reqwest::Client`
/// connection pool + TLS session cache are reused across turns (a per-context
/// override would otherwise rebuild a fresh client every turn, discarding
/// keep-alive). The cache is invalidated wholesale on [`set_config`] (after a
/// Settings save), so config changes take effect on the next turn.
pub struct ConfigModelResolver {
    config: Arc<RwLock<Config>>,
    /// The shared request/response trace log, wired into every provider this
    /// resolver builds (the same `Arc` the IPC layer exposes to the UI).
    trace: Arc<crate::provider::trace::LlmRequestLog>,
    /// Cached providers keyed by `(endpoint_name, model_id, effort)`. Reused
    /// across turns so the HTTP connection pool survives. Invalidated on
    /// `set_config`. The per-context effort is part of the key: two contexts
    /// pointing at the same endpoint+model with different efforts must not
    /// share a provider.
    cache: RwLock<HashMap<(String, String, Option<String>), Arc<dyn LlmClient>>>,
}

impl ConfigModelResolver {
    /// Create a resolver over a shared config handle. `trace` is the shared
    /// [`LlmRequestLog`](crate::provider::trace::LlmRequestLog) every provider
    /// built by this resolver records its requests into.
    pub fn new(
        config: Arc<RwLock<Config>>,
        trace: Arc<crate::provider::trace::LlmRequestLog>,
    ) -> Self {
        Self {
            config,
            trace,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Replace the config (after a Settings save reloads config). The resolver
    /// reads config per call, so this is only needed when the in-memory config
    /// changes — e.g. the IPC layer swaps in a freshly loaded `Config` after
    /// `save_settings` / `save_endpoints`. Also invalidates the provider
    /// cache, since endpoint base_urls / API keys / capabilities may have
    /// changed.
    pub fn set_config(&self, config: Config) {
        *self
            .config
            .write()
            .expect("ConfigModelResolver config lock poisoned") = config;
        self.cache
            .write()
            .expect("ConfigModelResolver cache lock poisoned")
            .clear();
    }
}

impl ModelResolver for ConfigModelResolver {
    fn resolve(&self, context: ModelContext<'_>) -> Option<ModelRef> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        let models = &config.general.models;

        // 1. Skill override (highest priority).
        if let Some(name) = context.skill_name {
            if let Some(m) = models.skill.get(name) {
                if let Some(resolved) = config.resolve_model_ref(Some(m)) {
                    return Some(resolved);
                }
            }
        }

        // 2. Subagent override (only for subagents).
        if context.is_subagent {
            if let Some(resolved) = config.resolve_model_ref(models.subagent.as_ref()) {
                return Some(resolved);
            }
        }

        // 3. Bug-fixing plan-kind override (2027-01-07): while the ACTIVE
        // (top-of-stack) plan's kind is bug_fixing and the workflow is in
        // that plan's active lifecycle (Executing or Reviewing),
        // [models.bug_fixing] wins over the state slot — a bug-fixing
        // session can run on a max-reasoning model while implementation
        // plans keep [models.executing]. The slot is validated as its own
        // link: unset or dangling (endpoint deleted) falls through to the
        // state chain below, which is exactly today's behavior. Deliberately
        // NOT engaged in the Complete state (the plan is finished — the
        // complete slot governs), for subagents (arm 2's role state), or
        // while a skill is active (arm 1's deliberate context switch).
        if context.plan_kind == Some(PlanKind::BugFixing)
            && matches!(
                context.workflow_state,
                WorkflowState::Executing | WorkflowState::Reviewing
            )
        {
            if let Some(resolved) = config.resolve_model_ref(models.bug_fixing.as_ref()) {
                return Some(resolved);
            }
        }

        // 4. Workflow-state override.
        //
        // Design decision: the Skill state has NO dedicated state-override
        // field — it is covered exclusively by the skill-name lookup above.
        // So while a skill is active, the executing/planning/complete
        // overrides do NOT apply: if no skill override matched, the agent
        // falls back to the DEFAULT model (not the underlying pre-skill
        // state's override). This keeps skill runs on a single, predictable
        // model (the skill's configured one, or the default) rather than
        // silently switching mid-skill when the pre-skill state changes.
        let state_model = match context.workflow_state {
            WorkflowState::Planning => models.planning.as_ref(),
            WorkflowState::Executing => models.executing.as_ref(),
            // [models.reviewing] is reviewer-spawn-only (2026-12-06): the
            // main agent does NOT switch models when it enters the Reviewing
            // state — it keeps the executing model through the review
            // closing sequence. The reviewing slot itself is consumed solely
            // by resolve_reviewer_model (the spawn_agent reviewer pin).
            WorkflowState::Reviewing => models.executing.as_ref(),
            WorkflowState::Complete => models.complete.as_ref(),
            WorkflowState::Skill => None,
            // Subagent (2026-01-03): a parented sub-agent's role state, never
            // a lifecycle phase — so NO lifecycle-state override applies. Its
            // model is `[models.subagent]` (arm 2 above) or the DEFAULT model;
            // it deliberately does NOT inherit the main plan's state model
            // (previously a sub-agent spawned mid-Executing silently ran
            // `models.executing` because its workflow mirrored the main
            // plan's derived state — that mirroring is gone).
            WorkflowState::Subagent => None,
        };
        config.resolve_model_ref(state_model)
    }

    fn resolve_reviewer_model(&self) -> Option<ModelRef> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        let models = &config.general.models;
        // Reviewer-spawn-only chain: reviewing -> executing -> subagent. Each
        // link is validated independently so a dangling reference falls
        // through to the next (this replaces the old two-step
        // resolve(Reviewing, …) dance, which validated the combined
        // reviewing.or(executing) spec as one link).
        config
            .resolve_model_ref(models.reviewing.as_ref())
            .or_else(|| config.resolve_model_ref(models.executing.as_ref()))
            .or_else(|| config.resolve_model_ref(models.subagent.as_ref()))
    }

    fn resolve_summarize_model(&self) -> Option<ModelRef> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        let models = &config.general.models;
        // Sole link: [models.summarize], validated independently — a dangling
        // reference falls through to None (the summary call sites then ride
        // the turn's provider).
        config.resolve_model_ref(models.summarize.as_ref())
    }

    fn display_effort_for(&self, model: &ModelRef) -> Option<String> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        config
            .endpoint(&model.endpoint)
            .map(|ep| crate::provider::client_factory::resolve_display_effort(ep, model))
    }

    fn build_turn_provider(
        &self,
        model: &ModelRef,
        fill_rate: f64,
    ) -> Option<(Arc<dyn LlmClient>, ContextManager)> {
        // Read the config once (cheap RwLock read) so both the cache-hit and
        // cache-miss paths can apply the preflight settings from [context].
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        let preflight = config.general.context.preflight_compact;
        let headroom = config.general.context.compact_headroom_tokens;
        let ceiling = config.general.context.proxy_cache_ceiling_tokens;
        // Check the cache first — a provider for this (endpoint, model,
        // effort) may already exist from a prior turn (reusing its
        // reqwest::Client connection pool). The cache read is a cheap
        // RwLock read.
        let key = (
            model.endpoint.clone(),
            model.model.clone(),
            model.reasoning_effort.clone(),
        );
        {
            let cache = self
                .cache
                .read()
                .expect("ConfigModelResolver cache lock poisoned");
            if let Some(provider) = cache.get(&key) {
                let max_context = provider.capabilities().max_context;
                let cm = ContextManager::new(max_context, fill_rate)
                    .with_preflight(preflight, headroom)
                    .with_proxy_cache_ceiling(ceiling);
                return Some((Arc::clone(provider), cm));
            }
        }
        // Cache miss — build the provider + cache it for future turns.
        let provider = crate::provider::client_factory::build_provider_for(
            &config,
            model,
            Some(self.trace.clone()),
        )?;
        let max_context = provider.capabilities().max_context;
        let cm = ContextManager::new(max_context, fill_rate)
            .with_preflight(preflight, headroom)
            .with_proxy_cache_ceiling(ceiling);
        self.cache
            .write()
            .expect("ConfigModelResolver cache lock poisoned")
            .insert(key, Arc::clone(&provider));
        Some((provider, cm))
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        config
            .endpoints
            .iter()
            .map(|ep| ModelInfo {
                endpoint: ep.name.clone(),
                models: ep.model_ids(),
            })
            .collect()
    }

    fn resolve_model_id(&self, model_id: &str) -> Option<ModelRef> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        // Find the first endpoint that lists this model id. (If a model id is
        // served by more than one endpoint, the first in config order wins —
        // the caller can't disambiguate by id alone, and that's an acceptable
        // tie-break.)
        let endpoint = config
            .endpoints
            .iter()
            .find(|ep| ep.has_model(model_id))?
            .name
            .clone();
        let model_ref = ModelRef {
            endpoint,
            model: model_id.to_string(),
            // No per-context effort: a bare model id (or a 429 alternate
            // endpoint) inherits the model's own effort default.
            reasoning_effort: None,
        };
        // Validate the endpoint still exists (it does — we just read its name —
        // but this also normalizes through config's resolver for consistency
        // with the rest of the resolution path).
        config.resolve_model_ref(Some(&model_ref))
    }

    fn find_alternate_endpoint(&self, model_id: &str, exclude_endpoint: &str) -> Option<ModelRef> {
        let config = self
            .config
            .read()
            .expect("ConfigModelResolver config lock poisoned");
        // Find the first endpoint (in config order) that serves this model id
        // AND is NOT the excluded endpoint (the one that just 429'd). Config
        // order is the natural tie-break — the user controls priority by the
        // order of [[endpoint]] entries in endpoints.toml.
        let endpoint = config
            .endpoints
            .iter()
            .find(|ep| ep.name != exclude_endpoint && ep.has_model(model_id))?
            .name
            .clone();
        let model_ref = ModelRef {
            endpoint,
            model: model_id.to_string(),
            // No per-context effort: a bare model id (or a 429 alternate
            // endpoint) inherits the model's own effort default.
            reasoning_effort: None,
        };
        // Normalize through config's resolver (validates the endpoint exists +
        // returns None on a dangling reference, consistent with resolve_model_id).
        config.resolve_model_ref(Some(&model_ref))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Endpoint, GeneralConfig, ModelsConfig};
    use std::collections::HashMap;

    /// Build a Config with two endpoints ("openai" + "deepseek") so overrides
    /// can reference them.
    fn config_with_endpoints(models: ModelsConfig) -> Config {
        Config {
            general: GeneralConfig {
                models,
                ..GeneralConfig::default()
            },
            endpoints: vec![
                Endpoint {
                    name: "openai".into(),
                    base_url: "https://api.openai.com/v1/".into(),
                    models: vec!["gpt-4o".into(), "o3".into()],
                    ..Endpoint::test_default()
                },
                Endpoint {
                    name: "deepseek".into(),
                    base_url: "https://api.deepseek.com/v1/".into(),
                    models: vec!["deepseek-v4-flash".into()],
                    ..Endpoint::test_default()
                },
            ],
            pricing: vec![],
            keys: Default::default(),
            mcp: Vec::new(),
            projects: Default::default(),
        }
    }

    fn resolver(config: Config) -> ConfigModelResolver {
        ConfigModelResolver::new(
            Arc::new(RwLock::new(config)),
            Arc::new(crate::provider::trace::LlmRequestLog::new()),
        )
    }

    #[test]
    fn returns_none_when_no_overrides() {
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        let ctx = ModelContext::new(WorkflowState::Planning, None, false, None);
        assert!(r.resolve(ctx).is_none());
    }

    #[test]
    fn state_override_resolves() {
        let r = resolver(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let ctx = ModelContext::new(WorkflowState::Planning, None, false, None);
        let m = r.resolve(ctx).expect("planning override");
        assert_eq!(m.endpoint, "openai");
        assert_eq!(m.model, "o3");
    }

    #[test]
    fn subagent_override_beats_state_override() {
        let r = resolver(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            subagent: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        // A subagent in Planning gets the subagent override, not the planning one.
        let ctx = ModelContext::new(WorkflowState::Planning, None, true, None);
        let m = r.resolve(ctx).expect("subagent override");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn skill_override_beats_subagent_and_state() {
        let r = resolver(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            subagent: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            skill: HashMap::from([(
                "merge_to_main".into(),
                ModelRef {
                    endpoint: "deepseek".into(),
                    model: "deepseek-v4-flash".into(),
                    reasoning_effort: None,
                },
            )]),
            ..ModelsConfig::default()
        }));
        let ctx = ModelContext::new(WorkflowState::Skill, Some("merge_to_main"), true, None);
        let m = r.resolve(ctx).expect("skill override");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn dangling_endpoint_reference_is_dropped() {
        // An override pointing at a non-existent endpoint is dropped (falls
        // back to None) rather than building a provider against nothing.
        let r = resolver(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "ghost".into(),
                model: "x".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let ctx = ModelContext::new(WorkflowState::Planning, None, false, None);
        assert!(r.resolve(ctx).is_none());
    }

    #[test]
    fn skill_state_with_no_skill_name_falls_through() {
        // In the Skill state with no skill name (shouldn't normally happen),
        // there's no state override, so None.
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        let ctx = ModelContext::new(WorkflowState::Skill, None, false, None);
        assert!(r.resolve(ctx).is_none());
    }

    #[test]
    fn executing_and_complete_states_resolve() {
        let r = resolver(config_with_endpoints(ModelsConfig {
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            complete: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r
            .resolve(ModelContext::new(WorkflowState::Executing, None, false, None))
            .expect("executing override");
        assert_eq!(m.model, "deepseek-v4-flash");
        let m = r
            .resolve(ModelContext::new(WorkflowState::Complete, None, false, None))
            .expect("complete override");
        assert_eq!(m.model, "o3");
    }

    #[test]
    fn reviewing_state_keeps_executing_model_not_reviewing_slot() {
        // Decision 2026-12-06: [models.reviewing] is reviewer-spawn-only. The
        // main agent in the Reviewing state resolves the EXECUTING slot — it
        // never switches to the reviewing model mid-review.
        let r = resolver(config_with_endpoints(ModelsConfig {
            reviewing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r
            .resolve(ModelContext::new(WorkflowState::Reviewing, None, false, None))
            .expect("executing override while reviewing");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "deepseek-v4-flash");

        // Reviewing set but executing unset → STILL None for the main agent:
        // the reviewing slot must not leak into the state chain.
        let r = resolver(config_with_endpoints(ModelsConfig {
            reviewing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        assert!(r
            .resolve(ModelContext::new(WorkflowState::Reviewing, None, false, None))
            .is_none());

        // Neither set → None (default model).
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        assert!(r
            .resolve(ModelContext::new(WorkflowState::Reviewing, None, false, None))
            .is_none());
    }

    #[test]
    fn bug_fixing_plan_kind_resolves_the_bug_fixing_slot() {
        // 2027-01-07: while the ACTIVE plan's kind is bug_fixing and the
        // workflow is in that plan's active lifecycle (Executing or
        // Reviewing), [models.bug_fixing] wins over the state slot — a
        // bug-fixing session runs on its own model + effort while
        // implementation plans keep [models.executing].
        let r = resolver(config_with_endpoints(ModelsConfig {
            bug_fixing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: Some("max".into()),
            }),
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: Some("low".into()),
            }),
            ..ModelsConfig::default()
        }));

        // Executing + bug_fixing plan → the bug-fixing slot, effort included.
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("bug-fixing slot while executing a bug plan");
        assert_eq!(m.endpoint, "openai");
        assert_eq!(m.model, "o3");
        assert_eq!(m.reasoning_effort.as_deref(), Some("max"));

        // Reviewing (the closing sequence of the same bug plan) → still the
        // bug-fixing slot.
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Reviewing,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("bug-fixing slot while reviewing a bug plan");
        assert_eq!(m.model, "o3");

        // Executing + implementation plan → the executing slot (unchanged).
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::Implementation),
            ))
            .expect("executing slot for an implementation plan");
        assert_eq!(m.model, "deepseek-v4-flash");
        assert_eq!(m.reasoning_effort.as_deref(), Some("low"));

        // No plan kind (e.g. Planning with an empty stack) → unchanged.
        let m = r
            .resolve(ModelContext::new(WorkflowState::Executing, None, false, None))
            .expect("executing slot without a plan kind");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn bug_fixing_slot_unset_or_dangling_inherits_the_state_chain() {
        // Unset → today's behavior: the executing slot.
        let r = resolver(config_with_endpoints(ModelsConfig {
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("unset bug-fixing slot inherits executing");
        assert_eq!(m.model, "deepseek-v4-flash");

        // Dangling (endpoint deleted) → falls through to the state chain,
        // each link validated independently.
        let r = resolver(config_with_endpoints(ModelsConfig {
            bug_fixing: Some(ModelRef {
                endpoint: "gone".into(),
                model: "whatever".into(),
                reasoning_effort: None,
            }),
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Executing,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("dangling bug-fixing slot falls through to executing");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn bug_fixing_slot_only_engages_in_the_plan_lifecycle() {
        // The slot is scoped to the bug plan's ACTIVE lifecycle: not in the
        // Complete state (the plan is finished — the complete slot governs),
        // not for subagents (the Subagent role state), not while a skill is
        // active (the skill slot wins).
        let r = resolver(config_with_endpoints(ModelsConfig {
            bug_fixing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            complete: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            subagent: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            skill: HashMap::from([(
                "merge_to_main".into(),
                ModelRef {
                    endpoint: "deepseek".into(),
                    model: "deepseek-v4-flash".into(),
                    reasoning_effort: None,
                },
            )]),
            ..ModelsConfig::default()
        }));

        // Complete: the finished bug plan must not pin the bug-fixing model.
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Complete,
                None,
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("complete override after a finished bug plan");
        assert_eq!(m.model, "deepseek-v4-flash");

        // Subagent: the role state wins, never the lifecycle slots.
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Subagent,
                None,
                true,
                Some(PlanKind::BugFixing),
            ))
            .expect("subagent override inside a bug plan");
        assert_eq!(m.model, "deepseek-v4-flash");

        // Skill: the deliberate context switch wins.
        let m = r
            .resolve(ModelContext::new(
                WorkflowState::Skill,
                Some("merge_to_main"),
                false,
                Some(PlanKind::BugFixing),
            ))
            .expect("skill override inside a bug plan");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn subagent_state_never_resolves_a_lifecycle_model() {
        // 2026-01-03 decision (backlog c5ded15d): a sub-agent's workflow state
        // is the Subagent role state, never a lifecycle phase — so the state
        // tier does not apply to it. [models.subagent] wins when set;
        // otherwise the DEFAULT model (None here), never models.executing
        // (previously a sub-agent spawned mid-Executing silently ran the
        // executing model via the mirrored state).
        let r = resolver(config_with_endpoints(ModelsConfig {
            subagent: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            executing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r
            .resolve(ModelContext::new(WorkflowState::Subagent, None, true, None))
            .expect("subagent override wins");
        assert_eq!(m.model, "deepseek-v4-flash");

        // No subagent override → None (the default model), even with
        // models.executing set: the lifecycle tier is gone for sub-agents.
        let r = resolver(config_with_endpoints(ModelsConfig {
            executing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        assert!(
            r.resolve(ModelContext::new(WorkflowState::Subagent, None, true, None))
                .is_none(),
            "Subagent state must not fall back to models.executing"
        );
    }

    #[test]
    fn reviewer_model_chain_reviewing_executing_subagent() {
        // resolve_reviewer_model (the spawn-time reviewer pin) is the ONLY
        // consumer of [models.reviewing]: reviewing → executing
        // (back-compat) → subagent → None, each link validated independently.
        let r = resolver(config_with_endpoints(ModelsConfig {
            reviewing: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            subagent: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        // All three set → reviewing wins.
        let m = r.resolve_reviewer_model().expect("reviewing model");
        assert_eq!(m.endpoint, "openai");
        assert_eq!(m.model, "o3");

        // Dangling reviewing reference → falls through to executing.
        let r = resolver(config_with_endpoints(ModelsConfig {
            reviewing: Some(ModelRef {
                endpoint: "ghost".into(),
                model: "x".into(),
                reasoning_effort: None,
            }),
            executing: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r.resolve_reviewer_model().expect("executing fallback");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn reviewer_model_falls_back_to_subagent_then_none() {
        // Reviewing + executing unset → the subagent slot applies.
        let r = resolver(config_with_endpoints(ModelsConfig {
            subagent: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r.resolve_reviewer_model().expect("subagent fallback");
        assert_eq!(m.model, "deepseek-v4-flash");

        // Nothing set → None (the spawned reviewer runs on the default model).
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        assert!(r.resolve_reviewer_model().is_none());
    }

    #[test]
    fn summarize_model_slot_set_dangling_unset() {
        // resolve_summarize_model is the sole consumer of [models.summarize]:
        // set → the slot; dangling → None (the summary call sites then ride
        // the turn's provider); unset → None (today's behavior).
        let r = resolver(config_with_endpoints(ModelsConfig {
            summarize: Some(ModelRef {
                endpoint: "deepseek".into(),
                model: "deepseek-v4-flash".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let m = r.resolve_summarize_model().expect("summarize model");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "deepseek-v4-flash");

        // Dangling reference → None (no fallback chain — the turn's provider
        // is the fallback, applied at the call site).
        let r = resolver(config_with_endpoints(ModelsConfig {
            summarize: Some(ModelRef {
                endpoint: "ghost".into(),
                model: "x".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        assert!(r.resolve_summarize_model().is_none());

        // Unset → None.
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        assert!(r.resolve_summarize_model().is_none());
    }

    #[test]
    fn build_turn_provider_caches_provider_across_turns() {
        // P1: two turns with the same override must reuse one provider (Arc
        // pointer identity), so the reqwest::Client connection pool survives
        // across turns instead of being rebuilt every turn.
        let r = resolver(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let model = r
            .resolve(ModelContext::new(WorkflowState::Planning, None, false, None))
            .expect("planning override");
        let (p1, _) = r.build_turn_provider(&model, 0.5).expect("first build");
        let (p2, _) = r
            .build_turn_provider(&model, 0.5)
            .expect("second build (cached)");
        // Arc pointer identity — same provider, not a rebuild.
        assert!(
            Arc::ptr_eq(&p1, &p2),
            "the same override must reuse the cached provider across turns"
        );
    }

    #[test]
    fn set_config_invalidates_the_provider_cache() {
        // After a Settings save (set_config), the cache is cleared so the
        // next build picks up the new endpoint config (e.g. a changed API key
        // or base_url).
        let r = resolver(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let model = r
            .resolve(ModelContext::new(WorkflowState::Planning, None, false, None))
            .expect("planning override");
        let (p1, _) = r.build_turn_provider(&model, 0.5).expect("first build");
        // set_config clears the cache.
        r.set_config(config_with_endpoints(ModelsConfig {
            planning: Some(ModelRef {
                endpoint: "openai".into(),
                model: "o3".into(),
                reasoning_effort: None,
            }),
            ..ModelsConfig::default()
        }));
        let (p2, _) = r
            .build_turn_provider(&model, 0.5)
            .expect("rebuild after invalidate");
        // A fresh provider was built (different Arc) — the cache was cleared.
        assert!(
            !Arc::ptr_eq(&p1, &p2),
            "set_config must invalidate the cache so a fresh provider is built"
        );
    }

    #[test]
    fn build_turn_provider_keys_the_cache_on_effort() {
        // Two contexts pointing at the same endpoint+model with different
        // per-context efforts must not share a cached provider; the same
        // effort reuses the cached one (same Arc).
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        let base = ModelRef {
            endpoint: "openai".into(),
            model: "o3".into(),
            reasoning_effort: None,
        };
        let low = ModelRef {
            reasoning_effort: Some("low".into()),
            ..base.clone()
        };
        let (p_none, _) = r.build_turn_provider(&base, 0.5).expect("build base");
        let (p_low, _) = r.build_turn_provider(&low, 0.5).expect("build low");
        assert!(
            !Arc::ptr_eq(&p_none, &p_low),
            "a different per-context effort must build a distinct provider"
        );
        let (p_low_again, _) = r.build_turn_provider(&low, 0.5).expect("rebuild low");
        assert!(
            Arc::ptr_eq(&p_low, &p_low_again),
            "the same (endpoint, model, effort) must reuse the cached provider"
        );
    }

    // --- list_models / resolve_model_id --------------------------------------

    #[test]
    fn list_models_returns_all_endpoints_with_their_models() {
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        let infos = r.list_models();
        assert_eq!(infos.len(), 2, "both endpoints should be listed");
        assert_eq!(infos[0].endpoint, "openai");
        assert_eq!(
            infos[0].models,
            vec!["gpt-4o".to_string(), "o3".to_string()]
        );
        assert_eq!(infos[1].endpoint, "deepseek");
        assert_eq!(infos[1].models, vec!["deepseek-v4-flash".to_string()]);
    }

    #[test]
    fn resolve_model_id_finds_the_serving_endpoint() {
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        // A model served by the "deepseek" endpoint.
        let m = r
            .resolve_model_id("deepseek-v4-flash")
            .expect("known model id should resolve");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "deepseek-v4-flash");
        // A model served by the "openai" endpoint.
        let m = r
            .resolve_model_id("o3")
            .expect("known model id should resolve");
        assert_eq!(m.endpoint, "openai");
        assert_eq!(m.model, "o3");
    }

    #[test]
    fn resolve_model_id_returns_none_for_unknown_id() {
        let r = resolver(config_with_endpoints(ModelsConfig::default()));
        assert!(r.resolve_model_id("does-not-exist").is_none());
    }

    #[test]
    fn resolve_model_id_returns_none_when_no_endpoints_configured() {
        // A config with zero endpoints — no model id can resolve.
        let r = resolver(Config::default());
        assert!(r.resolve_model_id("anything").is_none());
        assert!(r.list_models().is_empty());
    }

    // --- find_alternate_endpoint ------------------------------------------

    /// Build a Config with two endpoints that BOTH serve "shared-model", so a
    /// 429 on one can fall back to the other. "primary" also serves "only-here"
    /// (no alternate for that one).
    fn config_with_shared_model() -> Config {
        Config {
            general: GeneralConfig::default(),
            endpoints: vec![
                Endpoint {
                    name: "primary".into(),
                    base_url: "https://primary.example.com/v1/".into(),
                    models: vec!["shared-model".into(), "only-here".into()],
                    ..Endpoint::test_default()
                },
                Endpoint {
                    name: "backup".into(),
                    base_url: "https://backup.example.com/v1/".into(),
                    models: vec!["shared-model".into()],
                    ..Endpoint::test_default()
                },
            ],
            pricing: vec![],
            keys: Default::default(),
            mcp: Vec::new(),
            projects: Default::default(),
        }
    }

    #[test]
    fn find_alternate_endpoint_finds_other_endpoint_serving_same_model() {
        // A 429 on "primary" serving "shared-model" → the resolver finds
        // "backup" (the other endpoint serving the same model id).
        let r = resolver(config_with_shared_model());
        let alt = r
            .find_alternate_endpoint("shared-model", "primary")
            .expect("an alternate endpoint should be found");
        assert_eq!(alt.endpoint, "backup");
        assert_eq!(alt.model, "shared-model");
    }

    #[test]
    fn find_alternate_endpoint_excludes_the_failed_endpoint() {
        // A 429 on "backup" → the resolver finds "primary" (the other direction).
        let r = resolver(config_with_shared_model());
        let alt = r
            .find_alternate_endpoint("shared-model", "backup")
            .expect("an alternate endpoint should be found");
        assert_eq!(alt.endpoint, "primary");
    }

    #[test]
    fn find_alternate_endpoint_returns_none_when_no_alternate() {
        // "only-here" is served by "primary" alone — no alternate exists.
        let r = resolver(config_with_shared_model());
        assert!(
            r.find_alternate_endpoint("only-here", "primary").is_none(),
            "no alternate endpoint serves 'only-here'"
        );
    }

    #[test]
    fn find_alternate_endpoint_returns_none_for_unknown_model() {
        let r = resolver(config_with_shared_model());
        assert!(
            r.find_alternate_endpoint("does-not-exist", "primary")
                .is_none(),
            "unknown model id has no alternate"
        );
    }

    #[test]
    fn find_alternate_endpoint_returns_none_when_no_endpoints_configured() {
        let r = resolver(Config::default());
        assert!(r.find_alternate_endpoint("anything", "primary").is_none());
    }
}
