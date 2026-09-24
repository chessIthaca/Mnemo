// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Build an LLM client from an endpoint + model.
//!
//! Centralizes the api-key fallback chain + endpoint-kind dispatch that were
//! previously duplicated across several call sites (the main provider in
//! `main.rs`, the vision provider in `main.rs`, `set_model` in
//! `ipc/commands.rs`/`config_io.rs`, the Settings re-sync, memory
//! maintenance). Keeping the credential-resolution chain and the
//! OpenAI-vs-Anthropic dispatch in one place prevents drift (e.g. a new env
//! var added to one copy and forgotten in the others, or an Anthropic
//! endpoint accidentally built as an OpenAI-compatible client).

use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::agent::context::ContextManager;
use crate::config::{Config, Endpoint, EndpointKind, ModelRef};
use crate::memory::classifier::{Classifier, ClassifierStatus, LayaClassifier};
#[cfg(feature = "embeddings")]
use crate::memory::embedder::BundledEmbedder;
use crate::memory::embedder::{Embedder, EmbedderStatus, HashEmbedder};
use crate::provider::anthropic::{AnthropicClient, AnthropicClientConfig};
use crate::provider::openai::{OpenAiClient, OpenAiClientConfig};
use crate::provider::trace::LlmRequestLog;
use crate::provider::vision::{ImageDescriber, VisionClient};
use crate::provider::{LlmClient, ProviderKind};

/// Resolve the API key for an endpoint: the stored key first, then the
/// kind-appropriate environment variable, falling back to a placeholder
/// ("dummy") when nothing is configured (local endpoints often need no key,
/// but the client requires a non-empty value).
///
/// Key preference by kind:
/// - Anthropic: stored → `ANTHROPIC_API_KEY` → `ANTHROPIC_AUTH_TOKEN` → dummy.
/// - OpenAI/Local: stored → `OPENAI_API_KEY` → `ANTHROPIC_AUTH_TOKEN` → dummy
///   (`ANTHROPIC_AUTH_TOKEN` is the historical generic fallback shared by all
///   kinds; the Anthropic kind prefers its own conventional env var first).
fn resolve_api_key(config: &Config, endpoint_name: &str, kind: EndpointKind) -> String {
    let stored = config.key_for(endpoint_name).map(|s| s.to_string());
    match kind {
        EndpointKind::Anthropic => stored
            .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
            .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok())
            .unwrap_or_else(|| "dummy".to_string()),
        EndpointKind::OpenAI | EndpointKind::Local => stored
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok())
            .unwrap_or_else(|| "dummy".to_string()),
    }
}

/// Map an endpoint kind to a provider kind.
///
/// Used by the OpenAI-compatible construction path (`openai_client_config`),
/// which the Anthropic kind never goes through — that kind is dispatched to
/// [`build_anthropic_client`] by [`build_client`]. The mapping stays total
/// only so the exhaustive match compiles; the Anthropic entry is unreachable
/// from the OpenAI path in practice.
pub fn provider_kind(kind: EndpointKind) -> ProviderKind {
    match kind {
        EndpointKind::OpenAI => ProviderKind::OpenAI,
        EndpointKind::Local => ProviderKind::Local,
        EndpointKind::Anthropic => ProviderKind::Anthropic,
    }
}

/// Build an `OpenAiClientConfig` for the given endpoint + model, resolving the
/// api key via the shared fallback chain. `multimodal` overrides the
/// endpoint's flag (the vision client is always multimodal regardless of the
/// endpoint's declared value); `reasoning_effort` is passed through (use
/// `endpoint.effective_reasoning_effort_for(Some(model))` for the default —
/// the per-model override wins over the endpoint's value). The model's
/// per-model caps (`max_context` / `max_output_tokens`) override the
/// endpoint-level values, and the endpoint/model `reasoning_effort_off_wire`
/// config rides along as the off-encoding override (None = the built-in
/// provider policy applies in the builder).
///
/// NOTE: only meaningful for OpenAI/Local kinds — an Anthropic endpoint must
/// go through [`anthropic_client_config`] / [`build_client`] (OpenAiClient
/// cannot speak the Messages API).
pub fn openai_client_config(
    config: &Config,
    endpoint: &Endpoint,
    model: &str,
    multimodal: bool,
    reasoning_effort: Option<String>,
) -> OpenAiClientConfig {
    OpenAiClientConfig {
        base_url: endpoint.base_url.clone(),
        api_key: resolve_api_key(config, &endpoint.name, endpoint.kind),
        model: model.to_string(),
        kind: provider_kind(endpoint.kind),
        provider: endpoint.name.clone(),
        max_context: endpoint.max_context_for(model),
        max_output_tokens: endpoint.max_output_tokens_for(model),
        multimodal,
        strict_schema: endpoint.supports_strict_schema,
        reasoning_effort,
        reasoning_effort_off_wire: endpoint.reasoning_effort_off_wire_for(model),
        use_responses_api: false,
        temperature: endpoint.temperature_for(model),
        top_p: endpoint.top_p_for(model),
        stop: endpoint.stop_for(model),
        stop_token_ids: endpoint.stop_token_ids_for(model),
        stop_boundary_strings: endpoint.stop_boundary_strings_for(model),
        extra_body: endpoint.extra_body_json_for(model),
    }
}

/// Build an `AnthropicClientConfig` for the given endpoint + model, resolving
/// the api key via the shared fallback chain (the Anthropic kind prefers
/// `ANTHROPIC_API_KEY` over `OPENAI_API_KEY`). `multimodal` overrides the
/// endpoint's flag. The model's per-model caps override the endpoint-level
/// values.
pub fn anthropic_client_config(
    config: &Config,
    endpoint: &Endpoint,
    model: &str,
    multimodal: bool,
) -> AnthropicClientConfig {
    AnthropicClientConfig {
        base_url: endpoint.base_url.clone(),
        api_key: resolve_api_key(config, &endpoint.name, endpoint.kind),
        model: model.to_string(),
        provider: endpoint.name.clone(),
        max_context: endpoint.max_context_for(model),
        max_output_tokens: endpoint.max_output_tokens_for(model),
        multimodal,
        workspace_id: endpoint.workspace_id.clone(),
    }
}

/// Build an [`LlmClient`] for the given endpoint + model, dispatching on the
/// endpoint kind: OpenAI/Local kinds build the OpenAI-compatible client,
/// Anthropic builds the native Messages API client. This is the single
/// construction path for the main provider, the `set_model` runtime swap, and
/// every re-sync, so the dispatch can never drift.
///
/// `trace` wires request/response capture into the shared [`LlmRequestLog`]
/// (pass `None` to disable — the vision/embedder paths do).
pub fn build_client(
    config: &Config,
    endpoint: &Endpoint,
    model: &str,
    multimodal: bool,
    reasoning_effort: Option<String>,
    trace: Option<Arc<LlmRequestLog>>,
) -> Arc<dyn LlmClient> {
    match endpoint.kind {
        EndpointKind::OpenAI | EndpointKind::Local => {
            build_openai_client(config, endpoint, model, multimodal, reasoning_effort, trace)
        }
        EndpointKind::Anthropic => Arc::new(AnthropicClient::new_with_trace(
            anthropic_client_config(config, endpoint, model, multimodal),
            trace,
        )),
    }
}

/// Build an OpenAI-compatible [`LlmClient`] for the given endpoint + model.
///
/// Uses the endpoint's declared `multimodal` flag and default reasoning effort.
/// This is the OpenAI/Local branch of the shared construction path — prefer
/// [`build_client`] (which dispatches by endpoint kind) unless the caller
/// specifically needs the OpenAI-compatible client (the vision/embedder
/// paths). `trace` wires request/response capture into the shared
/// [`LlmRequestLog`] (pass `None` to disable).
pub fn build_openai_client(
    config: &Config,
    endpoint: &Endpoint,
    model: &str,
    multimodal: bool,
    reasoning_effort: Option<String>,
    trace: Option<Arc<LlmRequestLog>>,
) -> Arc<dyn LlmClient> {
    Arc::new(OpenAiClient::new_with_trace(
        openai_client_config(config, endpoint, model, multimodal, reasoning_effort),
        trace,
    ))
}

/// Build an [`LlmClient`] for a per-context model override ([`ModelRef`]).
///
/// Looks up the referenced endpoint in `config` (returning `None` when it
/// doesn't exist — a dangling reference is dropped so the caller falls back to
/// the default model), then builds a client via the shared
/// [`openai_client_config`] path. The model's effective multimodal flag
/// ([`Endpoint::multimodal_for`] — per-model override, else the endpoint
/// flag) + reasoning effort are used, mirroring the main provider build in
/// `main.rs` so a per-context model behaves like the default one. The
/// effort resolves the per-context `ModelRef::reasoning_effort` override
/// first (normalized through the same gate / allow-list clamp / "off" wire
/// encoding as configured values), else the model's own default chain
/// ([`Endpoint::effective_reasoning_effort_for`]).
///
/// Used by the agent loop at turn time to build a throwaway provider for a
/// resolved per-context override. `trace` wires capture into the shared
/// [`LlmRequestLog`] (pass `None` to disable).
pub fn build_provider_for(
    config: &Config,
    model: &ModelRef,
    trace: Option<Arc<LlmRequestLog>>,
) -> Option<Arc<dyn LlmClient>> {
    let endpoint = config.endpoint(&model.endpoint)?;
    Some(build_client(
        config,
        endpoint,
        &model.model,
        endpoint.multimodal_for(&model.model),
        resolve_effort(endpoint, model),
        trace,
    ))
}

/// The reasoning effort for a per-context model build: the context's
/// `ModelRef::reasoning_effort` override when set (normalized through the
/// same gate / allow-list clamp / "off" wire encoding as configured
/// values), else the model's own default chain.
fn resolve_effort(endpoint: &Endpoint, model: &ModelRef) -> Option<String> {
    match model.reasoning_effort.as_deref() {
        Some(e) => endpoint.normalize_reasoning_effort_for(&model.model, e),
        None => endpoint.effective_reasoning_effort_for(Some(&model.model)),
    }
}

/// The DISPLAY-space counterpart of [`resolve_effort`]: the same resolution
/// (the context's `ModelRef::reasoning_effort` override when set, else the
/// model's own default chain) but display-normalized — `"off"` stays
/// `"off"` (no off-wire encoding). This is what the status bar shows: the
/// effective effort of the model serving the current context, in UI
/// vocabulary (`"off" | "low" | "medium" | "high" | "max"`).
pub fn resolve_display_effort(endpoint: &Endpoint, model: &ModelRef) -> String {
    match model.reasoning_effort.as_deref() {
        Some(e) => endpoint.display_normalize_reasoning_effort_for(&model.model, e),
        None => endpoint.display_reasoning_effort_for(Some(&model.model)),
    }
}

/// Build a [`ContextManager`] sized to the given model's context window, using
/// the provided fill rate. Mirrors the factory's
/// [`context_manager_for`](crate::agent::factory::AgentLoopFactory::context_manager_for)
/// logic so a per-context model summarizes at the right threshold.
///
/// Returns `None` when the referenced endpoint doesn't exist (the caller falls
/// back to the default context manager).
pub fn context_manager_for_model(
    config: &Config,
    model: &ModelRef,
    fill_rate: f64,
) -> Option<ContextManager> {
    // Context sizing only — no capture needed (the real turn provider, built
    // by the resolver with the shared log, does the recording).
    let provider = build_provider_for(config, model, None)?;
    let max_context = provider.capabilities().max_context;
    Some(
        ContextManager::new(max_context, fill_rate)
            .with_proxy_cache_ceiling(config.general.context.proxy_cache_ceiling_tokens),
    )
}

/// Build the vision image-to-text client from config, if configured and the
/// named endpoint exists. Returns `None` when `[general.vision_model]` is
/// unset or its endpoint is missing. Shared by startup and Settings rewire.
///
/// Vision is OpenAI-compatible only — the Anthropic Messages API has no
/// image-to-text endpoint this client can speak. A vision_model pointed at an
/// Anthropic-kind endpoint logs a clear warning and returns `None` (the
/// image-to-text fallback stays disabled) rather than silently building a
/// client that would fail at call time.
pub fn build_vision_client(config: &Config) -> Option<Arc<dyn ImageDescriber>> {
    let vm = config.general.general.vision_model.as_ref()?;
    let ep = config.endpoint(&vm.endpoint)?;
    if ep.kind == EndpointKind::Anthropic {
        eprintln!(
            "warning: vision_model endpoint '{}' is kind 'anthropic' — vision is \
             OpenAI-compatible only; image-to-text fallback disabled",
            vm.endpoint
        );
        return None;
    }
    let client: Arc<dyn ImageDescriber> = Arc::new(VisionClient::new(openai_client_config(
        config, ep, &vm.model, true, None,
    )));
    Some(client)
}

/// Build the memory embedder from config.
///
/// When `[general.bundled_embedding_model]` is set (a model id like
/// `"all-MiniLM-L6-v2"`), returns a `BundledEmbedder` (the `embeddings`
/// feature) — an in-process
/// `fastembed` (ONNX Runtime) model that runs entirely on the user's machine
/// (no Ollama, no cloud, no API key). The model downloads on first use and
/// caches under `cache_dir`. Inference is failure-protected: a load or
/// inference failure returns a zero vector + flips the shared status to
/// `Fallback`, so recall degrades to keyword + tier + strength rather than
/// dying.
///
/// The sentinel value `"hash"`
/// ([`EMBEDDING_MODEL_SENTINEL_HASH`](crate::config::EMBEDDING_MODEL_SENTINEL_HASH))
/// is the explicit keyword-only opt-out: it returns the
/// [`HashEmbedder`] directly with status `Ready` (no download attempt).
///
/// `status` is a shared `Arc<RwLock<EmbedderStatus>>` — the same handle the
/// IPC layer exposes to the UI, so status transitions surface live.
///
/// When `bundled_embedding_model` fails to load, falls back to the
/// deterministic, in-process [`HashEmbedder`] — offline, dependency-free,
/// keyword-overlap only — so memory always works (with the shared status set
/// to `Failed` so the user sees the config issue).
pub fn build_embedder(
    config: &Config,
    cache_dir: &Path,
    status: Arc<RwLock<EmbedderStatus>>,
) -> Arc<dyn Embedder> {
    if let Some(model_id) = &config.general.general.bundled_embedding_model {
        if model_id.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH) {
            // Explicit keyword-only opt-out — same as the pre-default era:
            // hash embedder, no download, status Ready.
            *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
            return Arc::new(HashEmbedder::new());
        }
        return load_bundled_or_fallback(model_id, cache_dir, &status);
    }
    // Hash mode — always ready (no external service).
    *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
    Arc::new(HashEmbedder::new())
}

/// Build the optional Laya classifier from the config — `None` unless Laya is
/// enabled *and* a non-blank endpoint is configured.
///
/// This is the hard requirement's gate (backlog bb54bdcc): with Laya disabled
/// (the default, including an absent `[general.laya]` section) or enabled
/// without an endpoint, no HTTP client is built, no connection is ever opened,
/// and the app behaves exactly as before — `status` is set to `Disabled`.
/// When enabled with an endpoint, returns a [`LayaClassifier`] bound to that
/// `laya-serve` instance and sets `status` to `Ready` (a failed call flips it
/// to `Failed`).
///
/// `status` is the shared `Arc<RwLock<ClassifierStatus>>` the IPC layer
/// exposes to the UI, so transitions surface live.
///
/// Never fails: enabling Laya without an endpoint — and, in the extreme, an
/// unbuildable HTTP client — degrades to `None` with a logged warning, so
/// startup is never blocked.
pub fn build_classifier(
    config: &Config,
    status: Arc<RwLock<ClassifierStatus>>,
) -> Option<Arc<dyn Classifier>> {
    let laya = &config.general.general.laya;
    let endpoint = if laya.enabled {
        laya.endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
    } else {
        None
    };
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => {
            if laya.enabled {
                eprintln!(
                    "warning: the Laya classifier is enabled but no endpoint URL is \
                     configured; it stays disabled"
                );
            }
            *status.write().expect("classifier status lock poisoned") = ClassifierStatus::Disabled;
            return None;
        }
    };
    match LayaClassifier::new(endpoint, Arc::clone(&status)) {
        Ok(classifier) => {
            *status.write().expect("classifier status lock poisoned") = ClassifierStatus::Ready;
            Some(Arc::new(classifier))
        }
        Err(e) => {
            eprintln!(
                "warning: failed to build the Laya classifier HTTP client for '{endpoint}': {e}; \
                 it stays unavailable"
            );
            *status.write().expect("classifier status lock poisoned") = ClassifierStatus::Failed;
            None
        }
    }
}

/// Load the configured bundled model, with the hash fallback on load error
/// (the `embeddings`-feature branch of [`build_embedder`]'s configured-model
/// path, kept as a separate fn so each feature world has a total body).
#[cfg(feature = "embeddings")]
fn load_bundled_or_fallback(
    model_id: &str,
    cache_dir: &Path,
    status: &Arc<RwLock<EmbedderStatus>>,
) -> Arc<dyn Embedder> {
    match BundledEmbedder::new(model_id, cache_dir, status.clone()) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            eprintln!(
                "warning: failed to load bundled embedding model '{model_id}': {e}; \
                 falling back to built-in hash embedder"
            );
            // Mark Failed (so the UI surfaces the config issue) but still
            // return a working HashEmbedder so recall degrades gracefully
            // rather than dying. Do NOT overwrite to Ready — the user
            // should see the failure.
            *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Failed;
            Arc::new(HashEmbedder::new())
        }
    }
}

/// Light-build branch (`embeddings` feature off): the bundled-model machinery
/// was not compiled in, so a configured model surfaces as `Failed` with a
/// clear reason and recall degrades to the built-in hash embedder.
#[cfg(not(feature = "embeddings"))]
fn load_bundled_or_fallback(
    model_id: &str,
    _cache_dir: &Path,
    status: &Arc<RwLock<EmbedderStatus>>,
) -> Arc<dyn Embedder> {
    eprintln!(
        "warning: bundled embedding model '{model_id}' configured but this build \
         was compiled without the `embeddings` feature; falling back to the \
         built-in hash embedder"
    );
    *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Failed;
    Arc::new(HashEmbedder::new())
}

/// How startup should build the memory embedder for the configured bundled
/// model. Extracted as a pure decision so the startup path (main.rs) stays
/// thin and the branching is unit-testable without touching the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedderStartupPlan {
    /// The `"hash"` sentinel — explicit keyword-only opt-out. Hash embedder,
    /// no download, ever.
    HashOptOut,
    /// No bundled model configured. Hash embedder via [`build_embedder`]'s
    /// fallback path.
    HashDefault,
    /// A bundled model is configured AND already installed in the cache dir.
    /// Build the [`HashEmbedder`] immediately so startup never blocks on the
    /// ~110 MB ONNX load on the main thread (hang diagnostics, AppHangB1
    /// 2026-08-20), then load the real embedder in the background and swap it
    /// into the live memory store (mirror of the download path's swap).
    /// Carries the model id.
    UseHashThenLocalLoad(String),
    /// A bundled model is configured but NOT installed yet (first run on this
    /// machine). Build the [`HashEmbedder`] immediately so startup never
    /// blocks on a ~100 MB download, then download the model in the
    /// background; on success the task swaps the real embedder into the live
    /// memory store. Carries the model id to download.
    UseHashThenDownload(String),
}

/// Decide how startup builds the embedder: the `"hash"` opt-out and an unset
/// value take the hash paths; an installed bundled model starts as hash with
/// a pending background local load (see
/// [`EmbedderStartupPlan::UseHashThenLocalLoad`]); a missing one starts as
/// hash with a pending background download (see
/// [`EmbedderStartupPlan::UseHashThenDownload`]). The sentinel comparison is
/// case-insensitive, mirroring [`build_embedder`].
pub fn embedder_startup_plan(bundled_model: Option<&str>, cache_dir: &Path) -> EmbedderStartupPlan {
    let Some(model_id) = bundled_model else {
        return EmbedderStartupPlan::HashDefault;
    };
    if model_id.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH) {
        return EmbedderStartupPlan::HashOptOut;
    }
    installed_model_plan(cache_dir, model_id)
}

/// The installed-vs-download branch of [`embedder_startup_plan`] (kept as a
/// separate fn so each feature world has a total body).
///
/// Feature on: an already-installed model starts as hash with a pending
/// background local load; a missing one starts as hash with a pending
/// background download.
#[cfg(feature = "embeddings")]
fn installed_model_plan(cache_dir: &Path, model_id: &str) -> EmbedderStartupPlan {
    if crate::memory::embedder::is_model_installed(cache_dir, model_id) {
        EmbedderStartupPlan::UseHashThenLocalLoad(model_id.to_string())
    } else {
        EmbedderStartupPlan::UseHashThenDownload(model_id.to_string())
    }
}

/// Light-build branch (`embeddings` feature off): no bundled-model machinery
/// was compiled in, so a configured model degrades to the plain hash path —
/// nothing can load or download. [`build_embedder`]'s light branch is what
/// surfaces the config mismatch (status `Failed`).
#[cfg(not(feature = "embeddings"))]
fn installed_model_plan(_cache_dir: &Path, _model_id: &str) -> EmbedderStartupPlan {
    EmbedderStartupPlan::HashDefault
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EndpointKind;

    fn endpoint(kind: EndpointKind) -> Endpoint {
        Endpoint {
            kind,
            base_url: "http://localhost/v1/".into(),
            ..Endpoint::test_default()
        }
    }

    #[test]
    fn config_resolves_key_and_maps_kind() {
        let config = Config::default();
        let ep = endpoint(EndpointKind::OpenAI);
        let cfg = openai_client_config(&config, &ep, "gpt-4o", false, Some("high".into()));
        assert_eq!(cfg.model, "gpt-4o");
        assert_eq!(cfg.base_url, "http://localhost/v1/");
        assert!(matches!(cfg.kind, ProviderKind::OpenAI));
        // No key stored for "test" and (in CI) no env var → falls back to "dummy".
        assert!(!cfg.api_key.is_empty());
        assert_eq!(cfg.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn config_maps_local_kind() {
        let config = Config::default();
        let ep = endpoint(EndpointKind::Local);
        let cfg = openai_client_config(&config, &ep, "m", false, None);
        assert!(matches!(cfg.kind, ProviderKind::Local));
    }

    #[test]
    fn anthropic_kind_builds_anthropic_client_via_dispatch() {
        // The dispatch gate: an Anthropic endpoint must NEVER be built as an
        // OpenAI-compatible client (OpenAiClient can't speak the Messages
        // API). build_client routes it to AnthropicClient instead.
        let config = Config::default();
        let ep = endpoint(EndpointKind::Anthropic);
        let client = build_client(&config, &ep, "claude-sonnet-4-5", false, None, None);
        assert_eq!(client.kind(), ProviderKind::Anthropic);
        assert_eq!(client.model(), "claude-sonnet-4-5");
        assert!(client.capabilities().supports_tool_choice);
        assert!(!client.capabilities().supports_strict_schema);
    }

    #[test]
    fn strict_schema_override_flows_into_client_caps() {
        // The per-endpoint override reaches the built client's capabilities:
        // an OpenAI-kind endpoint behind a litellm/vertex proxy opts out of
        // strict schemas; unset keeps the kind default (enforced).
        let config = Config::default();
        let mut ep = endpoint(EndpointKind::OpenAI);
        ep.supports_strict_schema = Some(false);
        let client = build_client(&config, &ep, "gpt-4o", false, None, None);
        assert!(!client.capabilities().supports_strict_schema);

        let ep = endpoint(EndpointKind::OpenAI);
        let client = build_client(&config, &ep, "gpt-4o", false, None, None);
        assert!(client.capabilities().supports_strict_schema);
    }

    #[test]
    fn anthropic_config_resolves_key_via_anthropic_env_preference() {
        // The Anthropic kind prefers ANTHROPIC_API_KEY over the generic
        // OPENAI_API_KEY / ANTHROPIC_AUTH_TOKEN fallbacks. Test with a
        // stored-key config to keep the env-var surface untouched (stored
        // wins regardless of kind).
        let mut keys = crate::config::KeyStore::default();
        keys.insert("test".to_string(), "stored-secret".to_string());
        let config = Config {
            keys,
            ..Config::default()
        };
        let ep = endpoint(EndpointKind::Anthropic);
        let cfg = anthropic_client_config(&config, &ep, "claude-sonnet-4-5", false);
        assert_eq!(
            cfg.api_key, "stored-secret",
            "stored key wins for Anthropic too"
        );
        assert_eq!(cfg.model, "claude-sonnet-4-5");
        assert_eq!(cfg.base_url, "http://localhost/v1/");
        assert_eq!(cfg.max_context, None);
        assert_eq!(cfg.max_output_tokens, None);
        assert!(!cfg.multimodal);
    }

    /// Env-var tests mutate the process environment, so they must run one at
    /// a time — `cargo test` runs tests in parallel by default.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_api_key_anthropic_kind_prefers_anthropic_api_key_env() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // The Anthropic kind must prefer ANTHROPIC_API_KEY over the generic
        // ANTHROPIC_AUTH_TOKEN fallback (and never fall to OPENAI_API_KEY
        // first) — the branch the stored-key test cannot reach.
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-anthropic-env");
        std::env::set_var("OPENAI_API_KEY", "sk-openai-env");
        let config = Config::default(); // no stored key
        let key = resolve_api_key(&config, "test", EndpointKind::Anthropic);
        assert_eq!(key, "sk-anthropic-env");

        // And the OpenAI kind must still prefer OPENAI_API_KEY.
        let key = resolve_api_key(&config, "test", EndpointKind::OpenAI);
        assert_eq!(key, "sk-openai-env");
    }

    #[test]
    fn vision_client_rejects_anthropic_kind_endpoint() {
        // Vision is OpenAI-compatible only; an Anthropic-kind vision endpoint
        // must come back None (with a warning) instead of building a client
        // that would fail at call time.
        let config = Config {
            general: crate::config::GeneralConfig {
                general: crate::config::GeneralSection {
                    vision_model: Some(crate::config::VisionModel {
                        endpoint: "claude".into(),
                        model: "claude-sonnet-4-5".into(),
                    }),
                    ..crate::config::GeneralSection::default()
                },
                ..crate::config::GeneralConfig::default()
            },
            endpoints: vec![Endpoint {
                name: "claude".into(),
                kind: EndpointKind::Anthropic,
                base_url: "https://api.anthropic.com/v1/".into(),
                models: vec!["claude-sonnet-4-5".into()],
                ..Endpoint::test_default()
            }],
            ..Config::default()
        };
        assert!(
            build_vision_client(&config).is_none(),
            "Anthropic-kind vision endpoint must not build a VisionClient"
        );
    }

    #[test]
    fn multimodal_override_is_respected() {
        // The vision client forces multimodal=true even when the endpoint
        // declares false; the main provider passes the endpoint's own flag.
        let config = Config::default();
        let ep = endpoint(EndpointKind::OpenAI);
        let cfg = openai_client_config(&config, &ep, "m", true, None);
        assert!(cfg.multimodal);
    }

    #[tokio::test]
    async fn build_embedder_returns_hash_for_hash_sentinel() {
        use crate::memory::embedder::EMBEDDING_DIM;
        // The "hash" sentinel = explicit keyword-only opt-out → HashEmbedder
        // (deterministic), no download attempt, status Ready.
        let config = Config {
            general: crate::config::GeneralConfig {
                general: crate::config::GeneralSection {
                    bundled_embedding_model: Some(
                        crate::config::EMBEDDING_MODEL_SENTINEL_HASH.into(),
                    ),
                    ..crate::config::GeneralSection::default()
                },
                ..crate::config::GeneralConfig::default()
            },
            ..Config::default()
        };
        let status = Arc::new(RwLock::new(EmbedderStatus::Checking));
        let cache = std::env::temp_dir().join("mh-embed-test-hash");
        let e = build_embedder(&config, &cache, status.clone());
        // Same text → same vector (deterministic).
        let v1 = e.embed("hello world").await;
        let v2 = e.embed("hello world").await;
        assert_eq!(v1, v2);
        // Correct fixed dimension.
        assert_eq!(v1.len(), EMBEDDING_DIM);
        assert_eq!(*status.read().unwrap(), EmbedderStatus::Ready);
    }

    #[cfg(feature = "embeddings")]
    #[tokio::test]
    async fn build_embedder_falls_back_to_hash_for_unknown_bundled_model() {
        // A bundled_embedding_model set to an unknown id → BundledEmbedder::new
        // errors → falls back to HashEmbedder + status Failed (not a panic).
        use crate::config::{GeneralConfig, GeneralSection};
        use crate::memory::embedder::EMBEDDING_DIM;
        let config = Config {
            general: GeneralConfig {
                general: GeneralSection {
                    bundled_embedding_model: Some("not-a-real-model".into()),
                    ..GeneralSection::default()
                },
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let status = Arc::new(RwLock::new(EmbedderStatus::Checking));
        let cache = std::env::temp_dir().join("mh-embed-test-unknown");
        let e = build_embedder(&config, &cache, status.clone());
        // Falls back to hash (deterministic, correct dim).
        let v = e.embed("hello world").await;
        assert_eq!(v.len(), EMBEDDING_DIM);
        assert_eq!(
            *status.read().unwrap(),
            EmbedderStatus::Failed,
            "unknown model → status Failed"
        );
    }

    #[test]
    fn build_provider_for_resolves_known_endpoint() {
        use crate::config::{Endpoint, GeneralConfig, ModelRef};
        let config = Config {
            general: GeneralConfig::default(),
            endpoints: vec![Endpoint {
                name: "openai".into(),
                base_url: "https://api.openai.com/v1/".into(),
                models: vec!["gpt-4o".into()],
                ..Endpoint::test_default()
            }],
            pricing: vec![],
            keys: Default::default(),
            mcp: Vec::new(),
            projects: Default::default(),
        };
        let provider = build_provider_for(
            &config,
            &ModelRef {
                endpoint: "openai".into(),
                model: "gpt-4o".into(),
                reasoning_effort: None,
            },
            None,
        )
        .expect("known endpoint should resolve");
        assert_eq!(provider.model(), "gpt-4o");
    }

    #[test]
    fn build_provider_for_resolves_per_model_multimodal() {
        // Mixed endpoint (backlog a634835c): one Ollama-style endpoint
        // hosting a text-only GLM and a vision-capable GLM flash — the
        // per-model flag overrides the endpoint default at client
        // construction, so the vision model receives image blocks while the
        // text-only model strips them (and falls back to the configured
        // vision model instead).
        use crate::config::{Endpoint, GeneralConfig, ModelRef, ModelSpec};
        let config = Config {
            general: GeneralConfig::default(),
            endpoints: vec![Endpoint {
                name: "ollama".into(),
                kind: EndpointKind::Local,
                base_url: "http://localhost:11434/v1/".into(),
                models: vec![
                    ModelSpec::from("glm"),
                    ModelSpec {
                        id: "glm-flash".into(),
                        multimodal: Some(true),
                        ..ModelSpec::test_default()
                    },
                ],
                ..Endpoint::test_default()
            }],
            pricing: vec![],
            keys: Default::default(),
            mcp: Vec::new(),
            projects: Default::default(),
        };
        let vision = build_provider_for(
            &config,
            &ModelRef {
                endpoint: "ollama".into(),
                model: "glm-flash".into(),
                reasoning_effort: None,
            },
            None,
        )
        .expect("known endpoint should resolve");
        assert!(
            vision.capabilities().multimodal,
            "per-model multimodal=true overrides the endpoint default"
        );
        let text_only = build_provider_for(
            &config,
            &ModelRef {
                endpoint: "ollama".into(),
                model: "glm".into(),
                reasoning_effort: None,
            },
            None,
        )
        .expect("known endpoint should resolve");
        assert!(
            !text_only.capabilities().multimodal,
            "models without a flag inherit the endpoint default (false)"
        );
    }

    #[test]
    fn resolve_effort_prefers_the_context_override() {
        // A per-context ModelRef.reasoning_effort wins over the model's own
        // default chain; unset inherits it; a context value outside the
        // model's list clamps like a configured one.
        use crate::config::ModelSpec;
        let endpoint = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_effort: Some("high".into()),
                reasoning_efforts: vec!["high".into(), "medium".into(), "low".into()],
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        let context = |effort: Option<&str>| ModelRef {
            endpoint: "gateway".into(),
            model: "glm".into(),
            reasoning_effort: effort.map(str::to_string),
        };
        // The context override wins.
        assert_eq!(
            resolve_effort(&endpoint, &context(Some("low"))).as_deref(),
            Some("low")
        );
        // A context value outside the model's list clamps to the first entry.
        assert_eq!(
            resolve_effort(&endpoint, &context(Some("max"))).as_deref(),
            Some("high")
        );
        // Unset inherits the model's own default.
        assert_eq!(
            resolve_effort(&endpoint, &context(None)).as_deref(),
            Some("high")
        );
    }

    #[test]
    fn build_provider_for_returns_none_for_dangling_endpoint() {
        use crate::config::ModelRef;
        let config = Config::default(); // no endpoints
        let provider = build_provider_for(
            &config,
            &ModelRef {
                endpoint: "ghost".into(),
                model: "x".into(),
                reasoning_effort: None,
            },
            None,
        );
        assert!(provider.is_none(), "dangling reference should drop to None");
    }

    #[test]
    fn context_manager_for_model_sizes_to_provider_window() {
        use crate::config::{Endpoint, GeneralConfig, ModelRef};
        let config = Config {
            general: GeneralConfig::default(),
            endpoints: vec![Endpoint {
                name: "openai".into(),
                base_url: "https://api.openai.com/v1/".into(),
                models: vec!["gpt-4o".into()],
                max_context: Some(200_000),
                ..Endpoint::test_default()
            }],
            pricing: vec![],
            keys: Default::default(),
            mcp: Vec::new(),
            projects: Default::default(),
        };
        let cm = context_manager_for_model(
            &config,
            &ModelRef {
                endpoint: "openai".into(),
                model: "gpt-4o".into(),
                reasoning_effort: None,
            },
            0.5,
        )
        .expect("known endpoint should resolve");
        // The provider's resolved max_context (OpenAI default when the endpoint
        // sets Some(200_000)) drives the threshold at 50%.
        assert!(cm.max_tokens() > 0);
        assert_eq!(cm.summarize_at(), cm.max_tokens() / 2);
    }

    /// Lay out a fake installed HF cache for `all-MiniLM-L6-v2` (snapshots dir
    /// with an ONNX model file inside), as `is_model_installed` expects.
    #[cfg(feature = "embeddings")]
    fn fake_installed_cache(root: &std::path::Path) -> std::path::PathBuf {
        let cache = root.join("models");
        let rev = cache
            .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
            .join("snapshots")
            .join("deadbeef");
        std::fs::create_dir_all(&rev).unwrap();
        std::fs::write(rev.join("model.onnx"), b"onnx bytes").unwrap();
        cache
    }

    #[test]
    fn embedder_startup_plan_unset_is_hash_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            embedder_startup_plan(None, dir.path()),
            EmbedderStartupPlan::HashDefault
        );
    }

    #[test]
    fn embedder_startup_plan_hash_sentinel_is_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        // Case-insensitive, mirroring build_embedder's sentinel check.
        assert_eq!(
            embedder_startup_plan(
                Some(crate::config::EMBEDDING_MODEL_SENTINEL_HASH),
                dir.path()
            ),
            EmbedderStartupPlan::HashOptOut
        );
        assert_eq!(
            embedder_startup_plan(Some("HASH"), dir.path()),
            EmbedderStartupPlan::HashOptOut
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn embedder_startup_plan_installed_model_defers_local_load() {
        let dir = tempfile::tempdir().unwrap();
        let cache = fake_installed_cache(dir.path());
        assert_eq!(
            embedder_startup_plan(Some("all-MiniLM-L6-v2"), &cache),
            EmbedderStartupPlan::UseHashThenLocalLoad("all-MiniLM-L6-v2".into())
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn embedder_startup_plan_missing_model_defers_download() {
        let dir = tempfile::tempdir().unwrap();
        // Empty cache dir — the model is configured but not downloaded yet.
        assert_eq!(
            embedder_startup_plan(Some("all-MiniLM-L6-v2"), dir.path()),
            EmbedderStartupPlan::UseHashThenDownload("all-MiniLM-L6-v2".into())
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn embedder_startup_plan_unknown_model_also_defers() {
        // An unknown id never resolves to "installed" — it defers to the
        // background task, which fails gracefully (status Failed) instead of
        // blocking startup.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            embedder_startup_plan(Some("not-a-real-model"), dir.path()),
            EmbedderStartupPlan::UseHashThenDownload("not-a-real-model".into())
        );
    }

    #[cfg(not(feature = "embeddings"))]
    #[tokio::test]
    async fn build_embedder_without_embeddings_feature_reports_failed_and_runs_hash() {
        // Light build: a configured bundled model can't load (the machinery
        // wasn't compiled in) → status Failed with a working hash embedder so
        // recall degrades to keyword instead of dying.
        use crate::config::{GeneralConfig, GeneralSection};
        use crate::memory::embedder::EMBEDDING_DIM;
        let config = Config {
            general: GeneralConfig {
                general: GeneralSection {
                    bundled_embedding_model: Some("all-MiniLM-L6-v2".into()),
                    ..GeneralSection::default()
                },
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let status = Arc::new(RwLock::new(EmbedderStatus::Checking));
        let cache = std::env::temp_dir().join("mh-embed-test-light");
        let e = build_embedder(&config, &cache, status.clone());
        let v = e.embed("hello world").await;
        assert_eq!(v.len(), EMBEDDING_DIM);
        assert_eq!(
            *status.read().unwrap(),
            EmbedderStatus::Failed,
            "configured model without the feature → status Failed"
        );
    }

    #[test]
    fn openai_client_config_propagates_sampling_and_stop_params() {
        use crate::config::{Endpoint, ModelSpec};

        let mut ep_extra = toml::Table::new();
        ep_extra.insert("ep_key".into(), toml::Value::String("ep_val".into()));
        ep_extra.insert("shared_key".into(), toml::Value::Integer(1));

        let mut model_extra = toml::Table::new();
        model_extra.insert("model_key".into(), toml::Value::String("model_val".into()));
        model_extra.insert("shared_key".into(), toml::Value::Integer(2));

        let ep = Endpoint {
            name: "test-vllm".into(),
            kind: EndpointKind::OpenAI,
            base_url: "http://localhost:8000/v1/".into(),
            models: vec![
                ModelSpec {
                    id: "glm-5.3-flash".into(),
                    temperature: Some(0.2),
                    top_p: Some(0.8),
                    stop: vec!["<|endoftext|>".into(), "<|observation|>".into()],
                    stop_token_ids: vec![151329, 151336],
                    stop_boundary_strings: vec![
                        "\u{3c}|model-a|\u{3e}".into(),
                        "\u{3c}|model-b|\u{3e}".into(),
                    ],
                    extra_body: Some(model_extra),
                    ..Default::default()
                },
                ModelSpec {
                    id: "default-model".into(),
                    ..Default::default()
                },
            ],
            temperature: Some(0.7),
            top_p: Some(0.9),
            stop: vec!["<|endoftext|>".into()],
            stop_token_ids: vec![151329],
            stop_boundary_strings: vec!["\u{3c}|endpoint|\u{3e}".into()],
            extra_body: Some(ep_extra),
            ..Default::default()
        };

        let config = Config {
            endpoints: vec![ep.clone()],
            ..Config::default()
        };

        // For overridden model
        let cfg1 = openai_client_config(&config, &ep, "glm-5.3-flash", false, None);
        assert_eq!(cfg1.temperature, Some(0.2));
        assert_eq!(cfg1.top_p, Some(0.8));
        assert_eq!(
            cfg1.stop,
            vec!["<|endoftext|>", "<|observation|>"]
        );
        assert_eq!(cfg1.stop_token_ids, vec![151329, 151336]);
        assert_eq!(
            cfg1.stop_boundary_strings,
            vec!["\u{3c}|model-a|\u{3e}", "\u{3c}|model-b|\u{3e}"]
        );
        let extra1 = cfg1.extra_body.unwrap();
        assert_eq!(extra1.get("ep_key").unwrap(), "ep_val");
        assert_eq!(extra1.get("model_key").unwrap(), "model_val");
        assert_eq!(extra1.get("shared_key").unwrap(), 2);

        // For default model
        let cfg2 = openai_client_config(&config, &ep, "default-model", false, None);
        assert_eq!(cfg2.temperature, Some(0.7));
        assert_eq!(cfg2.top_p, Some(0.9));
        assert_eq!(cfg2.stop, vec!["<|endoftext|>"]);
        assert_eq!(cfg2.stop_token_ids, vec![151329]);
        assert_eq!(cfg2.stop_boundary_strings, vec!["\u{3c}|endpoint|\u{3e}"]);
        let extra2 = cfg2.extra_body.unwrap();
        assert_eq!(extra2.get("ep_key").unwrap(), "ep_val");
        assert_eq!(extra2.get("shared_key").unwrap(), 1);
        assert!(extra2.get("model_key").is_none());
    }

    #[cfg(not(feature = "embeddings"))]
    #[test]
    fn embedder_startup_plan_without_feature_degrades_to_hash_default() {
        // Light build: nothing can load or download, so a configured model
        // degrades to the plain hash path.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            embedder_startup_plan(Some("all-MiniLM-L6-v2"), dir.path()),
            EmbedderStartupPlan::HashDefault
        );
    }

    #[test]
    fn build_classifier_disabled_returns_none_and_leaves_status_disabled() {
        // The hard requirement's regression pin: Laya disabled (the default —
        // an absent [general.laya] section deserializes to exactly this) ⇒ no
        // classifier, status Disabled, and no client built: zero behavior
        // change.
        let config = Config::default();
        // Start from a non-Disabled status so the builder must overwrite it.
        let status = Arc::new(RwLock::new(ClassifierStatus::Ready));
        assert!(build_classifier(&config, Arc::clone(&status)).is_none());
        assert_eq!(*status.read().unwrap(), ClassifierStatus::Disabled);
    }

    #[test]
    fn build_classifier_enabled_with_endpoint_returns_the_laya_backend() {
        use crate::config::{GeneralConfig, GeneralSection, LayaConfig};
        let config = Config {
            general: GeneralConfig {
                general: GeneralSection {
                    laya: LayaConfig {
                        enabled: true,
                        endpoint: Some("http://127.0.0.1:8000".into()),
                    },
                    ..GeneralSection::default()
                },
                ..GeneralConfig::default()
            },
            ..Config::default()
        };
        let status = Arc::new(RwLock::new(ClassifierStatus::Disabled));
        let classifier = build_classifier(&config, Arc::clone(&status))
            .expect("enabled + endpoint must build the Laya backend");
        // Configured with an endpoint ⇒ calls are attempted: Ready until the
        // first failed call.
        assert_eq!(*status.read().unwrap(), ClassifierStatus::Ready);
        drop(classifier);
    }

    #[test]
    fn build_classifier_enabled_without_endpoint_returns_none() {
        use crate::config::{GeneralConfig, GeneralSection, LayaConfig};
        // Enabled but unconfigured — a null or blank endpoint: a logged
        // warning, no client, status Disabled. Never fatal.
        for endpoint in [None, Some("   ".to_string())] {
            let config = Config {
                general: GeneralConfig {
                    general: GeneralSection {
                        laya: LayaConfig {
                            enabled: true,
                            endpoint: endpoint.clone(),
                        },
                        ..GeneralSection::default()
                    },
                    ..GeneralConfig::default()
                },
                ..Config::default()
            };
            let status = Arc::new(RwLock::new(ClassifierStatus::Ready));
            assert!(
                build_classifier(&config, Arc::clone(&status)).is_none(),
                "endpoint {endpoint:?} is unconfigured"
            );
            assert_eq!(*status.read().unwrap(), ClassifierStatus::Disabled);
        }
    }
}
