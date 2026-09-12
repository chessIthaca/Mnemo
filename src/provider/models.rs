// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Live model discovery from provider `/models` endpoints.
//!
//! The Settings → Endpoints and Settings → Vision model pickers fetch the
//! live model list so the user can click a served model name instead of
//! typing an id by hand. This is a settings-UI concern, not a client
//! concern: the fetchers build their own short-lived HTTP clients (10s
//! connect / 15s total, no redirects) and never touch the streaming chat
//! clients.
//!
//! Two fetchers, one parser, one output type:
//! - [`fetch_models_with_vision`] — the OpenAI-compatible shape (Bearer
//!   auth): OpenAI, Ollama, vLLM, LM Studio, OpenRouter, gateways.
//! - [`fetch_models_anthropic`] — the Anthropic shape (`x-api-key` +
//!   `anthropic-version` headers). It lives here rather than in
//!   `anthropic.rs` because it shares [`parse_models_with_vision`] and
//!   [`ModelWithVision`] with the OpenAI fetcher (backlog 76efacba
//!   relocated the family out of `openai.rs`).
//!
//! [`parse_models_with_vision`] handles the response shapes leniently —
//! OpenAI `data[]`, Ollama `models[]`, OpenRouter modality/caps fields —
//! and [`ModelWithVision`] carries the id + vision flag + token caps the
//! UI needs.

use crate::error::{Error, Result};

use super::sse_util::check_response;

/// A model id paired with whether the provider reports it as vision-capable
/// (i.e. it accepts image inputs). Used by the Settings → Vision picker to
/// filter the live `/models` list down to vision-capable models.
///
/// `vision_capable` is `true` only when the provider explicitly exposes image
/// input modality (e.g. OpenRouter's `architecture.input_modalities` array
/// containing `"image"`). Providers that don't expose modality info (vanilla
/// OpenAI, Ollama) report `false` for every model — the UI then falls back to
/// showing all models with a note rather than silently hiding them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelWithVision {
    /// The model id (as returned by `/models`).
    pub id: String,
    /// Whether the provider reports this model accepts image inputs.
    pub vision_capable: bool,
    /// The model's context window in tokens as reported by the provider
    /// (Ollama `context_length`, LM Studio `max_context_length`, vLLM
    /// `max_model_len`, OpenRouter `top_provider.context_length`). `None`
    /// when the `/models` entry exposes no such field (vanilla OpenAI / z.ai)
    /// or the value is malformed — best-effort discovery, never an error.
    pub context_length: Option<u64>,
    /// The model's per-request output-token cap as reported by the provider
    /// (`max_completion_tokens`, OpenRouter `top_provider.max_completion_tokens`).
    /// `None` when not exposed or malformed.
    pub max_output_tokens: Option<u64>,
}

/// Fetch the models served by an Anthropic-compatible `/models` endpoint
/// (Anthropic itself, OpenRouter, gateways), each annotated with whether the
/// provider reports it as vision-capable and with the token caps the provider
/// exposes.
///
/// HTTP contract: GET `{base}/models`, `x-api-key` + `anthropic-version`
/// headers, 10s connect / 15s total timeout, gzip/brotli/deflate. The
/// response shape is `{ data: [{ id, display_name, created_at }] }` — parsed
/// via [`parse_models_with_vision`] (which reads `data[].id`); `display_name`
/// and per-model caps are not exposed by the Anthropic endpoint, so
/// `vision_capable` is always `false` and both caps `None`. This makes the
/// Settings picker list the served model ids (click-to-choose works); the
/// vision picker simply falls back to its "all models with a note" behavior.
pub async fn fetch_models_anthropic(base_url: &str, api_key: &str) -> Result<Vec<ModelWithVision>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .gzip(true)
        .brotli(true)
        .deflate(true)
        // Never follow redirects — see the main client (Review L1).
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Provider(format!("failed to build HTTP client: {e}")))?;

    let response = client
        .get(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| Error::Provider(format!("failed to reach {url}: {e}")))?;

    if !response.status().is_success() {
        return Err(match check_response(response, &url, "fetch models").await {
            Ok(_) => unreachable!("status was not success"),
            Err((_, _, error)) => error,
        });
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| Error::Provider(format!("failed to parse /models response as JSON: {e}")))?;

    let models = parse_models_with_vision(&body).ok_or_else(|| {
        Error::Provider(
            "could not find a model list in the response (expected Anthropic `{ data: [{ id }] }`)"
                .to_string(),
        )
    })?;
    Ok(models)
}

/// Fetch the models served by an OpenAI-compatible `/models` endpoint, each
/// annotated with whether the provider reports it as vision-capable and with
/// the token caps the provider exposes.
///
/// HTTP contract: GET `{base}/models`, Bearer key, 10s connect / 15s total
/// timeout, gzip/brotli/deflate. Parses via
/// [`parse_models_with_vision`], then de-duplicates + sorts: duplicate entries
/// for the same id OR their `vision_capable` flags together (so a model listed
/// twice that is vision-capable in at least one entry is reported as capable)
/// and take the max of each reported cap. Blank/whitespace-only ids are
/// dropped (trimmed first). Returns the sorted `Vec<ModelWithVision>`.
pub async fn fetch_models_with_vision(
    base_url: &str,
    api_key: &str,
) -> Result<Vec<ModelWithVision>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(15))
        .gzip(true)
        .brotli(true)
        .deflate(true)
        // Never follow redirects — see the main client (Review L1).
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::Provider(format!("failed to build HTTP client: {e}")))?;

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| Error::Provider(format!("failed to reach {url}: {e}")))?;

    if !response.status().is_success() {
        return Err(match check_response(response, &url, "fetch models").await {
            Ok(_) => unreachable!("status was not success"),
            Err((_, _, error)) => error,
        });
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| Error::Provider(format!("failed to parse /models response as JSON: {e}")))?;

    let models = parse_models_with_vision(&body).ok_or_else(|| {
        Error::Provider(
            "could not find a model list in the response (expected OpenAI `{ data: [{ id }] }` or Ollama `{ models: [{ name }] }`)"
                .to_string(),
        )
    })?;

    // De-duplicate by id, OR-ing the vision flag and taking the max of each
    // reported cap across duplicate entries so a model reported as capable
    // (or with a larger window) in any one entry wins. BTreeMap keeps the
    // output sorted by id.
    let mut unique: std::collections::BTreeMap<String, (bool, ModelCaps)> =
        std::collections::BTreeMap::new();
    for m in models {
        let id = m.id.trim().to_string();
        if id.is_empty() {
            continue;
        }
        let entry = unique.entry(id).or_insert((false, ModelCaps::default()));
        entry.0 = entry.0 || m.vision_capable;
        entry.1.context_length = entry.1.context_length.max(m.context_length);
        entry.1.max_output_tokens = entry.1.max_output_tokens.max(m.max_output_tokens);
    }
    Ok(unique
        .into_iter()
        .map(|(id, (vision_capable, caps))| ModelWithVision {
            id,
            vision_capable,
            context_length: caps.context_length,
            max_output_tokens: caps.max_output_tokens,
        })
        .collect())
}

/// Whether a single `/models` entry explicitly reports image input modality.
///
/// Returns `true` only when the entry exposes an `architecture.input_modalities`
/// array containing the string `"image"` (the OpenRouter shape). Returns `false`
/// for any other shape — including vanilla OpenAI/Ollama entries that carry no
/// modality field at all (conservatively "unknown", not "incapable"). The UI
/// uses the absence of *any* vision-capable model to fall back to showing all
/// models with a note, rather than silently hiding them.
fn model_supports_vision(entry: &serde_json::Value) -> bool {
    entry
        .get("architecture")
        .and_then(|a| a.get("input_modalities"))
        .and_then(|m| m.as_array())
        .map(|modalities| {
            modalities
                .iter()
                .any(|m| m.as_str().is_some_and(|s| s == "image"))
        })
        .unwrap_or(false)
}

/// The per-model token caps discovered from a single `/models` entry.
///
/// Extracted leniently by [`model_reported_caps`]: every field is `None`
/// when the entry doesn't expose it or carries a malformed value — discovery
/// is best-effort and never surfaces an error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ModelCaps {
    /// Context window in tokens, when reported.
    context_length: Option<u64>,
    /// Per-request output-token cap, when reported.
    max_output_tokens: Option<u64>,
}

/// Read a model entry's reported token caps (context + output), leniently.
///
/// Key precedence per provider convention, first hit wins:
/// - context: `context_length` (Ollama), `max_context_length` (LM Studio),
///   `max_model_len` (vLLM), nested `top_provider.context_length` (OpenRouter).
/// - output: `max_completion_tokens`, nested `top_provider.max_completion_tokens`.
///
/// Non-numeric, negative, or zero values read as `None` (a zero/negative cap
/// is meaningless, so treat it as unreported rather than poisoning the
/// endpoint config with it). Never errors — an unparseable entry simply
/// contributes no caps.
fn model_reported_caps(entry: &serde_json::Value) -> ModelCaps {
    /// Read the first numeric key (as u64) from `keys`, skipping values that
    /// are not positive integers. `None` when no key hits.
    fn first_positive_u64(entry: &serde_json::Value, keys: &[&str]) -> Option<u64> {
        keys.iter()
            .find_map(|k| entry.get(*k).and_then(|v| v.as_u64()).filter(|n| *n > 0))
    }

    // OpenRouter nests the live limits under `top_provider`.
    let top = entry.get("top_provider");

    let context_length = first_positive_u64(
        entry,
        &["context_length", "max_context_length", "max_model_len"],
    )
    .or_else(|| {
        top.and_then(|t| {
            first_positive_u64(
                t,
                &["context_length", "max_context_length", "max_model_len"],
            )
        })
    });
    let max_output_tokens = first_positive_u64(entry, &["max_completion_tokens"])
        .or_else(|| top.and_then(|t| first_positive_u64(t, &["max_completion_tokens"])));

    ModelCaps {
        context_length,
        max_output_tokens,
    }
}

/// Extract models (id + vision flag) from an OpenAI-compatible or Ollama
/// `/models` response.
///
/// Shape handling and the `Some`/`None` return contract:
/// - OpenAI: walks `data`, taking `id` from each entry.
/// - Ollama: walks `models`, taking `name` (older) or `model` (newer).
/// - `data` takes precedence when both keys are present.
/// - `Some(vec![])` for a present-but-empty array; `None` for a non-empty array
///   yielding no ids (malformed); `None` when neither array key is present.
///
/// The vision flag comes from [`model_supports_vision`]; ids are returned in
/// array order (de-dup/sort happens later in [`fetch_models_with_vision`]).
fn parse_models_with_vision(body: &serde_json::Value) -> Option<Vec<ModelWithVision>> {
    if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
        let models: Vec<ModelWithVision> = data
            .iter()
            .filter_map(|m| {
                m.get("id").and_then(|i| i.as_str()).map(|s| {
                    let caps = model_reported_caps(m);
                    ModelWithVision {
                        id: s.to_string(),
                        vision_capable: model_supports_vision(m),
                        context_length: caps.context_length,
                        max_output_tokens: caps.max_output_tokens,
                    }
                })
            })
            .collect();
        // A present-but-empty `data` array is a valid OpenAI empty model list;
        // a non-empty array with no usable ids is malformed.
        return if data.is_empty() {
            Some(vec![])
        } else if models.is_empty() {
            None
        } else {
            Some(models)
        };
    }
    if let Some(models) = body.get("models").and_then(|m| m.as_array()) {
        let entries: Vec<ModelWithVision> = models
            .iter()
            .filter_map(|m| {
                m.get("name")
                    .or_else(|| m.get("model"))
                    .and_then(|i| i.as_str())
                    .map(|s| {
                        let caps = model_reported_caps(m);
                        ModelWithVision {
                            id: s.to_string(),
                            vision_capable: model_supports_vision(m),
                            context_length: caps.context_length,
                            max_output_tokens: caps.max_output_tokens,
                        }
                    })
            })
            .collect();
        return if models.is_empty() {
            Some(vec![])
        } else if entries.is_empty() {
            None
        } else {
            Some(entries)
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // The /models discovery tests moved from openai.rs when the family was
    // extracted into this module (backlog 76efacba; same names, same
    // assertions — history stays greppable).

    #[test]
    fn parse_models_with_vision_openai_data_takes_precedence() {
        // If both keys exist, the OpenAI `data` shape wins.
        let body = serde_json::json!({
            "data": [{ "id": "a" }],
            "models": [{ "name": "b" }]
        });
        assert_eq!(
            parse_models_with_vision(&body),
            Some(vec![ModelWithVision {
                id: "a".into(),
                vision_capable: false,
                context_length: None,
                max_output_tokens: None
            }])
        );
    }

    #[test]
    fn parse_models_with_vision_empty_data_is_valid_empty_list() {
        // A present-but-empty `data` array is a valid OpenAI empty model list
        // (Some(vec![])), NOT a fallback to `models` and NOT a parse error —
        // the UI shows an "empty list" state, not an error.
        let body = serde_json::json!({ "data": [] });
        assert_eq!(parse_models_with_vision(&body), Some(vec![]));
    }

    #[test]
    fn parse_models_with_vision_empty_models_array_is_valid_empty_list() {
        // Same for the Ollama `models` shape.
        let body = serde_json::json!({ "models": [] });
        assert_eq!(parse_models_with_vision(&body), Some(vec![]));
    }

    #[test]
    fn parse_models_with_vision_unknown_shape_returns_none() {
        assert!(parse_models_with_vision(&serde_json::json!({ "foo": "bar" })).is_none());
        // Missing id/name fields → no usable ids → None.
        assert!(
            parse_models_with_vision(&serde_json::json!({ "data": [{ "object": "model" }] }))
                .is_none()
        );
    }

    #[test]
    fn parse_models_with_vision_preserves_ids_untrimmed_in_array_order() {
        // parse_models_with_vision itself must NOT trim or de-dup — that happens
        // later in fetch_models_with_vision. Whitespace-only ids are kept here
        // (blank dropping is the fetch layer's job), and array order is preserved.
        let body = serde_json::json!({
            "data": [
                { "id": "gpt-4o", "object": "model" },
                { "id": "  spaced  " }
            ]
        });
        assert_eq!(
            parse_models_with_vision(&body).expect("openai shape"),
            vec![
                ModelWithVision {
                    id: "gpt-4o".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
                ModelWithVision {
                    id: "  spaced  ".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
            ]
        );
    }

    #[test]
    fn parse_models_with_vision_openrouter_shape() {
        // OpenRouter exposes architecture.input_modalities; "image" marks a
        // vision-capable model.
        let body = serde_json::json!({
            "data": [
                { "id": "gpt-4o", "architecture": { "input_modalities": ["text", "image"] } },
                { "id": "gpt-3.5", "architecture": { "input_modalities": ["text"] } }
            ]
        });
        let models = parse_models_with_vision(&body).expect("openrouter shape");
        assert_eq!(
            models,
            vec![
                ModelWithVision {
                    id: "gpt-4o".into(),
                    vision_capable: true,
                    context_length: None,
                    max_output_tokens: None
                },
                ModelWithVision {
                    id: "gpt-3.5".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
            ]
        );
    }

    #[test]
    fn parse_models_with_vision_vanilla_openai_has_no_modality() {
        // Vanilla OpenAI /models carries no architecture key → vision_capable
        // is conservatively false for every model (unknown, not incapable).
        let body = serde_json::json!({
            "data": [{ "id": "gpt-4o" }, { "id": "gpt-3.5" }]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        assert_eq!(
            models,
            vec![
                ModelWithVision {
                    id: "gpt-4o".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
                ModelWithVision {
                    id: "gpt-3.5".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
            ]
        );
    }

    #[test]
    fn parse_models_with_vision_ollama_shape() {
        // Ollama entries carry no modality info → vision_capable is false.
        let body = serde_json::json!({
            "models": [
                { "name": "llama3:latest" },
                { "model": "qwen2:7b" }
            ]
        });
        let models = parse_models_with_vision(&body).expect("ollama shape");
        assert_eq!(
            models,
            vec![
                ModelWithVision {
                    id: "llama3:latest".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
                ModelWithVision {
                    id: "qwen2:7b".into(),
                    vision_capable: false,
                    context_length: None,
                    max_output_tokens: None
                },
            ]
        );
    }

    #[test]
    fn model_caps_ollama_context_length() {
        // Ollama's /models reports context_length per entry (OpenAI `data`
        // shape served by newer Ollama; the `models` shape parses the same).
        let body = serde_json::json!({
            "data": [{ "id": "llama3:latest", "context_length": 8192 }]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        assert_eq!(
            models[0].context_length,
            Some(8192),
            "context_length should be discovered from the Ollama key"
        );
        assert_eq!(models[0].max_output_tokens, None);
    }

    #[test]
    fn model_caps_lmstudio_max_context_length() {
        // LM Studio uses max_context_length.
        let body = serde_json::json!({
            "data": [{ "id": "qwen2.5-7b", "max_context_length": 32768 }]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        assert_eq!(models[0].context_length, Some(32768));
    }

    #[test]
    fn model_caps_vllm_max_model_len() {
        // vLLM uses max_model_len.
        let body = serde_json::json!({
            "data": [{ "id": "meta-llama/Llama-3.1-8B", "max_model_len": 131072 }]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        assert_eq!(models[0].context_length, Some(131072));
    }

    #[test]
    fn model_caps_openrouter_nested_top_provider() {
        // OpenRouter nests the live limits under top_provider.
        let body = serde_json::json!({
            "data": [{
                "id": "openai/gpt-4o",
                "top_provider": { "context_length": 128000, "max_completion_tokens": 16384 }
            }]
        });
        let models = parse_models_with_vision(&body).expect("openrouter shape");
        assert_eq!(models[0].context_length, Some(128000));
        assert_eq!(models[0].max_output_tokens, Some(16384));
    }

    #[test]
    fn model_caps_key_precedence_first_hit_wins() {
        // When several context keys are present, the first in precedence
        // order (context_length → max_context_length → max_model_len) wins.
        let body = serde_json::json!({
            "data": [{
                "id": "m",
                "context_length": 4096,
                "max_context_length": 8192,
                "max_model_len": 16384
            }]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        assert_eq!(models[0].context_length, Some(4096));
    }

    #[test]
    fn model_caps_vanilla_openai_reports_nothing() {
        // Vanilla OpenAI / z.ai entries carry no cap fields → discovery
        // yields None (the feature is a no-op there, by design).
        let body = serde_json::json!({
            "data": [{ "id": "glm-5.3", "object": "model", "owned_by": "z.ai" }]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        assert_eq!(models[0].context_length, None);
        assert_eq!(models[0].max_output_tokens, None);
    }

    #[test]
    fn model_caps_malformed_values_are_ignored() {
        // Zero / negative / non-numeric values read as unreported — never
        // an error, never a poison value in the endpoint config.
        let body = serde_json::json!({
            "data": [
                { "id": "zero", "context_length": 0 },
                { "id": "negative", "context_length": -8 },
                { "id": "string", "context_length": "8192" },
                { "id": "float", "context_length": 8192.5 },
                { "id": "null", "context_length": null }
            ]
        });
        let models = parse_models_with_vision(&body).expect("openai shape");
        for m in &models {
            assert_eq!(m.context_length, None, "id {}", m.id);
        }
    }

    #[test]
    fn model_supports_vision_false_without_architecture_key() {
        // No architecture key → false (conservatively unknown).
        assert!(!model_supports_vision(
            &serde_json::json!({ "id": "gpt-4o" })
        ));
        // architecture present but no input_modalities array → false.
        assert!(!model_supports_vision(
            &serde_json::json!({ "id": "gpt-4o", "architecture": {} })
        ));
        // input_modalities without "image" → false.
        assert!(!model_supports_vision(&serde_json::json!({
            "id": "gpt-4o",
            "architecture": { "input_modalities": ["text"] }
        })));
        // "image" present → true.
        assert!(model_supports_vision(&serde_json::json!({
            "id": "gpt-4o",
            "architecture": { "input_modalities": ["text", "image"] }
        })));
    }
}
