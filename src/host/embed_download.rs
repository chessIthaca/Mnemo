// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Bundled embedding-model surface (job 4 D6, §5.8 :542-547) — the
//! download/load core MOVED here from the app's `ipc/embeddings.rs`,
//! Tauri-free: the shared `EmbedderStatus` Arc is the state carrier and
//! [`EmbedderStatusSink`] is the push (the app's command layer forwards
//! to `embedder://status`; the headless `Setup` feeds its
//! `EventReceiver<SetupEvent>`).
//!
//! The cache dir is a PARAMETER — callers derive it from their config
//! dir (`<config_dir>/models`, the same derivation the brain's startup
//! build uses). The app passes the machine-global dir; a `Setup` with
//! a custom config dir isolates its installs (G4's discipline).
//!
//! The models run in-process via `fastembed` (ONNX Runtime) — no
//! Ollama, no cloud. A model downloads on first use: building a
//! `BundledEmbedder` makes fastembed pull it; progress is sampled from
//! the cache-dir size against the catalog's `size_mb`. Everything that
//! touches the ONNX runtime is `#[cfg(feature = "embeddings")]`; the
//! catalog DTO + the air-gapped copy work in every build (the copy is
//! plain fs).

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(feature = "embeddings")]
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::memory::embedder::EmbedderStatus;

/// The status-change push (D6): the moved core writes the shared
/// [`EmbedderStatus`] Arc AND hands every change to this sink — the
/// app's command layer forwards to `embedder://status`, the headless
/// `Setup::install_embedding_model` feeds its
/// `EventReceiver<SetupEvent>`. `None` = status writes only (the
/// console twin).
pub type EmbedderStatusSink = Arc<dyn Fn(&EmbedderStatus) + Send + Sync>;

/// A catalog entry for the headless surface (§5.8 :542): the bundled
/// model info + whether it is installed in the cache this catalog was
/// listed against. The app's wire type maps over it unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddingModelDto {
    /// The model id (matches `fastembed::EmbeddingModel` + the config value).
    pub id: String,
    /// A human-friendly display name.
    pub name: String,
    /// The vector dimension this model produces.
    pub dim: usize,
    /// Approximate download size in MB.
    pub size_mb: usize,
    /// Installed in the cache dir this entry was listed against?
    pub installed: bool,
}

/// The curated catalog, each entry flagged `installed` against
/// `cache_dir`. Without the `embeddings` feature the catalog is empty —
/// semantic search runs on the deterministic hash embedder only.
pub fn embedding_model_catalog(cache_dir: &Path) -> Vec<EmbeddingModelDto> {
    #[cfg(feature = "embeddings")]
    {
        crate::memory::embedder::BundledEmbedder::available_models()
            .into_iter()
            .map(|m| EmbeddingModelDto {
                installed: crate::memory::embedder::is_model_installed(cache_dir, &m.id),
                id: m.id,
                name: m.name,
                dim: m.dim,
                size_mb: m.size_mb,
            })
            .collect()
    }
    #[cfg(not(feature = "embeddings"))]
    {
        // No ONNX runtime in this build: the curated catalog is empty —
        // semantic search runs on the deterministic hash embedder only.
        let _ = &cache_dir;
        Vec::new()
    }
}

/// Copy an already-downloaded model tree into the cache — the
/// air-gapped install (§5.8 :544: `install_embedding_model_from`):
/// `source_dir` holds the HF-cache layout (`models--<org>--<name>/…`,
/// e.g. a USB stick or an internal mirror), and the model lands under
/// `cache_dir` ready to load. Plain fs — no network, every build.
pub fn copy_model_from(
    source_dir: &Path,
    model_id: &str,
    cache_dir: &Path,
) -> std::io::Result<PathBuf> {
    let Some(subdir) = crate::memory::embedder::hf_cache_subdir(model_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unknown embedding model '{model_id}'"),
        ));
    };
    let src = source_dir.join(&subdir);
    if !src.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "'{model_id}' is not present under {} (expected the \
                 '{subdir}' subdir of the HuggingFace cache layout)",
                source_dir.display()
            ),
        ));
    }
    let dest = cache_dir.join(&subdir);
    copy_dir_recursive(&src, &dest)?;
    Ok(dest)
}

/// Recursively copy a dir tree (create parents as needed; overwrites
/// per-file). Small trees — the ONNX model files only.
fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The air-gapped copy reproduces the HF cache layout under the
    /// cache dir, unknown ids reject, and a missing source tree rejects.
    #[test]
    fn copy_model_from_copies_the_hf_layout_and_validates() {
        let source = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // The all-MiniLM HF layout: models--Qdrant/all-MiniLM-L6-v2-onnx
        // → snapshots/<rev>/model_quantized.onnx (the exact file
        // `is_model_installed` probes for).
        let onnx = source
            .path()
            .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
            .join("snapshots")
            .join("deadbeef");
        std::fs::create_dir_all(&onnx).unwrap();
        std::fs::write(onnx.join("model_quantized.onnx"), b"onnx-bytes").unwrap();

        copy_model_from(
            source.path(),
            "all-MiniLM-L6-v2",
            cache.path().join("models").as_path(),
        )
        .unwrap();
        let copied = cache
            .path()
            .join("models")
            .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
            .join("snapshots")
            .join("deadbeef")
            .join("model_quantized.onnx");
        assert_eq!(
            std::fs::read(&copied).unwrap(),
            b"onnx-bytes".to_vec(),
            "the copy reproduces the HF tree"
        );

        // Unknown ids reject with InvalidInput.
        let err =
            copy_model_from(source.path(), "no-such-model", cache.path().join("models").as_path())
                .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

        // A source dir without the model's tree rejects with NotFound.
        let empty = tempfile::tempdir().unwrap();
        let err =
            copy_model_from(empty.path(), "all-MiniLM-L6-v2", cache.path().join("models").as_path())
                .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}// ---------------------------------------------------------------------------
// The download/load core (needs the ONNX runtime — every item gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "embeddings")]
use crate::memory::embedder::{is_model_installed, BundledEmbedder, Embedder};
#[cfg(feature = "embeddings")]
use crate::memory::{MemoryStore, MemoryStoreTrait};

/// The status write + sink push (the moved write + `app.emit` pair).
#[cfg(feature = "embeddings")]
fn push_status(
    sink: Option<&EmbedderStatusSink>,
    status: &Arc<RwLock<EmbedderStatus>>,
    s: EmbedderStatus,
) {
    *status.write().expect("embedder status lock poisoned") = s.clone();
    if let Some(sink) = sink {
        sink(&s);
    }
}

/// Estimate download progress (0.0–1.0) by sampling the model's cache
/// subdir size against the catalog's `size_mb`. Coarse (dir size vs.
/// expected), but fastembed exposes no callback fraction — the best
/// available signal.
#[cfg(feature = "embeddings")]
fn cache_dir_progress(cache_dir: &Path, model_id: &str, size_mb: usize) -> f64 {
    let Some(subdir) = crate::memory::embedder::hf_cache_subdir(model_id) else {
        return 0.0;
    };
    let model_dir = cache_dir.join(subdir);
    let bytes = crate::host::laya::dir_size(&model_dir);
    let expected = (size_mb as u64).saturating_mul(1024 * 1024);
    if expected == 0 {
        return 0.0;
    }
    (bytes as f64 / expected as f64).clamp(0.0, 0.99)
}

/// What a finished background download does with the freshly built
/// embedder.
#[cfg(feature = "embeddings")]
pub enum DownloadOutcome {
    /// Just flip the status to `Ready` (the Settings download — the
    /// live embedder is swapped by the next settings save, when the
    /// model is on disk and `build_embedder` loads it synchronously).
    StatusOnly,
    /// Install the embedder into the live memory store + re-embed all
    /// rows (first-run startup, where the store currently runs the
    /// hash embedder).
    SwapIntoStore(Arc<MemoryStore>),
}

/// Late-arrival guard: may the freshly loaded `loaded_model` replace the
/// store's current `current_model`?
///
/// `"hash"` is the [`Embedder`] trait's default fingerprint — the
/// interim startup embedder both background paths install over, so
/// installing is always wanted there. A same-model install is an
/// idempotent no-op (also allowed). Anything else means a newer model
/// selection won while the background load/download was in flight —
/// skip, so the late arrival can't clobber the user's choice.
#[cfg(feature = "embeddings")]
fn install_allowed(current_model: &str, loaded_model: &str) -> bool {
    current_model == "hash" || current_model == loaded_model
}

/// Install a freshly downloaded embedder into the live memory store and
/// re-embed all rows when the stored vectors don't already match (first
/// run: hash vectors; or a model change). Mirrors the rewire fingerprint
/// semantics. The store keeps its standing (hash) embedder on any
/// failure — memory degrades gracefully rather than dying.
///
/// Late-arrival guard (review finding, 2026-08-21): both startup paths
/// (download and local load) install from a background task that can
/// take 1–30 s, so a user can switch models in Settings while it is in
/// flight — the rewire already swapped the store to the newly selected
/// model, and a late-arriving old-model embedder must not clobber that
/// choice (see [`install_allowed`]). The status flips to `Ready`
/// regardless (both models are valid `Ready` states; the rewire's own
/// status write stays authoritative for the model the user picked).
#[cfg(feature = "embeddings")]
pub async fn install_embedder(store: Arc<MemoryStore>, embedder: Arc<dyn Embedder>) {
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
    // (same rationale as the rewire: cross-machine or interrupted
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
}/// The load pipeline shared by the startup paths: build the
/// `BundledEmbedder` on the blocking pool (ONNX init is blocking), flip
/// the status to `Ready`/`Failed`, and on success run the live-store
/// swap. The sink receives each status change (the app emits
/// `embedder://status`; the console passes `None`); the cache dir is
/// the caller's derivation.
#[cfg(feature = "embeddings")]
async fn run_startup_load(
    sink: Option<EmbedderStatusSink>,
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
    cache_dir: PathBuf,
) {
    let load_status = status.clone();
    let load_model = model.clone();
    let load_dir = cache_dir.clone();
    let result =
        tokio::task::spawn_blocking(move || BundledEmbedder::new(&load_model, &load_dir, load_status))
            .await;

    match result {
        Ok(Ok(embedder)) => {
            push_status(sink.as_ref(), &status, EmbedderStatus::Ready);
            install_embedder(store, Arc::new(embedder)).await;
        }
        Ok(Err(e)) => {
            // Same graceful degradation as the old synchronous path:
            // keep the standing hash embedder, surface the failure.
            eprintln!("error: bundled model local load failed: {e}");
            push_status(sink.as_ref(), &status, EmbedderStatus::Failed);
        }
        Err(e) => {
            eprintln!("error: bundled model load task panicked: {e}");
            push_status(sink.as_ref(), &status, EmbedderStatus::Failed);
        }
    }
}

/// The moved startup-load task wrapper: fire-and-forget on tokio (the
/// app's GUI twin passes its emit sink, the console twin `None`).
#[cfg(feature = "embeddings")]
pub fn spawn_startup_load_task(
    sink: Option<EmbedderStatusSink>,
    store: Arc<MemoryStore>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
    cache_dir: PathBuf,
) {
    tokio::spawn(run_startup_load(sink, store, status, model, cache_dir));
}

/// The shared download engine (Settings download + the first-run
/// startup path): mark `Downloading`, spawn the progress poller + the
/// blocking model build (fastembed pulls the model on init), then run
/// the outcome. Fire-and-forget.
#[cfg(feature = "embeddings")]
pub fn spawn_download(
    sink: Option<EmbedderStatusSink>,
    status: Arc<RwLock<EmbedderStatus>>,
    model: String,
    size_mb: usize,
    cache_dir: PathBuf,
    outcome: DownloadOutcome,
) {
    // Mark Downloading immediately so the UI shows activity.
    push_status(
        sink.as_ref(),
        &status,
        EmbedderStatus::Downloading {
            model: model.clone(),
            progress: 0.0,
        },
    );

    tokio::spawn(async move {
        // Progress poller: sample the cache dir size every 500 ms while
        // the build task runs, pushing Downloading { progress }. Stops
        // once the model is installed (the build finished).
        let poll_status = status.clone();
        let poll_model = model.clone();
        let poll_sink = sink.clone();
        let poll_dir = cache_dir.clone();
        let poll_handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let progress = cache_dir_progress(&poll_dir, &poll_model, size_mb);
                let s = EmbedderStatus::Downloading {
                    model: poll_model.clone(),
                    progress,
                };
                push_status(poll_sink.as_ref(), &poll_status, s);
                if is_model_installed(&poll_dir, &poll_model) {
                    break;
                }
            }
        });

        // The build: loads the model (downloads on first use). The
        // blocking pool — ONNX init + the HF download are blocking.
        let build_status = status.clone();
        let build_dir = cache_dir.clone();
        let build_model = model.clone();
        let result = tokio::task::spawn_blocking(move || {
            BundledEmbedder::new(&build_model, &build_dir, build_status)
        })
        .await;

        // Stop the progress poller BEFORE propagating a failure: a
        // panicked build must not leak the poller clobbering the
        // status every 500 ms (the laya pipeline's rule).
        poll_handle.abort();

        match result {
            Ok(Ok(embedder)) => {
                push_status(sink.as_ref(), &status, EmbedderStatus::Ready);
                if let DownloadOutcome::SwapIntoStore(store) = outcome {
                    install_embedder(store, Arc::new(embedder)).await;
                }
            }
            Ok(Err(e)) => {
                eprintln!("error: bundled model download failed: {e}");
                push_status(sink.as_ref(), &status, EmbedderStatus::Failed);
            }
            Err(e) => {
                eprintln!("error: bundled model build task panicked: {e}");
                push_status(sink.as_ref(), &status, EmbedderStatus::Failed);
            }
        }
    });
}

#[cfg(test)]
mod download_core_tests {
    #[cfg(feature = "embeddings")]
    use super::install_allowed;

    /// Review finding 1 (2026-08-21): a background startup load that
    /// lands AFTER the user switched models in Settings must not
    /// clobber the newer selection.
    #[cfg(feature = "embeddings")]
    #[test]
    fn newer_model_selection_wins_over_late_arrival() {
        // The interim hash embedder (trait default model id "hash") is
        // always installable-over — the normal startup case.
        assert!(install_allowed("hash", "all-MiniLM-L6-v2"));
        // Same model → idempotent no-op, allowed.
        assert!(install_allowed("all-MiniLM-L6-v2", "all-MiniLM-L6-v2"));
        // A DIFFERENT bundled model already installed → a newer
        // selection won; the late-arriving load must not replace it.
        assert!(!install_allowed("all-MiniLM-L12-v2", "all-MiniLM-L6-v2"));
    }
}