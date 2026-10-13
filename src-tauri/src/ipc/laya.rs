// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The managed-Laya IPC layer — the commands + the Tauri glue. Job 4 D5
//! (§5.1 :168) moved the Tauri-free core (the manager, the setup
//! pipeline, the server lifecycle — all of it unit-tested there) into
//! the lib at `mnemo::host::laya`. What stays here is exactly the
//! app-side surface: the `classifier://status` forwarding sink, the
//! wrappers that keep the `Option<AppHandle>` signatures the app's
//! callers (the startup hook, the console fallback, the fine-tune
//! hot-swap, the Settings rewire) already use, the wire type, and the
//! two commands.
//!
//! The status Arc `IpcState.classifier_status` holds is the SAME Arc the
//! manager drives — every write the core makes surfaces through the
//! sink below.

use std::sync::Arc;
use std::sync::RwLock;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use mnemo::config::Config;
use mnemo::host::laya::StatusSink;
use mnemo::memory::classifier::{Classifier, ClassifierStatus};

use crate::ipc::state::IpcState;

// The Tauri-free core (job 4 D5): the manager, the catalog, the pipeline,
// the server lifecycle. Re-exported so the app's callers keep their
// `crate::ipc::laya::…` paths (IpcState's field type, the startup hook,
// the fine-tune hot-swap, the Settings rewire).
pub use mnemo::host::laya::{
    build_managed_classifier, checkpoint_catalog, english_checkpoint, fresh_disabled,
    hide_console_window, LayaCheckpoint, LayaManager,
};

/// The app's status sink: forward every core status write to the Tauri
/// bus so Settings → Classifier stays live (§5.8 :554's headless twin
/// is the Setup surface's `EventReceiver<SetupEvent>`). A `None` app
/// handle means status writes only.
fn status_sink(app: &Option<AppHandle>) -> Option<StatusSink> {
    app.as_ref().map(|app| {
        let app = app.clone();
        Arc::new(move |s: &ClassifierStatus| {
            let _ = app.emit("classifier://status", s);
        }) as StatusSink
    })
}

/// Start (or restart) the managed `laya-serve` on `port` — the lib core
/// with the app's `classifier://status` sink attached (the wrapper keeps
/// the pre-D5 signature the startup hook and the console fallback use).
pub async fn start_managed_server(
    app: &Option<AppHandle>,
    manager: Arc<LayaManager>,
    checkpoint: &LayaCheckpoint,
    port: u16,
) -> Option<u16> {
    let sink = status_sink(app);
    mnemo::host::laya::start_managed_server(sink.as_ref(), manager, checkpoint, port).await
}

/// Start (or restart) the managed `laya-serve` serving `models` — the
/// startup fine-tune hot-swap's entry (finetune.rs); the wrapper keeps
/// the pre-D5 signature.
pub(super) async fn start_server_models(
    app: &Option<AppHandle>,
    manager: Arc<LayaManager>,
    models: &str,
    port: u16,
    progress: Option<&LayaCheckpoint>,
) -> Option<u16> {
    let sink = status_sink(app);
    mnemo::host::laya::start_server_models(sink.as_ref(), manager, models, port, progress).await
}/// Fire-and-forget setup (Settings → Classifier → Download) — the lib
/// pipeline with the app's sink; the wrapper keeps the pre-D5 signature
/// the `laya_setup` command uses.
pub fn spawn_setup_task(
    app: Option<AppHandle>,
    manager: Arc<LayaManager>,
    checkpoint: &'static LayaCheckpoint,
    config: Arc<tokio::sync::Mutex<Config>>,
    classifier_slot: Option<Arc<RwLock<Option<Arc<dyn Classifier>>>>>,
) {
    let sink = status_sink(&app);
    mnemo::host::laya::spawn_setup_task(sink, manager, checkpoint, config, classifier_slot);
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// The wire form of a catalog entry (Settings → Classifier).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LayaCheckpointWire {
    /// The checkpoint id (always `"english"` — the only catalog entry).
    pub id: String,
    /// Human-facing name.
    pub name: String,
    /// Approximate download size (MiB).
    pub size_mb: u64,
    /// Was this checkpoint downloaded by a completed setup?
    pub installed: bool,
}

/// List the managed-Laya checkpoints with their installed flags.
#[tauri::command]
pub async fn laya_catalog(
    state: State<'_, IpcState>,
) -> Result<Vec<LayaCheckpointWire>, String> {
    let manager = &state.runtime.laya;
    Ok(checkpoint_catalog()
        .iter()
        .map(|c| LayaCheckpointWire {
            id: c.id.to_string(),
            name: c.name.to_string(),
            size_mb: c.size_mb,
            installed: manager.is_checkpoint_installed(c.id),
        })
        .collect())
}

/// Download the managed Laya runtime (uv + venv + `laya[serve]` + the
/// English checkpoint). Fire-and-forget: progress arrives via
/// `classifier://status`; when the current config enables Laya, the server
/// starts on success.
#[tauri::command]
pub async fn laya_setup(app: AppHandle, state: State<'_, IpcState>) -> Result<(), String> {
    let manager = Arc::clone(&state.runtime.laya);
    let cp = english_checkpoint();
    if !manager.begin_setup() {
        return Err("a Laya setup is already running".into());
    }
    // The autostart decision is re-derived from the LIVE config when the
    // (multi-minute) setup finishes — the core re-reads it at completion
    // (see `mnemo::host::laya::spawn_setup_task`) — so no snapshot rides
    // along from download start.
    spawn_setup_task(
        Some(app),
        manager,
        cp,
        state.project.config.clone(),
        Some(state.runtime.classifier.clone()),
    );
    Ok(())
}