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

use mnemo::provider::client_factory::{build_embedder, build_vision_client};
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
    // toggling it takes effect without a restart. Disabled ⇒
    // `None` + status Disabled (no client, no calls); the slot swap is what
    // items 2-5 read. The shared status is written below, so the
    // Settings section sees the new state on its next read.
    //
    // Laya is managed-only: the app owns the runtime, so the rewire stops any
    // running child first (a reconfigure must never leave a stale sidecar
    // bound to a dead port), and when enabled + the checkpoint is installed
    // swaps the slot to a pre-allocated loopback port and starts the new
    // sidecar in the background (status Starting → probe → Ready/Failed on
    // `classifier://status`). The server start never blocks the save call.
    let laya_cfg = &cfg.general.general.laya;
    let laya = state.runtime.laya.clone();
    laya.stop();
    let status = state.runtime.classifier_status.clone();
    let checkpoint = super::laya::english_checkpoint();
    let port = if laya.is_checkpoint_installed(checkpoint.id) {
        super::laya::LayaManager::alloc_free_port()
    } else {
        None
    };
    if laya_cfg.enabled && port.is_some() {
        // Some(port): the client points at the fresh port right away; the
        // background task drives Starting → Ready/Failed.
        let port = port.expect("checked is_some above");
        *status.write().expect("classifier status lock poisoned") =
            mnemo::memory::classifier::ClassifierStatus::Starting;
        *state
            .runtime
            .classifier
            .write()
            .expect("classifier lock poisoned") =
            super::laya::build_managed_classifier(port, status.clone());
        let app_handle = Some(app.clone());
        tauri::async_runtime::spawn(async move {
            super::laya::start_managed_server(&app_handle, laya, checkpoint, port).await;
        });
        eprintln!(
            "rewire: classifier managed (checkpoint '{}', restarting sidecar)",
            checkpoint.id
        );
    } else if laya_cfg.enabled {
        // Enabled but the checkpoint is not installed or no free port:
        // no client, and the hint says where to fix it.
        *state
            .runtime
            .classifier
            .write()
            .expect("classifier lock poisoned") = None;
        *status.write().expect("classifier status lock poisoned") =
            mnemo::memory::classifier::ClassifierStatus::Disabled;
        eprintln!(
            "rewire: classifier managed but not ready (checkpoint '{}' not installed \
             or no free loopback port) — run the setup in Settings → Classifier",
            checkpoint.id
        );
    } else {
        *state
            .runtime
            .classifier
            .write()
            .expect("classifier lock poisoned") = None;
        *status.write().expect("classifier status lock poisoned") =
            mnemo::memory::classifier::ClassifierStatus::Disabled;
        eprintln!("rewire: classifier cleared (managed disabled)");
    }
    // Auto-typing flag (backlog a147b63c): memory_write reads the shared
    // classifier slot + this mirrored flag at call time, so the Settings
    // toggle lands on the very next write — no restart, no registry
    // rebuild. The slot itself needs no touch here: every swap above
    // writes the same Arc the factory's tools hold.
    if let Some(factory) = &state.runtime.factory {
        factory.set_auto_typing_enabled(laya_cfg.auto_type_memories);
        // Tool-choice steering (backlog e2c47d5f): the same live-toggle
        // contract — the search/search_read tools read the shared classifier
        // slot + this mirrored flag per call, so a Settings save lands on the
        // next search with no rebuild. The slot needs no touch here: every
        // swap above writes the same Arc the tools hold.
        factory.set_tool_choice_enabled(laya_cfg.steer_tool_choice);
        // Failure triage (backlog 1a4049c1): the same live-toggle contract —
        // the loop's dispatch site and both provider retry layers read the
        // mirrored flag at failure time, so a Settings save lands on the
        // next failure with no rebuild. The classifier slot needs no touch
        // here: every swap above writes the same Arc the gate holds.
        factory.set_failure_triage_enabled(laya_cfg.failure_triage);
        // The kNN overlay flag (item 4b): the same live-toggle contract —
        // the gate consults the overlay before the shared slot while this
        // is on. The overlay reads the store's live embedder through its
        // getter and tracks the training-log file itself, so an embedder
        // change needs no extra wiring here — only the flag mirror.
        factory.set_failure_triage_knn_enabled(laya_cfg.failure_triage_knn);
        // Reflex (backlog a8495cc1) -- the escalation lane ladder's classifier
        // (backlog ad56c7bd): the same live-toggle contract -- the loop reads
        // the shared classifier slot + this mirrored flag at each plan-step
        // boundary, so a Settings save lands on the next step with no rebuild.
        factory.set_reflex_enabled(laya_cfg.reflex);
        // Token-optimizer levers ([general.optimizer], backlog e4a50d22): the
        // same live-toggle contract — every lever-bearing tool reads the
        // shared Arc<RwLock<OptimizerConfig>> per call, so a Settings save
        // lands on the next tool call with no registry rebuild. Without this
        // write the mirror kept its startup value, so a toggle silently needed
        // an app restart.
        factory.set_optimizer_config(cfg.general.general.optimizer.clone());
    }
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

    // Mirror the startup emit so a save-driven enable/disable change
    // reaches listeners immediately (the Settings section also re-polls after
    // a save; the event keeps any future listener correct).
    if let Ok(status) = state.classifier_status() {
        let _ = app.emit("classifier://status", &status);
    }
}

/// Push a reloaded config into the per-context model resolver so `[models]`
/// overrides take effect on the next request (per-request resolution,
/// 2027-01-25). No-op when the resolver is absent
/// (startup-error fallback). Shared by `save_settings` and `save_endpoints`.
pub(super) fn sync_model_resolver(state: &IpcState, cfg: &mnemo::config::Config) {
    if let Some(resolver) = &state.runtime.model_resolver {
        resolver.set_config(cfg.clone());
    }
}
