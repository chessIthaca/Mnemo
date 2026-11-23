// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Build an LLM client from an endpoint + model.
//!
//! Centralizes the api-key fallback chain + endpoint-kind dispatch that were
//! previously duplicated across several call sites (the main provider and
//! vision provider builds, `set_model`, the Settings re-sync, memory
//! maintenance). Keeping the credential-resolution chain and the
//! OpenAI-vs-Anthropic dispatch in one place prevents drift (e.g. a new env
//! var added to one copy and forgotten in the others, or an Anthropic
//! endpoint accidentally built as an OpenAI-compatible client).

use std::path::Path;
use std::sync::{Arc, RwLock};

use crate::agent::context::ContextManager;
use crate::config::{Config, Endpoint, EndpointKind, ModelRef};
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
    run_dir: Option<std::path::PathBuf>,
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
        run_dir,
        max_concurrent: endpoint.max_concurrent,
    }
}

/// Build an `AnthropicClientConfig` for the given endpoint + model, resolving
/// the api key via the shared fallback chain (the Anthropic kind prefers
/// `ANTHROPIC_API_KEY` over `OPENAI_API_KEY`). `multimodal` overrides the
/// endpoint's flag. The model's per-model caps override the endpoint-level
/// values.
///
/// `reasoning_effort` is the value resolved by
/// [`Endpoint::effective_reasoning_effort_for`](crate::config::Endpoint::effective_reasoning_effort_for)
/// (the model's override wins, the model's allow-list clamps; `off` normally
/// resolves to `None` for this kind — a hand-configured
/// `reasoning_effort_off_wire` can instead yield its wire value, which the
/// client's effort table then drops, so `off` still sends nothing). It lands
/// on the Messages API's `output_config.effort` — the client never sends a
/// `thinking` field.
pub fn anthropic_client_config(
    config: &Config,
    endpoint: &Endpoint,
    model: &str,
    multimodal: bool,
    reasoning_effort: Option<String>,
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
        reasoning_effort,
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
    run_dir: Option<std::path::PathBuf>,
) -> Arc<dyn LlmClient> {
    match endpoint.kind {
        EndpointKind::OpenAI | EndpointKind::Local => build_openai_client(
            config,
            endpoint,
            model,
            multimodal,
            reasoning_effort,
            trace,
            run_dir,
        ),
        // Anthropic endpoints have no machine-wide limit (plan d3336a8a
        // step 4): the `max_concurrent` field is OpenAI/Local-only.
        EndpointKind::Anthropic => Arc::new(AnthropicClient::new_with_trace(
            anthropic_client_config(config, endpoint, model, multimodal, reasoning_effort),
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
    run_dir: Option<std::path::PathBuf>,
) -> Arc<dyn LlmClient> {
    Arc::new(OpenAiClient::new_with_trace(
        openai_client_config(
            config,
            endpoint,
            model,
            multimodal,
            reasoning_effort,
            run_dir,
        ),
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
/// flag) + reasoning effort are used, mirroring the startup provider build
/// so a per-context model behaves like the default one. The
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
    run_dir: Option<std::path::PathBuf>,
) -> Option<Arc<dyn LlmClient>> {
    let endpoint = config.endpoint(&model.endpoint)?;
    Some(build_client(
        config,
        endpoint,
        &model.model,
        endpoint.multimodal_for(&model.model),
        resolve_effort(endpoint, model),
        trace,
        run_dir,
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
/// vocabulary (`"off" | "minimal" | "low" | "medium" | "high" | "max" |
/// "xhigh"`).
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
    let provider = build_provider_for(config, model, None, None)?;
    let max_context = provider.capabilities().max_context;
    Some(ContextManager::new(max_context, fill_rate))
}

/// Why the configured `[general.vision_model]` cannot serve the image tools — or
/// only a warning about a model that may still work.
///
/// The single rule shared by [`build_vision_client`] and the Settings surfaces
/// (Mnemo2's Models page), so the image tools' availability and the warning a
/// user sees can never disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionIssue {
    /// The named endpoint is not in `endpoints.toml`.
    MissingEndpoint,
    /// The endpoint is Anthropic-kind; the vision client is OpenAI-only.
    AnthropicEndpoint,
    /// The endpoint lists models and the vision model id is not among them —
    /// a warning only, since an unlisted id can still work.
    UnlistedModel,
}

impl VisionIssue {
    /// The user-facing one-liner for this issue (Settings → Models, under the
    /// Vision field), naming the offending endpoint and model.
    pub fn message(&self, endpoint: &str, model: &str) -> String {
        match self {
            Self::MissingEndpoint => format!(
                "Vision endpoint '{endpoint}' is not in endpoints.toml, so workers and agents have no image tools."
            ),
            Self::AnthropicEndpoint => format!(
                "Vision endpoint '{endpoint}' is an Anthropic endpoint, which the vision client does not support."
            ),
            Self::UnlistedModel => format!(
                "Vision model '{model}' is not listed for endpoint '{endpoint}'; it may still work."
            ),
        }
    }
}

/// Classify `[general.vision_model]` against the loaded endpoints: `None` when
/// it is unset (the multimodal main model serves vision) or resolves cleanly.
fn vision_issue(config: &Config) -> Option<VisionIssue> {
    let vm = config.general.general.vision_model.as_ref()?;
    let ep = match config.endpoint(&vm.endpoint) {
        Some(ep) => ep,
        None => return Some(VisionIssue::MissingEndpoint),
    };
    if ep.kind == EndpointKind::Anthropic {
        return Some(VisionIssue::AnthropicEndpoint);
    }
    // An endpoint that lists no models makes no claim to contradict, and an
    // unlisted id can still be served — a warning, never a disabled client.
    if !ep.models.is_empty() && !ep.has_model(&vm.model) {
        return Some(VisionIssue::UnlistedModel);
    }
    None
}

/// The Settings-facing reason `[general.vision_model]` yields no image tools, or
/// a warning that it may not — `None` when vision is unset or resolves cleanly.
///
/// Mnemo2's Models page shows it under the Vision field, so an unresolvable
/// vision model is visible where the user sets it instead of only on stderr.
pub fn vision_warning(config: &Config) -> Option<String> {
    let vm = config.general.general.vision_model.as_ref()?;
    vision_issue(config).map(|issue| issue.message(&vm.endpoint, &vm.model))
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
    // A clean config MUST build — `vision_issue` returns `None` when there is
    // no problem, so never `?` it here: an early return on `None` would starve
    // every worker of the image tools for a perfectly valid vision model. Only
    // a missing or Anthropic-kind endpoint disables the client; an unlisted
    // model is a warning (the endpoint may still serve it).
    if let Some(issue) = vision_issue(config) {
        match issue {
            VisionIssue::UnlistedModel => {}
            VisionIssue::MissingEndpoint => return None,
            VisionIssue::AnthropicEndpoint => {
                eprintln!(
                    "warning: vision_model endpoint '{}' is kind 'anthropic' — vision is \
                     OpenAI-compatible only; image-to-text fallback disabled",
                    vm.endpoint
                );
                return None;
            }
        }
    }
    let ep = config.endpoint(&vm.endpoint)?;
    // Vision sends stay outside the machine-wide endpoint limits (plan
    // d3336a8a step 4): this builder has no config dir to key the slots on,
    // and a description request is a one-shot side call — the chat/completion
    // sends are the ones that can pile up across instances.
    let client: Arc<dyn ImageDescriber> = Arc::new(VisionClient::new(openai_client_config(
        config, ep, &vm.model, true, None, None,
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
///
/// `cache_dir` is the model cache directory under the config dir
/// (`<config>/models` — every caller passes `embedder_cache_dir`). With
/// the `embeddings` feature a machine-wide shared models host (plan
/// d3336a8a step 2) is preferred over loading the model into this process:
/// the share is derived from `cache_dir` ONLY while its last component is
/// `models` — any other cache directory has no share and keeps today's
/// in-process behaviour.
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
        return configured_model_embedder(model_id, cache_dir, &status);
    }
    // Hash mode — always ready (no external service).
    *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
    Arc::new(HashEmbedder::new())
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

/// The configured-model branch of [`build_embedder`] with the `embeddings`
/// feature on: remote-first — a machine-wide shared models host (plan
/// d3336a8a step 2) is preferred over loading the model into this process.
/// No host, or an unusable one, falls back to today's synchronous
/// in-process load (never a worse state than before).
///
/// The acquire retries with the step-17 backoff while a host is serveable
/// (finding rewireNoRetryAndDoc): every caller of this branch runs on a
/// background or blocking thread — the settings rewire's off-path build
/// (rewire.rs), the headless reembed's `spawn_blocking`, never an apply
/// critical path — and a single attempt could pin an in-process model
/// after a save during a cold start. Not serveable → the single fast
/// attempt of today.
#[cfg(feature = "embeddings")]
fn configured_model_embedder(
    model_id: &str,
    cache_dir: &Path,
    status: &Arc<RwLock<EmbedderStatus>>,
) -> Arc<dyn Embedder> {
    // Remote-first: the retrying acquire itself derives the config dir
    // from cache_dir (no share when the last component is not "models")
    // and falls back to None on any failure.
    if let Some(remote) = acquire_remote_embedder_with_retry(model_id, cache_dir, status.clone()) {
        return remote;
    }
    load_bundled_or_fallback(model_id, cache_dir, status)
}

/// The configured-model branch of [`build_embedder`] on a light build:
/// nothing can be shared (no bundled-model machinery compiled in), so
/// today's in-process shape stands.
#[cfg(not(feature = "embeddings"))]
fn configured_model_embedder(
    model_id: &str,
    cache_dir: &Path,
    status: &Arc<RwLock<EmbedderStatus>>,
) -> Arc<dyn Embedder> {
    load_bundled_or_fallback(model_id, cache_dir, status)
}

/// Attach-ONLY to a healthy, published, machine-wide models host serving
/// `model_id` — the startup leg (brain's installed-model arm). Never spawns
/// and never waits for a spawn: the health probe is bounded by
/// [`crate::host::models_host::STARTUP_PROBE`] (at most 1 s), so the startup
/// path stays as cheap as today's. `Some` is a `RemoteEmbedder` with the
/// shared status flipped to `Ready`; `None` leaves the caller's status
/// untouched (the caller falls back to today's hash + deferred-load shape).
///
/// `cache_dir` must be the models cache under the config dir (last
/// component `models`) — any other cache directory has no share and returns
/// `None` (see [`crate::host::models_host::shared_config_dir`]).
#[cfg(feature = "embeddings")]
pub fn build_remote_embedder(
    model_id: &str,
    cache_dir: &Path,
    status: Arc<RwLock<EmbedderStatus>>,
) -> Option<Arc<dyn Embedder>> {
    let config_dir = crate::host::models_host::shared_config_dir(cache_dir)?;
    let client = crate::host::models_host::shared_client(&config_dir);
    client.attach(model_id, crate::host::models_host::STARTUP_PROBE)?;
    *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
    Some(Arc::new(crate::memory::embedder::RemoteEmbedder::new(
        client, model_id, status,
    )))
}

/// Attach-or-spawn a machine-wide models host and return a `RemoteEmbedder`
/// for `model_id` — the background paths only (the deferred startup load,
/// the settings rewire's off-path embedder leg, reembed, the code
/// embedder): spawn budgets are fine there, never on the startup path.
/// The post-download swap is NOT one of them — embed_download.rs builds
/// its `BundledEmbedder` in-process once a download finishes (review
/// finding postDownloadDoc). A host THIS instance
/// spawns serves `model_id` plus the code-search model (the exact-set
/// rule: an instance attaches only when its requested model is in the
/// descriptor's list). `None` on any failure (the caller falls back
/// in-process).
#[cfg(feature = "embeddings")]
pub fn acquire_remote_embedder(
    model_id: &str,
    cache_dir: &Path,
    status: Arc<RwLock<EmbedderStatus>>,
) -> Option<Arc<dyn Embedder>> {
    let config_dir = crate::host::models_host::shared_config_dir(cache_dir)?;
    let client = crate::host::models_host::shared_client(&config_dir);
    let mut serve = vec![model_id.to_string()];
    if model_id != crate::memory::embedder::CODE_EMBEDDING_MODEL {
        serve.push(crate::memory::embedder::CODE_EMBEDDING_MODEL.to_string());
    }
    let serve: Vec<&str> = serve.iter().map(String::as_str).collect();
    client.acquire(model_id, &serve)?;
    *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
    Some(Arc::new(crate::memory::embedder::RemoteEmbedder::new(
        client, model_id, status,
    )))
}

/// The code-search embedder (plan d3336a8a step 9's half here):
/// [`crate::memory::embedder::CODE_EMBEDDING_MODEL`] remote-first — the
/// shared host serves it alongside the memory model — with today's
/// in-process load as the fallback. Wired only into the code-graph's
/// background symbol-embed pass: the code-graph vectors and the
/// `find_code` ranking own its use, nothing in memory.
///
/// `memory_model` is the configured bundled memory model
/// (`general.bundled_embedding_model`): when THIS side spawns the host,
/// the serve list includes it too ([`code_serve_models`]), so the
/// memory side later attaches to the same host instead of spawning a
/// second one — one host per machine, whichever side starts it. The
/// list only matters on the spawn leg: the exact-set attach rule takes
/// any host already serving the code model regardless, and a later
/// Settings change of the memory model never needs to update a spawned
/// host.
///
/// The in-process fallback drives its OWN fresh status `Arc` — the memory
/// embedder's shared status is untouched.
#[cfg(feature = "embeddings")]
pub fn build_code_embedder(cache_dir: &Path, memory_model: Option<&str>) -> Arc<dyn Embedder> {
    let status = Arc::new(RwLock::new(EmbedderStatus::Ready));
    if let Some(config_dir) = crate::host::models_host::shared_config_dir(cache_dir) {
        let client = crate::host::models_host::shared_client(&config_dir);
        let serve = code_serve_models(memory_model);
        // d3336a8a step 17 A: while a host is serveable the code embedder
        // is remote-ONLY. A failed acquire is retried with the doubling
        // backoff (the observed failure is the spawn's health wait racing
        // a slow cold start — the host it spawned is usually healthy
        // seconds later); when the retries run out the NOT-READY signal
        // comes back instead of an in-process ONNX session: the slot
        // parks its retry gate, this pass round is skipped, find_code
        // falls back to text search. A window that fell back in-process
        // mid-pass held ~3.5 GB of weights and ONNX arena the host
        // already serves machine-wide.
        if retry_while_serveable(cache_dir, |fresh| {
            if fresh {
                client
                    .acquire(crate::memory::embedder::CODE_EMBEDDING_MODEL, &serve)
            } else {
                // One spawn per round (finding spawnChurnPerRound): the
                // retries attach only, to what the fresh leg published.
                client.attach(
                    crate::memory::embedder::CODE_EMBEDDING_MODEL,
                    crate::host::models_host::ATTACH_PROBE,
                )
            }
        })
        .is_some()
        {
            return Arc::new(crate::memory::embedder::RemoteEmbedder::new(
                client,
                crate::memory::embedder::CODE_EMBEDDING_MODEL,
                status,
            ));
        }
        if host_is_serveable(cache_dir) {
            *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Failed;
            return Arc::new(crate::memory::embedder::HashEmbedder::new());
        }
    }
    // The truly-offline case (step 17 A): no host binary resolves, no live
    // descriptor is published, or no share is possible at all — today's
    // in-process path, exactly as before.
    #[cfg(test)]
    CODE_INPROCESS_LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    load_bundled_or_fallback(crate::memory::embedder::CODE_EMBEDDING_MODEL, cache_dir, &status)
}

/// Whether a shared models host is SERVEABLE for `cache_dir` (plan
/// d3336a8a step 17 A): the `mnemo-models` binary resolves (the env
/// override, the sibling of this executable, or the config dir's `bin`
/// install), or a service descriptor is published whose pid is still
/// alive. While a host is serveable the CODE model must never load its
/// ONNX session in-process — the in-process load is the offline path's
/// fallback only, for when nothing can serve at all.
#[cfg(feature = "embeddings")]
pub fn host_is_serveable(cache_dir: &Path) -> bool {
    let Some(config_dir) = crate::host::models_host::shared_config_dir(cache_dir) else {
        return false;
    };
    if crate::host::models_host::host_binary_lookup(&config_dir).is_some() {
        return true;
    }
    let dir = crate::host::shared_service::service_dir(
        &config_dir,
        crate::host::models_host::SERVICE_NAME,
    );
    crate::host::shared_service::read_service(&dir)
        .is_some_and(|info| crate::process_tree::pid_alive(info.pid))
}

/// The acquire-retry cadence (step 17 A): the first attempt runs
/// immediately, then up to [`ACQUIRE_RETRY_STEPS`] more attempts with a
/// doubling backoff from [`ACQUIRE_RETRY_BASE_MS`] (2 s → 2, 4, 8, 16 s ≈
/// 30 s in total) — but only while the host stays serveable. Not
/// serveable → the single first attempt (today's behaviour, and the
/// offline case's fast in-process path).
#[cfg(feature = "embeddings")]
static ACQUIRE_RETRY_BASE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(2_000);

/// How many backoff retries follow the first acquire attempt.
#[cfg(feature = "embeddings")]
const ACQUIRE_RETRY_STEPS: u32 = 4;

/// Tests only: count the code model's in-process constructions — the
/// step-17 regression asserts the count never moves while a host is
/// serveable.
#[cfg(all(test, feature = "embeddings"))]
static CODE_INPROCESS_LOADS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// The shared acquire retry loop (step 17 A): attempt now, then up to
/// [`ACQUIRE_RETRY_STEPS`] more attempts with the doubling backoff — but
/// only while the host stays serveable, re-checked before every sleep.
/// Not serveable → the single first attempt returns immediately.
///
/// `attempt(fresh)` receives `true` for the FIRST attempt and `false`
/// for every retry: a round allows exactly ONE spawn (finding
/// spawnChurnPerRound, plan d3336a8a step 17) — the `fresh` leg is
/// attach-or-spawn, the retries attach ONLY to whatever the first leg
/// left published, so a host that can never become healthy is not
/// re-spawned by every retry.
#[cfg(feature = "embeddings")]
fn retry_while_serveable<T>(
    cache_dir: &Path,
    mut attempt: impl FnMut(bool) -> Option<T>,
) -> Option<T> {
    if let Some(found) = attempt(true) {
        return Some(found);
    }
    for step in 0..ACQUIRE_RETRY_STEPS {
        if !host_is_serveable(cache_dir) {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(
            ACQUIRE_RETRY_BASE_MS.load(std::sync::atomic::Ordering::Relaxed) * (1_u64 << step),
        ));
        if let Some(found) = attempt(false) {
            return Some(found);
        }
    }
    None
}

/// [`acquire_remote_embedder`] with the step-17 retry: while a host is
/// serveable, a failed acquire is retried with the doubling backoff (~30 s
/// total) — the spawn's health wait can race a slow cold start, and the
/// host it spawned is usually healthy seconds later. Not serveable → the
/// single attempt of today (the offline case falls back fast). The memory
/// model keeps its in-process fallback after `None` (recall must keep
/// working); the code model never loads in-process while serveable (see
/// [`build_code_embedder`]).
#[cfg(feature = "embeddings")]
pub fn acquire_remote_embedder_with_retry(
    model_id: &str,
    cache_dir: &Path,
    status: Arc<RwLock<EmbedderStatus>>,
) -> Option<Arc<dyn Embedder>> {
    retry_while_serveable(cache_dir, |fresh| {
        if fresh {
            acquire_remote_embedder(model_id, cache_dir, status.clone())
        } else {
            // One spawn per round (finding spawnChurnPerRound): the retry
            // legs attach only, to whatever the fresh leg left published.
            build_remote_embedder(model_id, cache_dir, status.clone())
        }
    })
}

/// The serve list for a host the code side spawns (plan d3336a8a step 9):
/// the code model plus the configured memory model, so ONE host serves
/// both embedders whichever side spawns it. The memory model joins only
/// when it is a real bundled model — `None`, the `"hash"` opt-out, a
/// model unknown to the catalog (`model_for_id` refuses it, the same
/// validation `acquire` applies to its whole list), or the code model
/// itself all degrade to the code model alone.
#[cfg(feature = "embeddings")]
fn code_serve_models(memory_model: Option<&str>) -> Vec<&str> {
    let mut serve = vec![crate::memory::embedder::CODE_EMBEDDING_MODEL];
    if let Some(model) = memory_model {
        let real = !model.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH)
            && model != crate::memory::embedder::CODE_EMBEDDING_MODEL
            && crate::memory::embedder::model_for_id(model).is_ok();
        if real {
            serve.push(model);
        }
    }
    serve
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
        let cfg = openai_client_config(&config, &ep, "gpt-4o", false, Some("high".into()), None);
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
        let cfg = openai_client_config(&config, &ep, "m", false, None, None);
        assert!(matches!(cfg.kind, ProviderKind::Local));
    }

    #[test]
    fn anthropic_kind_builds_anthropic_client_via_dispatch() {
        // The dispatch gate: an Anthropic endpoint must NEVER be built as an
        // OpenAI-compatible client (OpenAiClient can't speak the Messages
        // API). build_client routes it to AnthropicClient instead.
        let config = Config::default();
        let ep = endpoint(EndpointKind::Anthropic);
        let client = build_client(&config, &ep, "claude-sonnet-4-5", false, None, None, None);
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
        let client = build_client(&config, &ep, "gpt-4o", false, None, None, None);
        assert!(!client.capabilities().supports_strict_schema);

        let ep = endpoint(EndpointKind::OpenAI);
        let client = build_client(&config, &ep, "gpt-4o", false, None, None, None);
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
        let cfg = anthropic_client_config(&config, &ep, "claude-sonnet-4-5", false, None);
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

    /// A config that resolves cleanly MUST build a client. Regression: an early
    /// `?` on the issue classifier returned `None` for a *healthy* config, so a
    /// valid vision model (Ollama Cloud / `deepseek-v4.1-flash:1017`) silently
    /// left every worker without the image tools. Fails without the fix.
    #[test]
    fn build_vision_client_builds_for_a_valid_endpoint() {
        let config = Config {
            general: crate::config::GeneralConfig {
                general: crate::config::GeneralSection {
                    vision_model: Some(crate::config::VisionModel {
                        endpoint: "ollama-cloud".into(),
                        model: "deepseek-v4.1-flash:1017".into(),
                    }),
                    ..crate::config::GeneralSection::default()
                },
                ..crate::config::GeneralConfig::default()
            },
            endpoints: vec![Endpoint {
                name: "ollama-cloud".into(),
                kind: EndpointKind::OpenAI,
                base_url: "https://ollama.com/v1/".into(),
                models: vec!["deepseek-v4.1-flash:1017".into()],
                ..Endpoint::test_default()
            }],
            ..Config::default()
        };
        assert_eq!(vision_issue(&config), None, "the fixture must resolve cleanly");
        assert!(
            build_vision_client(&config).is_some(),
            "a resolvable vision config must build a VisionClient — without it no worker has image tools"
        );
        assert_eq!(vision_warning(&config), None);
    }

    /// An endpoint that lists models but not the vision model is a WARNING, not
    /// a disable: the id can still be served, so the client must build.
    #[test]
    fn build_vision_client_builds_for_an_unlisted_model() {
        let config = Config {
            general: crate::config::GeneralConfig {
                general: crate::config::GeneralSection {
                    vision_model: Some(crate::config::VisionModel {
                        endpoint: "ep1".into(),
                        model: "deepseek-v4.1-flash:1017".into(),
                    }),
                    ..crate::config::GeneralSection::default()
                },
                ..crate::config::GeneralConfig::default()
            },
            endpoints: vec![Endpoint {
                name: "ep1".into(),
                kind: EndpointKind::OpenAI,
                base_url: "http://127.0.0.1:9/v1/".into(),
                models: vec!["plan-model".into()],
                ..Endpoint::test_default()
            }],
            ..Config::default()
        };
        assert_eq!(vision_issue(&config), Some(VisionIssue::UnlistedModel));
        assert!(
            build_vision_client(&config).is_some(),
            "an unlisted model is a warning only — the client still builds"
        );
        assert!(
            vision_warning(&config).is_some(),
            "and that warning is what the Models page shows"
        );
    }

    #[test]
    fn multimodal_override_is_respected() {
        // The vision client forces multimodal=true even when the endpoint
        // declares false; the main provider passes the endpoint's own flag.
        let config = Config::default();
        let ep = endpoint(EndpointKind::OpenAI);
        let cfg = openai_client_config(&config, &ep, "m", true, None, None);
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
            tiers: Default::default(),
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
            tiers: Default::default(),
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
            tiers: Default::default(),
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
        let cfg1 = openai_client_config(&config, &ep, "glm-5.3-flash", false, None, None);
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
        let cfg2 = openai_client_config(&config, &ep, "default-model", false, None, None);
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

    #[cfg(feature = "embeddings")]
    #[tokio::test]
    async fn build_remote_embedder_is_none_and_leaves_status_without_a_host() {
        // The startup attach-only leg: no descriptor, no spawn, no wait —
        // the caller's status is untouched and today's shape is kept.
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        let status = Arc::new(RwLock::new(EmbedderStatus::Checking));
        assert!(build_remote_embedder("all-MiniLM-L6-v2", &cache, status.clone()).is_none());
        assert_eq!(*status.read().unwrap(), EmbedderStatus::Checking);
    }

    #[cfg(feature = "embeddings")]
    #[tokio::test]
    async fn build_remote_embedder_attaches_to_a_healthy_host() {
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        crate::host::models_host::spawn_test_host(
            home.path(),
            &["all-MiniLM-L6-v2"],
            crate::host::models_host::HOST_IDLE,
        );
        let status = Arc::new(RwLock::new(EmbedderStatus::Checking));
        let embedder = build_remote_embedder("all-MiniLM-L6-v2", &cache, status.clone())
            .expect("attach to the healthy host");
        assert_eq!(embedder.model_id(), "all-MiniLM-L6-v2");
        assert_eq!(embedder.dim(), 384);
        assert_eq!(*status.read().unwrap(), EmbedderStatus::Ready);
    }

    #[cfg(feature = "embeddings")]
    #[tokio::test]
    async fn build_embedder_prefers_a_healthy_host_over_in_process() {
        // build_embedder's remote-first leg: with a healthy host published
        // under the config dir, the embedder is the remote one (status
        // Ready) and nothing is loaded or downloaded into this process.
        use crate::config::{GeneralConfig, GeneralSection};
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        crate::host::models_host::spawn_test_host(
            home.path(),
            &["all-MiniLM-L6-v2"],
            crate::host::models_host::HOST_IDLE,
        );
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
        let embedder = build_embedder(&config, &cache, status.clone());
        assert_eq!(embedder.model_id(), "all-MiniLM-L6-v2");
        assert_eq!(*status.read().unwrap(), EmbedderStatus::Ready);
        // The no-host leg keeps today's shape — that is
        // build_embedder_falls_back_to_hash_for_unknown_bundled_model's
        // ground (an unknown model can never load, so no download).
    }

    #[cfg(feature = "embeddings")]
    #[tokio::test]
    async fn build_code_embedder_prefers_a_healthy_host() {
        // The code embedder is remote-first too: a host serving the code
        // model is used, nothing is downloaded into this process. The
        // memory model in the serve list only matters on the spawn leg —
        // the exact-set attach rule takes this host as-is.
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        crate::host::models_host::spawn_test_host(
            home.path(),
            &[crate::memory::embedder::CODE_EMBEDDING_MODEL],
            crate::host::models_host::HOST_IDLE,
        );
        let embedder = build_code_embedder(&cache, Some("all-MiniLM-L6-v2"));
        assert_eq!(embedder.model_id(), crate::memory::embedder::CODE_EMBEDDING_MODEL);
        assert_eq!(embedder.dim(), 768);
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn code_serve_models_widens_only_for_a_real_memory_model() {
        // Plan d3336a8a step 9: the host the code side spawns serves both
        // models — the code model plus the configured memory model, so
        // the memory side later attaches instead of spawning a second
        // host. Only a REAL bundled memory model joins the list; the
        // opt-out, an unknown id, and the code model itself degrade to
        // the code model alone.
        let code = crate::memory::embedder::CODE_EMBEDDING_MODEL;
        assert_eq!(code_serve_models(None), vec![code]);
        assert_eq!(code_serve_models(Some("hash")), vec![code]);
        assert_eq!(code_serve_models(Some("HASH")), vec![code]);
        assert_eq!(code_serve_models(Some("not-a-bundled-model")), vec![code]);
        assert_eq!(code_serve_models(Some(code)), vec![code]);
        assert_eq!(
            code_serve_models(Some("all-MiniLM-L6-v2")),
            vec![code, "all-MiniLM-L6-v2"]
        );
    }

    /// Publish a live (this-process) descriptor that does NOT serve the
    /// code model: serveable (the pid is alive), but every acquire
    /// attempt rejects it fast — the descriptor is validated against the
    /// requested model, not probed.
    #[cfg(feature = "embeddings")]
    fn publish_live_descriptor(config_dir: &std::path::Path, port: u16, token: &str) {
        crate::host::shared_service::write_service(
            &crate::host::shared_service::service_dir(config_dir, "models"),
            &crate::host::shared_service::ServiceInfo {
                pid: std::process::id(),
                port,
                token: token.to_string(),
                version: "test".into(),
                started_ms: crate::host::shared_service::now_ms(),
                created: crate::process_tree::process_creation_id(std::process::id())
                    .unwrap_or_default(),
                models: vec!["some-other-model".into()],
            },
        )
        .expect("publish the test descriptor");
    }

    /// Step 17 A regression: while a host is serveable the code embedder
    /// NEVER constructs the in-process ONNX session. The live descriptor
    /// does not serve the code model, so every acquire attempt fails —
    /// after the (backoff-shrunk) retries the build returns the not-ready
    /// signal (the hash embedder, not the code model) and the in-process
    /// load counter never moves.
    #[cfg(feature = "embeddings")]
    #[test]
    fn build_code_embedder_skips_in_process_while_a_host_is_serveable() {
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        publish_live_descriptor(home.path(), 64000, "stub-token");
        ACQUIRE_RETRY_BASE_MS.store(0, std::sync::atomic::Ordering::Relaxed);
        let before = CODE_INPROCESS_LOADS.load(std::sync::atomic::Ordering::Relaxed);
        let embedder = build_code_embedder(&cache, Some("all-MiniLM-L6-v2"));
        assert_ne!(
            embedder.model_id(),
            crate::memory::embedder::CODE_EMBEDDING_MODEL
        );
        assert_eq!(
            CODE_INPROCESS_LOADS.load(std::sync::atomic::Ordering::Relaxed),
            before,
            "no in-process session may be constructed while a host is serveable"
        );
    }

    /// The serveable rule itself (step 17 A): a live descriptor makes a
    /// config dir serveable even without a host binary; a dead pid does
    /// not, and neither does a bare home. This is the seam that keeps the
    /// code embedder on the remote-only path exactly when a host could
    /// serve, and off it only when nothing can.
    #[cfg(feature = "embeddings")]
    #[test]
    fn host_is_serveable_follows_the_binary_or_live_descriptor_rule() {
        // A bare home: no binary (the test binary's sibling lookup finds
        // nothing, MNEMO_MODELS_EXE is unset) and no descriptor.
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        assert!(!host_is_serveable(&cache));
        // A live descriptor (this test process) — no binary needed.
        publish_live_descriptor(home.path(), 64000, "stub-token");
        assert!(host_is_serveable(&cache));
        // A descriptor whose pid is gone (no process holds u32::MAX) is
        // not serveable — the stale-descriptor case.
        crate::host::shared_service::write_service(
            &crate::host::shared_service::service_dir(home.path(), "models"),
            &crate::host::shared_service::ServiceInfo {
                pid: u32::MAX,
                port: 64000,
                token: "stub-token".into(),
                version: "test".into(),
                started_ms: crate::host::shared_service::now_ms(),
                created: crate::process_tree::process_creation_id(u32::MAX).unwrap_or_default(),
                models: vec!["some-other-model".into()],
            },
        )
        .expect("publish the dead-pid descriptor");
        assert!(
            !host_is_serveable(&cache),
            "a stale descriptor is not serveable"
        );
    }

    /// The retry cadence itself (step 17 A): a serveable host keeps the
    /// acquire retrying — the first attempt plus [`ACQUIRE_RETRY_STEPS`]
    /// more with the backoff shrunk to zero, and the FRESH leg runs
    /// exactly once (finding spawnChurnPerRound: one spawn-allowed
    /// attempt per round, the retries are attach-only); a non-serveable
    /// setup stops after the first attempt.
    #[cfg(feature = "embeddings")]
    #[test]
    fn acquire_retries_only_while_a_host_is_serveable() {
        let home = tempfile::tempdir().unwrap();
        let cache = crate::config::embedder_cache_dir(home.path());
        ACQUIRE_RETRY_BASE_MS.store(0, std::sync::atomic::Ordering::Relaxed);
        publish_live_descriptor(home.path(), 64000, "stub-token");
        let mut fresh_flags: Vec<bool> = Vec::new();
        let found: Option<()> = retry_while_serveable(&cache, |fresh| {
            fresh_flags.push(fresh);
            None
        });
        assert!(found.is_none());
        assert_eq!(
            fresh_flags,
            vec![true, false, false, false, false],
            "the spawn-allowed leg runs exactly once per round"
        );
        let bare = tempfile::tempdir().unwrap();
        let bare_cache = crate::config::embedder_cache_dir(bare.path());
        let mut bare_flags: Vec<bool> = Vec::new();
        let found: Option<()> = retry_while_serveable(&bare_cache, |fresh| {
            bare_flags.push(fresh);
            None
        });
        assert!(found.is_none());
        assert_eq!(bare_flags, vec![true]);
    }
}
