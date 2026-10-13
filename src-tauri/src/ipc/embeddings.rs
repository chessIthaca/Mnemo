// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Bundled embedding model commands — the Tauri glue. Job 4 D6 moved
//! the download/load core (the progress poller, the blocking model
//! build, the install guard + swap) into the lib at
//! `mnemo::host::embed_download`; what stays here is the command
//! surface (the catalog wire, the Settings download command) and the
//! startup twins the app's hook consumes — each building the app's
//! `embedder://status` sink over the shared status Arc.
//!
//! The models run in-process via `fastembed` (ONNX Runtime) — no
//! Ollama, no cloud. The cache dir derives from the app's config dir
//! (the machine-global one — `~/.mnemo` or `MNEMO_HOME`); a headless
//! `Setup` with a custom dir isolates its own installs.

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use mnemo::host::embed_download::{
    embedding_model_catalog, spawn_download, spawn_startup_load_task, DownloadOutcome,
    EmbedderStatusSink,
};
use mnemo::memory::embedder::EmbedderStatus;
use mnemo::memory::MemoryStore;

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The cache dir for bundled embedding models — the app's config dir
/// (the machine-global one) via D6's parameterized derivation.
fn embedder_cache_dir() -> std::path::PathBuf {
    mnemo::config::embedder_cache_dir(&mnemo::config::global_config_dir())
}

/// The app's status sink: forward every core status write to the
/// Tauri bus so Settings stays live (`embedder://status`).
fn status_sink(app: &AppHandle) -> EmbedderStatusSink {
    let app = app.clone();
    Arc::new(move |s: &EmbedderStatus| {
        let _ = app.emit("embedder://status", s);
    })
}

/// A catalog entry as sent to the frontend — the model info + whether
/// it's already downloaded on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledModelWire {
    pub id: String,
    pub name: String,
    pub dim: usize,
    pub size_mb: usize,
    pub installed: bool,
}

/// List the curated bundled embedding models, each flagged `installed`
/// when its cache subdir already exists on this machine.
#[tauri::command]
pub async fn list_bundled_embedding_models() -> Result<Vec<BundledModelWire>, IpcError> {
    Ok(embedding_model_catalog(&embedder_cache_dir())
        .into_iter()
        .map(|m| BundledModelWire {
            id: m.id,
            name: m.name,
            dim: m.dim,
            size_mb: m.size_mb,
            installed: m.installed,
        })
        .collect())
}

/// Download a bundled model in the background, emitting `embedder://status`
/// events with `Downloading { model, progress }` as it goes, then `Ready`
/// on completion (or `Failed` on error). The download is driven by building
/// a `BundledEmbedder` (fastembed pulls the model on init); progress is
/// polled by sampling the cache dir size against the catalog's `size_mb`.
#[tauri::command]
pub async fn download_bundled_model(
    app: AppHandle,
    state: State<'_, IpcState>,
    model: String,
) -> Result<(), IpcError> {
    // Validate the model id is in the catalog.
    let info = embedding_model_catalog(&embedder_cache_dir())
        .into_iter()
        .find(|m| m.id == model)
        .ok_or_else(|| IpcError::msg(format!("unknown embedding model '{model}'")))?;
    let cache_dir = embedder_cache_dir();
    let status = state.runtime.embedder_status.clone();
    // The Settings path only downloads + flips status — the live embedder
    // is swapped by the config-save rewire (the model is installed on
    // disk by then, so build_embedder loads it synchronously).
    spawn_download(
        Some(status_sink(&app)),
        status,
        model,
        info.size_mb,
        cache_dir,
        DownloadOutcome::StatusOnly,
    );
    Ok(())
}/// First-run startup path: the configured bundled model isn't installed
/// yet, so startup built the hash embedder instead. Downloads the model
/// in the background (progress via `embedder://status`) and on success
/// installs the fresh embedder into the live memory store + re-embeds
/// all rows, so semantic recall goes live without a restart (mirrors
/// the Settings rewire path). Fire-and-forget — never blocks startup.
pub fn spawn_startup_download(
    app: AppHandle,
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
) {
    // An unknown model id reaching here (deferred by embedder_startup_plan)
    // has no catalog entry — treat it as size 0 and let the build fail
    // gracefully (status Failed).
    let size_mb = embedding_model_catalog(&embedder_cache_dir())
        .into_iter()
        .find(|m| m.id == model)
        .map(|m| m.size_mb)
        .unwrap_or(0);
    spawn_download(
        Some(status_sink(&app)),
        status,
        model,
        size_mb,
        embedder_cache_dir(),
        DownloadOutcome::SwapIntoStore(store),
    );
}

/// Installed-model startup path: the configured bundled model is
/// already on disk, but its ~110 MB ONNX load was deferred off the
/// main thread (hang diagnostics, AppHangB1 2026-08-20). Loads it in
/// the background on the blocking pool and on success installs it into
/// the live memory store + re-embeds rows — the same install path the
/// download uses, minus the network. Fire-and-forget — never blocks
/// startup. Status transitions are emitted on `embedder://status`.
pub fn spawn_startup_load(
    app: AppHandle,
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
) {
    spawn_startup_load_task(
        Some(status_sink(&app)),
        store,
        status,
        model,
        embedder_cache_dir(),
    );
}

/// Console-mode twin of [`spawn_startup_load`]: the same background
/// load + install swap, minus the Tauri status event (the console has
/// no frontend to feed). Review finding 1 (2026-08-20): without this,
/// `-console` with an installed bundled model silently degraded to the
/// hash embedder with the status stuck at `Checking` for the whole
/// session — semantic memory was quietly lost.
pub fn spawn_startup_load_console(
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
) {
    spawn_startup_load_task(None, store, status, model, embedder_cache_dir());
}