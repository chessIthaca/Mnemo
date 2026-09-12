// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Bundled embedding model commands — list the catalog, download a model with
//! progress, and query the selected model's status.
//!
//! The models run in-process via `fastembed` (ONNX Runtime) — no Ollama, no
//! cloud. A model downloads on first use and caches under the global config
//! dir's `models/` subdir (NOT `.coding/`, which is committed to git — model
//! binaries are per-machine).
//!
//! The first-run startup path lives here too: when the configured model isn't
//! installed yet, [`spawn_startup_download`] pulls it in the background and
//! installs it into the live memory store, so app startup never blocks on the
//! download. An INSTALLED model takes the same shape — [`spawn_startup_load`]
//! (GUI) / [`spawn_startup_load_console`] (console mode) load it in the
//! background and swap it into the live store, so the ~110 MB ONNX init never
//! blocks startup either.

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use mnemo::config::global_config_dir;
use mnemo::memory::embedder::{
    is_model_installed, BundledEmbedder, BundledModelInfo, Embedder, EmbedderStatus,
};
use mnemo::memory::{MemoryStore, MemoryStoreTrait};

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The cache dir for bundled embedding models (under the global config dir).
fn embedder_cache_dir() -> std::path::PathBuf {
    global_config_dir().join("models")
}

/// A catalog entry as sent to the frontend — the model info + whether it's
/// already downloaded on this machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundledModelWire {
    pub id: String,
    pub name: String,
    pub dim: usize,
    pub size_mb: usize,
    pub installed: bool,
}

/// List the curated bundled embedding models, each flagged `installed` when
/// its cache subdir already exists on this machine.
#[tauri::command]
pub async fn list_bundled_embedding_models() -> Result<Vec<BundledModelWire>, IpcError> {
    let cache_dir = embedder_cache_dir();
    Ok(BundledEmbedder::available_models()
        .into_iter()
        .map(|m| to_wire(m, &cache_dir))
        .collect())
}

/// Download a bundled model in the background, emitting `embedder://status`
/// events with `Downloading { model, progress }` as it goes, then `Ready` on
/// completion (or `Failed` on error). The download is driven by building a
/// `BundledEmbedder` (fastembed pulls the model on init); progress is polled
/// by sampling the cache dir size against the catalog's `size_mb`.
#[tauri::command]
pub async fn download_bundled_model(
    app: AppHandle,
    state: State<'_, IpcState>,
    model: String,
) -> Result<(), IpcError> {
    // Validate the model id is in the catalog.
    let info = BundledEmbedder::available_models()
        .into_iter()
        .find(|m| m.id == model)
        .ok_or_else(|| IpcError::msg(format!("unknown embedding model '{model}'")))?;
    let cache_dir = embedder_cache_dir();
    let status = state.runtime.embedder_status.clone();
    // The Settings path only downloads + flips status — the live embedder is
    // swapped by the config-save rewire (the model is installed on disk by
    // then, so build_embedder loads it synchronously).
    spawn_download(
        app,
        status,
        model,
        info.size_mb,
        cache_dir,
        DownloadOutcome::StatusOnly,
    );
    Ok(())
}

/// First-run startup path: the configured bundled model isn't installed yet,
/// so startup built the hash embedder instead. Downloads the model in the
/// background (progress via `embedder://status`) and on success installs the
/// fresh embedder into the live memory store + re-embeds all rows, so
/// semantic recall goes live without a restart (mirrors the Settings rewire
/// path, rewire.rs). Fire-and-forget — never blocks startup.
pub fn spawn_startup_download(
    app: AppHandle,
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
) {
    // An unknown model id reaching here (deferred by embedder_startup_plan)
    // has no catalog entry — treat it as size 0 and let the build fail
    // gracefully (status Failed).
    let size_mb = BundledEmbedder::available_models()
        .into_iter()
        .find(|m| m.id == model)
        .map(|m| m.size_mb)
        .unwrap_or(0);
    let cache_dir = embedder_cache_dir();
    spawn_download(
        app,
        status,
        model,
        size_mb,
        cache_dir,
        DownloadOutcome::SwapIntoStore(store),
    );
}

/// What a finished background download does with the freshly built embedder.
enum DownloadOutcome {
    /// Just flip the status to Ready (Settings download command — the live
    /// embedder is swapped by the next config save).
    StatusOnly,
    /// Install the embedder into the live memory store + re-embed all rows
    /// (first-run startup, where the store currently runs the hash embedder).
    SwapIntoStore(Arc<MemoryStore>),
}

/// Installed-model startup path: the configured bundled model is already on
/// disk, but its ~110 MB ONNX load was deferred off the main thread (hang
/// diagnostics, AppHangB1 2026-08-20). Loads it in the background on the
/// blocking pool and on success installs it into the live memory store +
/// re-embeds rows — the same [`install_embedder`] swap the download path
/// uses, minus the network. Fire-and-forget — never blocks startup.
///
/// GUI flavor: status transitions are emitted on `embedder://status` so the
/// frontend's status bar stays honest. Console mode has no frontend — it
/// uses the twin [`spawn_startup_load_console`].
pub fn spawn_startup_load(
    app: AppHandle,
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
) {
    tauri::async_runtime::spawn(run_startup_load(store, status, model, move |s| {
        let _ = app.emit("embedder://status", s);
    }));
}

/// Console-mode twin of [`spawn_startup_load`]: the same background load +
/// install swap, minus the Tauri status event (the console has no frontend
/// to feed).
///
/// Review finding 1 (2026-08-20): without this, `-console` with an
/// installed bundled model silently degraded to the hash embedder with the
/// status stuck at `Checking` for the whole session — semantic memory was
/// quietly lost. The GUI setup hook and the console both consume
/// `Brain::pending_model_load` now.
pub fn spawn_startup_load_console(
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
) {
    tokio::spawn(run_startup_load(store, status, model, |_| {}));
}

/// The load pipeline shared by the GUI and console startup paths: build the
/// `BundledEmbedder` on the blocking pool (ONNX init is blocking), flip the
/// status to [`Ready`](EmbedderStatus::Ready) / [`Failed`](EmbedderStatus::Failed),
/// and on success install the live-store swap. `emit` is invoked with each
/// status change (the GUI emits `embedder://status`; console passes a no-op).
async fn run_startup_load(
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
    emit: impl Fn(&EmbedderStatus) + Send + Sync + 'static,
) {
    let cache_dir = embedder_cache_dir();
    let load_status = status.clone();
    let load_model = model.clone();
    let load_dir = cache_dir.clone();
    let result = tokio::task::spawn_blocking(move || {
        BundledEmbedder::new(&load_model, &load_dir, load_status)
    })
    .await;

    match result {
        Ok(Ok(embedder)) => {
            let s = EmbedderStatus::Ready;
            *status.write().expect("embedder status lock poisoned") = s.clone();
            emit(&s);
            install_embedder(store, Arc::new(embedder)).await;
        }
        Ok(Err(e)) => {
            // Same graceful degradation as the old synchronous path:
            // keep the standing hash embedder, surface the failure.
            eprintln!("error: bundled model local load failed: {e}");
            let s = EmbedderStatus::Failed;
            *status.write().expect("embedder status lock poisoned") = s.clone();
            emit(&s);
        }
        Err(e) => {
            eprintln!("error: bundled model load task panicked: {e}");
            let s = EmbedderStatus::Failed;
            *status.write().expect("embedder status lock poisoned") = s.clone();
            emit(&s);
        }
    }
}

/// The shared download engine: mark `Downloading`, spawn the progress poller
/// + blocking model build, then run the outcome. Fire-and-forget.
fn spawn_download(
    app: AppHandle,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
    size_mb: usize,
    cache_dir: std::path::PathBuf,
    outcome: DownloadOutcome,
) {
    // Mark Downloading immediately so the UI shows activity.
    {
        let s = EmbedderStatus::Downloading {
            model: model.clone(),
            progress: 0.0,
        };
        *status.write().expect("embedder status lock poisoned") = s.clone();
        let _ = app.emit("embedder://status", &s);
    }

    // Spawn the download + progress poll. The build blocks on the ONNX model
    // load (which triggers the HF download), so it runs on the blocking pool.
    // A separate poll task samples the cache dir size to emit progress.
    let app_clone = app.clone();
    let status_clone = status.clone();
    let model_clone = model.clone();
    tauri::async_runtime::spawn(async move {
        // Progress poller: sample the cache dir size every 500ms while the
        // build task runs, emitting Downloading { progress }.
        let poll_status = status_clone.clone();
        let poll_model = model_clone.clone();
        let poll_app = app_clone.clone();
        let poll_dir = cache_dir.clone();
        let poll_handle = tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let progress = cache_dir_progress(&poll_dir, &poll_model, size_mb);
                let s = EmbedderStatus::Downloading {
                    model: poll_model.clone(),
                    progress,
                };
                *poll_status.write().expect("embedder status lock poisoned") = s.clone();
                let _ = poll_app.emit("embedder://status", &s);
                // Stop polling once the model is installed (the build finished).
                if is_model_installed(&poll_dir, &poll_model) {
                    break;
                }
            }
        });

        // The build: loads the model (downloads on first use). Runs on the
        // blocking pool because ONNX init + the HF download are blocking.
        let build_status = status.clone();
        let build_dir = cache_dir.clone();
        let build_model = model_clone.clone();
        let result = tokio::task::spawn_blocking(move || {
            BundledEmbedder::new(&build_model, &build_dir, build_status)
        })
        .await;

        // Stop the progress poller.
        poll_handle.abort();

        match result {
            Ok(Ok(embedder)) => {
                let s = EmbedderStatus::Ready;
                *status.write().expect("embedder status lock poisoned") = s.clone();
                let _ = app_clone.emit("embedder://status", &s);
                if let DownloadOutcome::SwapIntoStore(store) = outcome {
                    install_embedder(store, Arc::new(embedder)).await;
                }
            }
            Ok(Err(e)) => {
                eprintln!("error: bundled model download failed: {e}");
                let s = EmbedderStatus::Failed;
                *status.write().expect("embedder status lock poisoned") = s.clone();
                let _ = app_clone.emit("embedder://status", &s);
            }
            Err(e) => {
                eprintln!("error: bundled model build task panicked: {e}");
                let s = EmbedderStatus::Failed;
                *status.write().expect("embedder status lock poisoned") = s.clone();
                let _ = app_clone.emit("embedder://status", &s);
            }
        }
    });
}

/// Late-arrival guard: may the freshly loaded `loaded_model` replace the
/// store's current `current_model`?
///
/// `"hash"` is the [`Embedder`] trait's default fingerprint — the interim
/// startup embedder both background paths install over, so installing is
/// always wanted there. A same-model install is an idempotent no-op (also
/// allowed). Anything else means a newer model selection won while the
/// background load/download was in flight — skip, so the late arrival can't
/// clobber the user's choice.
fn install_allowed(current_model: &str, loaded_model: &str) -> bool {
    current_model == "hash" || current_model == loaded_model
}

/// Install a freshly downloaded embedder into the live memory store and
/// re-embed all rows when the stored vectors don't already match (first run:
/// hash vectors; or a model change). Mirrors the rewire.rs fingerprint
/// semantics. The store keeps its standing (hash) embedder on any failure —
/// memory degrades gracefully rather than dying.
///
/// Late-arrival guard (review finding, 2026-08-21): both startup paths
/// (download and local load) install from a background task that can take
/// 1–30 s, so a user can switch models in Settings while it is in flight —
/// the rewire already swapped the store to the newly selected model, and a
/// late-arriving old-model embedder must not clobber that choice (see
/// [`install_allowed`]). The status flips to `Ready` regardless (both models
/// are valid `Ready` states; the rewire's own status write stays
/// authoritative for the model the user picked).
async fn install_embedder(store: Arc<MemoryStore>, embedder: Arc<dyn Embedder>) {
    let expected_model = embedder.model_id().to_string();
    let current_model = store.embedder_handle().model_id().to_string();
    if !install_allowed(&current_model, &expected_model) {
        eprintln!(
            "info: skipping embedder install for '{expected_model}' — the store \
             already runs '{current_model}' (a newer model selection won while \
             the background load was in flight)"
        );
        return;
    }
    let expected_dim = embedder.dim();
    // Re-embed unless the stored set is EXACTLY the expected fingerprint
    // (same rationale as main.rs/rewire.rs: cross-machine or interrupted
    // re-embed states also mismatch).
    match store.stored_model_fingerprints().await {
        Ok(fps) if fps.len() == 1 && fps[0].0 == expected_model && fps[0].1 == expected_dim => {}
        Ok(_) => {
            eprintln!(
                "info: re-embedding memories for model '{expected_model}' (dim {expected_dim})"
            );
            match store.reembed_all(embedder.clone(), None).await {
                Ok(n) => eprintln!("info: re-embedded {n} memories"),
                Err(e) => eprintln!("warning: reembed_all failed: {e}"),
            }
        }
        Err(e) => eprintln!("warning: stored_model_fingerprints failed: {e}"),
    }
    // Swap the live embedder either way — the model is on disk + loaded now.
    store.set_embedder(embedder);
    eprintln!("info: bundled embedding model '{expected_model}' active");
}

/// Estimate download progress (0.0–1.0) by sampling the model's cache subdir
/// size against the catalog's `size_mb`. Coarse (dir size vs. expected), but
/// fastembed exposes no callback fraction — this is the best available signal.
fn cache_dir_progress(cache_dir: &std::path::Path, model_id: &str, size_mb: usize) -> f64 {
    let Some(subdir) = mnemo::memory::embedder::hf_cache_subdir(model_id) else {
        return 0.0;
    };
    let model_dir = cache_dir.join(subdir);
    let bytes = dir_size(&model_dir);
    let expected = (size_mb as u64).saturating_mul(1024 * 1024);
    if expected == 0 {
        return 0.0;
    }
    (bytes as f64 / expected as f64).clamp(0.0, 0.99)
}

/// Recursively sum file sizes under a dir (0 if missing).
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// Map a catalog entry to its wire form, flagging `installed`.
fn to_wire(m: BundledModelInfo, cache_dir: &std::path::Path) -> BundledModelWire {
    BundledModelWire {
        installed: is_model_installed(cache_dir, &m.id),
        id: m.id,
        name: m.name,
        dim: m.dim,
        size_mb: m.size_mb,
    }
}

#[cfg(test)]
mod install_guard_tests {
    use super::install_allowed;

    /// Review finding 1 (2026-08-21): a background startup load that lands
    /// AFTER the user switched models in Settings must not clobber the newer
    /// selection.
    #[test]
    fn newer_model_selection_wins_over_late_arrival() {
        // The interim hash embedder (trait default model id "hash") is always
        // installable-over — the normal startup case.
        assert!(install_allowed("hash", "all-MiniLM-L6-v2"));
        // Same model → idempotent no-op, allowed.
        assert!(install_allowed("all-MiniLM-L6-v2", "all-MiniLM-L6-v2"));
        // A DIFFERENT bundled model already installed → a newer selection won;
        // the late-arriving load must not replace it.
        assert!(!install_allowed("all-MiniLM-L12-v2", "all-MiniLM-L6-v2"));
    }
}
