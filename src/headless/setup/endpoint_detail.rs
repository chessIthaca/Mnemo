// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The rich endpoint + tier settings surface (Mnemo2's Settings dialog).
//!
//! The lean [`EndpointDto`](super::EndpointDto) carries bare model ids; this
//! module adds per-model specs, per-model pricing, the `[tiers]` table and
//! the usage/test helpers a settings UI needs. Everything persists into the
//! same `endpoints.toml` / `keys.toml` Mnemo1 reads, preserving every field
//! the DTOs do not carry.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::Setup;
use crate::config::keys::Secret;
use crate::config::{Config, Endpoint, ModelSpec, PricingEntry, Tier, TierLadder};
use crate::headless::error::EngineError;

/// Per-1M-token model prices (USD).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PriceDto {
    /// $ per 1M input tokens.
    pub input_per_1m: f64,
    /// $ per 1M output tokens.
    pub output_per_1m: f64,
    /// $ per 1M cached input tokens.
    pub cached_per_1m: f64,
}

/// One model of an endpoint with its spec and price.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelDetailDto {
    /// The model id.
    pub id: String,
    /// INPUT ONLY: the id this model had before a rename, so tier entries
    /// and settings follow it. Omitted on output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_id: Option<String>,
    /// Context window override.
    #[serde(default)]
    pub max_context: Option<usize>,
    /// Output-token cap override.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    /// Allowed reasoning efforts (empty = endpoint defaults).
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
    /// Default reasoning effort.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Wire value that switches reasoning off.
    #[serde(default)]
    pub reasoning_effort_off_wire: Option<String>,
    /// Multimodal override (`None` = inherit the endpoint's flag).
    #[serde(default)]
    pub multimodal: Option<bool>,
    /// Sampling temperature override.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Price, when configured.
    #[serde(default)]
    pub price: Option<PriceDto>,
}

/// One endpoint with all of its models (the Mnemo2 settings wire).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EndpointDetailDto {
    /// Unique endpoint name.
    pub name: String,
    /// `"openai"`, `"local"` or `"anthropic"`.
    pub kind: String,
    /// The base URL.
    pub base_url: String,
    /// OUTPUT ONLY: whether a key is available.
    #[serde(default)]
    pub has_api_key: bool,
    /// Endpoint-level context-window cap.
    #[serde(default)]
    pub max_context: Option<usize>,
    /// Endpoint-level output-token cap.
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    /// Whether the endpoint's models are multimodal by default.
    #[serde(default)]
    pub multimodal: bool,
    /// Whether the endpoint accepts reasoning-effort overrides.
    #[serde(default)]
    pub supports_reasoning_effort: bool,
    /// Endpoint-level default reasoning effort.
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Strict tool-schema support (`None` = auto).
    #[serde(default)]
    pub supports_strict_schema: Option<bool>,
    /// Anthropic workspace id.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Endpoint-level sampling temperature.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// The endpoint's models, in order.
    #[serde(default)]
    pub models: Vec<ModelDetailDto>,
}

/// One rung in the settings wire representation of the unified ladder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierDto {
    /// Stable tier id.
    pub id: String,
    /// Editable display name.
    pub name: String,
    /// Editable routing definition.
    pub definition: String,
    /// Ordered `endpoint/model` specs, first = default.
    #[serde(default)]
    pub models: Vec<String>,
    /// Optional reasoning-effort override.
    #[serde(default)]
    pub effort: Option<String>,
    /// `"escalate"` on the escalation-only rung, otherwise `null`.
    #[serde(default)]
    pub role: Option<String>,
    /// The rung this tier currently folds down to (output-only).
    #[serde(default)]
    pub resolves_to: String,
}

/// The five-tier ladder in cheapest-first order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TiersDto {
    /// Exactly `trivial`, `routine`, `medium`, `high`, `escalate`, in that order.
    pub tiers: Vec<TierDto>,
}

impl Default for TiersDto {
    fn default() -> Self {
        TierLadder::default().into()
    }
}

impl From<TierLadder> for TiersDto {
    fn from(ladder: TierLadder) -> Self {
        let tiers = ladder
            .ordered()
            .iter()
            .map(|tier| TierDto {
                id: tier.id.clone(),
                name: tier.name.clone(),
                definition: tier.definition.clone(),
                models: tier.models.clone(),
                effort: tier.effort.clone(),
                role: tier.role.clone(),
                resolves_to: ladder
                    .resolve(&tier.id)
                    .map(|(ran_tier, _)| ran_tier)
                    .unwrap_or_default(),
            })
            .collect();
        Self { tiers }
    }
}

impl TryFrom<TiersDto> for TierLadder {
    type Error = EngineError;

    fn try_from(dto: TiersDto) -> Result<Self, Self::Error> {
        let ladder = Self {
            tiers: dto
                .tiers
                .into_iter()
                .map(|tier| Tier {
                    id: tier.id,
                    name: tier.name,
                    definition: tier.definition,
                    models: tier.models,
                    effort: tier.effort,
                    role: tier.role,
                })
                .collect(),
        };
        ladder.validate().map_err(err)?;
        Ok(ladder)
    }
}

/// The outcome of an endpoint connection test.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointTestDto {
    /// Whether `/models` answered.
    pub ok: bool,
    /// How many models the provider listed (0 unless `ok`).
    pub model_count: usize,
    /// The error text (None on success).
    pub error: Option<String>,
}

/// One model a provider reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModelDto {
    /// The model id.
    pub id: String,
    /// Whether the provider reports image input support.
    pub vision_capable: bool,
    /// Reported context window.
    pub context_length: Option<u64>,
    /// Reported output cap.
    pub max_output_tokens: Option<u64>,
    /// Reported reasoning efforts, highest first.
    pub effort_levels: Option<Vec<String>>,
}

/// Split an `endpoint/model` spec against the configured endpoints.
fn resolve_spec<'a>(endpoints: &'a [Endpoint], spec: &'a str) -> Option<(&'a Endpoint, &'a str)> {
    endpoints.iter().find_map(|e| {
        let rest = spec.strip_prefix(e.name.as_str())?.strip_prefix('/')?;
        e.has_model(rest).then_some((e, rest))
    })
}

fn err(message: impl Into<String>) -> EngineError {
    EngineError::start_failed(message.into())
}

impl Setup {
    /// Re-publish the on-disk config to the attached engine (no-op without
    /// one): the shared config + the model resolver, so the very next spawn
    /// sees new endpoints, models and keys.
    pub async fn refresh_live(&self) {
        let Some(live) = &self.live else { return };
        if let Ok(cfg) = self.load_config() {
            crate::host::rewire::sync_model_resolver(Some(&live.model_resolver), &cfg);
            *live.config.lock().await = cfg;
        }
    }

    fn endpoint_detail_dto(cfg: &Config, e: &Endpoint, has_key: bool) -> EndpointDetailDto {
        EndpointDetailDto {
            name: e.name.clone(),
            kind: crate::config::kind_wire(e.kind).to_string(),
            base_url: e.base_url.clone(),
            has_api_key: has_key,
            max_context: e.max_context,
            max_output_tokens: e.max_output_tokens,
            multimodal: e.multimodal,
            supports_reasoning_effort: e.supports_reasoning_effort,
            reasoning_effort: e.reasoning_effort.clone(),
            supports_strict_schema: e.supports_strict_schema,
            workspace_id: e.workspace_id.clone(),
            temperature: e.temperature,
            models: e
                .models
                .iter()
                .map(|m| ModelDetailDto {
                    id: m.id.clone(),
                    previous_id: None,
                    max_context: m.max_context,
                    max_output_tokens: m.max_output_tokens,
                    reasoning_efforts: m.reasoning_efforts.clone(),
                    reasoning_effort: m.reasoning_effort.clone(),
                    reasoning_effort_off_wire: m.reasoning_effort_off_wire.clone(),
                    multimodal: m.multimodal,
                    temperature: m.temperature,
                    price: cfg.pricing_for(&m.id).map(|p| PriceDto {
                        input_per_1m: p.input_per_1m,
                        output_per_1m: p.output_per_1m,
                        cached_per_1m: p.cached_per_1m,
                    }),
                })
                .collect(),
        }
    }

    /// The configured endpoints with per-model specs and prices.
    pub fn endpoints_detail(&self) -> Result<Vec<EndpointDetailDto>, EngineError> {
        let cfg = self.load_config()?;
        let keys_path = self.config_dir.join("keys.toml");
        Ok(cfg
            .endpoints
            .iter()
            .map(|e| {
                Self::endpoint_detail_dto(&cfg, e, self.key_source.has_key(&keys_path, &e.name))
            })
            .collect())
    }

    fn endpoints_path(&self) -> std::path::PathBuf {
        self.config_dir.join("endpoints.toml")
    }

    /// Create or update one endpoint with its models and prices.
    /// `previous_name` (when it differs from `dto.name`) renames the
    /// endpoint: its key moves, the default provider and every tier entry
    /// follow. Model renames ride [`ModelDetailDto::previous_id`]. Fields the
    /// DTO does not carry (top_p, stop sequences, extra_body, ...) are
    /// preserved from the previous entry.
    pub async fn upsert_endpoint_detail(
        &self,
        dto: EndpointDetailDto,
        previous_name: Option<String>,
    ) -> Result<(), EngineError> {
        let current = self.load_config()?;
        let new_name = dto.name.trim().to_string();
        let prev_name = previous_name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| new_name.clone());
        let prev_ep = current
            .endpoints
            .iter()
            .find(|e| e.name == prev_name)
            .cloned();
        if prev_name != new_name {
            if prev_ep.is_none() {
                return Err(err(format!("endpoint '{prev_name}' does not exist")));
            }
            if current.endpoints.iter().any(|e| e.name == new_name) {
                return Err(err(format!("endpoint '{new_name}' already exists")));
            }
        }
        if new_name.contains('/') {
            return Err(err("endpoint names must not contain '/'"));
        }
        // Model list: unique ids, price sanity.
        let mut seen = HashSet::new();
        for m in &dto.models {
            let id = m.id.trim();
            if id.is_empty() {
                return Err(err("model ids must not be empty"));
            }
            if !seen.insert(id.to_string()) {
                return Err(err(format!(
                    "duplicate model '{id}' in endpoint '{new_name}'"
                )));
            }
            if let Some(p) = &m.price {
                for v in [p.input_per_1m, p.output_per_1m, p.cached_per_1m] {
                    if !v.is_finite() || v < 0.0 {
                        return Err(err(format!(
                            "model '{id}': prices must be non-negative numbers"
                        )));
                    }
                }
            }
        }
        // Per-model specs, based on the previous spec so unknown fields stay.
        let mut rename: HashMap<String, String> = HashMap::new();
        let mut model_configs = Vec::new();
        for m in &dto.models {
            let id = m.id.trim().to_string();
            let prior_id = m
                .previous_id
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or(&id)
                .to_string();
            if prior_id != id {
                rename.insert(prior_id.clone(), id.clone());
            }
            let mut spec: ModelSpec = prev_ep
                .as_ref()
                .and_then(|e| e.model_spec(&prior_id))
                .cloned()
                .unwrap_or_default();
            spec.id = id;
            spec.max_context = m.max_context;
            spec.max_output_tokens = m.max_output_tokens;
            spec.reasoning_efforts = m.reasoning_efforts.clone();
            spec.reasoning_effort = m.reasoning_effort.clone().filter(|s| !s.trim().is_empty());
            spec.reasoning_effort_off_wire = m
                .reasoning_effort_off_wire
                .clone()
                .filter(|s| !s.trim().is_empty());
            spec.multimodal = m.multimodal;
            spec.temperature = m.temperature;
            model_configs.push(spec);
        }
        let mut endpoint =
            crate::config::patch::validate_endpoint(crate::config::patch::EndpointDraft {
                name: new_name.clone(),
                kind: dto.kind.clone(),
                base_url: dto.base_url.clone(),
                models: dto.models.iter().map(|m| m.id.clone()).collect(),
                model_configs,
                max_context: dto.max_context,
                max_output_tokens: dto.max_output_tokens,
                multimodal: dto.multimodal,
                supports_reasoning_effort: dto.supports_reasoning_effort,
                reasoning_effort: dto
                    .reasoning_effort
                    .clone()
                    .filter(|s| !s.trim().is_empty()),
                workspace_id: dto.workspace_id.clone(),
            })
            .map_err(err)?;
        endpoint.temperature = dto.temperature;
        endpoint.supports_strict_schema = dto.supports_strict_schema;
        if let Some(prev) = &prev_ep {
            endpoint.reasoning_effort_off_wire = prev.reasoning_effort_off_wire.clone();
            endpoint.top_p = prev.top_p;
            endpoint.stop = prev.stop.clone();
            endpoint.stop_token_ids = prev.stop_token_ids.clone();
            endpoint.stop_boundary_strings = prev.stop_boundary_strings.clone();
            endpoint.extra_body = prev.extra_body.clone();
        }

        // The endpoint set (replace in place, or append).
        let mut built = current.endpoints.clone();
        match built.iter().position(|e| e.name == prev_name) {
            Some(i) => built[i] = endpoint.clone(),
            None => built.push(endpoint.clone()),
        }

        // Keys: the survivors' keys; a renamed endpoint's key moves.
        let mut api_keys: HashMap<String, String> = HashMap::new();
        for ep in &built {
            let source = if ep.name == new_name {
                &prev_name
            } else {
                &ep.name
            };
            if let Some(k) = current.keys.get(source) {
                api_keys.insert(ep.name.clone(), k.to_string());
            }
        }

        // Defaults follow a rename; a vanished default model is refused.
        let mut default_provider = current.general.general.default_provider.clone();
        let mut default_model = current.general.general.default_model.clone();
        let hosts_default = default_provider.as_deref() == Some(prev_name.as_str())
            || (default_provider.is_none()
                && current.endpoints.first().map(|e| e.name.as_str()) == Some(prev_name.as_str()));
        if default_provider.as_deref() == Some(prev_name.as_str()) {
            default_provider = Some(new_name.clone());
        }
        if hosts_default {
            if let Some(dm) = default_model.clone() {
                if let Some(renamed) = rename.get(&dm) {
                    default_model = Some(renamed.clone());
                } else if prev_ep.as_ref().is_some_and(|p| p.has_model(&dm))
                    && !endpoint.has_model(&dm)
                {
                    return Err(err(format!(
                        "'{dm}' is the configured default model of '{prev_name}'; \
                         change the default model before removing it"
                    )));
                }
            }
        }
        crate::config::patch::validate_endpoint_set(
            &built,
            default_provider.as_deref(),
            default_model.as_deref(),
            default_model.as_deref(),
        )
        .map_err(err)?;

        // Pricing: written for this endpoint's models; orphans dropped.
        let others: HashSet<&str> = built
            .iter()
            .filter(|e| e.name != new_name)
            .flat_map(|e| e.models.iter().map(|m| m.id.as_str()))
            .collect();
        let mut pricing: Vec<PricingEntry> = current.pricing.clone();
        let prior_ids: Vec<String> = prev_ep
            .as_ref()
            .map(|e| e.models.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        for old in &prior_ids {
            if !endpoint.has_model(old) && !others.contains(old.as_str()) {
                pricing.retain(|p| &p.model != old);
            }
        }
        for m in &dto.models {
            let id = m.id.trim();
            match &m.price {
                Some(p) => {
                    let entry = PricingEntry {
                        model: id.to_string(),
                        input_per_1m: p.input_per_1m,
                        output_per_1m: p.output_per_1m,
                        cached_per_1m: p.cached_per_1m,
                    };
                    match pricing.iter_mut().find(|e| e.model == id) {
                        Some(slot) => *slot = entry,
                        None => pricing.push(entry),
                    }
                }
                None if !others.contains(id) => pricing.retain(|e| e.model != id),
                None => {}
            }
        }

        // Tier entries follow renames and drop removed models.
        let old_tiers = crate::config::endpoints::load_tier_ladder(&self.endpoints_path(), None)
            .map_err(|e| err(e.to_string()))?;
        let mut tiers = old_tiers.clone();
        for tier in &mut tiers.tiers {
            let list = &mut tier.models;
            let mut out: Vec<String> = Vec::new();
            for spec in list.iter() {
                let rewritten = match resolve_spec(&current.endpoints, spec) {
                    Some((e, model)) if e.name == prev_name => {
                        let id = rename.get(model).map(String::as_str).unwrap_or(model);
                        endpoint.has_model(id).then(|| format!("{new_name}/{id}"))
                    }
                    _ => Some(spec.clone()),
                };
                if let Some(s) = rewritten {
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
            *list = out;
        }
        if old_tiers.first_model("trivial").is_some()
            && tiers.first_model("trivial").is_none()
        {
            return Err(err(format!(
                "The trivial tier needs a model; give it another model before removing {prev_name}"
            )));
        }

        let mut new_config = crate::config::patch::apply_endpoints(
            &current,
            built,
            default_provider,
            default_model,
            &api_keys,
        );
        new_config.pricing = pricing;
        if let Some(slot) = new_config
            .endpoints
            .iter_mut()
            .find(|e| e.name == new_name)
        {
            *slot = endpoint;
        }
        new_config
            .save_all(&self.config_dir)
            .map_err(|e| err(e.to_string()))?;
        if tiers != old_tiers {
            crate::config::endpoints::save_tier_ladder(&self.endpoints_path(), &tiers)
                .map_err(|e| err(e.to_string()))?;
        }
        self.refresh_live().await;
        Ok(())
    }

    /// Remove an endpoint, its key, its tier entries and any price no other
    /// endpoint still needs; then refresh the attached engine.
    pub async fn remove_endpoint_applied(&self, name: &str) -> Result<(), EngineError> {
        self.remove_endpoint(name)?;
        self.refresh_live().await;
        Ok(())
    }

    /// Store an endpoint key and refresh the attached engine.
    pub async fn set_api_key_applied(
        &self,
        endpoint_name: &str,
        secret: Secret,
    ) -> Result<(), EngineError> {
        if !self.endpoints()?.iter().any(|e| e.name == endpoint_name) {
            return Err(err(format!("endpoint '{endpoint_name}' does not exist")));
        }
        self.set_api_key(endpoint_name, secret)?;
        self.refresh_live().await;
        Ok(())
    }

    /// Try the provider's `/models` with the given (or saved) credentials.
    pub async fn test_endpoint_detail(
        &self,
        endpoint_name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
        kind: Option<String>,
    ) -> EndpointTestDto {
        match self
            .list_models(endpoint_name, base_url, api_key, kind)
            .await
        {
            Ok(models) => EndpointTestDto {
                ok: true,
                model_count: models.len(),
                error: None,
            },
            Err(e) => EndpointTestDto {
                ok: false,
                model_count: 0,
                error: Some(e.to_string()),
            },
        }
    }

    /// The provider's live model list as a plain DTO.
    pub async fn list_provider_models(
        &self,
        endpoint_name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
        kind: Option<String>,
    ) -> Result<Vec<ProviderModelDto>, EngineError> {
        Ok(self
            .list_models(endpoint_name, base_url, api_key, kind)
            .await?
            .into_iter()
            .map(|m| ProviderModelDto {
                id: m.id,
                vision_capable: m.vision_capable,
                context_length: m.context_length,
                max_output_tokens: m.max_output_tokens,
                effort_levels: m.effort_levels,
            })
            .collect())
    }

    /// The configured tiers.
    pub fn tiers(&self) -> Result<TiersDto, EngineError> {
        Ok(self.load_config()?.tiers.into())
    }

    /// Validate and persist the tiers: every spec must resolve to a
    /// configured `endpoint/model`. Blank entries and duplicates are dropped.
    pub async fn set_tiers(&self, tiers: TiersDto) -> Result<TiersDto, EngineError> {
        let cfg = self.load_config()?;
        let mut tiers: TierLadder = tiers.try_into()?;
        for tier in &mut tiers.tiers {
            let name = &tier.id;
            let list = &mut tier.models;
            let mut out: Vec<String> = Vec::new();
            for spec in list.iter() {
                let spec = spec.trim();
                if spec.is_empty() {
                    continue;
                }
                if resolve_spec(&cfg.endpoints, spec).is_none() {
                    return Err(err(format!(
                        "tier '{name}': '{spec}' is not a configured endpoint/model"
                    )));
                }
                if !out.iter().any(|s| s == spec) {
                    out.push(spec.to_string());
                }
            }
            *list = out;
        }
        crate::config::endpoints::save_tier_ladder(&self.endpoints_path(), &tiers)
            .map_err(|e| err(e.to_string()))?;
        Ok(tiers.into())
    }

    /// What still references an endpoint: tier entries and the configured
    /// default model. Empty = safe to delete.
    pub fn endpoint_usage(&self, name: &str) -> Result<Vec<String>, EngineError> {
        let cfg = self.load_config()?;
        let mut out = Vec::new();
        let prefix = format!("{name}/");
        for tier in cfg.tiers.ordered() {
            for spec in tier.models.iter().filter(|s| s.starts_with(&prefix)) {
                out.push(format!("tier {}: {spec}", tier.id));
            }
        }
        let g = &cfg.general.general;
        let host = g
            .default_provider
            .clone()
            .or_else(|| cfg.endpoints.first().map(|e| e.name.clone()));
        if host.as_deref() == Some(name) {
            if let Some(dm) = &g.default_model {
                out.push(format!("default model: {name}/{dm}"));
            } else {
                out.push("default provider".to_string());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_with(toml: &str) -> (tempfile::TempDir, Setup) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("endpoints.toml"), toml).unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[general]\ndefault_provider = \"a\"\ndefault_model = \"m1\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("keys.toml"), "[a]\napi_key = \"sk-a\"\n").unwrap();
        let setup = Setup::open(Some(dir.path().to_path_buf())).unwrap();
        (dir, setup)
    }

    fn models<'a>(dto: &'a TiersDto, id: &str) -> &'a Vec<String> {
        &dto.tiers
            .iter()
            .find(|tier| tier.id == id)
            .expect("tier")
            .models
    }

    fn valid_dto() -> TiersDto {
        let mut dto = TiersDto::default();
        dto.tiers[0].models = vec!["a/m2".into()];
        dto
    }

    pub(super) const BASE: &str = r#"
[[endpoint]]
name = "a"
kind = "openai"
base_url = "http://127.0.0.1:9/v1/"
models = ["m1", "m2"]
top_p = 0.5
stop = ["END"]

[[endpoint]]
name = "b"
kind = "openai"
base_url = "http://127.0.0.1:9/v1/"
models = ["x"]

[[pricing]]
model = "m1"
input_per_1m = 1.0
output_per_1m = 2.0
cached_per_1m = 0.5

[tiers]
difficult = ["a/m1", "b/x"]
simple = ["a/m2"]
"#;

    #[test]
    fn tiers_round_trip_and_absence() {
        let (dir, setup) = setup_with(BASE);
        let t = setup.tiers().unwrap();
        assert_eq!(models(&t, "high").as_slice(), ["a/m1", "b/x"]);
        assert!(models(&t, "routine").is_empty());
        std::fs::write(
            dir.path().join("endpoints.toml"),
            "[[endpoint]]\nname = \"a\"\nkind = \"openai\"\nbase_url = \"http://h/\"\nmodels = [\"m1\"]\n",
        )
        .unwrap();
        assert_eq!(setup.tiers().unwrap(), TiersDto::default());
        let cfg = Config::load(dir.path()).unwrap();
        cfg.save_all(dir.path()).unwrap();
        let text = std::fs::read_to_string(dir.path().join("endpoints.toml")).unwrap();
        assert!(!text.contains("[[tier]]"));
        assert!(!text.contains("[tiers]"));
    }

    #[test]
    fn mnemo1_save_preserves_tiers() {
        let (dir, _setup) = setup_with(BASE);
        let current = Config::load(dir.path()).unwrap();
        let built = current.endpoints.clone();
        let new = crate::config::patch::apply_endpoints(
            &current,
            built,
            Some("a".into()),
            Some("m1".into()),
            &HashMap::new(),
        );
        new.save_all(dir.path()).unwrap();
        let tiers = crate::config::endpoints::load_tier_ladder(
            &dir.path().join("endpoints.toml"),
            None,
        )
        .unwrap();
        assert_eq!(tiers.get("high").unwrap().models, vec!["a/m1", "b/x"]);
        assert_eq!(tiers.get("trivial").unwrap().models, vec!["a/m2"]);
    }

    #[tokio::test]
    async fn set_tiers_rejects_unknown_specs() {
        let (_dir, setup) = setup_with(BASE);
        let empty = setup.set_tiers(TiersDto::default()).await.unwrap_err().to_string();
        assert!(empty.contains("lowest tier 'trivial'"), "{empty}");

        let mut bad = valid_dto();
        bad.tiers[1].models = vec!["a/nope".into()];
        let e = setup.set_tiers(bad).await.unwrap_err().to_string();
        assert!(e.contains("a/nope") && e.contains("routine"), "{e}");
        let mut ok = valid_dto();
        ok.tiers[1].models = vec![" b/x ".into(), "b/x".into(), "".into()];
        let saved = setup.set_tiers(ok).await.unwrap();
        assert_eq!(models(&saved, "routine").as_slice(), ["b/x"]);
        assert_eq!(
            saved.tiers.iter().find(|tier| tier.id == "high").unwrap().resolves_to,
            "routine"
        );
        assert_eq!(models(&setup.tiers().unwrap(), "routine").as_slice(), ["b/x"]);
    }

    #[tokio::test]
    async fn rename_rewrites_tiers_and_keeps_extras() {
        let (dir, setup) = setup_with(BASE);
        let mut dto = setup.endpoints_detail().unwrap().remove(0);
        assert!(dto.has_api_key);
        assert_eq!(dto.models[0].price.unwrap().input_per_1m, 1.0);
        dto.name = "alpha".into();
        dto.models[0].previous_id = Some("m1".into());
        dto.models[0].id = "m1-new".into();
        dto.models[0].price = Some(PriceDto {
            input_per_1m: 3.0,
            output_per_1m: 4.0,
            cached_per_1m: 1.0,
        });
        setup
            .upsert_endpoint_detail(dto, Some("a".into()))
            .await
            .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(
            cfg.general.general.default_provider.as_deref(),
            Some("alpha")
        );
        assert_eq!(cfg.general.general.default_model.as_deref(), Some("m1-new"));
        let ep = cfg.endpoint("alpha").unwrap();
        assert_eq!(ep.top_p, Some(0.5));
        assert_eq!(ep.stop, vec!["END"]);
        assert_eq!(cfg.key_for("alpha"), Some("sk-a"));
        assert!(cfg.pricing_for("m1").is_none());
        assert_eq!(cfg.pricing_for("m1-new").unwrap().output_per_1m, 4.0);
        let t = setup.tiers().unwrap();
        assert_eq!(models(&t, "high").as_slice(), ["alpha/m1-new", "b/x"]);
        assert_eq!(models(&t, "trivial").as_slice(), ["alpha/m2"]);
    }

    #[tokio::test]
    async fn upsert_preserves_pricing_and_drops_removed_model_from_tiers() {
        let (dir, setup) = setup_with(BASE);
        let mut dto = setup.endpoints_detail().unwrap().remove(1); // b
        dto.models.push(ModelDetailDto {
            id: "y".into(),
            price: Some(PriceDto {
                input_per_1m: 9.0,
                output_per_1m: 9.0,
                cached_per_1m: 9.0,
            }),
            ..Default::default()
        });
        setup.upsert_endpoint_detail(dto, None).await.unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.pricing_for("m1").unwrap().input_per_1m, 1.0);
        assert_eq!(cfg.pricing_for("y").unwrap().input_per_1m, 9.0);
        // Now drop model x from b: its tier entry goes.
        let mut dto = setup.endpoints_detail().unwrap().remove(1);
        dto.models.retain(|m| m.id != "x");
        setup.upsert_endpoint_detail(dto, None).await.unwrap();
        assert_eq!(models(&setup.tiers().unwrap(), "high").as_slice(), ["a/m1"]);
    }

    #[tokio::test]
    async fn remove_drops_tier_entries_and_reports_usage() {
        let (_dir, setup) = setup_with(BASE);
        let usage = setup.endpoint_usage("b").unwrap();
        assert_eq!(usage, vec!["tier high: b/x"]);
        let usage = setup.endpoint_usage("a").unwrap();
        assert!(usage.iter().any(|u| u == "default model: a/m1"), "{usage:?}");
        setup.remove_endpoint_applied("b").await.unwrap();
        assert_eq!(models(&setup.tiers().unwrap(), "high").as_slice(), ["a/m1"]);
        assert!(setup.endpoint_usage("b").unwrap().is_empty());
    }

    #[tokio::test]
    async fn default_model_removal_is_refused() {
        let (_dir, setup) = setup_with(BASE);
        let mut dto = setup.endpoints_detail().unwrap().remove(0);
        dto.models.retain(|m| m.id != "m1");
        let e = setup.upsert_endpoint_detail(dto, None).await.unwrap_err();
        assert!(e.to_string().contains("default model"), "{e}");
    }
}


#[cfg(test)]
mod review_regression_tests {
    use super::*;

    fn setup_with(toml: &str) -> (tempfile::TempDir, Setup) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("endpoints.toml"), toml).unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[general]\ndefault_provider = \"a\"\ndefault_model = \"m1\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("keys.toml"), "[a]\napi_key = \"sk-a\"\n").unwrap();
        let setup = Setup::open(Some(dir.path().to_path_buf())).unwrap();
        (dir, setup)
    }

    #[tokio::test]
    async fn removing_the_last_trivial_model_is_refused_without_writing() {
        let (dir, setup) = setup_with(&super::tests::BASE.replace(
            "simple = [\"a/m2\"]",
            "simple = [\"b/x\"]",
        ));
        // Complete the one-time legacy conversion before taking the snapshot.
        setup.tiers().unwrap();
        let path = dir.path().join("endpoints.toml");
        let before = std::fs::read_to_string(&path).unwrap();

        let error = setup.remove_endpoint_applied("b").await.unwrap_err().to_string();
        assert!(
            error.ends_with(
                "The trivial tier needs a model; give it another model before removing b"
            ),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
    }

    #[tokio::test]
    async fn set_tiers_preserves_the_original_lane_migration_backup() {
        let (dir, setup) = setup_with(super::tests::BASE);
        let original = r#"[general]
default_provider = "a"
default_model = "m1"

[general.routing.cheap]
endpoint = "a"
model = "m1"
reasoning_effort = "minimal"
"#;
        std::fs::write(dir.path().join("config.toml"), original).unwrap();
        let mut dto = TiersDto::default();
        dto.tiers[0].models = vec!["a/m2".into()];
        setup.set_tiers(dto).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.lanes.bak")).unwrap(),
            original
        );
    }
}
