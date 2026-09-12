// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Shared config-write helpers — the duplicated persist+reload and
//! live-provider-swap logic extracted from `settings.rs` and `agent.rs`.
//!
//! These two operations were duplicated across `save_endpoints`, `save_settings`,
//! and `set_model`. Centralizing them here keeps the behavior identical (the
//! swap ALWAYS clears `resolved_model`) while removing the duplication. This is
//! a pure refactor — the "live drift" where `save_endpoints` didn't clear
//! `resolved_model` was already fixed; this module preserves that correct
//! behavior. (Runtime-safety sync lives on `IpcState::set_safety`.)

use std::sync::Arc;

use mnemo::agent::context::ContextManager;
use mnemo::agent::factory::AgentLoopFactory;
use mnemo::config::{Config, Endpoint};
use mnemo::provider::client_factory::{build_client, provider_kind};
use mnemo::provider::policy::reasoning_effort_off_wire_value;
use mnemo::provider::trace::LlmRequestLog;
use mnemo::provider::LlmClient;
use mnemo::runtime::channels::SerializableAgentEvent;
use mnemo::runtime::AgentId;
use tauri::{AppHandle, State};

use crate::ipc::error::IpcError;
use crate::ipc::events::emit_agent_event;
use crate::ipc::state::{AgentLoopMap, IpcState};

/// Persist `new_config` to disk, reload it, and swap it into the shared
/// `state.project.config` handle.
///
/// Extracted from `save_endpoints` (settings.rs) and `save_settings`
/// (settings.rs) — both did the same `save_all` + `load` + swap. Returns the
/// reloaded config so the caller can rewire vision/embedder/resolver from it.
pub(crate) async fn persist_and_reload(
    state: &State<'_, IpcState>,
    new_config: Config,
) -> Result<Config, IpcError> {
    let config_dir = mnemo::config::global_config_dir();
    new_config
        .save_all(&config_dir)
        .map_err(|e| format!("failed to write config: {e}"))?;
    let reloaded = mnemo::config::Config::load(&config_dir)
        .map_err(|e| format!("config saved but failed to reload: {e}"))?;
    {
        let mut guard = state.project.config.lock().await;
        *guard = reloaded.clone();
    }
    Ok(reloaded)
}

/// The result of resolving a model selection into a live provider — the
/// AppHandle-free core of the status-bar model picker (`set_model`), shared
/// with the console runtime's `/model` + `/reasoning` commands.
pub(crate) struct ResolvedModel {
    /// The freshly built provider, wired to `trace`.
    pub provider: Arc<dyn LlmClient>,
    /// The endpoint's provider-kind label (e.g. `"OpenAI"`), for logging.
    pub kind_label: String,
    /// The reasoning effort baked into the provider (`None` = the request
    /// field is omitted). Reported for display after a swap.
    pub effective_effort: Option<String>,
    /// The DISPLAY-space effective reasoning effort (`"off" | "low" |
    /// "medium" | "high" | "max"`) — what the status bar shows after a
    /// swap: the requested value gated + clamped with `"off"` kept verbatim
    /// (no off-wire encoding), else the model's default chain
    /// (`ModelSpec::reasoning_effort` → the endpoint's value → `"max"`).
    /// Carried on the `ModelChanged` event + recorded on the loop so the UI
    /// tracks the effort of the model actually in use (backlog 51dab4da).
    pub display_effort: String,
}

/// Map a requested reasoning-effort value (the status-bar dropdown / console
/// `/reasoning` argument) to the value actually sent, honoring the endpoint's
/// capability flag + the model's per-model `reasoning_efforts` allow-list.
/// Pure, so it is unit-testable in isolation.
///
/// - `!supports_reasoning_effort` → always `None` (the field is never sent).
/// - `Some("off")` → the provider's off encoding: the model's
///   `reasoning_effort_off_wire` override, else the endpoint's, else the
///   built-in policy (`None` (omit the field) for most providers,
///   `Some("none")` for DeepSeek-family models — a literal `"off"` is an
///   instant non-retryable 400 there).
/// - `Some(v)` → `Some(v)` verbatim, unless the model's allow-list excludes
///   it (then the list's first entry — the highest supported — applies,
///   encoded per provider when it is `"off"`).
/// - `None` → the model-scoped configured value, defaulting to `"max"`.
pub(crate) fn resolve_reasoning_effort(
    ep: &Endpoint,
    model_id: &str,
    requested: Option<&str>,
) -> Option<String> {
    if !ep.supports_reasoning_effort {
        return None;
    }
    // The harness's "off" encodes per provider: omitted for most, "none"
    // (the enum's explicit thinking-off variant) for DeepSeek-family models —
    // a literal "off" is an instant non-retryable 400 there. The
    // endpoint/model `reasoning_effort_off_wire` config overrides the
    // name-based policy (the escape hatch for aliases/renames/fine-tunes);
    // the policy stays as the zero-config fallback.
    let off_wire = ep
        .reasoning_effort_off_wire_for(model_id)
        .or_else(|| {
            reasoning_effort_off_wire_value(provider_kind(ep.kind), model_id).map(str::to_string)
        });
    let off_wire = off_wire.as_deref();
    match requested {
        Some("off") => off_wire.map(str::to_string),
        Some(v) => {
            let allowed = ep.model_spec(model_id).and_then(|spec| {
                if spec.reasoning_efforts.is_empty() {
                    None
                } else {
                    Some(spec.reasoning_efforts.as_slice())
                }
            });
            match allowed {
                Some(list) if !list.iter().any(|e| e == v) => {
                    // Clamp to the list's first entry (the highest supported).
                    // "off" is a valid list entry but must never become a
                    // literal request value — encode it per provider.
                    match list[0].as_str() {
                        "off" => off_wire.map(str::to_string),
                        clamped => Some(clamped.to_string()),
                    }
                }
                _ => Some(v.to_string()),
            }
        }
        None => ep.effective_reasoning_effort_for(Some(model_id)),
    }
}

/// Validate an endpoint + model selection and build the provider for it — the
/// shared core of the IPC `set_model` command and the console runtime's
/// `/model` + `/reasoning` commands, extracted so both surfaces behave
/// identically (no drift on validation, effort mapping, or key resolution).
///
/// The model must be a member of the endpoint's configured `models` list —
/// the allowlist keeps callers from pointing the provider at an arbitrary
/// model. The global *default* model (`general.default_model`) is implicitly
/// allowed: after a config edit it may be the running model without appearing
/// in `models`, and re-selecting it must not fail.
///
/// Holds no locks and awaits nothing — callers pass the config guard they
/// already hold (the config lock stays short).
pub(crate) fn resolve_model_provider(
    config: &Config,
    endpoint_name: &str,
    model: &str,
    reasoning_effort: Option<&str>,
    trace: &Arc<LlmRequestLog>,
) -> Result<ResolvedModel, IpcError> {
    let ep = config
        .endpoint(endpoint_name)
        .ok_or_else(|| format!("endpoint '{endpoint_name}' not found in endpoints.toml"))?;
    // The model must be a member of the endpoint's configured list — the
    // allowlist keeps callers from pointing the provider at an arbitrary
    // model. The endpoint's *default* model is implicitly allowed: after a
    // config edit it may be the running model without appearing in `models`,
    // and re-selecting it must not fail.
    if !ep.has_model(model) && config.general.general.default_model.as_deref() != Some(model) {
        return Err(
            format!("model '{model}' is not configured at endpoint '{endpoint_name}'").into(),
        );
    }
    // Capability flag wins: non-reasoning endpoints never get the field.
    // Otherwise the caller's raw value applies; "off" means omit.
    let effective_effort = resolve_reasoning_effort(ep, model, reasoning_effort);
    // The display-space twin (backlog 51dab4da): the same gate + clamp but
    // "off" stays "off" — the UI vocabulary, never the off-wire encoding.
    let display_effort = match reasoning_effort {
        Some(e) => ep.display_normalize_reasoning_effort_for(model, e),
        None => ep.display_reasoning_effort_for(Some(model)),
    };
    let kind_label = format!("{:?}", ep.kind);
    // Shared construction path — same key-resolution + kind dispatch as the
    // main/vision providers in main.rs (no drift). Wire the shared trace log
    // so the swapped provider's requests land in the Trace tab.
    let provider = build_client(
        config,
        ep,
        model,
        ep.multimodal_for(model),
        effective_effort.clone(),
        Some(trace.clone()),
    );
    Ok(ResolvedModel {
        provider,
        kind_label,
        effective_effort,
        display_effort,
    })
}

/// Swap a provider into the factory + every live agent loop, clearing the
/// stale `resolved_model` on each loop — the AppHandle-free core of a live
/// model swap, shared by [`swap_live_provider`] (which then emits
/// `ModelChanged` per agent) and the console runtime (which prints instead).
///
/// The provider swap ALWAYS clears `resolved_model` — the provider just
/// changed, so the previous turn's per-context override is stale until the
/// next turn re-resolves it. Returns the ids of the agents whose loops were
/// swapped, so each caller can surface the change through its own channel.
pub(crate) async fn swap_provider_into_loops(
    factory: &Arc<AgentLoopFactory>,
    agent_loops: &AgentLoopMap,
    provider: Arc<dyn LlmClient>,
    context_manager: ContextManager,
    display_effort: Option<String>,
) -> Vec<AgentId> {
    factory.set_provider(provider.clone());
    // Keep the factory's default-effort record in sync (stamped onto every
    // loop built from now on — backlog 51dab4da).
    factory.set_default_display_effort(display_effort.clone());
    // Swap into every live agent loop + clear the stale resolved model
    // (mirrors `set_model`): the provider just changed, so the previous turn's
    // per-context override is stale until the next turn re-resolves it.
    // Collect the ids so the caller can surface the swap after dropping the
    // lock.
    let agent_ids: Vec<AgentId> = {
        let agent_loops = agent_loops.lock().await;
        for agent_loop in agent_loops.values() {
            agent_loop.set_provider(provider.clone(), context_manager.clone());
            agent_loop.set_resolved_model(None);
            // Clear the stale resolved ENDPOINT alongside the model (the
            // global default swap just replaced the provider for every loop).
            agent_loop.set_resolved_provider(None);
            // Record the new default's DISPLAY effort (backlog 51dab4da) so
            // the no-override resolution branch reports it, and clear the
            // stale per-turn effort mirror alongside the model.
            agent_loop.set_default_display_effort(display_effort.clone());
            agent_loop.set_resolved_effort(None);
        }
        agent_loops.keys().copied().collect()
    };
    agent_ids
}

/// Swap a provider into ONE agent's loop only — the per-agent model switch
/// used by the GUI model picker (`set_model` with an `agent_id`).
///
/// Models are agent-specific: switching the active agent's model must not
/// touch the factory (newly spawned agents keep the configured default) or
/// any other live agent's loop. The provider is pinned via
/// [`mnemo::agent::AgentLoop::set_explicit_provider`] so the picker's choice
/// beats the config-backed `[models.*]` override chain for the workflow
/// state it was picked in (bug 2026-08-22: switching kimi → deepseek, then
/// the next turn re-resolved the `[models.planning]` kimi override and ran
/// kimi anyway). The pin is STATE-SCOPED (2026-12-20): when the workflow
/// later transitions to a state with a configured `[models.*]` slot, that
/// configured model takes over; with nothing configured for the new state
/// the pin keeps serving. Clears that loop's `resolved_model` so
/// `list_agents` reports the new model immediately (the previous turn's
/// per-context override is stale until the next turn re-resolves it).
///
/// Returns [`SwapOutcome::NotFound`] when the agent id is not in the map (the
/// caller reports "no agent loop for agent {id}" and emits nothing).
/// [`SwapOutcome::Deferred`] when the new model has a smaller context window —
/// the swap is deferred so the turn loop can summarize first (the caller emits
/// a user-facing note instead of `ModelChanged`).
pub(crate) enum SwapOutcome {
    /// The agent id was not found in the live loop map.
    NotFound,
    /// The provider was swapped immediately (same or larger context window).
    Swapped,
    /// The swap was deferred — the new model has a smaller context window, so
    /// the turn loop will summarize using the OLD provider before completing
    /// the swap.
    Deferred,
}

pub(crate) async fn swap_provider_into_loop(
    agent_loops: &AgentLoopMap,
    agent_id: AgentId,
    provider: Arc<dyn LlmClient>,
    context_manager: ContextManager,
    display_effort: Option<String>,
) -> SwapOutcome {
    let agent_loop = {
        let agent_loops = agent_loops.lock().await;
        match agent_loops.get(&agent_id) {
            Some(l) => Arc::clone(l),
            None => return SwapOutcome::NotFound,
        }
    };
    agent_loop.set_explicit_provider(provider, context_manager);
    agent_loop.set_resolved_model(None);
    // The new default provider serves the next turn — clear the stale
    // resolved ENDPOINT alongside the stale model so `list_agents` reports
    // the new provider's endpoint immediately.
    agent_loop.set_resolved_provider(None);
    // Record the swap's DISPLAY effort (backlog 51dab4da): the pin's
    // build-time effort (what the picker requested, or the model's default)
    // and the new default slot's effort (the pin swap replaces both), so the
    // pin + no-override resolution branches report it. The stale per-turn
    // effort mirror is cleared alongside the model.
    agent_loop.set_pinned_display_effort(display_effort.clone());
    agent_loop.set_default_display_effort(display_effort);
    agent_loop.set_resolved_effort(None);
    if agent_loop.has_pending_swap() {
        SwapOutcome::Deferred
    } else {
        SwapOutcome::Swapped
    }
}

/// Swap the live provider into the factory + every live agent loop, clearing
/// the stale `resolved_model` on each loop, and emit a `ModelChanged` event
/// per agent so the UI updates immediately.
///
/// Extracted from `save_endpoints` (settings.rs) and `set_model` (agent.rs).
/// The AppHandle-free swap core lives in [`swap_provider_into_loops`]; this
/// wrapper adds the per-agent UI events. Returns `true` if a swap happened
/// (factory present + provider built), `false` otherwise.
pub(crate) async fn swap_live_provider(
    state: &State<'_, IpcState>,
    app: &AppHandle,
    provider: Arc<dyn LlmClient>,
    context_manager: ContextManager,
    model: String,
    display_effort: Option<String>,
) -> bool {
    let Some(factory) = state.runtime.factory.clone() else {
        return false;
    };
    // The serving endpoint name, captured BEFORE the provider Arc is moved
    // into the loops (the new default's name; empty for test mocks → None).
    let provider_label = {
        let n = provider.provider_name().to_string();
        (!n.is_empty()).then_some(n)
    };
    let agent_ids = swap_provider_into_loops(
        &factory,
        &state.runtime.agent_loops,
        provider,
        context_manager,
        display_effort.clone(),
    )
    .await;
    // Push the new model to the UI immediately via a `ModelChanged` event per
    // live agent (the provider was just swapped for every loop, so the new
    // default model id is what `list_agents` would report). Best-effort: a
    // failed emit is logged, not fatal.
    for id in agent_ids {
        emit_agent_event(
            app,
            id,
            SerializableAgentEvent::ModelChanged {
                model: model.clone(),
                provider: provider_label.clone(),
                reasoning_effort: display_effort.clone(),
            },
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use mnemo::config::{GeneralConfig, GeneralSection, SafetyMode};
    use mnemo::error::Result;
    use mnemo::project::ConstitutionSource;
    use mnemo::provider::{Capabilities, LlmEvent, ProviderKind, ToolSchema};
    use mnemo::tool::agent::sandbox::Sandbox;

    use async_trait::async_trait;
    use futures::stream::BoxStream;

    /// An `Endpoint` literal with the given models + effort support.
    fn ep(name: &str, models: &[&str], supports_effort: bool) -> Endpoint {
        Endpoint {
            name: name.into(),
            models: models
                .iter()
                .map(|s| mnemo::config::ModelSpec::from(s.to_string()))
                .collect(),
            supports_reasoning_effort: supports_effort,
            ..Endpoint::test_default()
        }
    }

    /// The model id the `ep` helper's first model gets (`"m"`).
    const FIRST_MODEL: &str = "m";

    /// A `Config` with the given endpoints + optional global default model.
    fn test_config(endpoints: Vec<Endpoint>, default_model: Option<&str>) -> Config {
        Config {
            general: GeneralConfig {
                general: GeneralSection {
                    default_model: default_model.map(str::to_string),
                    ..GeneralSection::default()
                },
                ..GeneralConfig::default()
            },
            endpoints,
            ..Config::default()
        }
    }

    fn trace() -> Arc<LlmRequestLog> {
        Arc::new(LlmRequestLog::new())
    }

    /// `unwrap_err` needs `Debug` on the Ok type, which `ResolvedModel`
    /// (holding `Arc<dyn LlmClient>`) can't derive — extract the error side.
    fn expect_err(res: std::result::Result<ResolvedModel, IpcError>) -> IpcError {
        match res {
            Ok(_) => panic!("expected an error, got a resolved provider"),
            Err(e) => e,
        }
    }

    #[test]
    fn resolve_reasoning_effort_off_maps_to_none() {
        let e = ep("a", &["m"], true);
        assert_eq!(resolve_reasoning_effort(&e, FIRST_MODEL, Some("off")), None);
    }

    #[test]
    fn resolve_reasoning_effort_off_wire_config_overrides_policy() {
        // The endpoint/model `reasoning_effort_off_wire` config encodes "off"
        // regardless of the model's name — the escape hatch for renamed/
        // aliased/fine-tuned DeepSeek models the name-based policy can't see.
        let mut e = ep("a", &["m"], true);
        e.reasoning_effort_off_wire = Some("none".into());
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, Some("off")),
            Some("none".to_string())
        );
        // The model-level entry wins over the endpoint's.
        e.models[0].reasoning_effort_off_wire = Some("disabled".into());
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, Some("off")),
            Some("disabled".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_passthrough() {
        let e = ep("a", &["m"], true);
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, Some("high")),
            Some("high".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_unsupported_endpoint_always_none() {
        // The capability flag wins: even an explicit effort value is dropped
        // for an endpoint that rejects the field.
        let e = ep("a", &["m"], false);
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, Some("high")),
            None
        );
        assert_eq!(resolve_reasoning_effort(&e, FIRST_MODEL, None), None);
    }

    #[test]
    fn resolve_reasoning_effort_none_falls_back_to_endpoint_default() {
        // No requested value → the endpoint default of "max".
        let e = ep("a", &["m"], true);
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, None),
            Some("max".to_string())
        );

        // ...or the endpoint's configured value, verbatim.
        let mut configured = ep("a", &["m"], true);
        configured.reasoning_effort = Some("low".into());
        assert_eq!(
            resolve_reasoning_effort(&configured, FIRST_MODEL, None),
            Some("low".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_none_uses_model_level_default() {
        // No requested value → the model's per-model default (the per-model
        // row dropdown) wins over the endpoint's configured value.
        let mut e = ep("a", &["m"], true);
        e.models[0].reasoning_effort = Some("low".into());
        e.reasoning_effort = Some("high".into());
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, None),
            Some("low".to_string())
        );
        // Without the per-model override, the endpoint's value applies.
        e.models[0].reasoning_effort = None;
        assert_eq!(
            resolve_reasoning_effort(&e, FIRST_MODEL, None),
            Some("high".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_clamps_into_model_allow_list() {
        // A requested value outside the model's `reasoning_efforts` list is
        // clamped to the list's first entry (the highest supported).
        let mut e = ep("a", &["m"], true);
        e.models[0].reasoning_efforts = vec!["high".into(), "off".into()];
        assert_eq!(
            resolve_reasoning_effort(&e, "m", Some("max")),
            Some("high".to_string())
        );
        // An allowed value passes through untouched.
        assert_eq!(
            resolve_reasoning_effort(&e, "m", Some("high")),
            Some("high".to_string())
        );
        // "off" is always allowed (it omits the field).
        assert_eq!(resolve_reasoning_effort(&e, "m", Some("off")), None);
    }

    #[test]
    fn resolve_reasoning_effort_off_only_list_never_sends_literal_off() {
        // Regression (review M1): a model whose allow-list is ["off"] with a
        // requested value outside it must clamp to NONE (omit the field) —
        // never a literal `Some("off")` request value.
        let mut e = ep("a", &["m"], true);
        e.models[0].reasoning_efforts = vec!["off".into()];
        assert_eq!(resolve_reasoning_effort(&e, "m", Some("max")), None);
        assert_eq!(resolve_reasoning_effort(&e, "m", Some("high")), None);
    }

    #[test]
    fn resolve_reasoning_effort_off_maps_to_none_for_deepseek() {
        // Regression (DeepSeek instant-400): "off" is not in DeepSeek's
        // reasoning_effort enum — the harness's off encodes as the enum's
        // explicit thinking-off value "none" for DeepSeek-family models.
        let e = ep("a", &["deepseek-v4-flash"], true);
        assert_eq!(
            resolve_reasoning_effort(&e, "deepseek-v4-flash", Some("off")),
            Some("none".to_string())
        );
    }

    #[test]
    fn resolve_reasoning_effort_off_clamp_maps_to_none_for_deepseek() {
        // The clamp arm must encode "off" the same way for DeepSeek-family
        // models: clamping into an allow-list that lands on "off" yields
        // "none", never a literal "off" request value.
        let mut e = ep("a", &["deepseek-v4-flash"], true);
        e.models[0].reasoning_efforts = vec!["off".into()];
        assert_eq!(
            resolve_reasoning_effort(&e, "deepseek-v4-flash", Some("max")),
            Some("none".to_string())
        );
    }

    #[test]
    fn resolve_model_provider_unknown_endpoint_errors() {
        let config = test_config(vec![ep("known", &["m"], true)], None);
        let err = expect_err(resolve_model_provider(
            &config,
            "ghost",
            "m",
            None,
            &trace(),
        ));
        assert!(
            err.message.contains("endpoint 'ghost' not found"),
            "{}",
            err.message
        );
    }

    #[test]
    fn resolve_model_provider_rejects_unlisted_model() {
        // The allowlist keeps callers from pointing the provider at an
        // arbitrary model id.
        let config = test_config(vec![ep("openai", &["gpt-4o"], true)], None);
        let err = expect_err(resolve_model_provider(
            &config,
            "openai",
            "gpt-5-turbo",
            None,
            &trace(),
        ));
        assert!(
            err.message
                .contains("model 'gpt-5-turbo' is not configured at endpoint 'openai'"),
            "{}",
            err.message
        );
    }

    #[test]
    fn resolve_model_provider_accepts_implicit_default_model() {
        // The global default model is allowed even when it doesn't appear in
        // the endpoint's models list (post-config-edit running model).
        let config = test_config(vec![ep("openai", &["gpt-4o"], true)], Some("special"));
        let Ok(resolved) = resolve_model_provider(&config, "openai", "special", None, &trace())
        else {
            panic!("implicit default model must be allowed");
        };
        assert_eq!(resolved.provider.model(), "special");
    }

    #[test]
    fn resolve_model_provider_builds_client_with_model_and_effort() {
        let config = test_config(vec![ep("openai", &["gpt-4o", "o3"], true)], None);
        let Ok(resolved) = resolve_model_provider(&config, "openai", "o3", Some("low"), &trace())
        else {
            panic!("listed model must resolve");
        };
        assert_eq!(resolved.provider.model(), "o3");
        assert_eq!(resolved.effective_effort.as_deref(), Some("low"));
        assert_eq!(resolved.kind_label, "OpenAI");
    }

    /// A provider whose model is configurable — swap tests assert the model
    /// landed. `complete` errors (never called in these tests).
    struct NamedProvider {
        model: String,
    }

    #[async_trait]
    impl LlmClient for NamedProvider {
        fn capabilities(&self) -> &Capabilities {
            use std::sync::OnceLock;
            static CAPS: OnceLock<Capabilities> = OnceLock::new();
            CAPS.get_or_init(Capabilities::openai)
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            &self.model
        }
        async fn complete(
            &self,
            _: &[mnemo::provider::Message],
            _: &[ToolSchema],
            _: Option<mnemo::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            Err(mnemo::error::Error::Provider(
                "named provider never completes".into(),
            ))
        }
    }

    /// Build a minimal factory over a temp dir (no memory, no vision) — the
    /// same shape as the factory tests in the lib crate.
    fn make_factory(dir: &std::path::Path) -> AgentLoopFactory {
        let provider: Arc<dyn LlmClient> = Arc::new(NamedProvider {
            model: "initial".into(),
        });
        let sandbox = Arc::new(Sandbox::new(dir).unwrap());
        let gpath = dir.join("global.md");
        let ppath = dir.join("project.md");
        std::fs::write(&gpath, "").unwrap();
        std::fs::write(&ppath, "").unwrap();
        let source = ConstitutionSource::new(&gpath, &ppath).unwrap();
        AgentLoopFactory::new(
            provider,
            source,
            None,
            sandbox,
            dir.to_path_buf(),
            None,
            Arc::new(std::sync::RwLock::new(SafetyMode::Autonomous)),
            ContextManager::new(128_000, 0.5),
            dir.join("plans"),
            None,
        )
    }

    #[tokio::test]
    async fn swap_provider_into_loops_updates_factory_and_live_loops() {
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(make_factory(dir.path()));
        let loop1 = factory.build_with_id(1);
        let loop2 = factory.build_with_id(2);
        let map: AgentLoopMap = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        map.lock().await.insert(1, loop1);
        map.lock().await.insert(2, loop2);

        let new_provider: Arc<dyn LlmClient> = Arc::new(NamedProvider {
            model: "swapped".into(),
        });
        let ids =
            swap_provider_into_loops(&factory, &map, new_provider, ContextManager::new(1000, 0.5), None)
                .await;

        // Every swapped live loop is reported (and now runs the new model).
        assert_eq!(ids.len(), 2);
        {
            let m = map.lock().await;
            assert!(
                m.values().all(|l| l.provider().model() == "swapped"),
                "every live loop must run the swapped model"
            );
        }
        // Agents built after the swap get it too (factory-level swap).
        let fresh = factory.build();
        assert_eq!(fresh.provider().model(), "swapped");
    }

    #[tokio::test]
    async fn swap_provider_into_loop_swaps_only_the_target_agent() {
        // Regression (backlog #83, 2026-08-22): the GUI model picker is
        // per-agent — switching the active agent's model must NOT touch the
        // factory (new agents keep the configured default) or any other live
        // agent's loop. Pre-fix, the picker routed through the global
        // swap_provider_into_loops, so every agent's model changed.
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(make_factory(dir.path()));
        let loop1 = factory.build_with_id(1);
        let loop2 = factory.build_with_id(2);
        let map: AgentLoopMap = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        map.lock().await.insert(1, loop1);
        map.lock().await.insert(2, loop2);

        // Seed a per-context override on the target so the cleared-state
        // assertion is meaningful (the swap must clear resolved_model so
        // list_agents reports the new provider's model immediately).
        {
            let m = map.lock().await;
            m.get(&1)
                .unwrap()
                .set_resolved_model(Some("override".into()));
        }

        let new_provider: Arc<dyn LlmClient> = Arc::new(NamedProvider {
            model: "swapped".into(),
        });
        let swapped =
            swap_provider_into_loop(&map, 1, new_provider, ContextManager::new(1000, 0.5), None).await;
        // swap_provider_into_loop returns SwapOutcome (not bool) since the
        // deferred-summarization path landed; these two assertions were not
        // updated with it, which broke the test build.
        assert!(
            matches!(swapped, SwapOutcome::Swapped),
            "the target agent is live, so the swap succeeds"
        );

        {
            let m = map.lock().await;
            assert_eq!(
                m.get(&1).unwrap().provider().model(),
                "swapped",
                "only the target agent runs the new model"
            );
            assert_eq!(
                m.get(&1).unwrap().resolved_model(),
                None,
                "the target's stale override is cleared so list_agents reports the new model"
            );
            assert_eq!(
                m.get(&2).unwrap().provider().model(),
                "initial",
                "other agents keep their models (models are agent-specific)"
            );
        }
        // The factory is untouched: agents spawned later keep the configured
        // default, not the switched model.
        let fresh = factory.build();
        assert_eq!(fresh.provider().model(), "initial");
    }

    #[tokio::test]
    async fn swap_provider_into_loop_unknown_agent_returns_false() {
        // The per-agent path must fail cleanly (no emit, no partial swap)
        // when the agent id is not a live loop — e.g. an agent that exited
        // between the frontend reading the tab list and the picker call.
        let dir = tempfile::tempdir().unwrap();
        let factory = Arc::new(make_factory(dir.path()));
        let map: AgentLoopMap = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        map.lock().await.insert(1, factory.build_with_id(1));

        let new_provider: Arc<dyn LlmClient> = Arc::new(NamedProvider {
            model: "swapped".into(),
        });
        let swapped =
            swap_provider_into_loop(&map, 999, new_provider, ContextManager::new(1000, 0.5), None).await;
        assert!(
            matches!(swapped, SwapOutcome::NotFound),
            "an unknown agent id must report failure"
        );
        // The live loop is untouched.
        assert_eq!(
            map.lock().await.get(&1).unwrap().provider().model(),
            "initial"
        );
    }
}
