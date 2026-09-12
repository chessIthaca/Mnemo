// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Embeddings — a deterministic hash embedder + an optional in-process bundled
//! embedder.
//!
//! The store always works with the built-in [`HashEmbedder`] (deterministic,
//! offline, no network). When `bundled_embedding_model` is configured, a
//! `BundledEmbedder` takes over — it runs a `fastembed` (ONNX Runtime) model
//! in-process for genuine semantic vectors. The bundled embedder is
//! failure-protected: a load or inference failure returns a zero vector +
//! flips the shared status to `Fallback`, so `recall`/`write` degrade to
//! keyword-driven ranking rather than dying.

#[cfg(feature = "embeddings")]
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The embedding dimension we use. Fixed per model — changing later requires
/// re-embedding everything. `nomic-embed-text` is 768-dim.
pub const EMBEDDING_DIM: usize = 768;

/// An embedder produces a fixed-dimension vector for a piece of text.
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single text.
    ///
    /// Infallible by design: a remote embedder that fails (timeout, auth
    /// error, service down) returns a zero vector of [`EMBEDDING_DIM`]
    /// rather than an error. This guarantees `recall`/`write` can never die
    /// from an embedding failure — a zero query vector degrades ranking to
    /// keyword + tier + strength (the existing fallback path), and a zero
    /// stored vector contributes nothing to cosine (well-defined: 0).
    async fn embed(&self, text: &str) -> Vec<f32>;

    /// The dimension of the vectors this embedder produces.
    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }

    /// A fingerprint identifying which model produced the stored vectors —
    /// used to detect when memories (committed to git) travel to a machine
    /// running a different model, so they can be auto re-embedded, and to
    /// tier recall's cosine weight (0.2 for the `"hash"` fallback, 0.35
    /// otherwise — see `recall_peek`). Real embedding models MUST override
    /// this: an implementation that forgets silently inherits the noise
    /// weight and its semantic matches are systematically understated. The
    /// default is `"hash"` (the offline fallback); `BundledEmbedder` (the
    /// `embeddings` feature) overrides with its model id
    /// (e.g. `"all-MiniLM-L6-v2"`).
    fn model_id(&self) -> &str {
        "hash"
    }
}

/// A deterministic embedder. Produces a stable hash-based vector so the memory
/// store works fully offline — no external service, no network calls, no
/// authentication. Same text → same vector.
pub struct HashEmbedder;

impl HashEmbedder {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Embedder for HashEmbedder {
    async fn embed(&self, text: &str) -> Vec<f32> {
        hash_embedding(text)
    }
}

/// The live status of the embedder, surfaced to the UI so the user knows
/// whether semantic recall is active or has degraded to keyword-only.
///
/// - `Ready` — the embedder is producing real vectors (hash or bundled).
/// - `Checking` — a startup probe / model load is in progress.
/// - `Fallback` — the embedder failed; recall degrades to keyword + tier +
///   strength (zero-vector queries). Used silently.
/// - `Pulling` — (legacy) the model is being pulled in the background.
/// - `Downloading` — a bundled model is downloading (carries model + progress).
/// - `Failed` — the embedder rejected init (bad model name, load failure); the
///   user must fix the configuration.
///
/// Serialization is externally tagged (the default): unit variants serialize
/// as bare lowercase strings (`"ready"`, `"checking"`, …) so the existing
/// frontend string comparisons keep working; `Downloading` serializes as
/// `{"downloading": {"model": "…", "progress": 0.5}}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EmbedderStatus {
    Ready,
    Checking,
    Fallback,
    Pulling,
    /// A bundled model is downloading. `progress` is 0.0–1.0.
    Downloading {
        model: String,
        progress: f64,
    },
    Failed,
}

impl Default for EmbedderStatus {
    fn default() -> Self {
        Self::Ready
    }
}

/// Info about a bundled embedding model the user can pick in Settings.
#[cfg(feature = "embeddings")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundledModelInfo {
    /// The model id (matches `fastembed::EmbeddingModel` + the config value).
    pub id: String,
    /// A human-friendly display name.
    pub name: String,
    /// The vector dimension this model produces.
    pub dim: usize,
    /// Approximate download size in MB.
    pub size_mb: usize,
}

/// A bundled in-process embedder backed by `fastembed` (ONNX Runtime).
///
/// Runs entirely on the user's machine — no Ollama, no cloud, no API key.
/// The model downloads on first use (cached under the app data dir) and
/// thereafter loads from disk. Inference is CPU-bound, so `embed()` runs the
/// ONNX forward pass on `spawn_blocking` to never stall the async runtime.
///
/// Failure-protected like the remote embedder: a load or inference failure
/// returns a zero vector + flips the shared status to `Fallback`, so recall
/// degrades to keyword + tier + strength rather than dying.
#[cfg(feature = "embeddings")]
pub struct BundledEmbedder {
    /// Behind an `Arc` so it can be cloned into the `spawn_blocking` task
    /// (`TextEmbedding` is not `Clone`).
    model: Arc<fastembed::TextEmbedding>,
    model_id: String,
    dim: usize,
    status: Arc<RwLock<EmbedderStatus>>,
}

#[cfg(feature = "embeddings")]
impl BundledEmbedder {
    /// Build a bundled embedder for the given model id, caching the downloaded
    /// model under `cache_dir`. Sets the shared status to `Ready` on success.
    ///
    /// Returns an error when the model id is unknown or the ONNX model fails
    /// to load (the caller falls back to `HashEmbedder`).
    pub fn new(
        model_id: &str,
        cache_dir: &std::path::Path,
        status: Arc<RwLock<EmbedderStatus>>,
    ) -> Result<Self, String> {
        let model = model_for_id(model_id)?;
        let dim = dim_for_model(&model);
        let init = fastembed::InitOptions::new(model).with_cache_dir(cache_dir.to_path_buf());
        let model = fastembed::TextEmbedding::try_new(init)
            .map_err(|e| format!("failed to load embedding model '{model_id}': {e}"))?;
        *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
        Ok(Self {
            model: Arc::new(model),
            model_id: model_id.to_string(),
            dim,
            status,
        })
    }

    /// The curated catalog of bundled models the user can pick in Settings.
    /// Ordered small → large so the default (first) is the lightest.
    pub fn available_models() -> Vec<BundledModelInfo> {
        vec![
            BundledModelInfo {
                id: "all-MiniLM-L6-v2".into(),
                name: "MiniLM L6 v2 (fast, small)".into(),
                dim: 384,
                size_mb: 90,
            },
            BundledModelInfo {
                id: "bge-small-en-v1.5".into(),
                name: "BGE Small EN v1.5 (balanced)".into(),
                dim: 384,
                size_mb: 130,
            },
            BundledModelInfo {
                id: "bge-base-en-v1.5".into(),
                name: "BGE Base EN v1.5 (higher quality)".into(),
                dim: 768,
                size_mb: 410,
            },
            BundledModelInfo {
                id: "nomic-embed-text-v1.5".into(),
                name: "Nomic Embed Text v1.5 (best quality)".into(),
                dim: 768,
                size_mb: 280,
            },
        ]
    }
}

#[cfg(feature = "embeddings")]
#[async_trait]
impl Embedder for BundledEmbedder {
    async fn embed(&self, text: &str) -> Vec<f32> {
        let model = Arc::clone(&self.model);
        let text = text.to_string();
        let dim = self.dim;
        let status = self.status.clone();
        // ONNX inference is CPU-bound — run it on the blocking pool so the
        // async runtime is never stalled. A failure returns a zero vector
        // (matching the Embedder contract) + flips the shared status.
        match tokio::task::spawn_blocking(move || model.embed(vec![text], None)).await {
            Ok(Ok(mut batches)) if !batches.is_empty() => {
                let v = batches.remove(0);
                if v.len() == dim {
                    *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
                    v
                } else {
                    // Dimension mismatch — shouldn't happen for a fixed model,
                    // but guard against a corrupt load.
                    *status.write().expect("embedder status lock poisoned") =
                        EmbedderStatus::Fallback;
                    vec![0.0_f32; dim]
                }
            }
            _ => {
                *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Fallback;
                vec![0.0_f32; dim]
            }
        }
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

/// Map a model-id string to the fastembed enum. Returns an error for an
/// unknown id (the caller surfaces it to the user).
#[cfg(feature = "embeddings")]
fn model_for_id(id: &str) -> Result<fastembed::EmbeddingModel, String> {
    match id {
        "all-MiniLM-L6-v2" => Ok(fastembed::EmbeddingModel::AllMiniLML6V2),
        "bge-small-en-v1.5" => Ok(fastembed::EmbeddingModel::BGESmallENV15),
        "bge-base-en-v1.5" => Ok(fastembed::EmbeddingModel::BGEBaseENV15),
        "nomic-embed-text-v1.5" => Ok(fastembed::EmbeddingModel::NomicEmbedTextV15),
        other => Err(format!("unknown embedding model '{other}'")),
    }
}

/// The vector dimension a fastembed model produces (used before the first
/// successful embed, for the zero-vector fallback).
#[cfg(feature = "embeddings")]
fn dim_for_model(model: &fastembed::EmbeddingModel) -> usize {
    match model {
        fastembed::EmbeddingModel::AllMiniLML6V2 => 384,
        fastembed::EmbeddingModel::BGESmallENV15 => 384,
        fastembed::EmbeddingModel::BGEBaseENV15 => 768,
        fastembed::EmbeddingModel::NomicEmbedTextV15 => 768,
        _ => EMBEDDING_DIM, // safe fallback for unlisted models
    }
}

/// The HuggingFace cache subdir name fastembed downloads a model into, for a
/// given model id (e.g. `"all-MiniLM-L6-v2"` → `"models--Qdrant--all-MiniLM-L6-v2-onnx"`).
///
/// Used to detect whether a model is already installed (the subdir exists +
/// has snapshot files) and to poll download progress (dir size vs. the
/// catalog's `size_mb`). Returns `None` for an unknown model id.
#[cfg(feature = "embeddings")]
pub fn hf_cache_subdir(model_id: &str) -> Option<String> {
    let repo = match model_id {
        "all-MiniLM-L6-v2" => "Qdrant/all-MiniLM-L6-v2-onnx",
        "bge-small-en-v1.5" => "Xenova/bge-small-en-v1.5",
        "bge-base-en-v1.5" => "Xenova/bge-base-en-v1.5",
        "nomic-embed-text-v1.5" => "nomic-ai/nomic-embed-text-v1.5",
        _ => return None,
    };
    // HF cache layout: models--<org>--<name> (slashes → double dashes).
    Some(format!("models--{}", repo.replace('/', "--")))
}

/// Whether a model's HuggingFace cache subdir exists + has the ONNX model
/// file downloaded (i.e. the model is ready to load, not mid-download).
/// Checking for the ONNX file specifically avoids a false positive when
/// hf-hub has written partial snapshot files during a download.
#[cfg(feature = "embeddings")]
pub fn is_model_installed(cache_dir: &std::path::Path, model_id: &str) -> bool {
    let Some(subdir) = hf_cache_subdir(model_id) else {
        return false;
    };
    let snapshots = cache_dir.join(subdir).join("snapshots");
    // The snapshots dir holds per-revision subdirs; a complete download has
    // the ONNX model file (model.onnx or model_quantized.onnx) present.
    if let Ok(entries) = std::fs::read_dir(&snapshots) {
        for entry in entries.flatten() {
            let rev_dir = entry.path();
            // Check for the ONNX model file at the top level or under onnx/.
            if rev_dir.join("model.onnx").exists()
                || rev_dir.join("model_quantized.onnx").exists()
                || rev_dir.join("onnx").join("model.onnx").exists()
                || rev_dir.join("onnx").join("model_quantized.onnx").exists()
            {
                return true;
            }
        }
    }
    false
}

/// Produce a deterministic EMBEDDING_DIM-dim vector from text using a hash-based
/// Produce a deterministic EMBEDDING_DIM-dim vector from text using a hash-based
/// projection. Tokenizes on whitespace and hashes each token into the vector.
pub fn hash_embedding(text: &str) -> Vec<f32> {
    let mut vec = vec![0.0_f32; EMBEDDING_DIM];
    let mut count = 0u32;
    for token in text.split_whitespace() {
        let hash = fxhash(token);
        // Use two hashes to place a +/- value at two positions (signed projection).
        let pos1 = (hash % EMBEDDING_DIM as u64) as usize;
        let pos2 = ((hash >> 32) % EMBEDDING_DIM as u64) as usize;
        vec[pos1] += 1.0;
        vec[pos2] -= 1.0;
        count += 1;
    }
    // Normalize to unit length so cosine similarity is meaningful.
    let norm = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut vec {
            *v /= norm;
        }
    }
    let _ = count;
    vec
}

/// A simple, fast string hash (FNV-1a 64-bit).
fn fxhash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "embeddings")]
    #[test]
    fn bundled_catalog_is_non_empty_and_unique() {
        let models = BundledEmbedder::available_models();
        assert!(!models.is_empty(), "catalog must list models");
        // Each model id is unique (it's the config key + fingerprint).
        let mut ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), models.len(), "model ids must be unique");
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn bundled_catalog_dims_match_known_models() {
        // Each catalog entry's dim must match the fastembed model's actual dim
        // (so the zero-vector fallback + fingerprint are correct before the
        // first real embed).
        for info in BundledEmbedder::available_models() {
            let model = model_for_id(&info.id).expect("catalog id maps to a model");
            assert_eq!(
                dim_for_model(&model),
                info.dim,
                "catalog dim for {} must match the model's dim",
                info.id
            );
        }
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn model_for_id_rejects_unknown() {
        assert!(model_for_id("not-a-real-model").is_err());
        assert!(model_for_id("all-MiniLM-L6-v2").is_ok());
    }

    #[tokio::test]
    async fn hash_embedder_is_deterministic() {
        let e = HashEmbedder::new();
        let v1 = e.embed("hello world").await;
        let v2 = e.embed("hello world").await;
        assert_eq!(v1, v2);
    }

    #[tokio::test]
    async fn hash_embedder_different_text_different_vector() {
        let e = HashEmbedder::new();
        let v1 = e.embed("hello world").await;
        let v2 = e.embed("goodbye universe").await;
        assert_ne!(v1, v2);
    }

    #[test]
    fn hash_embedding_is_unit_length() {
        let v = hash_embedding("some text here for embedding");
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}");
    }

    #[test]
    fn hash_embedding_correct_dim() {
        let v = hash_embedding("test");
        assert_eq!(v.len(), EMBEDDING_DIM);
    }

    #[test]
    fn hash_embedding_empty_text() {
        let v = hash_embedding("");
        assert_eq!(v.len(), EMBEDDING_DIM);
        // All zeros (norm 0, no normalization).
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn cosine_similarity_same_text_is_one() {
        let v = hash_embedding("the quick brown fox");
        let sim = cosine(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "sim = {sim}");
    }

    #[test]
    fn cosine_similarity_shared_words_higher() {
        let v1 = hash_embedding("the quick brown fox");
        let v2 = hash_embedding("the quick brown dog");
        let v3 = hash_embedding("completely different words here");
        let sim_shared = cosine(&v1, &v2);
        let sim_diff = cosine(&v1, &v3);
        // Shared words → higher similarity than completely different.
        assert!(
            sim_shared > sim_diff,
            "shared {sim_shared} > diff {sim_diff}"
        );
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }
}
