// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pure config-validation + patch-application logic, extracted from the IPC
//! adapter (`settings.rs`) so it is unit-testable without Tauri state.
//!
//! Covers the endpoint-save path (`validate_endpoint`, `validate_endpoint_set`,
//! `apply_endpoints`) and safety-mode parsing. The adapter deserializes its
//! wire DTOs, trims/converts to these lib inputs, and calls in; the persist +
//! reload + rewire (I/O + Tauri state) stays in the adapter. The non-endpoint
//! settings save path lives in `settings_dto.rs`
//! (`validate_and_apply_settings_patch`).

use std::collections::HashMap;

use crate::config::{Config, Endpoint, EndpointKind, KeyStore, SafetyMode};

// ── Endpoint save ───────────────────────────────────────────────────────

/// Validate one endpoint's fields + convert to an [`Endpoint`].
///
/// Validation: name non-empty (trimmed), base_url non-empty + normalized to
/// end with `/` (a missing trailing slash is auto-appended — forgiving, since
/// the providers already tolerate both forms via `trim_end_matches('/')`),
/// kind is a known value (`"openai"` / `"local"` / `"anthropic"`).
/// Empty/whitespace-only model ids are dropped from the `models` list.
/// `workspace_id` is trimmed; an empty/whitespace value clears to `None`
/// (the header is never sent empty).
pub fn validate_endpoint(
    name: &str,
    kind: &str,
    base_url: &str,
    models: Vec<String>,
    model_configs: Vec<crate::config::ModelSpec>,
    max_context: Option<usize>,
    max_output_tokens: Option<usize>,
    multimodal: bool,
    supports_reasoning_effort: bool,
    reasoning_effort: Option<String>,
    workspace_id: Option<String>,
) -> Result<Endpoint, String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("endpoint name must not be empty".to_string());
    }
    let base_url = base_url.trim();
    if base_url.is_empty() {
        return Err(format!("endpoint '{name}': base_url must not be empty"));
    }
    // Forgiving normalization: append the missing trailing slash instead of
    // rejecting. Without it the URL builder would still work (providers trim
    // trailing slashes), but the stored config would be inconsistent.
    let base_url = if base_url.ends_with('/') {
        base_url.to_string()
    } else {
        format!("{base_url}/")
    };
    let kind = match kind {
        "openai" => EndpointKind::OpenAI,
        "local" => EndpointKind::Local,
        "anthropic" => EndpointKind::Anthropic,
        other => {
            return Err(format!(
                "endpoint '{name}': unknown kind '{other}' (expected 'openai', 'local', or 'anthropic')"
            ));
        }
    };
    // Drop empty/whitespace-only model ids; they'd just be noise on disk.
    let model_ids = models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>();
    // Workspace id: trim; an empty/whitespace value clears to None so the
    // header is never sent empty. Anthropic-only: cleared for other kinds
    // (a stale value on an openai/local endpoint would be invisible once the
    // form hides the field). The value must be a valid HTTP header value —
    // every char printable ASCII (0x20..=0x7E, what
    // `HeaderValue::from_str` accepts) — so an invalid id is rejected here,
    // at Save time, with a readable error (surfaced in the settings error
    // dialog) instead of being silently dropped at request time.
    let workspace_id = workspace_id
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty());
    let workspace_id = match workspace_id {
        Some(_) if kind != EndpointKind::Anthropic => None,
        Some(w) => {
            if w.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c)) {
                Some(w)
            } else {
                return Err(format!(
                    "endpoint '{name}': workspace_id must contain only printable \
                     ASCII characters (it is sent as the anthropic-workspace-id \
                     HTTP header)"
                ));
            }
        }
        None => None,
    };
    // Merge the per-model configs onto the model list, PRESERVING the order
    // of `models`: every config whose (trimmed) id matches a model attaches
    // to that model; configs for unknown models are dropped (orphaned noise);
    // models without a config stay bare.
    let specs = model_configs
        .into_iter()
        .filter(|spec| model_ids.iter().any(|m| m == spec.id.trim()))
        .map(|mut spec| {
            spec.id = spec.id.trim().to_string();
            spec
        })
        .collect::<Vec<_>>();
    let models = model_ids
        .into_iter()
        .map(|id| {
            specs
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .unwrap_or_else(|| crate::config::ModelSpec::from(id))
        })
        .collect::<Vec<_>>();
    Ok(Endpoint {
        name,
        kind,
        base_url,
        models,
        max_context,
        max_output_tokens,
        multimodal,
        supports_strict_schema: None,
        supports_reasoning_effort,
        reasoning_effort,
        reasoning_effort_off_wire: None,
        workspace_id,
        temperature: None,
        top_p: None,
        stop: Vec::new(),
        stop_token_ids: Vec::new(),
        stop_boundary_strings: Vec::new(),
        extra_body: None,
    })
}

/// Cross-endpoint validation: default_provider references an endpoint,
/// default_model exists at the host (lenient re: `old_default_model`),
/// and the default endpoint yields a resolvable model. (Unique names are
/// checked separately by the caller — this fn does NOT check them.)
pub fn validate_endpoint_set(
    built: &[Endpoint],
    default_provider: Option<&str>,
    default_model: Option<&str>,
    old_default_model: Option<&str>,
) -> Result<(), String> {
    // The default provider (if set) must reference one of the endpoints.
    if let Some(dp) = default_provider {
        if !built.iter().any(|e| e.name == dp) {
            return Err(format!(
                "default_provider '{dp}' does not match any configured endpoint"
            ));
        }
    }
    // The default model (if set) must exist at the default endpoint (or any
    // endpoint if no default provider is set). Lenient: a model that is the
    // pre-save running model (`old_default_model`) is accepted even if not in
    // the endpoint's `models` list.
    if let Some(dm) = default_model {
        let host = default_provider
            .and_then(|n| built.iter().find(|e| e.name == n))
            .or_else(|| built.first());
        match host {
            Some(ep) if ep.has_model(dm) => {}
            Some(_ep) if old_default_model == Some(dm) => {}
            Some(ep) => {
                return Err(format!(
                    "default_model '{dm}' is not in endpoint '{}' models {:?}",
                    ep.name,
                    ep.model_ids()
                ));
            }
            None => {
                return Err(format!(
                    "default_model '{dm}' set but no endpoints are configured"
                ));
            }
        }
    }
    // The default endpoint must yield *some* model so the live provider isn't
    // built against a hardcoded fallback.
    {
        let host = default_provider
            .and_then(|n| built.iter().find(|e| e.name == n))
            .or_else(|| built.first());
        if let Some(ep) = host {
            let resolvable = default_model
                .filter(|m| !m.is_empty())
                .map(|_| true)
                .unwrap_or_else(|| !ep.models.is_empty());
            if !resolvable {
                return Err(format!(
                    "default endpoint '{}' has no models and no default_model is set; \
                     add a model or choose a default model",
                    ep.name
                ));
            }
        }
    }
    Ok(())
}

/// Build the new [`Config`] from an endpoint save. Preserves pricing +
/// projects + non-general sections; sets `general.default_provider`/
/// `default_model`; builds a [`KeyStore`] keeping only keys for existing
/// endpoints (dropping empty values).
pub fn apply_endpoints(
    current: &Config,
    mut built: Vec<Endpoint>,
    default_provider: Option<String>,
    default_model: Option<String>,
    api_keys: &HashMap<String, String>,
) -> Config {
    // Preserve advanced/sampling parameters from existing endpoints not editable in the UI
    for ep in &mut built {
        if let Some(orig) = current.endpoints.iter().find(|e| e.name == ep.name) {
            if ep.temperature.is_none() {
                ep.temperature = orig.temperature;
            }
            if ep.top_p.is_none() {
                ep.top_p = orig.top_p;
            }
            if ep.stop.is_empty() {
                ep.stop = orig.stop.clone();
            }
            if ep.stop_token_ids.is_empty() {
                ep.stop_token_ids = orig.stop_token_ids.clone();
            }
            if ep.stop_boundary_strings.is_empty() {
                ep.stop_boundary_strings = orig.stop_boundary_strings.clone();
            }
            if ep.extra_body.is_none() {
                ep.extra_body = orig.extra_body.clone();
            }
            if ep.reasoning_effort_off_wire.is_none() {
                ep.reasoning_effort_off_wire = orig.reasoning_effort_off_wire.clone();
            }
            for m in &mut ep.models {
                if let Some(orig_m) = orig.models.iter().find(|om| om.id == m.id) {
                    if m.temperature.is_none() {
                        m.temperature = orig_m.temperature;
                    }
                    if m.top_p.is_none() {
                        m.top_p = orig_m.top_p;
                    }
                    if m.stop.is_empty() {
                        m.stop = orig_m.stop.clone();
                    }
                    if m.stop_token_ids.is_empty() {
                        m.stop_token_ids = orig_m.stop_token_ids.clone();
                    }
                    if m.stop_boundary_strings.is_empty() {
                        m.stop_boundary_strings = orig_m.stop_boundary_strings.clone();
                    }
                    if m.extra_body.is_none() {
                        m.extra_body = orig_m.extra_body.clone();
                    }
                    if m.reasoning_effort_off_wire.is_none() {
                        m.reasoning_effort_off_wire = orig_m.reasoning_effort_off_wire.clone();
                    }
                }
            }
        }
    }

    let mut general = current.general.clone();
    general.general.default_provider = default_provider;
    general.general.default_model = default_model;
    let mut keys = KeyStore::default();
    for ep in &built {
        if let Some(k) = api_keys.get(&ep.name) {
            if !k.is_empty() {
                keys.insert(ep.name.clone(), k.clone());
            }
        }
    }
    Config {
        general,
        endpoints: built,
        pricing: current.pricing.clone(),
        keys,
        mcp: current.mcp.clone(),
        projects: current.projects.clone(),
    }
}

/// Parse a kebab-case safety-mode string into a [`SafetyMode`].
pub fn parse_safety_mode(s: &str) -> Result<SafetyMode, String> {
    match s {
        "approve-each-action" => Ok(SafetyMode::ApproveEachAction),
        "auto-read-approve-writes" => Ok(SafetyMode::AutoReadApproveWrites),
        "auto-approve-project" => Ok(SafetyMode::AutoApproveProject),
        "autonomous" => Ok(SafetyMode::Autonomous),
        other => Err(format!(
            "unknown safety mode '{other}' (expected approve-each-action, auto-read-approve-writes, auto-approve-project, or autonomous)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Endpoint, PricingEntry};

    fn ep(name: &str, kind: &str, base_url: &str, models: Vec<&str>) -> Endpoint {
        validate_endpoint(
            name,
            kind,
            base_url,
            models.into_iter().map(String::from).collect(),
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap()
    }

    #[test]
    fn validate_endpoint_rejects_empty_name() {
        let r = validate_endpoint(
            "",
            "openai",
            "https://x/v1/",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        );
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("name must not be empty"));
    }

    #[test]
    fn validate_endpoint_normalizes_base_url_without_slash() {
        // Forgiving (user-reported): a base_url missing the trailing slash is
        // auto-normalized, NOT rejected — the providers already tolerate both
        // forms via trim_end_matches('/'), so the stored config just gets the
        // canonical form.
        let ep = validate_endpoint(
            "x",
            "openai",
            "https://x/v1",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.base_url, "https://x/v1/");
    }

    #[test]
    fn validate_endpoint_normalizes_whitespace_padded_base_url() {
        // The trim also applies to surrounding whitespace (a pasted URL with
        // accidental spaces must not poison the stored config).
        let ep = validate_endpoint(
            "x",
            "openai",
            "  https://x/v1  ",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.base_url, "https://x/v1/");
    }

    #[test]
    fn validate_endpoint_rejects_unknown_kind() {
        let r = validate_endpoint(
            "x",
            "claude",
            "https://x/v1/",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        );
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unknown kind"));
    }

    #[test]
    fn validate_endpoint_accepts_anthropic_kind() {
        let ep = validate_endpoint(
            "claude",
            "anthropic",
            "https://api.anthropic.com/v1/",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.kind, EndpointKind::Anthropic);
    }

    #[test]
    fn validate_endpoint_drops_empty_models() {
        let ep = validate_endpoint(
            "x",
            "openai",
            "https://x/v1/",
            vec![
                "gpt-4o".to_string(),
                "".to_string(),
                "  ".to_string(),
                "mini".to_string(),
            ],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.model_ids(), vec!["gpt-4o", "mini"]);
    }

    #[test]
    fn validate_endpoint_trims_and_clears_workspace_id() {
        // A padded value is trimmed; a whitespace-only value clears to None
        // so the header is never sent empty.
        let ep = validate_endpoint(
            "claude",
            "anthropic",
            "https://api.anthropic.com/v1/",
            vec!["claude-sonnet-4-5".to_string()],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            Some("  ws_abc  ".to_string()),
        )
        .unwrap();
        assert_eq!(ep.workspace_id.as_deref(), Some("ws_abc"));
        let cleared = validate_endpoint(
            "claude",
            "anthropic",
            "https://api.anthropic.com/v1/",
            vec!["claude-sonnet-4-5".to_string()],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            Some("   ".to_string()),
        )
        .unwrap();
        assert_eq!(cleared.workspace_id, None, "whitespace-only clears to None");
    }

    #[test]
    fn validate_endpoint_rejects_non_header_safe_workspace_id() {
        // Review L1 (2026-12-06): the value rides as an HTTP header — reject
        // control/non-ASCII chars at Save time (surfaced in the settings
        // error dialog) instead of silently dropping the header at request
        // time.
        let r = validate_endpoint(
            "claude",
            "anthropic",
            "https://api.anthropic.com/v1/",
            vec!["claude-sonnet-4-5".to_string()],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            Some("ws_a\tb".to_string()),
        );
        assert!(r.is_err(), "control char must be rejected");
        let r2 = validate_endpoint(
            "claude",
            "anthropic",
            "https://api.anthropic.com/v1/",
            vec!["claude-sonnet-4-5".to_string()],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            Some("ws_ä".to_string()),
        );
        assert!(r2.is_err(), "non-ASCII must be rejected");
        // A tab-free, ASCII-only value still passes (boundary sanity).
        let ok = validate_endpoint(
            "claude",
            "anthropic",
            "https://api.anthropic.com/v1/",
            vec!["claude-sonnet-4-5".to_string()],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            Some("ws_ok-1.2".to_string()),
        )
        .unwrap();
        assert_eq!(ok.workspace_id.as_deref(), Some("ws_ok-1.2"));
    }

    #[test]
    fn validate_endpoint_clears_workspace_id_for_non_anthropic() {
        // Review L2 (2026-12-06): the form hides the field for non-anthropic
        // kinds, so a persisted value would be invisible and un-clearable —
        // clear it at Save time instead.
        let cleared = validate_endpoint(
            "openai",
            "openai",
            "https://api.openai.com/v1/",
            vec!["gpt-4o".to_string()],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            Some("ws_leftover".to_string()),
        )
        .unwrap();
        assert_eq!(cleared.workspace_id, None, "non-anthropic kinds drop it");
    }

    #[test]
    fn validate_endpoint_trims_name() {
        let ep = validate_endpoint(
            "  openai  ",
            "openai",
            "https://x/v1/",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.name, "openai");
    }

    #[test]
    fn validate_endpoint_merges_per_model_configs() {
        // Per-model configs attach to their model, preserve order, and configs
        // for unknown models are dropped (orphaned noise on disk).
        let specs = vec![
            crate::config::ModelSpec {
                id: "gpt-4o".into(),
                max_context: Some(200000),
                max_output_tokens: Some(16384),
                reasoning_efforts: vec!["max".into(), "high".into()],
                reasoning_effort: Some("low".into()),
                ..crate::config::ModelSpec::test_default()
            },
            crate::config::ModelSpec {
                id: "ghost".into(), // not in `models` → dropped
                max_context: Some(1),
                ..crate::config::ModelSpec::test_default()
            },
        ];
        let ep = validate_endpoint(
            "x",
            "openai",
            "https://x/v1/",
            vec!["gpt-4o".to_string(), "mini".to_string()],
            specs,
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.model_ids(), vec!["gpt-4o", "mini"]);
        assert_eq!(ep.model_spec("gpt-4o").unwrap().max_context, Some(200000));
        assert_eq!(
            ep.model_spec("gpt-4o").unwrap().reasoning_efforts,
            vec!["max", "high"]
        );
        assert_eq!(
            ep.model_spec("gpt-4o").unwrap().reasoning_effort.as_deref(),
            Some("low")
        );
        // The bare model has no per-model config.
        assert_eq!(ep.model_spec("mini").unwrap().max_context, None);
        assert!(ep.model_spec("mini").unwrap().reasoning_efforts.is_empty());
        assert_eq!(ep.model_spec("mini").unwrap().reasoning_effort, None);
    }

    #[test]
    fn validate_endpoint_trims_padded_config_ids_before_matching() {
        // Regression (review #4): a config id with surrounding whitespace must
        // match its trimmed model id instead of being dropped as unknown.
        let specs = vec![crate::config::ModelSpec {
            id: "  gpt-4o  ".into(),
            max_context: Some(64000),
            ..crate::config::ModelSpec::test_default()
        }];
        let ep = validate_endpoint(
            "x",
            "openai",
            "https://x/v1/",
            vec!["gpt-4o".to_string()],
            specs,
            None,
            None,
            false,
            true,
            None,
            None,
        )
        .unwrap();
        assert_eq!(ep.model_ids(), vec!["gpt-4o"]);
        assert_eq!(ep.model_spec("gpt-4o").unwrap().max_context, Some(64000));
    }

    #[test]
    fn validate_endpoint_set_rejects_bad_default_provider() {
        let built = vec![ep("openai", "openai", "https://x/v1/", vec!["gpt-4o"])];
        let r = validate_endpoint_set(&built, Some("nonexistent"), None, None);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("does not match"));
    }

    #[test]
    fn validate_endpoint_set_rejects_bad_default_model() {
        let built = vec![ep("openai", "openai", "https://x/v1/", vec!["gpt-4o"])];
        let r = validate_endpoint_set(&built, Some("openai"), Some("nonexistent"), None);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("not in endpoint"));
    }

    #[test]
    fn validate_endpoint_set_lenient_old_default_model() {
        let built = vec![ep("openai", "openai", "https://x/v1/", vec!["gpt-4o"])];
        // old_default_model is accepted even if not in the endpoint's models.
        let r = validate_endpoint_set(&built, Some("openai"), Some("old-model"), Some("old-model"));
        assert!(r.is_ok());
    }

    #[test]
    fn validate_endpoint_set_rejects_no_models_no_default() {
        let built = vec![ep("openai", "openai", "https://x/v1/", vec![])];
        let r = validate_endpoint_set(&built, Some("openai"), None, None);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("no models"));
    }

    #[test]
    fn apply_endpoints_preserves_pricing_and_projects() {
        let mut current = Config::default();
        current.pricing = vec![PricingEntry {
            model: "gpt-4o".into(),
            input_per_1m: 2.5,
            output_per_1m: 10.0,
            cached_per_1m: 1.25,
        }];
        current.projects.add("demo", "/demo");
        let built = vec![ep("openai", "openai", "https://x/v1/", vec!["gpt-4o"])];
        let mut keys = HashMap::new();
        keys.insert("openai".to_string(), "sk-key".to_string());
        let new = apply_endpoints(
            &current,
            built,
            Some("openai".into()),
            Some("gpt-4o".into()),
            &keys,
        );
        assert_eq!(new.general.general.default_provider, Some("openai".into()));
        assert_eq!(new.pricing.len(), 1);
        assert_eq!(new.keys.get("openai").as_deref(), Some("sk-key"));
        // Projects preserved.
        assert_eq!(new.projects.len(), 1);
    }

    #[test]
    fn apply_endpoints_drops_empty_keys() {
        let current = Config::default();
        let built = vec![ep("openai", "openai", "https://x/v1/", vec!["gpt-4o"])];
        let mut keys = HashMap::new();
        keys.insert("openai".to_string(), "".to_string()); // empty → dropped
        let new = apply_endpoints(&current, built, None, None, &keys);
        assert!(new.keys.get("openai").is_none());
    }

    #[test]
    fn apply_endpoints_drops_keys_for_deleted_endpoints() {
        // Keys for endpoints NOT in `built` are dropped (the fn iterates
        // `built`, not `api_keys`).
        let current = Config::default();
        let built = vec![ep("openai", "openai", "https://x/v1/", vec!["gpt-4o"])];
        let mut keys = HashMap::new();
        keys.insert("openai".to_string(), "sk-key".to_string());
        keys.insert("deleted".to_string(), "sk-old".to_string()); // not in built → dropped
        let new = apply_endpoints(&current, built, None, None, &keys);
        assert_eq!(new.keys.get("openai").as_deref(), Some("sk-key"));
        assert!(new.keys.get("deleted").is_none());
    }

    #[test]
    fn apply_endpoints_preserves_sampling_and_stop_parameters() {
        use crate::config::ModelSpec;

        let mut ep_extra = toml::Table::new();
        ep_extra.insert("key".into(), toml::Value::String("val".into()));

        let current = Config {
            endpoints: vec![crate::config::Endpoint {
                name: "vllm".into(),
                base_url: "http://localhost:8000/v1/".into(),
                models: vec![ModelSpec {
                    id: "glm-5.3-flash".into(),
                    temperature: Some(0.2),
                    top_p: Some(0.8),
                    stop: vec!["<|endoftext|>".into()],
                    stop_token_ids: vec![151329],
                    stop_boundary_strings: vec!["\u{3c}|model-bound|\u{3e}".into()],
                    extra_body: Some(ep_extra.clone()),
                    ..ModelSpec::test_default()
                }],
                temperature: Some(0.7),
                top_p: Some(0.9),
                stop: vec!["<|stop|>".into()],
                stop_token_ids: vec![100],
                stop_boundary_strings: vec!["\u{3c}|ep-bound|\u{3e}".into()],
                extra_body: Some(ep_extra),
                ..Endpoint::test_default()
            }],
            ..Config::default()
        };

        // Incoming built endpoint from UI save (does not carry sampling fields)
        let built = vec![crate::config::Endpoint {
            name: "vllm".into(),
            base_url: "http://localhost:8000/v1/".into(),
            models: vec![ModelSpec {
                id: "glm-5.3-flash".into(),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        }];

        let keys = HashMap::new();
        let new_cfg = apply_endpoints(&current, built, None, None, &keys);
        let saved_ep = &new_cfg.endpoints[0];

        assert_eq!(saved_ep.temperature, Some(0.7));
        assert_eq!(saved_ep.top_p, Some(0.9));
        assert_eq!(saved_ep.stop, vec!["<|stop|>"]);
        assert_eq!(saved_ep.stop_token_ids, vec![100]);
        assert_eq!(saved_ep.stop_boundary_strings, vec!["\u{3c}|ep-bound|\u{3e}"]);
        assert!(saved_ep.extra_body.is_some());

        let saved_model = saved_ep.model_spec("glm-5.3-flash").unwrap();
        assert_eq!(saved_model.temperature, Some(0.2));
        assert_eq!(saved_model.top_p, Some(0.8));
        assert_eq!(saved_model.stop, vec!["<|endoftext|>"]);
        assert_eq!(saved_model.stop_token_ids, vec![151329]);
        assert_eq!(saved_model.stop_boundary_strings, vec!["\u{3c}|model-bound|\u{3e}"]);
        assert!(saved_model.extra_body.is_some());
    }

    #[test]
    fn apply_endpoints_preserves_reasoning_effort_off_wire() {
        use crate::config::ModelSpec;

        let current = Config {
            endpoints: vec![crate::config::Endpoint {
                name: "custom-llm".into(),
                base_url: "http://localhost:8000/v1/".into(),
                models: vec![ModelSpec {
                    id: "my-finetune".into(),
                    reasoning_effort_off_wire: Some("disabled".into()),
                    ..ModelSpec::test_default()
                }],
                reasoning_effort_off_wire: Some("none".into()),
                ..Endpoint::test_default()
            }],
            ..Config::default()
        };

        // Incoming built endpoint from UI save (does not carry the field —
        // it is hand-edited TOML, like the sampling/stop parameters).
        let built = vec![crate::config::Endpoint {
            name: "custom-llm".into(),
            base_url: "http://localhost:8000/v1/".into(),
            models: vec![ModelSpec {
                id: "my-finetune".into(),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        }];

        let keys = HashMap::new();
        let new_cfg = apply_endpoints(&current, built, None, None, &keys);
        let saved_ep = &new_cfg.endpoints[0];

        assert_eq!(
            saved_ep.reasoning_effort_off_wire,
            Some("none".into()),
            "endpoint-level reasoning_effort_off_wire must survive a UI save"
        );
        let saved_model = saved_ep.model_spec("my-finetune").unwrap();
        assert_eq!(
            saved_model.reasoning_effort_off_wire,
            Some("disabled".into()),
            "model-level reasoning_effort_off_wire must survive a UI save"
        );
    }

    #[test]
    fn apply_endpoints_preserves_per_model_reasoning_effort() {
        use crate::config::ModelSpec;

        // Hand-edited endpoints.toml state: a per-model effort default.
        let current = Config {
            endpoints: vec![crate::config::Endpoint {
                name: "vllm".into(),
                base_url: "http://localhost:8000/v1/".into(),
                models: vec![ModelSpec {
                    id: "glm-5.3-flash".into(),
                    reasoning_effort: Some("low".into()),
                    ..ModelSpec::test_default()
                }],
                reasoning_effort: Some("high".into()),
                ..Endpoint::test_default()
            }],
            ..Config::default()
        };

        // Incoming built endpoint from UI save: the per-model row dropdown
        // and the endpoint-level Effort control both ride the DTO — they are
        // UI-editable, so (unlike sampling/stop) no carry-over is needed;
        // the DTO value is authoritative.
        let built = vec![crate::config::Endpoint {
            name: "vllm".into(),
            base_url: "http://localhost:8000/v1/".into(),
            models: vec![ModelSpec {
                id: "glm-5.3-flash".into(),
                reasoning_effort: Some("medium".into()),
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("high".into()),
            ..Endpoint::test_default()
        }];

        let keys = HashMap::new();
        let new_cfg = apply_endpoints(&current, built, None, None, &keys);
        let saved_ep = &new_cfg.endpoints[0];

        // The DTO-carried per-model value lands and wins at resolution time
        // over the endpoint's.
        assert_eq!(
            saved_ep
                .model_spec("glm-5.3-flash")
                .unwrap()
                .reasoning_effort
                .as_deref(),
            Some("medium")
        );
        assert_eq!(saved_ep.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(
            saved_ep.effective_reasoning_effort_for(Some("glm-5.3-flash")),
            Some("medium".into())
        );

        // An unset per-model value clears the override (back to the
        // endpoint's) — the DTO is authoritative, no silent restore.
        let built_unset = vec![crate::config::Endpoint {
            name: "vllm".into(),
            base_url: "http://localhost:8000/v1/".into(),
            models: vec![ModelSpec {
                id: "glm-5.3-flash".into(),
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("high".into()),
            ..Endpoint::test_default()
        }];
        let new_cfg = apply_endpoints(&current, built_unset, None, None, &keys);
        let saved_ep = &new_cfg.endpoints[0];
        assert_eq!(
            saved_ep
                .model_spec("glm-5.3-flash")
                .unwrap()
                .reasoning_effort,
            None
        );
        assert_eq!(
            saved_ep.effective_reasoning_effort_for(Some("glm-5.3-flash")),
            Some("high".into()),
            "unset per-model effort inherits the endpoint's"
        );
    }

    #[test]
    fn validate_endpoint_rejects_empty_base_url() {
        let r = validate_endpoint(
            "x",
            "openai",
            "",
            vec![],
            Vec::new(),
            None,
            None,
            false,
            true,
            None,
            None,
        );
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("base_url must not be empty"));
    }

    #[test]
    fn parse_safety_mode_accepts_all_variants() {
        assert!(parse_safety_mode("approve-each-action").is_ok());
        assert!(parse_safety_mode("auto-read-approve-writes").is_ok());
        assert!(parse_safety_mode("auto-approve-project").is_ok());
        assert!(parse_safety_mode("autonomous").is_ok());
    }

    #[test]
    fn parse_safety_mode_rejects_unknown() {
        let r = parse_safety_mode("yolo");
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("unknown safety mode"));
    }
}
