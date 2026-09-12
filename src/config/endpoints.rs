// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `endpoints.toml` — named endpoint definitions (base_url, kind, models).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::provider::ProviderKind;
use crate::provider::client_factory::provider_kind;
use crate::provider::policy::reasoning_effort_off_wire_value;

/// The wire-format file: a list of `[[endpoint]]` tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EndpointsFile {
    #[serde(default)]
    pub endpoint: Vec<Endpoint>,
    /// Per-model pricing entries (optional, lives in the same
    /// `endpoints.toml` file as `[[endpoint]]`).
    #[serde(default)]
    pub pricing: Vec<PricingEntry>,
}

/// Per-model pricing — $ per 1M tokens (input/output/cached). Lives in a
/// `[[pricing]]` table in `endpoints.toml`, keyed by model name. Used by the
/// Stats tab to estimate cost. `cached_per_1m` is the discounted rate for
/// cached prompt tokens (typically 50% of input).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingEntry {
    /// The model id this pricing applies to (matches an entry in some
    /// endpoint's `models` list).
    pub model: String,
    /// $ per 1M input (prompt) tokens.
    pub input_per_1m: f64,
    /// $ per 1M output (completion) tokens.
    pub output_per_1m: f64,
    /// $ per 1M cached prompt tokens (the discounted cache rate).
    pub cached_per_1m: f64,
}

/// A named LLM endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    /// Human-readable name; joins `endpoints.toml` to `keys.toml`.
    pub name: String,
    /// Provider kind — determines the capability set.
    pub kind: EndpointKind,
    /// OpenAI-compatible base URL (auto-normalized to end with `/` on save).
    pub base_url: String,
    /// Models available at this endpoint — each with its own optional caps +
    /// reasoning-effort list. Deserializes BOTH the legacy string-array form
    /// (`models = ["gpt-4o"]`) and the new `[[endpoint.models]]` table form
    /// (see [`deserialize_models`]); always serializes as tables.
    #[serde(default, deserialize_with = "deserialize_models")]
    pub models: Vec<ModelSpec>,
    /// Maximum context window in tokens (input + output). If set, overrides
    /// the default for the provider kind (OpenAI: 128K, Local: 32K).
    #[serde(default)]
    pub max_context: Option<usize>,
    /// Maximum output tokens per request (completion budget). If set,
    /// overrides the default of max_context / 2. Use this when the model
    /// has asymmetric input/output limits.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    /// The endpoint-level default for whether models here accept image
    /// inputs (multimodal). Defaults to `false`; individual models override
    /// it via [`ModelSpec::multimodal`] (resolved by
    /// [`Self::multimodal_for`]) so a mixed endpoint can host text-only and
    /// vision-capable models side by side. When a model resolves to `false`,
    /// image blocks are stripped from its requests, and a separate vision
    /// model (if configured) is used for image-to-text instead.
    #[serde(default)]
    pub multimodal: bool,
    /// Whether models at this endpoint accept the OpenAI-compatible
    /// `reasoning_effort` request field. Defaults to `true` (existing
    /// behaviour). Set to `false` for chat / non-reasoning models that
    /// reject unknown parameters — the field is then never sent, regardless
    /// of the toolbar effort dropdown or `reasoning_effort` config value.
    #[serde(default = "default_true")]
    pub supports_reasoning_effort: bool,
    /// The reasoning effort to send with requests to this endpoint
    /// (`reasoning_effort` in the OpenAI-compatible request body, e.g.
    /// `max`/`high`/`medium`/`low`/`minimal`). `None` (unset) means the
    /// runtime default of `max` applies — the toolbar dropdown overrides it
    /// live. Set to `"off"` to omit the field for a model that *does* support
    /// the parameter — except on DeepSeek-family models, where the harness's
    /// off encodes as the enum's explicit thinking-off value `"none"` (a
    /// literal `"off"` is an instant non-retryable 400 there). When
    /// [`Self::supports_reasoning_effort`] is `false`, this value is ignored
    /// and the field is always omitted.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// The wire value the effort `"off"` encodes as for models at this
    /// endpoint — the value sent in the `reasoning_effort` request field
    /// when the effective effort is `"off"`. `None` (unset) = the built-in
    /// provider policy applies (DeepSeek-family model names → `"none"`,
    /// everyone else omits the field). Set it explicitly when a renamed,
    /// aliased, or fine-tuned DeepSeek model needs the mapping the
    /// name-based policy can't see — resolution is by config, never by
    /// model name. Model-level entries override this. Ignored on
    /// `anthropic` endpoints (the Messages API has no `reasoning_effort`
    /// field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort_off_wire: Option<String>,
    /// Optional Anthropic workspace id (e.g. `ws_...`). When set on an
    /// `anthropic` endpoint, every request sends the
    /// `anthropic-workspace-id` HTTP header so usage is attributed to that
    /// workspace. Unset/empty omits the header entirely (never sent empty —
    /// some gateways reject an empty header value). Ignored for
    /// `openai`/`local` endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Optional temperature override for request sampling (0.0 to 2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Optional top_p override for request sampling (0.0 to 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Optional explicit stop sequences sent in the request body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Optional explicit stop token IDs sent in the request body (e.g. for vLLM/SGLang).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_token_ids: Vec<u64>,
    /// Vendor stop-boundary strings for models whose tokenizer emits its
    /// stop boundaries as raw text (e.g. GLM-5.3's role-tag tokens plus
    /// newline cascades). Sent in the request `stop` list AND enforced by
    /// the client-side SSE stream guard, so a model with leaky stop tokens
    /// is fixed by config, not code. Model-level entries override these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_boundary_strings: Vec<String>,
    /// Arbitrary extra body parameters merged into the request payload.
    /// Merged last: keys here can override any request field including model/messages/stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<toml::Table>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: EndpointKind::OpenAI,
            base_url: String::new(),
            models: Vec::new(),
            max_context: None,
            max_output_tokens: None,
            multimodal: false,
            supports_reasoning_effort: true,
            reasoning_effort: None,
            reasoning_effort_off_wire: None,
            workspace_id: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stop_token_ids: Vec::new(),
            stop_boundary_strings: Vec::new(),
            extra_body: None,
        }
    }
}

/// One model served by an endpoint — a plain id plus optional per-model
/// overrides. The per-model fields let a single endpoint host models with
/// different token budgets, reasoning-effort surfaces, and vision support
/// (the user-facing feature this struct adds):
/// `max_context`/`max_output_tokens` override the endpoint-level values for
/// THIS model only, `reasoning_efforts` is the explicit allow-list for the
/// toolbar's reasoning-effort dropdown (`"off"` is always implicitly
/// allowed; empty = the app's default list), `reasoning_effort` is the
/// per-model default effort (unset = the endpoint's value applies), and
/// `multimodal` overrides the endpoint's vision flag so one endpoint can mix
/// text-only (e.g. GLM) and vision-capable (e.g. GLM flash) models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    /// The model id (matches a `ModelRef.model` / `default_model` value).
    pub id: String,
    /// Per-model max-context override (`None` = the endpoint's value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context: Option<usize>,
    /// Per-model max-output override (`None` = the endpoint's value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<usize>,
    /// Per-model reasoning-effort allow-list. `None`/empty = the endpoint's
    /// supported list applies (all standard values). Values pass through
    /// verbatim to the request field; `"off"` (omit the field) is always
    /// available regardless of this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<String>,
    /// Per-model reasoning-effort default. `None` = the endpoint's
    /// `reasoning_effort` applies (then the app default `"max"`); a value
    /// here wins for THIS model only. Resolved (off-encoded per provider +
    /// clamped by the model's own allow-list) by
    /// [`Endpoint::effective_reasoning_effort_for`]. The toolbar dropdown
    /// stays the runtime override on top of this persisted default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Per-model wire value for the effort `"off"` (overrides the
    /// endpoint's `reasoning_effort_off_wire`): the value sent in the
    /// `reasoning_effort` request field when the effective effort is
    /// `"off"`. `None` = the endpoint's value applies, else the built-in
    /// provider policy (DeepSeek-family model names → `"none"`, others
    /// omit the field). Resolved by
    /// [`Endpoint::reasoning_effort_off_wire_for`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort_off_wire: Option<String>,
    /// Per-model multimodal (vision) override. `None` = the endpoint's
    /// `multimodal` flag applies; `Some(true)` sends image blocks to this
    /// model even at a text-only endpoint; `Some(false)` strips them even at
    /// a multimodal endpoint. Resolved by [`Endpoint::multimodal_for`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multimodal: Option<bool>,
    /// Optional temperature override for request sampling (0.0 to 2.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Optional top_p override for request sampling (0.0 to 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Optional explicit stop sequences sent in the request body.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    /// Optional explicit stop token IDs sent in the request body (e.g. for vLLM/SGLang).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_token_ids: Vec<u64>,
    /// Vendor stop-boundary strings for this model (override the endpoint's
    /// `stop_boundary_strings`): sent in the request `stop` list AND
    /// enforced by the client-side SSE stream guard. Use for models whose
    /// tokenizer emits stop boundaries as raw text (GLM-5.3 role tags +
    /// newline cascades) — including aliases and fine-tunes a name-based
    /// code path could never cover.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_boundary_strings: Vec<String>,
    /// Arbitrary extra body parameters merged into the request payload.
    /// Merged last: keys here can override any request field including model/messages/stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_body: Option<toml::Table>,
}

impl Default for ModelSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            max_context: None,
            max_output_tokens: None,
            reasoning_efforts: Vec::new(),
            reasoning_effort: None,
            reasoning_effort_off_wire: None,
            multimodal: None,
            temperature: None,
            top_p: None,
            stop: Vec::new(),
            stop_token_ids: Vec::new(),
            stop_boundary_strings: Vec::new(),
            extra_body: None,
        }
    }
}

impl From<&str> for ModelSpec {
    /// A bare id string — the legacy form, with no per-model overrides.
    fn from(id: &str) -> Self {
        Self {
            id: id.to_string(),
            ..Default::default()
        }
    }
}

impl From<String> for ModelSpec {
    fn from(id: String) -> Self {
        Self::from(id.as_str())
    }
}

impl ModelSpec {
    /// Test-only baseline: a model spec with id "m" and default everything
    /// else. See [`Endpoint::test_default`] for the rationale and the
    /// `test-support` gating.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_default() -> Self {
        Self {
            id: "m".into(),
            ..Default::default()
        }
    }
}

/// Accept BOTH the legacy string-array `models = ["gpt-4o"]` and the new
/// `[[endpoint.models]]` table form (`id = "gpt-4o", max_context = ...`).
/// A string is wrapped into a bare [`ModelSpec`] (no per-model overrides);
/// a table is deserialized as-is.
fn deserialize_models<'de, D>(d: D) -> std::result::Result<Vec<ModelSpec>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ModelEntry {
        Id(String),
        Spec(ModelSpec),
    }
    let entries = Vec::<ModelEntry>::deserialize(d)?;
    Ok(entries
        .into_iter()
        .map(|e| match e {
            ModelEntry::Id(id) => ModelSpec::from(id),
            ModelEntry::Spec(spec) => spec,
        })
        .collect())
}

/// The standard reasoning-effort values (the app's default allow-list when
/// neither the endpoint nor the model configures one).
pub const DEFAULT_REASONING_EFFORTS: &[&str] = &["max", "high", "medium", "low", "minimal"];

/// Serde default for capability flags that should stay on when the key is
/// absent from older `endpoints.toml` files (back-compat).
fn default_true() -> bool {
    true
}

impl Endpoint {
    /// The ids of every model at this endpoint (in config order).
    pub fn model_ids(&self) -> Vec<String> {
        self.models.iter().map(|m| m.id.clone()).collect()
    }

    /// Whether the endpoint serves the given model id.
    pub fn has_model(&self, model_id: &str) -> bool {
        self.models.iter().any(|m| m.id == model_id)
    }

    /// The per-model config for the given model id. `None` when the endpoint
    /// has no such model (callers then fall back to endpoint-level values).
    pub fn model_spec(&self, model_id: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|m| m.id == model_id)
    }

    /// The effective `max_context` for a model: the model's per-model
    /// override, else the endpoint's value.
    pub fn max_context_for(&self, model_id: &str) -> Option<usize> {
        self.model_spec(model_id)
            .and_then(|m| m.max_context)
            .or(self.max_context)
    }

    /// The effective `max_output_tokens` for a model: the model's per-model
    /// override, else the endpoint's value.
    pub fn max_output_tokens_for(&self, model_id: &str) -> Option<usize> {
        self.model_spec(model_id)
            .and_then(|m| m.max_output_tokens)
            .or(self.max_output_tokens)
    }

    /// The effective multimodal (vision) flag for a model: the model's
    /// per-model override, else the endpoint's `multimodal` value.
    pub fn multimodal_for(&self, model_id: &str) -> bool {
        self.model_spec(model_id)
            .and_then(|m| m.multimodal)
            .unwrap_or(self.multimodal)
    }

    /// The effective `reasoning_effort` to send for a model.
    ///
    /// Returns `None` (omit the request field) when:
    /// - [`Self::supports_reasoning_effort`] is `false`, or
    /// - the configured value is `"off"` and no off-encoding applies (no
    ///   `reasoning_effort_off_wire` config and not a DeepSeek-family model).
    ///
    /// A configured `"off"` resolves to the model's
    /// `reasoning_effort_off_wire` override, else the endpoint's, else the
    /// built-in provider policy (`"none"` on DeepSeek-family model names,
    /// omitted otherwise — a literal `"off"` is an instant 400 on DeepSeek).
    /// Otherwise returns the configured value verbatim, or
    /// `"max"` when unset (the app default). The toolbar dropdown overrides
    /// this live via `set_model`, but still cannot force the field onto an
    /// endpoint that has `supports_reasoning_effort = false`.
    pub fn effective_reasoning_effort(&self) -> Option<String> {
        self.effective_reasoning_effort_for(None)
    }

    /// Like [`Self::effective_reasoning_effort`], but scoped to one model:
    /// the model's `reasoning_effort` override wins over the endpoint's
    /// value (unset = inherit), and the model's `reasoning_efforts`
    /// allow-list clamps the resolved value (an explicitly-configured value
    /// outside the list is clamped to the list's first entry — the list is
    /// the model's supported surface).
    pub fn effective_reasoning_effort_for(&self, model_id: Option<&str>) -> Option<String> {
        let spec = model_id.and_then(|id| self.model_spec(id));
        // Model-level override wins; else the endpoint's value; else "max".
        let configured = spec
            .and_then(|spec| spec.reasoning_effort.as_deref())
            .or(self.reasoning_effort.as_deref())
            .unwrap_or("max");
        self.normalize_reasoning_effort_for(model_id.unwrap_or_default(), configured)
    }

    /// Normalize a requested reasoning-effort value for a model: the
    /// [`Self::supports_reasoning_effort`] gate, the per-provider `"off"`
    /// wire encoding, and the model's `reasoning_efforts` allow-list clamp
    /// (a value outside the list is clamped to the list's first entry — the
    /// list is the model's supported surface).
    ///
    /// Shared by the configured-default resolution above and explicit
    /// requested values (a per-context `ModelRef::reasoning_effort`
    /// override), so both get identical treatment.
    pub fn normalize_reasoning_effort_for(
        &self,
        model_id: &str,
        requested: &str,
    ) -> Option<String> {
        if !self.supports_reasoning_effort {
            return None;
        }
        let allowed = self.model_spec(model_id).and_then(|spec| {
            if spec.reasoning_efforts.is_empty() {
                None
            } else {
                Some(spec.reasoning_efforts.as_slice())
            }
        });
        // The harness's "off" encodes per provider: omitted for most,
        // "none" (the enum's explicit thinking-off variant) for DeepSeek-family
        // models — a literal "off" is an instant non-retryable 400 there.
        // The endpoint/model `reasoning_effort_off_wire` config overrides the
        // name-based policy (the escape hatch for aliases/renames/fine-tunes);
        // the policy stays as the zero-config fallback.
        let off_wire = self
            .reasoning_effort_off_wire_for(model_id)
            .or_else(|| {
                reasoning_effort_off_wire_value(provider_kind(self.kind), model_id)
                    .map(str::to_string)
            });
        let off_wire = off_wire.as_deref();
        let resolved = match requested {
            "off" => off_wire.map(str::to_string),
            v => Some(v.to_string()),
        };
        let resolved = resolved?;
        match allowed {
            Some(list) if !list.iter().any(|e| e == &resolved) => {
                // The requested effort isn't in the model's list — clamp to
                // the list's first entry (the highest supported). "off" is a
                // valid *list* entry but must never become a literal request
                // value — encode it per provider.
                match list[0].as_str() {
                    "off" => off_wire.map(str::to_string),
                    v => Some(v.to_string()),
                }
            }
            _ => Some(resolved),
        }
    }

    /// The DISPLAY-space effective reasoning effort for a model — what the
    /// status bar shows. Same resolution chain as
    /// [`Self::effective_reasoning_effort_for`] (the model's
    /// `reasoning_effort` override → the endpoint's value → `"max"`), same
    /// [`Self::supports_reasoning_effort`] gate and `reasoning_efforts`
    /// allow-list clamp — but `"off"` stays `"off"`: the off-wire encoding
    /// (`"none"` on DeepSeek-family models, or a configured
    /// `reasoning_effort_off_wire`) is a WIRE concern; the UI vocabulary is
    /// always `"off" | "low" | "medium" | "high" | "max"`.
    pub fn display_reasoning_effort_for(&self, model_id: Option<&str>) -> String {
        if !self.supports_reasoning_effort {
            return "off".to_string();
        }
        let spec = model_id.and_then(|id| self.model_spec(id));
        let configured = spec
            .and_then(|spec| spec.reasoning_effort.as_deref())
            .or(self.reasoning_effort.as_deref())
            .unwrap_or("max");
        self.display_normalize_reasoning_effort_for(model_id.unwrap_or_default(), configured)
    }

    /// Display-space counterpart of [`Self::normalize_reasoning_effort_for`]:
    /// the same gate and allow-list clamp, but `"off"` is kept verbatim (no
    /// off-wire encoding) — the value the UI displays and the toolbar sends.
    pub fn display_normalize_reasoning_effort_for(
        &self,
        model_id: &str,
        requested: &str,
    ) -> String {
        if !self.supports_reasoning_effort {
            return "off".to_string();
        }
        // "off" is kept verbatim — the wire twin encodes it per provider
        // (off_wire value, or omitted); the UI vocabulary is always "off".
        // (Early-out BEFORE the allow-list clamp: an "off" request must not
        // clamp to a literal effort level.)
        if requested == "off" {
            return "off".to_string();
        }
        let allowed = self.model_spec(model_id).and_then(|spec| {
            if spec.reasoning_efforts.is_empty() {
                None
            } else {
                Some(spec.reasoning_efforts.as_slice())
            }
        });
        match allowed {
            Some(list) if !list.iter().any(|e| e == requested) => list[0].clone(),
            _ => requested.to_string(),
        }
    }

    /// The reasoning-effort allow-list for the dropdown, for one model:
    /// the model's explicit list, else the endpoint-level list (computed
    /// from [`Self::reasoning_effort`] + the kind defaults), else the app's
    /// standard list. `"off"` is appended when the endpoint supports the
    /// field — it omits the parameter and is always valid.
    pub fn reasoning_efforts_for(&self, model_id: Option<&str>) -> Vec<String> {
        if !self.supports_reasoning_effort {
            return Vec::new();
        }
        let model_list = model_id
            .and_then(|id| self.model_spec(id))
            .and_then(|spec| {
                if spec.reasoning_efforts.is_empty() {
                    None
                } else {
                    Some(spec.reasoning_efforts.as_slice())
                }
            });
        let mut list: Vec<String> = match model_list {
            Some(specs) => specs.iter().cloned().collect(),
            None => DEFAULT_REASONING_EFFORTS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        };
        if !list.iter().any(|e| e == "off") {
            list.push("off".to_string());
        }
        list
    }

    /// Effective temperature for a model: model's override, else endpoint's.
    pub fn temperature_for(&self, model_id: &str) -> Option<f64> {
        self.model_spec(model_id)
            .and_then(|m| m.temperature)
            .or(self.temperature)
    }

    /// Effective top_p for a model: model's override, else endpoint's.
    pub fn top_p_for(&self, model_id: &str) -> Option<f64> {
        self.model_spec(model_id)
            .and_then(|m| m.top_p)
            .or(self.top_p)
    }

    /// Effective stop strings for a model. If the model specifies any, those
    /// take precedence; otherwise endpoint's stop strings are used.
    pub fn stop_for(&self, model_id: &str) -> Vec<String> {
        self.model_spec(model_id)
            .filter(|m| !m.stop.is_empty())
            .map(|m| m.stop.clone())
            .unwrap_or_else(|| self.stop.clone())
    }

    /// Effective stop_token_ids for a model. If the model specifies any, those
    /// take precedence; otherwise endpoint's stop_token_ids are used.
    pub fn stop_token_ids_for(&self, model_id: &str) -> Vec<u64> {
        self.model_spec(model_id)
            .filter(|m| !m.stop_token_ids.is_empty())
            .map(|m| m.stop_token_ids.clone())
            .unwrap_or_else(|| self.stop_token_ids.clone())
    }

    /// Effective stop_boundary_strings for a model. If the model specifies
    /// any, those take precedence; otherwise the endpoint's are used.
    /// Boundary strings are vendor stop sequences the model's tokenizer
    /// emits as raw text (e.g. GLM-5.3's role-tag tokens plus newline
    /// cascades) — sent in the request `stop` list AND enforced by the
    /// client-side SSE stream guard, so a model with leaky stop tokens
    /// (including aliases and fine-tunes) is fixed by config, not code.
    pub fn stop_boundary_strings_for(&self, model_id: &str) -> Vec<String> {
        self.model_spec(model_id)
            .filter(|m| !m.stop_boundary_strings.is_empty())
            .map(|m| m.stop_boundary_strings.clone())
            .unwrap_or_else(|| self.stop_boundary_strings.clone())
    }

    /// The configured wire value for the literal effort `"off"` — the
    /// value sent in the `reasoning_effort` request field when the
    /// effective effort is `"off"`. The model's
    /// `reasoning_effort_off_wire` wins over the endpoint's; `None`
    /// (unset everywhere) means callers fall back to the built-in provider
    /// policy ([`reasoning_effort_off_wire_value`]: DeepSeek-family model
    /// names → `"none"`, others omit the field). Anthropic-kind endpoints
    /// always resolve `None` — the Messages API has no `reasoning_effort`
    /// field to encode. This is the config escape hatch for renamed,
    /// aliased, or fine-tuned models the name-based policy can't see.
    pub fn reasoning_effort_off_wire_for(&self, model_id: &str) -> Option<String> {
        if provider_kind(self.kind) == ProviderKind::Anthropic {
            return None;
        }
        self.model_spec(model_id)
            .and_then(|m| m.reasoning_effort_off_wire.clone())
            .or_else(|| self.reasoning_effort_off_wire.clone())
    }

    /// Effective extra_body for a model: merged with model-level overrides taking
    /// precedence over endpoint-level keys.
    pub fn extra_body_for(&self, model_id: &str) -> Option<toml::Table> {
        let ep_extra = self.extra_body.as_ref();
        let model_extra = self.model_spec(model_id).and_then(|m| m.extra_body.as_ref());
        match (ep_extra, model_extra) {
            (None, None) => None,
            (Some(ep), None) => Some(ep.clone()),
            (None, Some(m)) => Some(m.clone()),
            (Some(ep), Some(m)) => {
                let mut merged = ep.clone();
                for (k, v) in m {
                    merged.insert(k.clone(), v.clone());
                }
                Some(merged)
            }
        }
    }

    /// Effective extra_body for a model converted to a JSON object map.
    pub fn extra_body_json_for(
        &self,
        model_id: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.extra_body_for(model_id).map(|t| toml_table_to_json_map(&t))
    }

    /// Test-only baseline: a local OpenAI-compatible endpoint named "test"
    /// serving one model "m" at `http://localhost:9999/v1/`.
    ///
    /// Tests override just the fields they care about via struct-update
    /// syntax (`..Endpoint::test_default()`), so adding a field to
    /// [`Endpoint`] only requires updating the struct, its [`Default`] impl,
    /// and this factory — never the dozens of test call sites. Compiled for
    /// this crate's own tests (`cfg(test)`) and for dependents' tests via
    /// the `test-support` cargo feature (enabled through dev-dependencies,
    /// e.g. src-tauri's test suite); never in release builds.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_default() -> Self {
        Self {
            name: "test".into(),
            base_url: "http://localhost:9999/v1/".into(),
            models: vec![ModelSpec::test_default()],
            ..Default::default()
        }
    }
}

/// Convert a TOML table into a JSON object map.
pub fn toml_table_to_json_map(table: &toml::Table) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for (k, v) in table {
        map.insert(k.clone(), toml_value_to_json(v));
    }
    map
}

/// Convert a TOML value into a JSON value.
pub fn toml_value_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => serde_json::Value::Object(toml_table_to_json_map(t)),
    }
}

/// What kind of provider an endpoint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointKind {
    /// Full OpenAI API — all capabilities.
    OpenAI,
    /// Self-hosted OpenAI-compatible (Ollama/vLLM/LM Studio) — degraded capabilities.
    Local,
    /// Native Anthropic Messages API (`/v1/messages`). Works with any
    /// Anthropic-compatible provider (Anthropic, OpenRouter, gateways) via a
    /// configurable `base_url` + stored key.
    Anthropic,
}

/// Load endpoints + pricing from `endpoints.toml`, or return empty lists if
/// missing.
///
/// Forgiving normalization (user-reported): a `base_url` missing the trailing
/// slash is fixed up here too — the save path already appends one
/// (`validate_endpoint`), but hand-edited files bypass it, and the URL
/// builders all tolerate both forms only by trimming at call time. Keeping
/// the loaded config consistent avoids every consumer re-implementing the
/// check.
pub fn load_or_default(path: &Path) -> Result<(Vec<Endpoint>, Vec<PricingEntry>)> {
    if !path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut file: EndpointsFile = toml::from_str(&text)?;
    for ep in &mut file.endpoint {
        let trimmed = ep.base_url.trim();
        ep.base_url = if trimmed.is_empty() {
            // An empty base_url stays empty (the save path rejects it; the
            // load path must not fabricate "/" — that would mask the
            // emptiness and fail confusingly at request time, review L1).
            String::new()
        } else if trimmed.ends_with('/') {
            trimmed.to_string()
        } else {
            format!("{trimmed}/")
        };
    }
    Ok((file.endpoint, file.pricing))
}

/// Write endpoints + pricing to `endpoints.toml`, serializing the `[[endpoint]]`
/// and `[[pricing]]` tables from the in-memory values. Fully rewrites the file
/// (the schema is the source of truth), so unknown keys are dropped. Uses a
/// temp-file + rename so a crash mid-write leaves the previous file intact.
pub fn save(path: &Path, endpoints: &[Endpoint], pricing: &[PricingEntry]) -> Result<()> {
    let file = EndpointsFile {
        endpoint: endpoints.to_vec(),
        pricing: pricing.to_vec(),
    };
    let text = toml::to_string_pretty(&file)?;
    crate::config::write_atomic(path, &text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_normalizes_missing_trailing_slash() {
        // Regression (backlog 2026-08-22): a hand-edited endpoints.toml with a
        // base_url missing the trailing slash loads normalized to end with
        // '/'. The save path already appends one; the load path must forgive
        // hand edits too.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        std::fs::write(
            &path,
            r#"
[[endpoint]]
name = "ex"
kind = "openai"
base_url = "https://example.com/v1"
models = ["m"]
"#,
        )
        .unwrap();
        let (endpoints, _) = load_or_default(&path).unwrap();
        assert_eq!(endpoints[0].base_url, "https://example.com/v1/");
    }

    #[test]
    fn load_preserves_slashed_and_path_urls() {
        // A URL that already ends with '/' (and carries a path) loads
        // unchanged — normalization must never rewrite a good value.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        std::fs::write(
            &path,
            r#"
[[endpoint]]
name = "ex"
kind = "openai"
base_url = "https://opencode.ai/zen/go/v1/"
models = ["m"]
"#,
        )
        .unwrap();
        let (endpoints, _) = load_or_default(&path).unwrap();
        assert_eq!(endpoints[0].base_url, "https://opencode.ai/zen/go/v1/");
    }

    #[test]
    fn load_leaves_empty_base_url_empty() {
        // Review 2026-08-22 L1: an empty base_url must load as "" (not "/") —
        // the save path rejects it; the load path must not fabricate a
        // slash-only URL that masks the emptiness.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        std::fs::write(
            &path,
            r#"
[[endpoint]]
name = "ex"
kind = "openai"
base_url = ""
models = ["m"]
"#,
        )
        .unwrap();
        let (endpoints, _) = load_or_default(&path).unwrap();
        assert_eq!(endpoints[0].base_url, "");
    }

    #[test]
    fn parses_two_endpoints() {
        let text = r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o", "gpt-4o-mini"]

[[endpoint]]
name = "ollama-local"
kind = "local"
base_url = "http://localhost:11434/v1/"
models = ["llama3.1", "qwen2.5"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint.len(), 2);
        assert_eq!(file.endpoint[0].name, "openai");
        assert_eq!(file.endpoint[0].kind, EndpointKind::OpenAI);
        assert_eq!(file.endpoint[1].kind, EndpointKind::Local);
        assert_eq!(file.endpoint[1].model_ids(), vec!["llama3.1", "qwen2.5"]);
    }

    #[test]
    fn workspace_id_round_trips_and_none_skips_serialization() {
        let text = r#"
[[endpoint]]
name = "claude"
kind = "anthropic"
base_url = "https://api.anthropic.com/v1/"
models = ["claude-sonnet-4-5"]
workspace_id = "ws_abc123"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].workspace_id.as_deref(), Some("ws_abc123"));
        // Absent field → None; and None must NOT serialize (back-compat:
        // hand-maintained endpoints.toml files stay unchanged).
        let mut bare = file.endpoint[0].clone();
        bare.workspace_id = None;
        let ser = toml::to_string_pretty(&EndpointsFile {
            endpoint: vec![bare],
            pricing: vec![],
        })
        .unwrap();
        assert!(
            !ser.contains("workspace_id"),
            "None workspace_id must be skipped when serializing"
        );
    }

    #[test]
    fn parses_anthropic_kind() {
        let text = r#"
[[endpoint]]
name = "claude"
kind = "anthropic"
base_url = "https://api.anthropic.com/v1/"
models = ["claude-sonnet-4-5", "claude-opus-4-1"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint.len(), 1);
        assert_eq!(file.endpoint[0].kind, EndpointKind::Anthropic);
        assert_eq!(
            file.endpoint[0].model_ids(),
            vec!["claude-sonnet-4-5", "claude-opus-4-1"]
        );
    }

    #[test]
    fn empty_file_gives_empty_list() {
        let file: EndpointsFile = toml::from_str("").unwrap();
        assert!(file.endpoint.is_empty());
    }

    #[test]
    fn missing_models_defaults_empty() {
        let text = r#"
[[endpoint]]
name = "bare"
kind = "local"
base_url = "http://localhost:11434/v1/"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert!(file.endpoint[0].models.is_empty());
    }

    #[test]
    fn max_context_override_parsed() {
        let text = r#"
[[endpoint]]
name = "gateway"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["glm-5.2"]
max_context = 64000
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].max_context, Some(64000));
    }

    #[test]
    fn max_context_defaults_none() {
        let text = r#"
[[endpoint]]
name = "default"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].max_context, None);
    }

    #[test]
    fn multimodal_defaults_false() {
        let text = r#"
[[endpoint]]
name = "text-only"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["glm-5.2"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert!(
            !file.endpoint[0].multimodal,
            "multimodal should default to false"
        );
    }

    #[test]
    fn multimodal_true_parsed() {
        let text = r#"
[[endpoint]]
name = "qwen-vl"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["qwen-vl"]
multimodal = true
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert!(
            file.endpoint[0].multimodal,
            "multimodal should parse as true"
        );
    }

    #[test]
    fn per_model_multimodal_overrides_endpoint_flag() {
        // One Ollama-style endpoint hosting a text-only model and a
        // vision-capable model (backlog a634835c): the per-model flag
        // overrides the endpoint default, models without one inherit it.
        let ep = Endpoint {
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
        };
        // The vision-capable model overrides the endpoint default…
        assert!(ep.multimodal_for("glm-flash"));
        // …the text-only model inherits the endpoint's `false`…
        assert!(!ep.multimodal_for("glm"));
        // …and unknown ids fall back to the endpoint level too (the
        // vision/embedder paths may reference models outside the list).
        assert!(!ep.multimodal_for("ghost"));
    }

    #[test]
    fn per_model_multimodal_false_forces_off_at_multimodal_endpoint() {
        let ep = Endpoint {
            name: "vision-host".into(),
            base_url: "https://vision.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "text-only".into(),
                multimodal: Some(false),
                ..ModelSpec::test_default()
            }],
            multimodal: true,
            ..Endpoint::test_default()
        };
        // The explicit `false` wins over the endpoint's `true`…
        assert!(!ep.multimodal_for("text-only"));
        // …while models without a flag inherit the endpoint's `true`.
        assert!(ep.multimodal_for("other-vision"));
    }

    #[test]
    fn per_model_multimodal_round_trips_through_toml() {
        let text = r#"
[[endpoint]]
name = "ollama"
kind = "local"
base_url = "http://localhost:11434/v1/"
multimodal = false

[[endpoint.models]]
id = "glm"

[[endpoint.models]]
id = "glm-flash"
multimodal = true
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        let ep = &file.endpoint[0];
        assert!(!ep.multimodal_for("glm"));
        assert!(ep.multimodal_for("glm-flash"));
        // A save round-trip keeps the override and omits the unset field.
        let out = toml::to_string_pretty(&file).unwrap();
        let reparsed: EndpointsFile = toml::from_str(&out).unwrap();
        assert_eq!(reparsed.endpoint[0].models[0].multimodal, None);
        assert_eq!(reparsed.endpoint[0].models[1].multimodal, Some(true));
        assert!(reparsed.endpoint[0].multimodal_for("glm-flash"));
    }

    #[test]
    fn reasoning_effort_parsed() {
        let text = r#"
[[endpoint]]
name = "reasoning"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["glm-5.2"]
reasoning_effort = "max"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn reasoning_effort_defaults_none() {
        let text = r#"
[[endpoint]]
name = "default"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].reasoning_effort, None);
    }

    #[test]
    fn parses_per_model_table_form() {
        // The new [[endpoint.models]] table form: each model carries its own
        // caps + reasoning-effort list.
        let text = r#"
[[endpoint]]
name = "gateway"
kind = "openai"
base_url = "https://gateway.example.com/v1/"

[[endpoint.models]]
id = "big-model"
max_context = 200000
max_output_tokens = 16384
reasoning_efforts = ["max", "high"]

[[endpoint.models]]
id = "fast-model"
max_context = 64000
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        let ep = &file.endpoint[0];
        assert_eq!(ep.model_ids(), vec!["big-model", "fast-model"]);
        assert_eq!(
            ep.model_spec("big-model").unwrap().max_context,
            Some(200000)
        );
        assert_eq!(
            ep.model_spec("big-model").unwrap().max_output_tokens,
            Some(16384)
        );
        assert_eq!(
            ep.model_spec("big-model").unwrap().reasoning_efforts,
            vec!["max", "high"]
        );
        assert_eq!(
            ep.model_spec("fast-model").unwrap().max_context,
            Some(64000)
        );
        assert!(ep
            .model_spec("fast-model")
            .unwrap()
            .reasoning_efforts
            .is_empty());
    }

    #[test]
    fn legacy_string_models_still_parse() {
        // Back-compat: an endpoints.toml written before the per-model tables
        // (models as a plain string array) must load unchanged.
        let text = r#"
[[endpoint]]
name = "legacy"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o", "gpt-4o-mini"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        let ep = &file.endpoint[0];
        assert_eq!(ep.model_ids(), vec!["gpt-4o", "gpt-4o-mini"]);
        for spec in &ep.models {
            assert_eq!(spec.max_context, None);
            assert_eq!(spec.max_output_tokens, None);
            assert!(spec.reasoning_efforts.is_empty());
        }
    }

    #[test]
    fn per_model_caps_override_endpoint_level() {
        let mut ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![
                ModelSpec::from("big"),
                ModelSpec {
                    id: "small".into(),
                    max_context: Some(8000),
                    max_output_tokens: Some(2048),
                    ..ModelSpec::test_default()
                },
            ],
            max_context: Some(128000),
            max_output_tokens: Some(8192),
            ..Endpoint::test_default()
        };
        // The per-model config wins for the configured model…
        assert_eq!(ep.max_context_for("small"), Some(8000));
        assert_eq!(ep.max_output_tokens_for("small"), Some(2048));
        // …and the endpoint-level value applies to models without one.
        assert_eq!(ep.max_context_for("big"), Some(128000));
        assert_eq!(ep.max_output_tokens_for("big"), Some(8192));
        // An unknown model id falls back to the endpoint level too (the
        // vision/embedder paths may reference models outside the list).
        assert_eq!(ep.max_context_for("ghost"), Some(128000));
        ep.models.clear();
        // An endpoint with no models must not panic (returns the endpoint
        // value / None instead).
        assert_eq!(ep.max_context_for("ghost"), Some(128000));
    }

    #[test]
    fn effective_reasoning_effort_clamps_into_model_list() {
        // A model whose allow-list excludes the endpoint's configured effort
        // gets clamped to the list's first (highest) entry.
        let mut ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_efforts: vec!["medium".into(), "low".into()],
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("max".into()),
            ..Endpoint::test_default()
        };
        // "max" isn't in the model's list → clamp to "medium" (first entry).
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("medium".into())
        );
        // The endpoint-level (model-less) view is unchanged.
        assert_eq!(ep.effective_reasoning_effort(), Some("max".into()));
        // An allowed value passes through.
        ep.reasoning_effort = Some("low".into());
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("low".into())
        );
    }

    #[test]
    fn normalize_reasoning_effort_for_requested_value() {
        // The requested-value seam shared with the configured-default
        // resolution: verbatim pass-through, allow-list clamp, and the
        // supports gate.
        let ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_efforts: vec!["medium".into(), "low".into()],
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        // An allowed requested value passes through verbatim.
        assert_eq!(
            ep.normalize_reasoning_effort_for("glm", "low"),
            Some("low".into())
        );
        // A requested value outside the model's list clamps to the first
        // entry — same treatment as a configured value.
        assert_eq!(
            ep.normalize_reasoning_effort_for("glm", "max"),
            Some("medium".into())
        );
        // The supports gate drops any requested value.
        let mut ep = ep;
        ep.supports_reasoning_effort = false;
        assert_eq!(ep.normalize_reasoning_effort_for("glm", "low"), None);
    }

    #[test]
    fn normalize_reasoning_effort_for_off_encodes_per_provider() {
        // A requested "off" encodes exactly like a configured one: "none"
        // for DeepSeek-family models, omitted for others.
        let deepseek = Endpoint {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1/".into(),
            models: vec![ModelSpec {
                id: "deepseek-v4-flash".into(),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            deepseek.normalize_reasoning_effort_for("deepseek-v4-flash", "off"),
            Some("none".into()),
            "DeepSeek-family models must encode \"off\" as \"none\""
        );
        let other = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "gpt-test".into(),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            other.normalize_reasoning_effort_for("gpt-test", "off"),
            None,
            "other providers omit the field for \"off\""
        );
    }

    #[test]
    fn effective_reasoning_effort_off_only_list_never_sends_literal_off() {
        // Regression (review M1): a model whose allow-list is ["off"] with an
        // endpoint effort outside the list must clamp to NONE (omit the
        // field) — never a literal `Some("off")` request value, which
        // providers reject (only the *omission* of the field is valid).
        let ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_efforts: vec!["off".into()],
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("max".into()),
            ..Endpoint::test_default()
        };
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            None,
            "\"off\" as the clamped value must mean omit the field"
        );
    }

    #[test]
    fn effective_reasoning_effort_off_maps_to_none_for_deepseek() {
        // Regression (DeepSeek instant-400): "off" is not in DeepSeek's
        // reasoning_effort enum (none|minimal|low|medium|high|xhigh|max) — the
        // harness's off encodes as the enum's explicit thinking-off value
        // "none" for DeepSeek-family models.
        let ep = Endpoint {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1/".into(),
            models: vec![ModelSpec {
                id: "deepseek-v4-flash".into(),
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("off".into()),
            ..Endpoint::test_default()
        };
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("deepseek-v4-flash")),
            Some("none".into()),
            "DeepSeek-family models must encode \"off\" as \"none\""
        );
    }

    #[test]
    fn effective_reasoning_effort_off_clamp_maps_to_none_for_deepseek() {
        // The clamp arm must encode "off" the same way for DeepSeek-family
        // models: clamping into an allow-list that lands on "off" yields
        // "none", never a literal "off" request value.
        let ep = Endpoint {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1/".into(),
            models: vec![ModelSpec {
                id: "deepseek-v4-flash".into(),
                reasoning_efforts: vec!["off".into()],
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("max".into()),
            ..Endpoint::test_default()
        };
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("deepseek-v4-flash")),
            Some("none".into()),
            "the clamped \"off\" must encode as \"none\" for DeepSeek-family"
        );
    }

    #[test]
    fn effective_reasoning_effort_off_wire_config_overrides_policy() {
        // The endpoint/model `reasoning_effort_off_wire` config is the escape
        // hatch for what the name-based policy can't see: a configured value
        // encodes "off" regardless of the model's name (renamed/aliased/
        // fine-tuned DeepSeek models), and the model-level entry wins over
        // the endpoint's.
        let ep = Endpoint {
            name: "custom-llm".into(),
            base_url: "http://localhost:8000/v1/".into(),
            models: vec![
                ModelSpec {
                    id: "my-finetune".into(),
                    reasoning_effort_off_wire: Some("disabled".into()),
                    ..ModelSpec::test_default()
                },
                ModelSpec {
                    id: "other-model".into(),
                    ..ModelSpec::test_default()
                },
            ],
            reasoning_effort: Some("off".into()),
            reasoning_effort_off_wire: Some("none".into()),
            ..Endpoint::test_default()
        };
        // Model-level config wins regardless of the (non-DeepSeek) name.
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("my-finetune")),
            Some("disabled".into()),
            "model-level reasoning_effort_off_wire must encode \"off\" regardless of model name"
        );
        // Endpoint-level config applies to models without their own entry.
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("other-model")),
            Some("none".into()),
            "endpoint-level reasoning_effort_off_wire must apply to models without their own"
        );
    }

    #[test]
    fn effective_reasoning_effort_off_wire_config_beats_deepseek_policy() {
        // When both the config and the name-based policy would apply, the
        // explicit config wins — the user's setting is authoritative over
        // the zero-config default.
        let ep = Endpoint {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1/".into(),
            models: vec![ModelSpec {
                id: "deepseek-v4-flash".into(),
                reasoning_effort_off_wire: Some("disabled".into()),
                ..ModelSpec::test_default()
            }],
            reasoning_effort: Some("off".into()),
            ..Endpoint::test_default()
        };
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("deepseek-v4-flash")),
            Some("disabled".into()),
            "an explicit reasoning_effort_off_wire must override the policy default"
        );
    }

    #[test]
    fn reasoning_effort_off_wire_for_ignores_anthropic_kind() {
        // The Messages API has no reasoning_effort field to encode — an
        // anthropic endpoint never resolves an off wire value, even when
        // the config is present (misconfiguration stays inert).
        let ep = Endpoint {
            name: "claude".into(),
            kind: EndpointKind::Anthropic,
            base_url: "https://api.anthropic.com/".into(),
            reasoning_effort: Some("off".into()),
            reasoning_effort_off_wire: Some("none".into()),
            ..Endpoint::test_default()
        };
        assert_eq!(ep.reasoning_effort_off_wire_for("m"), None);
        assert_eq!(ep.effective_reasoning_effort_for(Some("m")), None);
    }

    #[test]
    fn parses_and_resolves_reasoning_effort_off_wire() {
        let text = r#"
[[endpoint]]
name = "custom-llm"
kind = "openai"
base_url = "http://localhost:8000/v1/"
reasoning_effort = "off"
reasoning_effort_off_wire = "none"

[[endpoint.models]]
id = "my-finetune"
reasoning_effort_off_wire = "disabled"

[[endpoint.models]]
id = "other-model"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint.len(), 1);
        let ep = &file.endpoint[0];

        // Model-level wins; endpoint-level applies to models without their
        // own entry (including unknown ids).
        assert_eq!(
            ep.reasoning_effort_off_wire_for("my-finetune"),
            Some("disabled".into())
        );
        assert_eq!(
            ep.reasoning_effort_off_wire_for("other-model"),
            Some("none".into())
        );
        assert_eq!(
            ep.reasoning_effort_off_wire_for("unknown-model"),
            Some("none".into())
        );
        // The effective effort for "off" follows the config, not the name.
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("my-finetune")),
            Some("disabled".into())
        );
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("other-model")),
            Some("none".into())
        );
    }

    #[test]
    fn effective_reasoning_effort_model_level_beats_endpoint_level() {
        // A per-model `reasoning_effort` (the per-model row dropdown) wins
        // over the endpoint's value; an unset model inherits the endpoint's;
        // both unset falls back to the app default "max".
        let mut ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![
                ModelSpec {
                    id: "glm".into(),
                    reasoning_effort: Some("low".into()),
                    ..ModelSpec::test_default()
                },
                ModelSpec {
                    id: "glm-flash".into(),
                    ..ModelSpec::test_default()
                },
            ],
            reasoning_effort: Some("high".into()),
            ..Endpoint::test_default()
        };
        // Model-level wins.
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("low".into())
        );
        // Unset model inherits the endpoint's value.
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm-flash")),
            Some("high".into())
        );
        // An unknown model id falls back to the endpoint level too.
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("ghost")),
            Some("high".into())
        );
        // The endpoint-level (model-less) view is unchanged.
        assert_eq!(ep.effective_reasoning_effort(), Some("high".into()));
        // Both unset → the app default "max" — and the model-level override
        // still wins when the endpoint's value is unset.
        ep.reasoning_effort = None;
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm-flash")),
            Some("max".into())
        );
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("low".into())
        );
    }

    #[test]
    fn effective_reasoning_effort_model_level_off_encodes_per_provider() {
        // A per-model "off" goes through the same per-provider encoding as
        // the endpoint-level value: "none" for DeepSeek-family models,
        // omitted (None) elsewhere.
        let deepseek = Endpoint {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1/".into(),
            models: vec![ModelSpec {
                id: "deepseek-v4-flash".into(),
                reasoning_effort: Some("off".into()),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            deepseek.effective_reasoning_effort_for(Some("deepseek-v4-flash")),
            Some("none".into()),
            "a per-model \"off\" must encode as \"none\" for DeepSeek-family"
        );
        let other = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_effort: Some("off".into()),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            other.effective_reasoning_effort_for(Some("glm")),
            None,
            "a per-model \"off\" must omit the field for non-DeepSeek providers"
        );
    }

    #[test]
    fn effective_reasoning_effort_model_level_clamps_into_own_list() {
        // The model's own allow-list clamps its per-model effort the same
        // way it clamps the endpoint's: outside the list → the list's first
        // entry; an off-only list clamps to omission (never a literal
        // "off" request value).
        let mut ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_effort: Some("max".into()),
                reasoning_efforts: vec!["medium".into(), "low".into()],
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("medium".into()),
            "a per-model effort outside the model's own list clamps to the first entry"
        );
        // An in-list per-model value passes through verbatim.
        ep.models[0].reasoning_effort = Some("low".into());
        assert_eq!(
            ep.effective_reasoning_effort_for(Some("glm")),
            Some("low".into())
        );
        let off_only = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_effort: Some("high".into()),
                reasoning_efforts: vec!["off".into()],
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            off_only.effective_reasoning_effort_for(Some("glm")),
            None,
            "clamping a per-model effort into an off-only list must omit the field"
        );
    }

    #[test]
    fn reasoning_efforts_for_includes_off_and_uses_model_list() {
        let ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_efforts: vec!["medium".into(), "low".into()],
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        // The model's list + the always-available "off".
        assert_eq!(
            ep.reasoning_efforts_for(Some("glm")),
            vec!["medium", "low", "off"]
        );
        // No model → the standard list + "off".
        assert_eq!(
            ep.reasoning_efforts_for(None),
            vec!["max", "high", "medium", "low", "minimal", "off"]
        );
        // Unsupported endpoints get an empty list (the UI hides the dropdown).
        let mut no_effort = ep.clone();
        no_effort.supports_reasoning_effort = false;
        assert!(no_effort.reasoning_efforts_for(Some("glm")).is_empty());
    }

    #[test]
    fn effective_reasoning_effort_defaults_to_max() {
        let text = r#"
[[endpoint]]
name = "default"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(
            file.endpoint[0].effective_reasoning_effort(),
            Some("max".into())
        );
    }

    #[test]
    fn effective_reasoning_effort_passes_value_verbatim() {
        let text = r#"
[[endpoint]]
name = "reasoning"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["glm-5.2"]
reasoning_effort = "high"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(
            file.endpoint[0].effective_reasoning_effort(),
            Some("high".into())
        );
    }

    #[test]
    fn effective_reasoning_effort_off_omits_field() {
        let text = r#"
[[endpoint]]
name = "off"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["glm-5.2"]
reasoning_effort = "off"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].effective_reasoning_effort(), None);
    }

    #[test]
    fn supports_reasoning_effort_defaults_true() {
        let text = r#"
[[endpoint]]
name = "default"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert!(
            file.endpoint[0].supports_reasoning_effort,
            "supports_reasoning_effort should default to true for back-compat"
        );
    }

    #[test]
    fn supports_reasoning_effort_false_parsed() {
        let text = r#"
[[endpoint]]
name = "chat"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["gpt-4o-mini"]
supports_reasoning_effort = false
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert!(!file.endpoint[0].supports_reasoning_effort);
    }

    #[test]
    fn effective_reasoning_effort_omits_when_unsupported() {
        // Even with an explicit effort (or the default max), unsupported
        // endpoints must never surface a value for the request body.
        let text = r#"
[[endpoint]]
name = "chat"
kind = "openai"
base_url = "https://gateway.example.com/v1/"
models = ["gpt-4o-mini"]
supports_reasoning_effort = false
reasoning_effort = "high"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint[0].effective_reasoning_effort(), None);
    }

    #[test]
    fn display_reasoning_effort_for_mirrors_chain_but_keeps_off() {
        // REGRESSION (status-bar effort stale, backlog 51dab4da): the
        // DISPLAY-space resolution mirrors the wire chain (per-model
        // override → endpoint value → "max", unsupported → "off") but "off"
        // stays "off" — never the off-wire encoding ("none" on DeepSeek),
        // because the UI vocabulary is off|low|medium|high|max.
        let mut ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![
                ModelSpec {
                    id: "glm".into(),
                    reasoning_effort: Some("low".into()),
                    ..ModelSpec::test_default()
                },
                ModelSpec {
                    id: "glm-off".into(),
                    reasoning_effort: Some("off".into()),
                    ..ModelSpec::test_default()
                },
            ],
            reasoning_effort: Some("high".into()),
            ..Endpoint::test_default()
        };
        // Per-model override wins.
        assert_eq!(ep.display_reasoning_effort_for(Some("glm")), "low");
        // A per-model "off" stays "off" (the wire twin encodes it).
        assert_eq!(ep.display_reasoning_effort_for(Some("glm-off")), "off");
        // Unset model inherits the endpoint's value.
        assert_eq!(ep.display_reasoning_effort_for(Some("ghost")), "high");
        // Both unset → the app default "max".
        ep.reasoning_effort = None;
        assert_eq!(ep.display_reasoning_effort_for(Some("ghost")), "max");
        // Unsupported → "off" regardless of configured values.
        ep.supports_reasoning_effort = false;
        assert_eq!(ep.display_reasoning_effort_for(Some("glm")), "off");
    }

    #[test]
    fn display_reasoning_effort_deepseek_off_stays_off() {
        // The DeepSeek-family off-wire policy ("none") must NOT leak into
        // the display value — the status bar shows "off" while the wire
        // twin encodes "none" (the pair differs in vocabulary, not in
        // resolution).
        let deepseek = Endpoint {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1/".into(),
            models: vec![ModelSpec {
                id: "deepseek-v4-flash".into(),
                reasoning_effort: Some("off".into()),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        assert_eq!(
            deepseek.display_reasoning_effort_for(Some("deepseek-v4-flash")),
            "off"
        );
        assert_eq!(
            deepseek.effective_reasoning_effort_for(Some("deepseek-v4-flash")),
            Some("none".into())
        );
    }

    #[test]
    fn display_normalize_reasoning_effort_for_clamps_and_keeps_off() {
        // The display normalize: gate → "off", allow-list clamp, "off" kept
        // verbatim even when not in the list (the wire twin encodes it per
        // provider instead of clamping).
        let mut ep = Endpoint {
            name: "gateway".into(),
            base_url: "https://gateway.example.com/v1/".into(),
            models: vec![ModelSpec {
                id: "glm".into(),
                reasoning_efforts: vec!["low".into(), "medium".into()],
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        };
        // In-list value passes through.
        assert_eq!(ep.display_normalize_reasoning_effort_for("glm", "low"), "low");
        // Out-of-list clamps to the list's first entry.
        assert_eq!(ep.display_normalize_reasoning_effort_for("glm", "max"), "low");
        // "off" is kept verbatim even when not in the list.
        assert_eq!(ep.display_normalize_reasoning_effort_for("glm", "off"), "off");
        // Unsupported → "off".
        ep.supports_reasoning_effort = false;
        assert_eq!(ep.display_normalize_reasoning_effort_for("glm", "low"), "off");
    }

    #[test]
    fn pricing_parsed() {
        let text = r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o", "gpt-4o-mini"]

[[pricing]]
model = "gpt-4o"
input_per_1m = 2.50
output_per_1m = 10.00
cached_per_1m = 1.25

[[pricing]]
model = "gpt-4o-mini"
input_per_1m = 0.15
output_per_1m = 0.60
cached_per_1m = 0.075
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.pricing.len(), 2);
        assert_eq!(file.pricing[0].model, "gpt-4o");
        assert_eq!(file.pricing[0].input_per_1m, 2.50);
        assert_eq!(file.pricing[0].output_per_1m, 10.00);
        assert_eq!(file.pricing[0].cached_per_1m, 1.25);
        assert_eq!(file.pricing[1].model, "gpt-4o-mini");
    }

    #[test]
    fn pricing_absent_defaults_empty() {
        let text = r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert!(file.pricing.is_empty());
    }

    #[test]
    fn save_round_trips_endpoints_and_pricing() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        let endpoints = vec![
            Endpoint {
                name: "openai".into(),
                base_url: "https://api.openai.com/v1/".into(),
                models: vec!["gpt-4o".into(), "gpt-4o-mini".into()],
                max_context: Some(128000),
                max_output_tokens: Some(16384),
                multimodal: true,
                reasoning_effort: Some("high".into()),
                ..Endpoint::test_default()
            },
            Endpoint {
                name: "ollama-local".into(),
                kind: EndpointKind::Local,
                base_url: "http://localhost:11434/v1/".into(),
                models: vec!["llama3.1".into()],
                supports_reasoning_effort: false,
                ..Endpoint::test_default()
            },
        ];
        let pricing = vec![PricingEntry {
            model: "gpt-4o".into(),
            input_per_1m: 2.5,
            output_per_1m: 10.0,
            cached_per_1m: 1.25,
        }];
        save(&path, &endpoints, &pricing).unwrap();

        let (reloaded_eps, reloaded_pricing) = load_or_default(&path).unwrap();
        assert_eq!(reloaded_eps.len(), 2);
        assert_eq!(reloaded_eps[0].name, "openai");
        assert_eq!(reloaded_eps[0].kind, EndpointKind::OpenAI);
        assert_eq!(reloaded_eps[0].base_url, "https://api.openai.com/v1/");
        assert_eq!(reloaded_eps[0].model_ids(), vec!["gpt-4o", "gpt-4o-mini"]);
        assert_eq!(reloaded_eps[0].max_context, Some(128000));
        assert_eq!(reloaded_eps[0].max_output_tokens, Some(16384));
        assert!(reloaded_eps[0].multimodal);
        assert!(reloaded_eps[0].supports_reasoning_effort);
        assert_eq!(reloaded_eps[0].reasoning_effort.as_deref(), Some("high"));
        assert_eq!(reloaded_eps[1].kind, EndpointKind::Local);
        assert!(!reloaded_eps[1].supports_reasoning_effort);
        assert_eq!(reloaded_eps[1].reasoning_effort, None);
        assert_eq!(reloaded_pricing.len(), 1);
        assert_eq!(reloaded_pricing[0].model, "gpt-4o");
    }

    #[test]
    fn save_empty_writes_loadable_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        save(&path, &[], &[]).unwrap();
        let (eps, pricing) = load_or_default(&path).unwrap();
        assert!(eps.is_empty());
        assert!(pricing.is_empty());
    }

    #[test]
    fn parses_and_resolves_sampling_stop_and_extra_body() {
        let text = r#"
[[endpoint]]
name = "vllm-glm"
kind = "openai"
base_url = "http://localhost:8000/v1/"
temperature = 0.7
top_p = 0.95
stop = ["<|endoftext|>"]
stop_token_ids = [151329]
stop_boundary_strings = ["\u003C|endoftext|\u003E"]

[endpoint.extra_body]
chat_template = "glm"
seed = 42

[[endpoint.models]]
id = "glm-5.3-flash"
temperature = 0.2
top_p = 0.8
stop = ["<|endoftext|>", "<|user|>", "<|assistant|>", "<|observation|>", "\n\n\n\n"]
stop_token_ids = [151329, 151330, 151336]
stop_boundary_strings = ["\u003C|endoftext|\u003E", "\u003C|user|\u003E", "\u003C|assistant|\u003E", "\u003C|observation|\u003E", "\n\n\n\n", "\n\n\n"]

[endpoint.models.extra_body]
repetition_penalty = 1.05
seed = 100

[[endpoint.models]]
id = "generic-model"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        assert_eq!(file.endpoint.len(), 1);
        let ep = &file.endpoint[0];

        // For glm-5.3-flash: model-level overrides apply
        assert_eq!(ep.temperature_for("glm-5.3-flash"), Some(0.2));
        assert_eq!(ep.top_p_for("glm-5.3-flash"), Some(0.8));
        assert_eq!(
            ep.stop_for("glm-5.3-flash"),
            vec![
                "<|endoftext|>",
                "<|user|>",
                "<|assistant|>",
                "<|observation|>",
                "\n\n\n\n"
            ]
        );
        assert_eq!(
            ep.stop_token_ids_for("glm-5.3-flash"),
            vec![151329, 151330, 151336]
        );
        assert_eq!(
            ep.stop_boundary_strings_for("glm-5.3-flash"),
            vec![
                "\u{3c}|endoftext|\u{3e}",
                "\u{3c}|user|\u{3e}",
                "\u{3c}|assistant|\u{3e}",
                "\u{3c}|observation|\u{3e}",
                "\n\n\n\n",
                "\n\n\n"
            ]
        );
        let glm_extra = ep.extra_body_json_for("glm-5.3-flash").unwrap();
        assert_eq!(glm_extra.get("chat_template").unwrap(), "glm");
        assert_eq!(glm_extra.get("seed").unwrap(), 100);
        assert_eq!(glm_extra.get("repetition_penalty").unwrap(), 1.05);

        // For generic-model: falls back to endpoint-level configuration
        assert_eq!(ep.temperature_for("generic-model"), Some(0.7));
        assert_eq!(ep.top_p_for("generic-model"), Some(0.95));
        assert_eq!(ep.stop_for("generic-model"), vec!["<|endoftext|>"]);
        assert_eq!(ep.stop_token_ids_for("generic-model"), vec![151329]);
        assert_eq!(
            ep.stop_boundary_strings_for("generic-model"),
            vec!["\u{3c}|endoftext|\u{3e}"]
        );
        let generic_extra = ep.extra_body_json_for("generic-model").unwrap();
        assert_eq!(generic_extra.get("chat_template").unwrap(), "glm");
        assert_eq!(generic_extra.get("seed").unwrap(), 42);
        assert!(generic_extra.get("repetition_penalty").is_none());
    }

    #[test]
    fn stop_boundary_strings_for_resolves_model_then_endpoint() {
        // Model-level entries win; models without them (known or unknown)
        // fall back to the endpoint's; an endpoint with none configured
        // resolves none — the provider never matches model-name prefixes.
        // (Tag literals use \u escapes on both sides of the parse — never
        // raw angle-bracket text, which text transports can strip.)
        let text = r#"
[[endpoint]]
name = "mixed"
kind = "openai"
base_url = "http://localhost:8000/v1/"
stop_boundary_strings = ["\u003C|endpoint|\u003E"]

[[endpoint.models]]
id = "aliased-model"
stop_boundary_strings = ["\u003C|model|\u003E"]

[[endpoint.models]]
id = "plain-model"

[[endpoint]]
name = "bare"
kind = "openai"
base_url = "http://localhost:8001/v1/"

[[endpoint.models]]
id = "some-model"
"#;
        let file: EndpointsFile = toml::from_str(text).unwrap();
        let mixed = &file.endpoint[0];
        let bare = &file.endpoint[1];
        assert_eq!(
            mixed.stop_boundary_strings_for("aliased-model"),
            vec!["\u{3c}|model|\u{3e}"]
        );
        assert_eq!(
            mixed.stop_boundary_strings_for("plain-model"),
            vec!["\u{3c}|endpoint|\u{3e}"]
        );
        assert_eq!(
            mixed.stop_boundary_strings_for("unknown-model"),
            vec!["\u{3c}|endpoint|\u{3e}"]
        );
        assert!(bare.stop_boundary_strings_for("some-model").is_empty());
    }
}
