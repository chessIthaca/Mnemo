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

/// The code-search embedding model the shared models host serves alongside
/// the configured memory model (plan d3336a8a step 2; fastembed's
/// `JinaEmbeddingsV2BaseCode`, dim 768, ~611 MB). Not wired into memory:
/// step 9's code-graph vectors and the `find_code` ranking use it.
#[cfg(feature = "embeddings")]
pub const CODE_EMBEDDING_MODEL: &str = "jina-embeddings-v2-base-code";

/// The tokenizer's token cap for the code-search model (plan d3336a8a step
/// 15): the model's own config allows 8192-token inputs, and one long
/// batch spikes the ONNX attention working set into gigabytes of host RAM.
/// 512 bounds it (a full embedding chunk is ~150-250 tokens, so no real
/// chunk truncates) and matches the memory model's window.
#[cfg(feature = "embeddings")]
pub const CODE_EMBEDDING_MAX_TOKENS: usize = 512;

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

    /// Embed a batch of texts in one call.
    ///
    /// The default loops [`Embedder::embed`] once per text — correct for
    /// every embedder, and the only cost a trivial one pays. Real embedders
    /// override it natively: `BundledEmbedder` runs one ONNX pass and
    /// `RemoteEmbedder` sends one HTTP request, so a batch never costs N
    /// round trips (the fully-semantic recall pool and the code-graph bulk
    /// embed depend on this).
    async fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let mut vectors = Vec::with_capacity(texts.len());
        for text in texts {
            vectors.push(self.embed(text).await);
        }
        vectors
    }

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

    /// The live status of this embedder, when it tracks one (the bundled
    /// and remote embedders do; the hash embedder has nothing to
    /// report). `None` means "no tracked status" — callers treat it as
    /// healthy and rely on [`Embedder::model_id`]. The code-graph
    /// symbol-embed pass (plan d3336a8a step 9) uses it to refuse
    /// storing vectors from a degraded embedder: a `Failed` embedder's
    /// vectors are NOT the model's — the remote embedder's degraded
    /// shape is `Failed` plus non-zero hash-fallback vectors, not
    /// zeros — so they must never be stored in the vec table.
    fn status(&self) -> Option<EmbedderStatus> {
        None
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum EmbedderStatus {
    #[default]
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
    /// Behind an `Arc<Mutex<_>>` so it can be cloned into the `spawn_blocking`
    /// task (`TextEmbedding` is not `Clone`) while still being handed out
    /// mutably — fastembed 7's `TextEmbedding::embed` takes `&mut self`.
    model: Arc<std::sync::Mutex<fastembed::TextEmbedding>>,
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
        let init = text_init_options(model, model_id, cache_dir);
        let model = fastembed::TextEmbedding::try_new(init)
            .map_err(|e| format!("failed to load embedding model '{model_id}': {e}"))?;
        *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
        Ok(Self {
            model: Arc::new(std::sync::Mutex::new(model)),
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
            BundledModelInfo {
                id: "jina-embeddings-v2-base-code".into(),
                name: "Jina Embeddings v2 Base Code (code search)".into(),
                dim: 768,
                size_mb: 611,
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
        let embed = tokio::task::spawn_blocking(move || {
            // fastembed 7's `embed` takes `&mut self`; the guard is scoped to
            // this sync closure, so it never crosses an `.await`.
            let mut model = model.lock().expect("embedder model lock poisoned");
            model.embed(vec![text], None)
        });
        match embed.await {
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

    async fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let count = texts.len();
        if count == 0 {
            return Vec::new();
        }
        let model = Arc::clone(&self.model);
        let texts = texts.to_vec();
        let dim = self.dim;
        let status = Arc::clone(&self.status);
        // One ONNX pass for the whole batch: N texts cost one inference
        // run, not N (the single-text embed does the same with one slice).
        let embed = tokio::task::spawn_blocking(move || {
            // fastembed 7's `embed` takes `&mut self`; the guard is scoped
            // to this sync closure, so it never crosses an `.await`.
            let mut model = model.lock().expect("embedder model lock poisoned");
            model.embed(texts, None)
        });
        match embed.await {
            Ok(Ok(batches))
                if batches.len() == count && batches.iter().all(|batch| batch.len() == dim) =>
            {
                *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Ready;
                batches
            }
            _ => {
                *status.write().expect("embedder status lock poisoned") = EmbedderStatus::Fallback;
                vec![vec![0.0_f32; dim]; count]
            }
        }
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    /// The live status (the shared field this embedder keeps updated).
    /// A poisoned lock reads as Failed — the conservative answer for a
    /// caller deciding whether to store this embedder's vectors.
    fn status(&self) -> Option<EmbedderStatus> {
        Some(
            self.status
                .read()
                .map(|s| s.clone())
                .unwrap_or(EmbedderStatus::Failed),
        )
    }
}

/// The machine-wide shared models host as an [`Embedder`] (plan d3336a8a
/// step 2): the vectors come over loopback HTTP from the one host process
/// per machine, so instances never load the model into their own address
/// space while a host serves.
///
/// Failure-protected like the bundled embedder: on ANY remote error (dead
/// port, wrong token, unreadable answer, wrong shape) this embedder lazily
/// creates the in-process [`BundledEmbedder`] — today's path — and embeds
/// through it, logging at most once per minute. A fallback whose model
/// cannot load (an unknown id) becomes the permanent [`HashEmbedder`] with
/// status `Failed`, matching `build_embedder`'s failure shape. The remote
/// is retried once per cooldown window, and every retry re-reads the
/// descriptor first (the re-point rule: a host another instance replaced is
/// reached at its new port — never a cached port or token).
///
/// `dim` and `model_id` match the in-process embedder exactly, so stored
/// vectors and the store's fingerprint stay compatible across a swap
/// between remote and in-process.
#[cfg(feature = "embeddings")]
pub struct RemoteEmbedder {
    /// The per-config-dir client holding the lease and the endpoint cache.
    client: Arc<crate::host::models_host::ModelsHostClient>,
    model_id: String,
    dim: usize,
    status: Arc<RwLock<EmbedderStatus>>,
    /// The loopback HTTP client (None — always embed in-process — when it
    /// could not be built).
    http: Option<reqwest::Client>,
    /// The lazily created in-process fallback (created at most once).
    fallback: tokio::sync::Mutex<Option<Arc<dyn Embedder>>>,
    /// The cooldown window: no remote attempt until this instant passes.
    host_down_until: std::sync::Mutex<Option<std::time::Instant>>,
    /// Set by every remote failure: the next attempt re-reads the
    /// descriptor (the re-point rule) instead of trusting the cache.
    endpoint_failed: std::sync::atomic::AtomicBool,
    /// The at-most-once-a-minute error-log gate.
    last_error_log: std::sync::Mutex<Option<std::time::Instant>>,
    /// The cooldown length (tests shrink it).
    cooldown: std::time::Duration,
}

#[cfg(feature = "embeddings")]
impl RemoteEmbedder {
    /// Build the remote embedder for `model_id` through `client`. The
    /// caller has attached to (or spawned) a healthy host and owns the
    /// status transition to `Ready`; this wires the pieces — no I/O happens
    /// here.
    pub fn new(
        client: Arc<crate::host::models_host::ModelsHostClient>,
        model_id: &str,
        status: Arc<RwLock<EmbedderStatus>>,
    ) -> Self {
        let dim = model_for_id(model_id)
            .map(|model| dim_for_model(&model))
            .unwrap_or(EMBEDDING_DIM);
        let http = reqwest::Client::builder()
            .timeout(crate::host::models_host::REQUEST_TIMEOUT)
            .connect_timeout(std::time::Duration::from_secs(2))
            .build()
            .map_err(|err| {
                eprintln!(
                    "warning: could not build the models-host HTTP client ({err}); \
                     embedding in-process"
                );
                err
            })
            .ok();
        Self {
            client,
            model_id: model_id.to_string(),
            dim,
            status,
            http,
            fallback: tokio::sync::Mutex::new(None),
            host_down_until: std::sync::Mutex::new(None),
            endpoint_failed: std::sync::atomic::AtomicBool::new(false),
            last_error_log: std::sync::Mutex::new(None),
            cooldown: crate::host::models_host::HOST_DOWN_COOLDOWN,
        }
    }

    /// Tests only: shrink the host-down cooldown (the re-point window).
    #[cfg(test)]
    pub(crate) fn override_cooldown(&mut self, cooldown: std::time::Duration) {
        self.cooldown = cooldown;
    }

    /// The shared embed path: remote first (outside the cooldown), the
    /// in-process fallback on any error — a shared service is an
    /// optimisation, never a new single point of failure.
    async fn embed_texts(&self, texts: &[String]) -> Vec<Vec<f32>> {
        if texts.is_empty() {
            return Vec::new();
        }
        if self.host_down() {
            return self.embed_fallback(texts).await;
        }
        match self.embed_remote(texts).await {
            Some(vectors) => vectors,
            None => self.embed_fallback(texts).await,
        }
    }

    /// The one remote attempt: resolve the endpoint (re-reading the
    /// descriptor after any failure — the re-point rule), POST the batch,
    /// and validate the answer's shape. Errors mark the host down and
    /// return `None` (the caller embeds through the fallback).
    async fn embed_remote(&self, texts: &[String]) -> Option<Vec<Vec<f32>>> {
        let Some(http) = &self.http else {
            self.mark_host_down("no HTTP client could be built");
            return None;
        };
        let Some(endpoint) = self.usable_endpoint() else {
            self.mark_host_down("no healthy host is published");
            return None;
        };
        let request = crate::host::models_host::EmbedRequest {
            model: self.model_id.clone(),
            texts: texts.to_vec(),
        };
        let answer = http
            .post(format!("http://127.0.0.1:{}/v1/embed", endpoint.port))
            .bearer_auth(&endpoint.token)
            .json(&request)
            .send()
            .await;
        let answer = match answer {
            Ok(answer) if answer.status().as_u16() == 200 => answer,
            Ok(answer) => {
                self.mark_host_down(&format!(
                    "the host answered HTTP {}",
                    answer.status().as_u16()
                ));
                return None;
            }
            Err(err) => {
                self.mark_host_down(&format!("the request failed: {err}"));
                return None;
            }
        };
        let decoded = match answer
            .json::<crate::host::models_host::EmbedResponse>()
            .await
        {
            Ok(decoded) => decoded,
            Err(err) => {
                self.mark_host_down(&format!("the host answer was unreadable: {err}"));
                return None;
            }
        };
        if decoded.dim != self.dim
            || decoded.vectors.len() != texts.len()
            || decoded.vectors.iter().any(|vector| vector.len() != self.dim)
        {
            self.mark_host_down("the host answered vectors of the wrong shape");
            return None;
        }
        *self
            .host_down_until
            .lock()
            .expect("models-host cooldown lock poisoned") = None;
        *self
            .status
            .write()
            .expect("embedder status lock poisoned") = EmbedderStatus::Ready;
        Some(decoded.vectors)
    }

    /// The cached endpoint while it is fresh, or a re-read descriptor. The
    /// re-point rule: after any failure never trust the cached port or
    /// token — a host another instance replaced is reached at its new port.
    fn usable_endpoint(&self) -> Option<crate::host::models_host::HostEndpoint> {
        if !self
            .endpoint_failed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            if let Some(endpoint) = self.client.endpoint() {
                if endpoint.models.iter().any(|served| served == &self.model_id) {
                    return Some(endpoint);
                }
            }
        }
        let endpoint = self.client.refresh_endpoint(&self.model_id)?;
        self.endpoint_failed
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Some(endpoint)
    }

    /// Whether the cooldown window blocks remote attempts.
    fn host_down(&self) -> bool {
        matches!(
            *self
                .host_down_until
                .lock()
                .expect("models-host cooldown lock poisoned"),
            Some(until) if std::time::Instant::now() < until
        )
    }

    /// Record a remote failure: arm the cooldown, mark the endpoint stale
    /// (the next attempt re-reads the descriptor) and log at most once per
    /// minute.
    fn mark_host_down(&self, reason: &str) {
        *self
            .host_down_until
            .lock()
            .expect("models-host cooldown lock poisoned") =
            Some(std::time::Instant::now() + self.cooldown);
        self.endpoint_failed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.log_throttled(reason);
    }

    /// Log the fallback reason at most once per minute (every embed would
    /// otherwise repeat it).
    fn log_throttled(&self, reason: &str) {
        let mut last = self
            .last_error_log
            .lock()
            .expect("models-host log lock poisoned");
        if last.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(60)) {
            return;
        }
        eprintln!(
            "warning: the shared models host is unreachable ({reason}); embedding \
             in-process and retrying the host once a minute"
        );
        *last = Some(std::time::Instant::now());
    }

    /// Embed through the lazily created in-process fallback — today's path.
    /// The fallback is created at most once: a model that cannot load
    /// becomes the permanent hash embedder with status `Failed` (the
    /// `build_embedder` failure shape).
    async fn embed_fallback(&self, texts: &[String]) -> Vec<Vec<f32>> {
        let embedder = {
            let mut slot = self.fallback.lock().await;
            if slot.is_none() {
                let model = self.model_id.clone();
                let cache_dir =
                    crate::config::embedder_cache_dir(self.client.config_dir());
                let status = Arc::clone(&self.status);
                let built = tokio::task::spawn_blocking(move || {
                    BundledEmbedder::new(&model, &cache_dir, status)
                })
                .await;
                let fallback: Arc<dyn Embedder> = match built {
                    Ok(Ok(bundled)) => Arc::new(bundled),
                    Ok(Err(err)) => {
                        eprintln!(
                            "warning: the models host is unreachable and the in-process \
                             model failed to load: {err}; embedding with the built-in \
                             hash embedder"
                        );
                        *self
                            .status
                            .write()
                            .expect("embedder status lock poisoned") = EmbedderStatus::Failed;
                        Arc::new(HashEmbedder::new())
                    }
                    Err(join) => {
                        eprintln!(
                            "error: the in-process fallback load task panicked: {join}"
                        );
                        *self
                            .status
                            .write()
                            .expect("embedder status lock poisoned") = EmbedderStatus::Failed;
                        Arc::new(HashEmbedder::new())
                    }
                };
                *slot = Some(fallback);
            }
            Arc::clone(slot.as_ref().expect("the fallback is set"))
        };
        embedder.embed_batch(texts).await
    }
}

#[cfg(feature = "embeddings")]
#[async_trait]
impl Embedder for RemoteEmbedder {
    async fn embed(&self, text: &str) -> Vec<f32> {
        let texts = [text.to_string()];
        let mut vectors = self.embed_texts(&texts).await;
        vectors.pop().unwrap_or_else(|| vec![0.0_f32; self.dim])
    }

    async fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        self.embed_texts(texts).await
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }

    /// The live status (the shared field this embedder keeps updated).
    /// A poisoned lock reads as Failed — the conservative answer for a
    /// caller deciding whether to store this embedder's vectors.
    fn status(&self) -> Option<EmbedderStatus> {
        Some(
            self.status
                .read()
                .map(|s| s.clone())
                .unwrap_or(EmbedderStatus::Failed),
        )
    }
}

/// Map a model-id string to the fastembed enum. Returns an error for an
/// unknown id (the caller surfaces it to the user; the shared models host
/// also uses this to refuse spawning for a bogus config).
#[cfg(feature = "embeddings")]
pub fn model_for_id(id: &str) -> Result<fastembed::EmbeddingModel, String> {
    match id {
        "all-MiniLM-L6-v2" => Ok(fastembed::EmbeddingModel::AllMiniLML6V2),
        "bge-small-en-v1.5" => Ok(fastembed::EmbeddingModel::BGESmallENV15),
        "bge-base-en-v1.5" => Ok(fastembed::EmbeddingModel::BGEBaseENV15),
        "nomic-embed-text-v1.5" => Ok(fastembed::EmbeddingModel::NomicEmbedTextV15),
        "jina-embeddings-v2-base-code" => Ok(fastembed::EmbeddingModel::JinaEmbeddingsV2BaseCode),
        other => Err(format!("unknown embedding model '{other}'")),
    }
}

/// The fastembed load options of a bundled model — the ONE shared load
/// path of the models host and the in-process fallback (plan d3336a8a
/// step 15, so the two can never drift): the cache dir, plus the
/// code-search model's explicit token cap
/// ([`CODE_EMBEDDING_MAX_TOKENS`]). Every other model — the memory
/// models — keeps its own defaults.
///
/// The ORT CPU memory arena cannot be disabled from here: fastembed
/// exposes only `session_config` entries, and the arena is not one
/// (it is the `DisableCpuMemArena` C API, which fastembed never calls;
/// the session-options config-keys header has no arena key — an
/// unknown entry is stored and never consumed). The arena's growth is
/// bounded instead by the token cap plus the host's sub-batching:
/// fastembed pads every batch to its longest text
/// (`PaddingStrategy::BatchLongest`, hardcoded in fastembed — no
/// fixed-length padding either), so without the cap one long batch
/// spikes the padded attention (5.2 GB at step 17; the cap plus
/// 16-text sub-batches bounds a full 17.5k-symbol pass at ~1.4 GB).
#[cfg(feature = "embeddings")]
pub fn text_init_options(
    model: fastembed::EmbeddingModel,
    model_id: &str,
    cache_dir: &std::path::Path,
) -> fastembed::TextInitOptions {
    let mut init = fastembed::TextInitOptions::new(model).with_cache_dir(cache_dir.to_path_buf());
    if model_id == CODE_EMBEDDING_MODEL {
        init = init.with_max_length(CODE_EMBEDDING_MAX_TOKENS);
    }
    init
}

/// The vector dimension a fastembed model produces (used before the first
/// successful embed, for the zero-vector fallback).
#[cfg(feature = "embeddings")]
pub fn dim_for_model(model: &fastembed::EmbeddingModel) -> usize {
    match model {
        fastembed::EmbeddingModel::AllMiniLML6V2 => 384,
        fastembed::EmbeddingModel::BGESmallENV15 => 384,
        fastembed::EmbeddingModel::BGEBaseENV15 => 768,
        fastembed::EmbeddingModel::NomicEmbedTextV15 => 768,
        fastembed::EmbeddingModel::JinaEmbeddingsV2BaseCode => 768,
        _ => EMBEDDING_DIM, // safe fallback for unlisted models
    }
}

/// The HuggingFace cache subdir name fastembed downloads a model into, for a
/// given model id (e.g. `"all-MiniLM-L6-v2"` → `"models--Qdrant--all-MiniLM-L6-v2-onnx"`).
///
/// Used to detect whether a model is already installed (the subdir exists +
/// has snapshot files) and to poll download progress (dir size vs. the
/// catalog's `size_mb`). Returns `None` for an unknown model id.
pub fn hf_cache_subdir(model_id: &str) -> Option<String> {
    let repo = match model_id {
        "all-MiniLM-L6-v2" => "Qdrant/all-MiniLM-L6-v2-onnx",
        "bge-small-en-v1.5" => "Xenova/bge-small-en-v1.5",
        "bge-base-en-v1.5" => "Xenova/bge-base-en-v1.5",
        "nomic-embed-text-v1.5" => "nomic-ai/nomic-embed-text-v1.5",
        "jina-embeddings-v2-base-code" => "jinaai/jina-embeddings-v2-base-code",
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

    #[tokio::test]
    async fn embed_batch_default_loops() {
        struct CountingEmbedder {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }
        #[async_trait]
        impl Embedder for CountingEmbedder {
            async fn embed(&self, _text: &str) -> Vec<f32> {
                self.calls
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                vec![1.0_f32]
            }
        }
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let embedder = CountingEmbedder {
            calls: std::sync::Arc::clone(&calls),
        };
        let texts = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let vectors = embedder.embed_batch(&texts).await;
        assert_eq!(vectors.len(), 3);
        assert!(vectors.iter().all(|vector| vector == &vec![1.0_f32]));
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "the default embed_batch loops embed once per text"
        );
        assert!(embedder.embed_batch(&[]).await.is_empty());
    }

    #[tokio::test]
    async fn hash_embedder_embed_batch_matches_single_embeds() {
        let embedder = HashEmbedder::new();
        let texts = vec!["alpha beta".to_string(), "gamma".to_string()];
        let batched = embedder.embed_batch(&texts).await;
        for (batched, text) in batched.iter().zip(&texts) {
            assert_eq!(batched, &hash_embedding(text));
        }
    }

    /// The RemoteEmbedder against a stub host: the always-run stand-ins for
    /// the real host (which needs a downloaded model — see
    /// `remote_embedder_matches_in_process_vectors`).
    #[cfg(feature = "embeddings")]
    mod remote {
        use super::*;
        use std::io::{Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};
        use std::thread;

        /// A minimal loopback HTTP stub answering `GET /health` and
        /// `POST /v1/embed` with marker vectors. Serves at most `budget`
        /// requests, then stops: exhausting the budget drops the listener,
        /// which is how a test kills a stub host (its port goes dead).
        fn stub_host(marker: f32, models: &[&str], budget: usize) -> u16 {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub");
            let port = listener.local_addr().expect("a stub port").port();
            let models: Vec<String> = models.iter().map(|model| model.to_string()).collect();
            thread::spawn(move || {
                let mut served = 0;
                while served < budget {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    served += 1;
                    let Some((method, path, body)) = read_stub_request(&mut stream) else {
                        continue;
                    };
                    if method == "GET" && path == "/health" {
                        let answer =
                            serde_json::json!({ "models": models, "version": "stub" }).to_string();
                        write_stub_response(&mut stream, 200, &answer);
                    } else if method == "POST" && path == "/v1/embed" {
                        let Ok(request) =
                            serde_json::from_slice::<crate::host::models_host::EmbedRequest>(
                                &body,
                            )
                        else {
                            write_stub_response(
                                &mut stream,
                                400,
                                &serde_json::json!({ "error": "bad body" }).to_string(),
                            );
                            continue;
                        };
                        // The dim must match the embedder's derived dim: an
                        // unknown model id keeps EMBEDDING_DIM.
                        let dim = EMBEDDING_DIM;
                        let vectors: Vec<Vec<f32>> = request
                            .texts
                            .iter()
                            .map(|_| {
                                let mut vector = vec![0.0_f32; dim];
                                vector[0] = marker;
                                vector
                            })
                            .collect();
                        let answer =
                            serde_json::json!({ "vectors": vectors, "dim": dim }).to_string();
                        write_stub_response(&mut stream, 200, &answer);
                    } else {
                        write_stub_response(
                            &mut stream,
                            404,
                            &serde_json::json!({ "error": "unknown path" }).to_string(),
                        );
                    }
                }
            });
            port
        }

        /// Read one stub request: request line, headers, content-length body.
        fn read_stub_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let header_end = loop {
                let read = stream.read(&mut chunk).ok()?;
                if read == 0 {
                    return None;
                }
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(at) = buffer
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                {
                    break at;
                }
                if buffer.len() > 64 * 1024 {
                    return None;
                }
            };
            let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
            let mut lines = head.split("\r\n");
            let request_line = lines.next()?;
            let mut parts = request_line.split_whitespace();
            let method = parts.next()?.to_string();
            let path = parts.next()?.to_string();
            let content_length: usize = lines
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse().ok())
                .unwrap_or(0);
            let mut body = buffer[header_end + 4..].to_vec();
            while body.len() < content_length {
                let read = stream.read(&mut chunk).ok()?;
                if read == 0 {
                    return None;
                }
                body.extend_from_slice(&chunk[..read]);
            }
            body.truncate(content_length);
            Some((method, path, body))
        }

        fn write_stub_response(stream: &mut TcpStream, code: u16, body: &str) {
            let head = format!(
                "HTTP/1.1 {code} \r\nContent-Type: application/json\r\nContent-Length: \
                 {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body.as_bytes());
        }

        /// Publish a descriptor for the TEST process (alive, real creation
        /// identity) — the stand-in for a host another instance spawned.
        fn publish_descriptor(config_dir: &std::path::Path, port: u16, token: &str) {
            crate::host::shared_service::write_service(
                &crate::host::shared_service::service_dir(config_dir, "models"),
                &crate::host::shared_service::ServiceInfo {
                    pid: std::process::id(),
                    port,
                    token: token.to_string(),
                    version: "test".into(),
                    started_ms: crate::host::shared_service::now_ms(),
                    created: crate::process_tree::process_creation_id(std::process::id())
                        .unwrap_or_default(),
                    models: vec!["not-a-real-model".into()],
                },
            )
            .expect("publish the test descriptor");
        }

        #[tokio::test]
        async fn remote_embedder_serves_from_a_healthy_stub_host() {
            let home = tempfile::tempdir().unwrap();
            let port = stub_host(7.5, &["not-a-real-model"], 64);
            publish_descriptor(home.path(), port, "stub-token");
            let client = crate::host::models_host::shared_client(home.path());
            let endpoint = client
                .attach("not-a-real-model", crate::host::models_host::STARTUP_PROBE)
                .expect("attach to the stub host");
            assert_eq!(endpoint.port, port);
            let status = Arc::new(RwLock::new(EmbedderStatus::Ready));
            let remote = RemoteEmbedder::new(client, "not-a-real-model", Arc::clone(&status));
            let vector = remote.embed("hello").await;
            assert_eq!(vector.len(), EMBEDDING_DIM);
            assert!(
                (vector[0] - 7.5).abs() < 1e-6,
                "the vector came from the stub host"
            );
            // The batch is one wire call: one marker vector per text.
            let texts = vec!["a".to_string(), "b".to_string()];
            let batched = remote.embed_batch(&texts).await;
            assert_eq!(batched.len(), 2);
            assert_eq!(batched[0].len(), EMBEDDING_DIM);
            for vector in &batched {
                assert!((vector[0] - 7.5).abs() < 1e-6);
            }
            assert_eq!(
                *status.read().expect("embedder status lock poisoned"),
                EmbedderStatus::Ready
            );
        }

        #[tokio::test]
        async fn remote_embedder_falls_back_when_host_is_down() {
            let home = tempfile::tempdir().unwrap();
            // A port nothing listens on (bind :0, drop the socket).
            let dead_port = {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                drop(listener);
                port
            };
            let client = crate::host::models_host::shared_client(home.path());
            client.override_endpoint(dead_port, "stub-token", &["not-a-real-model"]);
            let status = Arc::new(RwLock::new(EmbedderStatus::Ready));
            let remote = RemoteEmbedder::new(client, "not-a-real-model", Arc::clone(&status));
            let vector = remote.embed("hello world").await;
            // The remote POST failed; the fallback's in-process load of an
            // unknown model fails fast (no download) → hash + Failed —
            // today's shape for a model that cannot load.
            assert_eq!(
                vector,
                hash_embedding("hello world"),
                "the fallback embeds with the hash embedder"
            );
            assert_eq!(
                *status.read().expect("embedder status lock poisoned"),
                EmbedderStatus::Failed
            );
        }

        #[tokio::test]
        async fn remote_embedder_repoints_to_a_replaced_host_after_the_cooldown() {
            let home = tempfile::tempdir().unwrap();
            // Host A: healthy, but its budget covers only the attach probe
            // and one embed — then its port goes dead.
            let port_a = stub_host(7.5, &["not-a-real-model"], 2);
            publish_descriptor(home.path(), port_a, "token-a");
            let client = crate::host::models_host::shared_client(home.path());
            let endpoint = client
                .attach("not-a-real-model", crate::host::models_host::STARTUP_PROBE)
                .expect("attach to host A");
            assert_eq!(endpoint.port, port_a);
            let status = Arc::new(RwLock::new(EmbedderStatus::Ready));
            let mut remote = RemoteEmbedder::new(client, "not-a-real-model", Arc::clone(&status));
            remote.override_cooldown(std::time::Duration::from_millis(50));
            let vector = remote.embed("one").await;
            assert!((vector[0] - 7.5).abs() < 1e-6, "host A served this one");

            // Host A is gone; another instance starts host B on a new port
            // with a new token and publishes over A's descriptor.
            let port_b = stub_host(8.5, &["not-a-real-model"], 64);
            assert_ne!(port_b, port_a);
            publish_descriptor(home.path(), port_b, "token-b");

            // The remote fails against dead A → the fallback answers (hash:
            // the unknown model cannot load) and the cooldown is armed.
            let vector = remote.embed("two").await;
            assert_eq!(
                vector,
                hash_embedding("two"),
                "the fallback answers while A is dead"
            );
            assert_eq!(
                *status.read().expect("embedder status lock poisoned"),
                EmbedderStatus::Failed
            );
            // After the (short) cooldown the retry re-reads the descriptor
            // — never the cached port or token — and reaches host B.
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            let vector = remote.embed("three").await;
            assert!(
                (vector[0] - 8.5).abs() < 1e-6,
                "the re-read descriptor reached replacement host B"
            );
            assert_eq!(
                *status.read().expect("embedder status lock poisoned"),
                EmbedderStatus::Ready,
                "a working remote flips the status back to Ready"
            );
        }

        /// The real host's vectors must equal the in-process embedder's for
        /// the same model — the compatibility contract that lets a store
        /// swap between remote and in-process without re-embedding.
        #[ignore = "downloads the embedding model on first run (~90-410 MB)"]
        #[tokio::test]
        async fn remote_embedder_matches_in_process_vectors() {
            let home = tempfile::tempdir().unwrap();
            // A REAL host (the serve loop the mnemo-models binary runs),
            // in-process, serving the smallest catalog model.
            let (_port, _token) = crate::host::models_host::spawn_test_host(
                home.path(),
                &["all-MiniLM-L6-v2"],
                crate::host::models_host::HOST_IDLE,
            );
            let client = crate::host::models_host::shared_client(home.path());
            client
                .attach("all-MiniLM-L6-v2", crate::host::models_host::STARTUP_PROBE)
                .expect("attach to the real host");
            let status = Arc::new(RwLock::new(EmbedderStatus::Ready));
            let remote = RemoteEmbedder::new(client, "all-MiniLM-L6-v2", Arc::clone(&status));
            let texts = vec![
                "the quick brown fox jumps over the lazy dog".to_string(),
                "semantic memory recall over shared vectors".to_string(),
                "a plain unrelated third sentence".to_string(),
            ];
            // The first embed makes the host lazily load (or download) the
            // model — that is the point of the laziness.
            let remote_vectors = remote.embed_batch(&texts).await;
            let local = BundledEmbedder::new(
                "all-MiniLM-L6-v2",
                &crate::config::embedder_cache_dir(home.path()),
                Arc::new(RwLock::new(EmbedderStatus::Checking)),
            )
            .expect("the model is installed (the host just loaded it)");
            assert_eq!(remote_vectors.len(), texts.len());
            for (remote_vector, text) in remote_vectors.iter().zip(&texts) {
                let local_vector = local.embed(text).await;
                assert_eq!(remote_vector.len(), local_vector.len(), "same dim");
                let drift = remote_vector
                    .iter()
                    .zip(&local_vector)
                    .map(|(remote, local)| (remote - local).abs())
                    .fold(0.0_f32, f32::max);
                assert!(drift < 1e-5, "remote vs in-process drift {drift}");
            }
        }
    }
}
