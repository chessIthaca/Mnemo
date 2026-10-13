// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The runtime-rewire core (§5.8 :538, job 4 D4) — the AppHandle-free
//! half moved from the app's ipc/rewire.rs: rebuild the vision client +
//! memory embedder from a config, push the factory's live toggles, and
//! sync the per-context model resolver. The headless
//! [`Setup::patch_settings`](crate::headless::setup::Setup::patch_settings)
//! and the GUI's save path share this ONE implementation.
//!
//! The caller owns the spawn policy: [`rewire_vision_embedder`] returns
//! the re-embed future when the bundled model changed and the new
//! embedder is Ready (the app spawns it on `tauri::async_runtime`, the
//! headless engine on tokio). The MANAGED-CLASSIFIER leg stays app-side
//! until job 4 D5 moves the Laya core — the classifier slot/status Arcs
//! live in the brain, so the swap becomes host-agnostic when the
//! manager moves.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use crate::agent::factory::AgentLoopFactory;
use crate::config::Config;
use crate::memory::embedder::EmbedderStatus;
use crate::memory::MemoryStore;
use crate::model_resolver::ConfigModelResolver;
use crate::provider::client_factory::{build_embedder, build_vision_client};

/// The runtime pieces a rewire touches (job 4 D4) — the AppHandle-free
/// view of what the app threaded through `IpcState`. Every live handle
/// is optional: a startup-error fallback leaves slots absent and the
/// rewire no-ops for them.
pub struct RewireTargets {
    /// The shared agent factory (the vision slot + the live toggles).
    pub factory: Option<Arc<AgentLoopFactory>>,
    /// The concrete memory store (the embedder swap + the `[memory]`
    /// knobs).
    pub memory_store: Option<Arc<MemoryStore>>,
    /// The shared embedder status (Ready gates the re-embed).
    pub embedder_status: Arc<RwLock<EmbedderStatus>>,
    /// The per-context model resolver.
    pub model_resolver: Option<Arc<ConfigModelResolver>>,
    /// The config dir the embedder cache derives from (D6:
    /// `<config_dir>/models`). `None` = the machine-global dir (the
    /// GUI's case — its config dir IS the global one).
    pub config_dir: Option<std::path::PathBuf>,
}

/// The background re-embed task [`rewire_vision_embedder`] returns —
/// spawned by the CALLER (the app on `tauri::async_runtime`, the
/// headless Setup on tokio): a full re-embed is long, so it never
/// blocks the save call.
pub type ReembedFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Rebuild the live vision client + memory embedder from `cfg` and
/// install them (the factory's vision slot + the store's embedder +
/// `[memory]` knobs) + push the factory's live toggles (the classifier
/// flags, the optimizer/verify/budget configs — every consumer reads
/// them at call time, so a save lands on the next request with no
/// registry rebuild). Absent handles no-op (startup-error fallback).
///
/// Returns the re-embed future when the bundled model changed and the
/// new embedder is Ready — a Failed/hash-fallback embedder has nothing
/// useful to re-embed with (it would corrupt the fingerprint
/// semantics), and the explicit `"hash"` opt-out never re-embeds
/// (it would destroy stored semantic vectors). The caller spawns it.
///
/// The managed-classifier leg is NOT here (it stays app-side until job
/// 4 D5 moves the Laya core); the factory toggles that mirror the
/// `[general.laya]` FLAGS are — they read config, never the runtime.
pub fn rewire_vision_embedder(targets: &RewireTargets, cfg: &Config) -> Option<ReembedFuture> {
    let mut reembed: Option<ReembedFuture> = None;
    if let Some(factory) = &targets.factory {
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
        // The factory's live toggles (moved from the app's rewire):
        // every consumer reads its flag/config at call time, so a save
        // lands on the very next call — no restart, no rebuild.
        let laya_cfg = &cfg.general.general.laya;
        // Auto-typing flag (backlog a147b63c): memory_write reads the
        // mirrored flag at call time.
        factory.set_auto_typing_enabled(laya_cfg.auto_type_memories);
        // Tool-choice steering (backlog e2c47d5f): search/search_read
        // read the flag per call.
        factory.set_tool_choice_enabled(laya_cfg.steer_tool_choice);
        // Failure triage (backlog 1a4049c1): the loop's dispatch site +
        // both provider retry layers read the flag at failure time.
        factory.set_failure_triage_enabled(laya_cfg.failure_triage);
        // The kNN overlay flag: the gate consults the overlay before
        // the shared slot while this is on.
        factory.set_failure_triage_knn_enabled(laya_cfg.failure_triage_knn);
        // Reflex (backlog a8495cc1): the escalation lane ladder reads
        // the flag at each plan-step boundary.
        factory.set_reflex_enabled(laya_cfg.reflex);
        // Token-optimizer levers ([general.optimizer]): every
        // lever-bearing tool reads the shared config per call.
        factory.set_optimizer_config(cfg.general.general.optimizer.clone());
        // Harness-run deterministic verify ([general.verify]): every
        // shared StepVerifyHandle reads the config at each plan-step
        // boundary.
        factory.set_verify_config(cfg.general.general.verify.clone());
        // The deterministic budget layer ([general.budget]): the cap
        // gate reads the config at every plan-step boundary.
        factory.set_budget_config(cfg.general.general.budget.clone());
    }
    if let Some(store) = &targets.memory_store {
        // The bundled model cache lives under the config dir (NOT
        // .coding/, which is committed to git — model binaries are
        // per-machine). D6: the caller's dir — a Setup with a custom
        // config dir isolates its installs; the machine-global one
        // when absent (the GUI's case).
        let cache_dir = match &targets.config_dir {
            Some(dir) => crate::config::embedder_cache_dir(dir),
            None => crate::config::embedder_cache_dir(&crate::config::global_config_dir()),
        };
        let new_embedder = build_embedder(cfg, &cache_dir, targets.embedder_status.clone());
        // When the bundled model changed (or memories are stale from
        // another machine via git), re-embed all rows so the stored
        // vectors match the new model. Only runs when the model loaded
        // successfully (status Ready) — a Failed/hash fallback has
        // nothing useful to re-embed with, and would corrupt the
        // fingerprint semantics. Skipped on the explicit "hash"
        // opt-out (re-embedding with the hash embedder would destroy
        // stored semantic vectors).
        let is_hash_opt_out = cfg
            .general
            .general
            .bundled_embedding_model
            .as_deref()
            .is_some_and(|m| {
                m.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH)
            });
        let status_snapshot = targets
            .embedder_status
            .read()
            .expect("embedder status lock poisoned")
            .clone();
        if !is_hash_opt_out && status_snapshot == EmbedderStatus::Ready {
            let store_for_reembed = store.clone();
            let embedder_for_reembed = new_embedder.clone();
            reembed = Some(Box::pin(async move {
                crate::memory::reembed_if_needed(&store_for_reembed, &embedder_for_reembed).await;
            }));
        }
        // The [memory] retrieval knobs ride the same save path — recall
        // + memory_write read the store's snapshot per call.
        store.set_memory_search_config(cfg.general.memory.clone());
        store.set_embedder(new_embedder);
        eprintln!("rewire: embedder set (from config)");
    }
    reembed
}

/// Push a reloaded config into the per-context model resolver so
/// `[models]` overrides take effect on the next request (per-request
/// resolution). No-op when absent (startup-error fallback).
pub fn sync_model_resolver(resolver: Option<&Arc<ConfigModelResolver>>, cfg: &Config) {
    if let Some(resolver) = resolver {
        resolver.set_config(cfg.clone());
    }
}