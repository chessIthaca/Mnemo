// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Runtime rewire of the vision client + memory embedder + optional Laya
//! classifier + model resolver after a config change.
//!
//! Shared by [`save_endpoints`](super::settings::save_endpoints) and
//! [`save_settings`](super::settings::save_settings): both reload the config
//! from disk, then call these to push the new values into the live runtime
//! (factory / memory store / model resolver) so the change takes effect
//! without a restart.

use mnemo::provider::client_factory::{build_classifier, build_embedder, build_vision_client};
use tauri::Emitter;

use crate::ipc::state::IpcState;

/// Rebuild the live vision client + memory embedder + optional Laya classifier
/// from `cfg` and install them into the factory / memory store / classifier
/// slot. No-op for the handles that are absent (startup-error fallback).
/// Shared by `save_settings` and `save_endpoints`.
///
/// `app` is used only to emit `classifier://status` when the classifier slot
/// is rebuilt — mirroring the startup emit (main.rs), so listeners (today the
/// Settings → Classifier section) see a save-driven rewire without polling.
pub(super) fn rewire_vision_embedder_and_classifier(
    app: &tauri::AppHandle,
    state: &IpcState,
    cfg: &mnemo::config::Config,
) {
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

    // Laya classifier: rebuild from the saved config so enabling/disabling or
    // repointing the endpoint takes effect without a restart. Disabled ⇒
    // `None` + status Disabled (no client, no calls); the slot swap is what
    // items 2-5 read. `build_classifier` also writes the shared status, so the
    // Settings section sees the new state on its next read.
    let new_classifier = build_classifier(cfg, state.runtime.classifier_status.clone());
    *state
        .runtime
        .classifier
        .write()
        .expect("classifier lock poisoned") = new_classifier;
    // Read back through the accessor items 2-5 will use, so the log shows the
    // installed state (a poisoned lock reads as "cleared").
    let classifier_active = state.classifier().map(|c| c.is_some()).unwrap_or(false);
    eprintln!(
        "rewire: classifier {}",
        if classifier_active {
            "active"
        } else {
            "cleared"
        }
    );

    // Mirror the startup emit so a save-driven enable/disable/endpoint change
    // reaches listeners immediately (the Settings section also re-polls after
    // a save; the event keeps any future listener correct).
    if let Ok(status) = state.classifier_status() {
        let _ = app.emit("classifier://status", &status);
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
