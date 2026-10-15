// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Startup helpers shared by the GUI, the console and the headless API —
//! pure functions over config and process arguments.

use crate::config::{Config, Endpoint};

/// The `--project <path>` CLI override, when present. Only the
/// space-separated form is honored (matching `--console`'s spelling); a
/// trailing `--project` with no value yields `None`.
pub fn project_flag_from_args(
    args: impl IntoIterator<Item = String>,
) -> Option<std::path::PathBuf> {
    args.into_iter()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "--project")
        .and_then(|w| w.get(1))
        .map(std::path::PathBuf::from)
}

/// The startup provider's endpoint + model, resolved from config with the
/// documented fallback ordering:
///
/// - endpoint: the `default_provider`-named endpoint, else the config's
///   default endpoint (the first when `default_provider` is unset or
///   names a deleted endpoint).
/// - model: `default_model`, else the endpoint's first model id, else
///   `"gpt-4o"` (the terminal fallback — the caller builds a dummy
///   provider when the endpoint is `None`).
pub fn resolve_startup_provider(config: &Config) -> (Option<&Endpoint>, String) {
    let endpoint_name = config.general.general.default_provider.as_deref();
    let endpoint = endpoint_name
        .and_then(|name| config.endpoint(name))
        .or_else(|| config.default_endpoint());

    let model = config
        .general
        .general
        .default_model
        .clone()
        .or_else(|| endpoint.and_then(|e| e.model_ids().first().cloned()))
        .unwrap_or_else(|| "gpt-4o".to_string());
    (endpoint, model)
}

/// Whether the cross-machine re-embed check should run at startup. Every
/// gate exists for a documented reason:
///
/// - A bundled model must be configured (`None` = hash-only setup, nothing
///   semantic to re-embed with).
/// - The explicit `"hash"` opt-out must skip (case-insensitive, matching
///   the sentinel contract in `build_embedder` / `embedder_startup_plan` —
///   a case-sensitive compare would let the hash embedder destroy stored
///   semantic vectors).
/// - A pending first-run download or local load must skip — the background
///   task owns the re-embed (running it here with the interim hash
///   embedder would destroy vectors).
/// - The status must be `Ready` — when the model failed to load (hash
///   fallback), there is nothing useful to re-embed with.
pub fn should_reembed_at_startup(
    configured: Option<&str>,
    pending_download: bool,
    pending_load: bool,
    status: &crate::memory::embedder::EmbedderStatus,
) -> bool {
    configured.is_some()
        && !configured.is_some_and(|v| {
            v.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH)
        })
        && !pending_download
        && !pending_load
        && *status == crate::memory::embedder::EmbedderStatus::Ready
}
