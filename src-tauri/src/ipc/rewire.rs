// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Runtime rewire of the vision client + memory embedder + model resolver
//! after a config change.
//!
//! Shared by [`save_endpoints`](super::settings::save_endpoints) and
//! [`save_settings`](super::settings::save_settings): both reload the config
//! from disk, then call these to push the new values into the live runtime
//! (factory / memory store / model resolver) so the change takes effect
//! without a restart.

use mnemo::provider::client_factory::{build_embedder, build_vision_client};

use crate::ipc::state::IpcState;

/// Rebuild the live vision client + memory embedder from `cfg` and install
/// them into the factory / memory store. No-op when those handles are absent
/// (startup-error fallback). Shared by `save_settings` and `save_endpoints`.
pub(super) fn rewire_vision_and_embedder(state: &IpcState, cfg: &mnemo::config::Config) {
    if let Some(factory) = &state.runtime.factory {
        let vision = build_vision_client(cfg);
        factory.set_vision(vision);
        eprintln!(
            "rewire: vision client {}",
            if factory.vision_slot().is_configured() {
                "active"
            } else {
                "cleared"
            }
        );
    }
    if let Some(store) = &state.runtime.memory_store {
        // The bundled model cache lives under the global config dir (NOT
        // .coding/, which is committed to git — model binaries are per-machine).
        let cache_dir = mnemo::config::global_config_dir().join("models");
        let new_embedder = build_embedder(cfg, &cache_dir, state.runtime.embedder_status.clone());
        // When the bundled model changed (or memories are stale from another
        // machine via git), re-embed all rows in the background so the stored
        // vectors match the new model. Only runs when the model loaded
        // successfully (status Ready) — a Failed/hash fallback has nothing
        // useful to re-embed with, and would corrupt the fingerprint semantics.
        // Skipped on the explicit "hash" opt-out (case-insensitive, matching
        // build_embedder's sentinel contract): re-embedding with the hash
        // embedder would destroy stored semantic vectors.
        // Fire-and-forget — never blocks the UI.
        let is_hash_opt_out = cfg
            .general
            .general
            .bundled_embedding_model
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case(mnemo::config::EMBEDDING_MODEL_SENTINEL_HASH));
        let status_snapshot = state
            .runtime
            .embedder_status
            .read()
            .expect("embedder status lock poisoned")
            .clone();
        if !is_hash_opt_out && status_snapshot == mnemo::memory::embedder::EmbedderStatus::Ready {
            let store_for_reembed = store.clone();
            let embedder_for_reembed = new_embedder.clone();
            tauri::async_runtime::spawn(async move {
                mnemo::memory::reembed_if_needed(&store_for_reembed, &embedder_for_reembed).await;
            });
        }
        // The [memory] retrieval knobs (decay, caps, digest budgets) ride the
        // same save path — recall + memory_write read the store's snapshot per
        // call, so this takes effect without a restart.
        store.set_memory_search_config(cfg.general.memory.clone());
        store.set_embedder(new_embedder);
        eprintln!("rewire: embedder set (from config)");
    }
}

/// Push a reloaded config into the per-context model resolver so `[models]`
/// overrides take effect on the next turn. No-op when the resolver is absent
/// (startup-error fallback). Shared by `save_settings` and `save_endpoints`.
pub(super) fn sync_model_resolver(state: &IpcState, cfg: &mnemo::config::Config) {
    if let Some(resolver) = &state.runtime.model_resolver {
        resolver.set_config(cfg.clone());
    }
}
