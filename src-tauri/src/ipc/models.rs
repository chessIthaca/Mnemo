// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Live model-listing Tauri commands.
//!
//! [`list_models`] and [`list_vision_models`] fetch the live model list from
//! an endpoint's `/models` endpoint — used by the Settings → Endpoints and
//! Settings → Vision tabs' model pickers so the user can click a model name
//! served by that entry instead of typing an id by hand. OpenAI/Local kinds
//! hit the OpenAI-compatible endpoint (Bearer auth); the Anthropic kind hits
//! the same path with `x-api-key` + `anthropic-version` headers (the
//! Anthropic Models API returns `{ data: [{ id, display_name }] }`).

use tauri::State;

use super::settings::endpoint_kind_wire;
use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// Resolve the credentials for a model-listing request: the explicit
/// override args first (empty strings treated as absent — the override
/// path keeps the picker in sync with unsaved edits in the dialog), then
/// the saved endpoint (`endpoints.toml` base_url + kind, `keys.toml`
/// key), then the kind-aware env fallback (mirroring the provider's
/// `resolve_api_key`), then `"dummy"` (local endpoints often need no
/// key). Returns `(base_url, api_key, kind-wire)`. Shared by
/// [`list_models`] and [`list_vision_models`] so the two pickers can
/// never drift (quality review HIGH 2).
fn resolve_endpoint_credentials(
    config: &mnemo::config::Config,
    endpoint_name: &str,
    base_url: Option<String>,
    api_key: Option<String>,
    kind: Option<String>,
) -> Result<(String, String, String), IpcError> {
    let saved = config.endpoint(endpoint_name);
    let base_url = base_url
        .filter(|u| !u.trim().is_empty())
        .or_else(|| saved.map(|e| e.base_url.clone()))
        .ok_or_else(|| {
            format!("endpoint '{endpoint_name}' has no base_url and does not match a saved endpoint")
        })?;
    let kind = kind
        .filter(|k| !k.trim().is_empty())
        .or_else(|| saved.map(|e| endpoint_kind_wire(e.kind).to_string()))
        .unwrap_or_else(|| "openai".to_string());
    // Kind-aware env fallback, mirroring the provider's resolve_api_key
    // (client_factory.rs): an Anthropic-kind endpoint prefers
    // ANTHROPIC_API_KEY; OpenAI/Local prefer OPENAI_API_KEY. Both share
    // the historical ANTHROPIC_AUTH_TOKEN fallback.
    let env_key = if kind == "anthropic" {
        std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok())
    } else {
        std::env::var("OPENAI_API_KEY")
            .ok()
            .or_else(|| std::env::var("ANTHROPIC_AUTH_TOKEN").ok())
    };
    let api_key = api_key
        .filter(|k| !k.trim().is_empty())
        .or_else(|| saved.and_then(|e| config.key_for(&e.name).map(|s| s.to_string())))
        .or_else(|| env_key)
        .unwrap_or_else(|| "dummy".to_string());
    Ok((base_url, api_key, kind))
}

/// Fetch the live model list via the library's HTTP helper selected by
/// the resolved kind wire string — `"anthropic"` uses the Anthropic
/// headers (`x-api-key` + `anthropic-version`), anything else the
/// OpenAI-compatible path. Delegates so the app crate doesn't pull in
/// reqwest directly (only the library depends on it); the error is
/// mapped to a plain human-readable string for the UI — no key is
/// leaked. Shared by [`list_models`] and [`list_vision_models`] so the
/// two pickers can never drift (quality review HIGH 2).
async fn fetch_by_kind(
    kind: &str,
    base_url: &str,
    api_key: &str,
) -> Result<Vec<mnemo::provider::models::ModelWithVision>, IpcError> {
    if kind == "anthropic" {
        mnemo::provider::models::fetch_models_anthropic(base_url, api_key)
            .await
            .map_err(IpcError::from)
    } else {
        mnemo::provider::models::fetch_models_with_vision(base_url, api_key)
            .await
            .map_err(IpcError::from)
    }
}

/// Fetch the live model list (with per-model token caps) from an endpoint's
/// `/models` endpoint — used by the Settings
/// → Endpoints tab's model picker so the user can click a model name served
/// by that entry instead of typing an id by hand, and so the endpoint card
/// can auto-fill empty Max context / Max output fields with the values the
/// provider reports (Ollama `context_length`, LM Studio
/// `max_context_length`, vLLM `max_model_len`, OpenRouter
/// `top_provider.context_length`). Providers that expose no cap fields
/// (vanilla OpenAI / z.ai) report `null` for both — discovery is a no-op
/// there. Anthropic-kind endpoints get no caps (the Anthropic `/models`
/// response exposes none) — the picker lists the served ids only.
///
/// Resolution (mirrors the provider's key fallback chain in
/// [`build_client`]):
/// - If `endpoint_name` matches a saved endpoint, that endpoint's `base_url`
///   and kind are used and its stored API key (from `keys.toml`) is resolved,
///   falling back to the kind-appropriate env vars, then `"dummy"` (local
///   endpoints often need no key).
/// - Otherwise (an unsaved/new card with no matching name) the passed
///   `base_url` + `api_key` are used directly (the `api_key` falls back to
///   the same env vars, then `"dummy"`). An empty `base_url` here is an
///   error.
/// - If both `base_url` and `api_key` are passed, they win over a matching
///   saved endpoint (so the picker reflects unsaved edits in the dialog).
///
/// `kind` is the endpoint kind as the card shows it (`"openai"`, `"local"`,
/// `"anthropic"`); when `None`, a saved endpoint's kind is used.
///
/// Returns the sorted, de-duplicated entries (`{ id, vision_capable,
/// context_length, max_output_tokens }`). On any HTTP or parse error returns
/// `Err(human-readable)` so the UI can surface it inline without leaking
/// the key.
#[tauri::command]
pub async fn list_models(
    state: State<'_, IpcState>,
    endpoint_name: String,
    base_url: Option<String>,
    api_key: Option<String>,
    kind: Option<String>,
) -> Result<Vec<mnemo::provider::models::ModelWithVision>, IpcError> {
    // Resolve base_url + api_key from the override args first, then the saved
    // endpoint, then env vars (see resolve_endpoint_credentials — shared
    // with the other model-listing command so the two pickers cannot drift).
    let (base_url, api_key, kind) = {
        let config = state.project.config.lock().await;
        resolve_endpoint_credentials(&config, &endpoint_name, base_url, api_key, kind)?
    };

    fetch_by_kind(&kind, &base_url, &api_key).await
}

/// Fetch the live model list from an endpoint's `/models`
/// endpoint, each annotated with whether the provider reports it as
/// vision-capable — used by the Settings → Vision tab's model picker so the
/// user can choose a vision-capable model served by that endpoint.
///
/// Resolution is identical to [`list_models`] (override `base_url`/`api_key`
/// → saved endpoint → env vars → `"dummy"`; `kind` `"anthropic"` uses the
/// Anthropic headers). Anthropic-kind endpoints report no modality, so every
/// model comes back `vision_capable: false` and the UI falls back to showing
/// all models with a note.
/// Returns the sorted, de-duplicated `Vec<ModelWithVision>` (`{ id,
/// vision_capable }`). `vision_capable` is `true` only when the provider
/// explicitly exposes image input modality (e.g. OpenRouter's
/// `architecture.input_modalities`); providers that don't expose modality
/// report `false` for every model, and the UI falls back to showing all models
/// with a note. On any HTTP or parse error returns `Err(human-readable)`
/// without leaking the key.
#[tauri::command]
pub async fn list_vision_models(
    state: State<'_, IpcState>,
    endpoint_name: String,
    base_url: Option<String>,
    api_key: Option<String>,
    kind: Option<String>,
) -> Result<Vec<mnemo::provider::models::ModelWithVision>, IpcError> {
    // Resolve base_url + api_key from the override args first, then the saved
    // endpoint, then env vars (see resolve_endpoint_credentials — shared
    // with the other model-listing command so the two pickers cannot drift).
    let (base_url, api_key, kind) = {
        let config = state.project.config.lock().await;
        resolve_endpoint_credentials(&config, &endpoint_name, base_url, api_key, kind)?
    };

    fetch_by_kind(&kind, &base_url, &api_key).await
}

#[cfg(test)]
mod tests {
    use super::resolve_endpoint_credentials;
    use mnemo::config::{Config, Endpoint, EndpointKind};

    /// Env-var tests mutate the process environment, so they must run one at
    /// a time — `cargo test` runs tests in parallel by default.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A saved-endpoint fixture: name "ep" with the given kind + base_url,
    /// no stored key unless the test inserts one.
    fn config_with_endpoint(kind: EndpointKind, base_url: &str) -> Config {
        let mut config = Config::default();
        config.endpoints = vec![Endpoint {
            name: "ep".into(),
            kind,
            base_url: base_url.into(),
            models: vec![],
            ..Endpoint::test_default()
        }];
        config
    }

    #[test]
    fn override_wins_over_saved() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = config_with_endpoint(EndpointKind::OpenAI, "https://saved/");
        config.keys.insert("ep".into(), "sk-saved".into());
        let (base_url, api_key, kind) = resolve_endpoint_credentials(
            &config,
            "ep",
            Some("https://override/".into()),
            Some("sk-override".into()),
            None,
        )
        .expect("override resolution must succeed");
        assert_eq!(base_url, "https://override/");
        assert_eq!(api_key, "sk-override");
        // No kind override with a saved endpoint: the saved kind's wire form.
        assert_eq!(kind, "openai");
        // An explicit kind override wins too.
        let (_, _, kind) = resolve_endpoint_credentials(
            &config,
            "ep",
            Some("https://override/".into()),
            Some("sk-override".into()),
            Some("anthropic".into()),
        )
        .expect("override resolution must succeed");
        assert_eq!(kind, "anthropic");
    }

    #[test]
    fn saved_endpoint_fallback() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = config_with_endpoint(EndpointKind::Anthropic, "https://saved/");
        config.keys.insert("ep".into(), "sk-saved".into());
        let (base_url, api_key, kind) =
            resolve_endpoint_credentials(&config, "ep", None, None, None)
                .expect("saved-endpoint resolution must succeed");
        assert_eq!(base_url, "https://saved/");
        assert_eq!(api_key, "sk-saved");
        assert_eq!(kind, "anthropic");
    }

    #[test]
    fn kind_aware_env_fallback_anthropic_vs_openai() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-anthropic-env");
        std::env::set_var("OPENAI_API_KEY", "sk-openai-env");
        // No stored key: the Anthropic-kind endpoint must prefer
        // ANTHROPIC_API_KEY, the OpenAI-kind endpoint OPENAI_API_KEY —
        // mirroring the provider's resolve_api_key chain.
        let anthropic = config_with_endpoint(EndpointKind::Anthropic, "https://a/");
        let (_, api_key, _) =
            resolve_endpoint_credentials(&anthropic, "ep", None, None, None).unwrap();
        assert_eq!(api_key, "sk-anthropic-env");
        let openai = config_with_endpoint(EndpointKind::OpenAI, "https://o/");
        let (_, api_key, _) =
            resolve_endpoint_credentials(&openai, "ep", None, None, None).unwrap();
        assert_eq!(api_key, "sk-openai-env");
    }

    #[test]
    fn empty_base_url_errors() {
        // No override and no matching saved endpoint.
        let config = Config::default();
        let err = resolve_endpoint_credentials(&config, "missing", None, None, None)
            .expect_err("must error without base_url");
        assert!(
            err.message.contains("has no base_url"),
            "unexpected message: {err:?}"
        );
        // An empty/whitespace override base_url is treated as absent (the
        // picker sends empty strings for unfilled fields).
        let err = resolve_endpoint_credentials(
            &config,
            "missing",
            Some("   ".into()),
            Some("sk-x".into()),
            None,
        )
        .expect_err("empty override base_url must error");
        assert!(
            err.message.contains("has no base_url"),
            "unexpected message: {err:?}"
        );
    }

    #[test]
    fn dummy_terminal_fallback() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        std::env::remove_var("OPENAI_API_KEY");
        // Saved endpoint, no stored key, no env vars: the terminal "dummy"
        // fallback (local endpoints often need no key).
        let config = config_with_endpoint(EndpointKind::Local, "http://localhost:11434/");
        let (base_url, api_key, kind) =
            resolve_endpoint_credentials(&config, "ep", None, None, None)
                .expect("dummy fallback must succeed");
        assert_eq!(base_url, "http://localhost:11434/");
        assert_eq!(api_key, "dummy");
        assert_eq!(kind, "local");
    }
}
