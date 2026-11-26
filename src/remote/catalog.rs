// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The node's model catalog: the hub's sanitised configuration, mirrored for a
//! remote worker node (plan a040713f, phase 2 step 4).
//!
//! A node holds no API keys and dials every model call through the hub, so it
//! never needs a real endpoint URL. [`ModelCatalog::from_config`] carries the
//! endpoint *shapes* — kind, model ids, capabilities, pricing — with every
//! `base_url` replaced by [`HUB_PLACEHOLDER_BASE_URL`]: a bug that tries to
//! reach a model directly fails on the unroutable placeholder instead of
//! leaking a request (and the hub never has to trust a node with its keys).
//! The vision model is dropped (the hub answers vision calls) and the bundled
//! hash embedder replaces the hub's; MCP servers and `keys.toml` are never part
//! of the mirror.
//!
//! The catalog is what the hub pushes over the link (step 5) and what the node
//! writes into its config dir, so the node's `~/.mnemo`-style tree resolves
//! models exactly as the hub does — through the hub.

use std::path::Path;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::{Config, Endpoint, GeneralConfig, PricingEntry};
use crate::error::{Error, Result};

/// The unroutable placeholder every mirrored endpoint's `base_url` carries.
/// A call that escapes the proxy hits this and fails, instead of reaching a
/// provider directly with no (or the wrong) credentials.
pub const HUB_PLACEHOLDER_BASE_URL: &str = "http://hub.invalid/";

/// The node-visible model catalog: revision plus the sanitised config files.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ModelCatalog {
    /// `sha256` hex over the canonical JSON of the other fields. A node
    /// compares it with the revision it mirrored to decide whether to rewrite
    /// its config dir.
    pub revision: String,
    /// The sanitised general configuration (`config.toml`).
    pub general: GeneralConfig,
    /// The endpoint shapes (`endpoints.toml`), base URLs replaced by the
    /// placeholder.
    pub endpoints: Vec<Endpoint>,
    /// Per-model pricing (`endpoints.toml`).
    pub pricing: Vec<PricingEntry>,
    /// The hub's global agent instructions (`agent.md`).
    pub global_agent_md: String,
}

impl ModelCatalog {
    /// Build the node-visible catalog from the hub's live config.
    ///
    /// Sanitises: every endpoint `base_url` becomes
    /// [`HUB_PLACEHOLDER_BASE_URL`], the vision model is dropped (the hub
    /// answers vision calls), and the bundled hash embedder is pinned. Keys
    /// and MCP servers never live in these fields, and [`Self::write_mirror`]
    /// writes only the three model files.
    pub fn from_config(cfg: &Config, global_agent_md: String) -> Self {
        let mut general = cfg.general.clone();
        general.general.vision_model = None;
        general.general.bundled_embedding_model = Some("hash".to_string());
        let endpoints: Vec<Endpoint> = cfg
            .endpoints
            .iter()
            .map(|endpoint| {
                let mut endpoint = endpoint.clone();
                endpoint.base_url = HUB_PLACEHOLDER_BASE_URL.to_string();
                endpoint
            })
            .collect();
        let pricing = cfg.pricing.clone();
        let revision = revision_of(&general, &endpoints, &pricing, &global_agent_md);
        Self {
            revision,
            general,
            endpoints,
            pricing,
            global_agent_md,
        }
    }

    /// Write the mirror into `dir`: `config.toml`, `endpoints.toml` and
    /// `agent.md`. Never `keys.toml`, never an MCP config.
    pub fn write_mirror(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Config(format!("create the catalog mirror dir: {e}")))?;
        self.general.save(&dir.join("config.toml"))?;
        crate::config::endpoints::save(&dir.join("endpoints.toml"), &self.endpoints, &self.pricing)?;
        crate::config::write_atomic(&dir.join("agent.md"), &self.global_agent_md)?;
        Ok(())
    }
}

/// The canonical-JSON sha256 of the catalog's payload (everything but the
/// revision itself).
fn revision_of(
    general: &GeneralConfig,
    endpoints: &[Endpoint],
    pricing: &[PricingEntry],
    global_agent_md: &str,
) -> String {
    #[derive(Serialize)]
    struct Canonical<'a> {
        general: &'a GeneralConfig,
        endpoints: &'a [Endpoint],
        pricing: &'a [PricingEntry],
        global_agent_md: &'a str,
    }
    let bytes = serde_json::to_vec(&Canonical {
        general,
        endpoints,
        pricing,
        global_agent_md,
    })
    .expect("the model catalog serialises");
    crate::remote::auth::hex_encode(&Sha256::digest(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EndpointKind, ModelSpec};

    fn test_dir(name: &str) -> std::path::PathBuf {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        std::fs::create_dir_all(&root).expect("target dir");
        let dir = root.join(format!(
            "mnemo-remote-catalog-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("test dir");
        dir
    }

    fn hub_config() -> Config {
        let mut cfg = Config::default();
        cfg.general.general.vision_model = Some(crate::config::VisionModel {
            endpoint: "vision".to_string(),
            model: "vlm".to_string(),
        });
        cfg.general.general.bundled_embedding_model = Some("onnx".to_string());
        cfg.endpoints = vec![Endpoint {
            name: "hub-openai".to_string(),
            kind: EndpointKind::OpenAI,
            base_url: "https://api.example.com/v1/".to_string(),
            models: vec![ModelSpec::test_default()],
            ..Endpoint::test_default()
        }];
        cfg
    }

    #[test]
    fn catalog_strips_base_urls_vision_and_embedder() {
        let cfg = hub_config();
        let catalog = ModelCatalog::from_config(&cfg, "# global".to_string());
        assert_eq!(catalog.endpoints.len(), 1);
        assert_eq!(catalog.endpoints[0].base_url, HUB_PLACEHOLDER_BASE_URL);
        assert!(
            !catalog
                .endpoints
                .iter()
                .any(|ep| ep.base_url.contains("api.example.com")),
            "the hub's real URL must never cross the link"
        );
        assert!(catalog.general.general.vision_model.is_none());
        assert_eq!(
            catalog.general.general.bundled_embedding_model.as_deref(),
            Some("hash")
        );
        assert_eq!(catalog.global_agent_md, "# global");
        assert_eq!(catalog.revision.len(), 64);
        assert!(catalog.revision.chars().all(|c| c.is_ascii_hexdigit()));
        // The revision is stable for the same payload and moves with it.
        let again = ModelCatalog::from_config(&cfg, "# global".to_string());
        assert_eq!(again.revision, catalog.revision);
        let changed = ModelCatalog::from_config(&cfg, "# changed".to_string());
        assert_ne!(changed.revision, catalog.revision);
    }

    #[test]
    fn catalog_mirror_has_no_keys_file() {
        let dir = test_dir("mirror");
        let catalog = ModelCatalog::from_config(&hub_config(), "# global".to_string());
        catalog.write_mirror(&dir).expect("writes the mirror");
        assert!(dir.join("config.toml").is_file());
        assert!(dir.join("endpoints.toml").is_file());
        assert!(dir.join("agent.md").is_file());
        assert!(
            !dir.join("keys.toml").exists(),
            "the node must never receive the hub's keys"
        );
        assert!(!dir.join("mcp.toml").exists());
        // The mirror round-trips as a config the node can load: no keys file
        // means empty credentials, and the endpoints parse with the shape.
        let mirrored = Config::load(&dir).expect("the mirror loads as a config");
        assert_eq!(mirrored.endpoints.len(), 1);
        assert_eq!(
            mirrored.endpoints[0].base_url,
            HUB_PLACEHOLDER_BASE_URL
        );
        assert!(mirrored.keys.is_empty(), "the node mirror carries no keys");
        let text = std::fs::read_to_string(dir.join("config.toml")).expect("config text");
        assert!(!text.contains("api.example.com"));
        let endpoints = std::fs::read_to_string(dir.join("endpoints.toml")).expect("endpoints");
        assert!(!endpoints.contains("api.example.com"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
