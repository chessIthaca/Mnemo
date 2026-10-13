// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The standalone setup + config API (§5.8 :486-589, U8) — everything the
//! GUI Settings, the project picker, and the first-run screens do,
//! reachable from code so a standalone install has no GUI dependency.
//!
//! Job 4 lands it in steps: D1 (this module) carries the readiness
//! report — [`Setup::open`], [`Setup::status`] (the [`SetupStatus`]
//! snapshot + the ordered [`SetupStep`] to-do list) and the
//! [`Engine::start`](crate::headless::Engine::start) precondition (an
//! unready config fails fast with error code `not_configured` instead
//! of deep inside the brain; a missing embedding model is NOT fatal —
//! the engine runs on the deterministic hash fallback and `semantic:
//! false` reports it). The mutating surface (endpoints + keys D2,
//! settings D4, the classifier D5, the embeddings D6, the project group
//! D8) threads through this type.
//!
//! G4 (§5.9.4 :676-677): this module never CALLS `global_config_dir` —
//! the dir arrives explicitly (or via
//! [`crate::config::resolve_config_dir`], which owns the `$MNEMO_HOME`
//! default) and every derivation helper lives in `config/`.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::keys::{KeySource, Secret};
use crate::config::{Config, EndpointKind};
use crate::headless::error::EngineError;
use crate::memory::classifier::ClassifierStatus;
use crate::memory::embedder::EmbedderStatus;

/// One to-do of a first-run flow (§5.8 :570) — the ordered list a setup
/// UI walks the user through. [`SetupStep::DefaultModel`] is the only
/// FATAL leg (it gates `Engine::start` for a caller without an injected
/// provider); the others are opt-in conveniences the engine runs
/// without (hash-fallback embeddings, an unscaffolded workspace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    /// Configure a usable default model: at least one endpoint (with a
    /// key, when its kind needs one) and `[general] default_model`
    /// served by it.
    DefaultModel,
    /// Install the configured bundled embedding model — optional: the
    /// engine runs on the deterministic hash fallback (`semantic:
    /// false`) until then.
    EmbedderModel,
    /// Scaffold the project (`.coding/` + the `agent.md` template) —
    /// optional: engines work on plain directories (G3).
    Project,
}

impl fmt::Display for SetupStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetupStep::DefaultModel => write!(
                f,
                "a default model (an endpoint with a key if its kind needs one, \
                 plus [general] default_model)"
            ),
            SetupStep::EmbedderModel => {
                write!(f, "the configured embedding model (install it)")
            }
            SetupStep::Project => write!(f, "a scaffolded project (.coding + agent.md)"),
        }
    }
}

/// A setup-pipeline progress event (§5.8 :588-589) — binding-ready: the
/// installers (the classifier D5, the embedder D6) stream `Progress`
/// while they run, then one terminal `Done`/`Failed` naming the step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SetupEvent {
    /// A pipeline is making progress. `fraction` is a best-effort
    /// 0.0–1.0 (`None` = indeterminate).
    Progress {
        /// The pipeline's step.
        step: SetupStep,
        /// A human-facing label (the asset being fetched, the phase).
        label: String,
        /// Best-effort completion fraction.
        fraction: Option<f64>,
    },
    /// The step completed.
    Done {
        /// The completed step.
        step: SetupStep,
    },
    /// The step failed.
    Failed {
        /// The failed step.
        step: SetupStep,
        /// The failure's description.
        error: String,
    },
}

/// One endpoint's readiness (§5.8 :564).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EndpointReadiness {
    /// The endpoint's config name.
    pub name: String,
    /// Whether a key is saved for it in `keys.toml`.
    pub has_key: bool,
    /// Whether a probe has ever reached it — `None` = not probed
    /// ([`Setup::status`] makes NO network calls; `test_endpoint` (D2)
    /// fills this).
    pub reachable: Option<bool>,
}

/// One endpoint's config wire (§5.8 :533-534, job 4 D2) — the standalone
/// surface's lean shape. The app's GUI DTO carries extra card fields
/// (per-model configs, pricing) and validates through the same lib
/// `validate_endpoint`; `has_api_key` is OUTPUT ONLY
/// ([`Setup::endpoints`] fills it; the upsert path ignores it).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EndpointDto {
    /// The endpoint's config name (unique, non-empty).
    pub name: String,
    /// The kind wire form: `"openai"`, `"local"`, or `"anthropic"`.
    pub kind: String,
    /// The base URL (non-empty; a missing trailing `/` is appended).
    pub base_url: String,
    /// The served model ids (empty entries dropped).
    pub models: Vec<String>,
    /// The endpoint-level context-window cap, if known.
    pub max_context: Option<usize>,
    /// The endpoint-level output-token cap, if known.
    pub max_output_tokens: Option<usize>,
    /// Whether the endpoint serves a multimodal default model.
    pub multimodal: bool,
    /// Whether the endpoint accepts reasoning-effort overrides.
    pub supports_reasoning_effort: bool,
    /// The endpoint-level default reasoning effort.
    pub reasoning_effort: Option<String>,
    /// The Anthropic workspace id (cleared for other kinds).
    pub workspace_id: Option<String>,
    /// OUTPUT ONLY: whether a key is available for this endpoint.
    pub has_api_key: bool,
}

impl EndpointDto {
    /// Validate + convert into the lib [`crate::config::Endpoint`] —
    /// the same `validate_endpoint` the GUI's save path uses (one
    /// implementation). The lean surface carries no per-model
    /// configs: model ids become bare specs.
    fn into_endpoint(self) -> Result<crate::config::Endpoint, String> {
        crate::config::patch::validate_endpoint(
            &self.name,
            &self.kind,
            &self.base_url,
            self.models,
            Vec::new(),
            self.max_context,
            self.max_output_tokens,
            self.multimodal,
            self.supports_reasoning_effort,
            self.reasoning_effort,
            self.workspace_id,
        )
    }
}

/// The semantic-search leg (§5.8 :567): which bundled model is
/// configured, whether it is installed in the cache, the embedder's
/// status, and whether semantic (vs. hash-fallback) vectors are live.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbedderReadiness {
    /// The configured bundled model id (`None` = the hash fallback —
    /// the `"hash"` sentinel is filtered exactly like the engine does).
    pub configured: Option<String>,
    /// Whether the configured model is present in the cache (always
    /// `false` in a build without the `embeddings` feature — semantic
    /// search is structurally unavailable there).
    pub installed: bool,
    /// The embedder status (§5.8 :567).
    pub status: EmbedderStatus,
    /// Whether real semantic vectors are live (`false` = the
    /// deterministic hash fallback — keyword ranking).
    pub semantic: bool,
}

/// The project leg (§5.8 :569) — plain fs facts about a workspace root.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectReadiness {
    /// A `.git` exists at the root.
    pub is_git_repo: bool,
    /// A `.coding/` dir exists.
    pub coding_dir: bool,
    /// The scaffold is in place (`.coding/plans/`).
    pub scaffolded: bool,
    /// The root `agent.md` constitution exists.
    pub agent_md: bool,
    /// A codegraph index exists (`.coding/codegraph.db`).
    pub codegraph_indexed: bool,
}

/// The readiness snapshot (§5.8 :562-571) — what a setup UI shows
/// first: where the config lives, which endpoints exist + have keys,
/// which model is default, whether the engine can run NOW, the
/// semantic-search + classifier legs, the project facts, and the
/// ordered to-do list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SetupStatus {
    /// The resolved config dir backing this report.
    pub config_dir: PathBuf,
    /// One readiness row per configured endpoint.
    pub endpoints: Vec<EndpointReadiness>,
    /// `[general] default_model` (the model an unqualified spawn uses).
    pub default_model: Option<String>,
    /// Whether the engine can run on this config: the default model is
    /// served by an endpoint that has it, with a key when its kind
    /// needs one (§5.8 :563).
    pub ready_to_run: bool,
    /// The semantic-search leg (a missing model is NOT fatal — the
    /// engine runs on the hash fallback, `semantic: false`).
    pub embedder: EmbedderReadiness,
    /// The classifier leg (config-derived: `Disabled` when Laya is off,
    /// `Ready` when enabled — the live managed-runtime status lands
    /// with the engine-backed surface, D5).
    pub classifier: ClassifierStatus,
    /// The project facts, present when a workspace root was given.
    pub project: Option<ProjectReadiness>,
    /// The ordered first-run to-do list (§5.8 :570).
    pub missing: Vec<SetupStep>,
}

/// The standalone setup + config handle (§5.8 :525-527) — a pure
/// config-dir handle in D1 (no engine state); the mutating surface
/// (D2+) grows on it, and `for_engine` (D4) attaches a live engine for
/// the apply-immediately legs.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The resolved config dir (explicit / `$MNEMO_HOME` / `~/.mnemo`).
    config_dir: PathBuf,
    /// Where keys come from (D2, §5.8 :578-580) — `File` (`keys.toml`)
    /// is the default; `Env`/`InMemory` are host-managed READ sources
    /// (the write paths refuse them).
    key_source: KeySource,
}

impl Setup {
    /// Open a Setup against a config dir (§5.8 :527): `None` resolves to
    /// `$MNEMO_HOME`/`~/.mnemo` — the ONE resolver the engine shares —
    /// and the dir is created when missing (`Config::load` tolerates
    /// absent files: a fresh install is a valid, if not-ready,
    /// install).
    pub fn open(config_dir: Option<PathBuf>) -> Result<Self, EngineError> {
        let config_dir = crate::config::resolve_config_dir(config_dir);
        std::fs::create_dir_all(&config_dir).map_err(|e| {
            EngineError::start_failed(format!(
                "cannot create config dir '{}': {e}",
                config_dir.display()
            ))
        })?;
        Ok(Self {
            config_dir,
            key_source: KeySource::default(),
        })
    }

    /// The resolved config dir this Setup targets.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Point this Setup at a different [`KeySource`] (§5.8 :578-580):
    /// `Env`/`InMemory` serve `has_api_key` reads without plaintext on
    /// disk — they are host-managed, so the write paths
    /// ([`Setup::set_api_key`], [`Setup::upsert_endpoint`]'s key)
    /// refuse; inject the keys at construction instead.
    pub fn with_key_source(mut self, source: KeySource) -> Self {
        self.key_source = source;
        self
    }

    /// Load the config for the endpoints group — one error mapping for
    /// every read.
    fn load_config(&self) -> Result<Config, EngineError> {
        Config::load(&self.config_dir).map_err(|e| {
            EngineError::start_failed(format!(
                "config load from '{}' failed: {e}",
                self.config_dir.display()
            ))
        })
    }

    /// The configured endpoints (§5.8 :533) with per-endpoint key
    /// availability — the lean [`EndpointDto`] wire.
    pub fn endpoints(&self) -> Result<Vec<EndpointDto>, EngineError> {
        let config = self.load_config()?;
        let keys_path = self.config_dir.join("keys.toml");
        Ok(config
            .endpoints
            .iter()
            .map(|e| EndpointDto {
                name: e.name.clone(),
                kind: crate::config::kind_wire(e.kind).to_string(),
                base_url: e.base_url.clone(),
                models: e.models.iter().map(|m| m.id.clone()).collect(),
                max_context: e.max_context,
                max_output_tokens: e.max_output_tokens,
                multimodal: e.multimodal,
                supports_reasoning_effort: e.supports_reasoning_effort,
                reasoning_effort: e.reasoning_effort.clone(),
                workspace_id: e.workspace_id.clone(),
                has_api_key: self.key_source.has_key(&keys_path, &e.name),
            })
            .collect())
    }

    /// Upsert one endpoint (§5.8 :534): the same lib validation the
    /// GUI's save path enforces (`validate_endpoint` + the SET
    /// re-validation — the default provider/model must still resolve),
    /// replace-or-append by name, then persist. `key`: `Some` stores
    /// the key alongside (the `File` source only); `None` PRESERVES
    /// the endpoint's existing key.
    pub fn upsert_endpoint(
        &self,
        dto: EndpointDto,
        key: Option<Secret>,
    ) -> Result<(), EngineError> {
        let endpoint = dto.into_endpoint().map_err(EngineError::start_failed)?;
        let current = self.load_config()?;

        // Preserve the set's order: replace-or-append by name.
        let mut built = current.endpoints.clone();
        match built.iter().position(|e| e.name == endpoint.name) {
            Some(i) => built[i] = endpoint.clone(),
            None => built.push(endpoint.clone()),
        }

        // The surviving endpoints' keys merged with the provided one
        // (apply_endpoints rebuilds the store keeping keys for
        // existing endpoints only).
        let mut api_keys: HashMap<String, String> = HashMap::new();
        for ep in &built {
            if let Some(k) = current.keys.get(&ep.name) {
                api_keys.insert(ep.name.clone(), k.to_string());
            }
        }
        if let Some(secret) = &key {
            match &self.key_source {
                KeySource::File => {
                    api_keys.insert(endpoint.name.clone(), secret.expose().to_string());
                }
                _ => {
                    return Err(EngineError::start_failed(
                        "storing a key requires the File key source — Env and InMemory \
                         sources are host-managed (inject keys at construction)",
                    ));
                }
            }
        }

        let default_provider = current.general.general.default_provider.clone();
        let default_model = current.general.general.default_model.clone();
        crate::config::patch::validate_endpoint_set(
            &built,
            default_provider.as_deref(),
            default_model.as_deref(),
            default_model.as_deref(),
        )
        .map_err(EngineError::start_failed)?;
        let new_config = crate::config::patch::apply_endpoints(
            &current,
            built,
            default_provider,
            default_model,
            &api_keys,
        );
        new_config
            .save_all(&self.config_dir)
            .map_err(|e| EngineError::start_failed(e.to_string()))?;
        Ok(())
    }

    /// Remove one endpoint by name (§5.8 :534): its key drops with it
    /// (the rebuilt store keeps keys for surviving endpoints only).
    /// Refused when the default provider still references it (the SET
    /// re-validation).
    pub fn remove_endpoint(&self, name: &str) -> Result<(), EngineError> {
        let current = self.load_config()?;
        let built: Vec<crate::config::Endpoint> = current
            .endpoints
            .iter()
            .filter(|e| e.name != name)
            .cloned()
            .collect();
        if built.len() == current.endpoints.len() {
            return Err(EngineError::start_failed(format!(
                "endpoint '{name}' does not exist"
            )));
        }
        let mut api_keys: HashMap<String, String> = HashMap::new();
        for ep in &built {
            if let Some(k) = current.keys.get(&ep.name) {
                api_keys.insert(ep.name.clone(), k.to_string());
            }
        }
        let default_provider = current.general.general.default_provider.clone();
        let default_model = current.general.general.default_model.clone();
        crate::config::patch::validate_endpoint_set(
            &built,
            default_provider.as_deref(),
            default_model.as_deref(),
            default_model.as_deref(),
        )
        .map_err(EngineError::start_failed)?;
        let new_config = crate::config::patch::apply_endpoints(
            &current,
            built,
            default_provider,
            default_model,
            &api_keys,
        );
        new_config
            .save_all(&self.config_dir)
            .map_err(|e| EngineError::start_failed(e.to_string()))?;
        Ok(())
    }

    /// Store an API key for an endpoint (§5.8 :535) — WRITE-ONLY: there
    /// is no getter (only [`Setup::has_api_key`]). The `File` source
    /// persists to `keys.toml` (atomic write + user-only permissions);
    /// `Env`/`InMemory` refuse (host-managed).
    pub fn set_api_key(&self, endpoint_name: &str, secret: Secret) -> Result<(), EngineError> {
        match &self.key_source {
            KeySource::File => {
                let keys_path = self.config_dir.join("keys.toml");
                let mut store = crate::config::keys::KeyStore::load_or_default(&keys_path)
                    .map_err(|e| EngineError::start_failed(e.to_string()))?;
                store.insert(endpoint_name.to_string(), secret.expose().to_string());
                store
                    .save(&keys_path)
                    .map_err(|e| EngineError::start_failed(e.to_string()))?;
                Ok(())
            }
            _ => Err(EngineError::start_failed(
                "set_api_key requires the File key source — Env and InMemory sources are \
                 host-managed (inject keys at construction)",
            )),
        }
    }

    /// Whether a key is AVAILABLE for an endpoint (§5.8 :535) — never
    /// the value. Consults the [`KeySource`] (file/env/map).
    pub fn has_api_key(&self, endpoint_name: &str) -> bool {
        self.key_source
            .has_key(&self.config_dir.join("keys.toml"), endpoint_name)
    }

    /// Fetch the live model list for an endpoint (§5.8 :536) — the
    /// shared credential resolution (override args first, then the
    /// saved endpoint + `keys.toml` key, then the kind-aware env
    /// fallback, then `"dummy"`), then the kind-selected fetcher.
    /// Never writes config.
    pub async fn list_models(
        &self,
        endpoint_name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
        kind: Option<String>,
    ) -> Result<Vec<crate::provider::models::ModelWithVision>, EngineError> {
        let config = self.load_config()?;
        let (base_url, api_key, kind) =
            crate::provider::models::resolve_endpoint_credentials(
                &config,
                endpoint_name,
                base_url,
                api_key,
                kind,
            )
            .map_err(EngineError::start_failed)?;
        crate::provider::models::fetch_by_kind(&kind, &base_url, &api_key)
            .await
            .map_err(|e| EngineError::start_failed(e.to_string()))
    }

    /// Probe an endpoint's `/models` (§5.8 :536): `true` when the probe
    /// returns a list — `false` on ANY error (the detail rides
    /// [`Setup::list_models`] when a diagnosis is needed).
    pub async fn test_endpoint(
        &self,
        endpoint_name: &str,
        base_url: Option<String>,
        api_key: Option<String>,
        kind: Option<String>,
    ) -> bool {
        self.list_models(endpoint_name, base_url, api_key, kind)
            .await
            .is_ok()
    }

    /// The readiness snapshot (§5.8 :562-571) for this config — and,
    /// when given, a workspace root's project facts. Pure fs + config
    /// reads; NO network calls (endpoints report `reachable: None`
    /// until `test_endpoint` (D2) probes one).
    pub fn status(&self, project: Option<&Path>) -> Result<SetupStatus, EngineError> {
        let config = self.load_config()?;
        Ok(Self::status_from_config(&self.config_dir, &config, project))
    }

    /// The shared status computation — the engine's start precondition
    /// passes its ALREADY-LOADED config (no double load); the public
    /// [`Setup::status`] re-loads from disk.
    pub(crate) fn status_from_config(
        config_dir: &Path,
        config: &Config,
        project: Option<&Path>,
    ) -> SetupStatus {
        let general = &config.general.general;

        let endpoints: Vec<EndpointReadiness> = config
            .endpoints
            .iter()
            .map(|e| EndpointReadiness {
                name: e.name.clone(),
                has_key: config.keys.get(&e.name).is_some(),
                reachable: None,
            })
            .collect();

        // The host rule (patch.rs's `validate_endpoint_set`): the
        // default provider's endpoint, else the first.
        let host = general
            .default_provider
            .as_deref()
            .and_then(|n| config.endpoints.iter().find(|e| e.name == n))
            .or_else(|| config.endpoints.first());

        let default_model = general.default_model.clone().filter(|m| !m.is_empty());

        // ready_to_run (§5.8 :563): the default model served by an
        // endpoint that has it, with a key when its kind needs one
        // (Local endpoints run keyless).
        let ready_to_run = match (&default_model, host) {
            (Some(model), Some(ep)) => {
                ep.has_model(model)
                    && (ep.kind == EndpointKind::Local || config.keys.get(&ep.name).is_some())
            }
            _ => false,
        };

        // The embedder leg (§5.8 :567) — the same sentinel filter the
        // engine applies (engine.rs's configured_embedding_model).
        let configured = general
            .bundled_embedding_model
            .clone()
            .filter(|m| !m.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH));
        let cache_dir = crate::config::embedder_cache_dir();
        #[cfg(feature = "embeddings")]
        let installed = configured
            .as_deref()
            .map(|id| crate::memory::embedder::is_model_installed(&cache_dir, id))
            .unwrap_or(false);
        #[cfg(not(feature = "embeddings"))]
        let installed = {
            // No ONNX runtime in this build: semantic search is
            // structurally unavailable — the hash fallback is the only
            // embedder.
            let _ = &cache_dir;
            false
        };
        let semantic = installed;
        let status = if configured.is_none() || installed {
            // No model configured: the deterministic hash embedder is
            // genuinely ready (keyword mode). Installed: the real one.
            EmbedderStatus::Ready
        } else {
            // Configured but missing from the cache — the engine
            // falls back to hash on start; the missing list carries
            // the install step.
            EmbedderStatus::Failed
        };

        // The classifier leg — config-derived approximation: enabled
        // means calls are attempted (`ClassifierStatus::Ready`'s
        // contract); the live managed-runtime status
        // (Installing/Downloading/Starting) rides the engine-backed
        // surface (D5).
        let classifier = if general.laya.enabled {
            ClassifierStatus::Ready
        } else {
            ClassifierStatus::Disabled
        };

        let project = project.map(|root| ProjectReadiness {
            is_git_repo: root.join(".git").exists(),
            coding_dir: root.join(".coding").is_dir(),
            scaffolded: root.join(".coding").join("plans").is_dir(),
            agent_md: root.join("agent.md").is_file(),
            codegraph_indexed: root.join(".coding").join("codegraph.db").exists(),
        });

        // The ordered to-do list (§5.8 :570): the fatal leg first.
        let mut missing = Vec::new();
        if !ready_to_run {
            missing.push(SetupStep::DefaultModel);
        }
        if configured.is_some() && !installed {
            missing.push(SetupStep::EmbedderModel);
        }
        if let Some(p) = &project {
            if !p.scaffolded {
                missing.push(SetupStep::Project);
            }
        }

        SetupStatus {
            config_dir: config_dir.to_path_buf(),
            endpoints,
            default_model,
            ready_to_run,
            embedder: EmbedderReadiness {
                configured,
                installed,
                status,
                semantic,
            },
            classifier,
            project,
            missing,
        }
    }

    /// The `not_configured` message's missing list ("a; b; c").
    pub(crate) fn format_missing(missing: &[SetupStep]) -> String {
        missing
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("; ")
    }
}