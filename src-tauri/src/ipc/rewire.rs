// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Runtime rewire of the vision client + memory embedder + optional Laya
//! classifier after a config change.
//!
//! Shared by [`save_endpoints`](super::settings::save_endpoints) and
//! [`save_settings`](super::settings::save_settings): both reload the
//! config from disk, then call these to push the new values into the
//! live runtime (factory / memory store / model resolver) so the change
//! takes effect without a restart.
//!
//! Job 4 D4 (§5.8 :538): the vision + embedder + live-toggle core MOVED
//! into the lib (`mnemo::rewire`) — the GUI and the headless
//! `Setup::patch_settings` share ONE implementation. This wrapper keeps
//! only the app-owned pieces: the managed-classifier leg (the Laya
//! runtime — job 4 D5 moves it), the re-embed spawn policy, and the
//! `classifier://status` emit.

use mnemo::rewire::{rewire_vision_embedder, RewireTargets};
use tauri::Emitter;

use crate::ipc::state::IpcState;

/// Rebuild the live vision client + memory embedder + optional Laya
/// classifier from `cfg` and install them into the factory / memory
/// store / classifier slot. No-op for the handles that are absent
/// (startup-error fallback). Shared by `save_settings` and
/// `save_endpoints`.
///
/// `app` is used only to emit `classifier://status` when the classifier
/// slot is rebuilt — mirroring the startup emit (main.rs), so listeners
/// (today the Settings → Classifier section) see a save-driven rewire
/// without polling.
pub(super) fn rewire_vision_embedder_and_classifier(
    app: &tauri::AppHandle,
    state: &IpcState,
    cfg: &mnemo::config::Config,
) {
    // Job 4 D4: the vision + embedder + factory live-toggles run in the
    // lib (one implementation, shared with `Setup::patch_settings`);
    // the re-embed future is ours to spawn (the headless Setup spawns
    // on tokio — here it is `tauri::async_runtime`).
    let targets = RewireTargets {
        factory: state.runtime.factory.clone(),
        memory_store: state.runtime.memory_store.clone(),
        embedder_status: state.runtime.embedder_status.clone(),
        model_resolver: state.runtime.model_resolver.clone(),
        // The GUI's config dir IS the machine-global one (D6's `None`
        // fallback) — byte-identical behavior.
        config_dir: None,
    };
    if let Some(task) = rewire_vision_embedder(&targets, cfg) {
        tauri::async_runtime::spawn(task);
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
    // (Job 4 D5 moves this leg + the Laya core into the lib.)
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
    match (laya_cfg.enabled, port) {
        (true, Some(port)) => {
            // The client points at the fresh port right away; the
            // background task drives Starting → Ready/Failed.
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
        }
        (true, None) => {
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
        }
        (false, _) => {
            *state
                .runtime
                .classifier
                .write()
                .expect("classifier lock poisoned") = None;
            *status.write().expect("classifier status lock poisoned") =
                mnemo::memory::classifier::ClassifierStatus::Disabled;
            eprintln!("rewire: classifier cleared (managed disabled)");
        }
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
/// (startup-error fallback). Shared by `save_settings` and `save_endpoints`;
/// the core lives in the lib (job 4 D4).
pub(super) fn sync_model_resolver(state: &IpcState, cfg: &mnemo::config::Config) {
    mnemo::rewire::sync_model_resolver(state.runtime.model_resolver.as_ref(), cfg);
}