// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The headless Engine (§5.3 :295-302) — the public face over the shared
//! host wiring: an EXPLICIT workspace + config (G4 — no discovery, no
//! global-config read, no picker), the brain built through the same
//! [`wire_brain`] tail the app uses, and the [`HostRuntime`] core (3a.2).
//! `spawn` + `AgentSession` land in 3a.5; the memory-backend / index /
//! embedder option consumption lands in 3b (typed on `EngineOptions`
//! now, wired there).

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::agent::policy::PermissionPolicy;
use crate::config::Config;
use crate::headless::error::EngineError;
use crate::headless::types::{EngineOptions, EventReceiver, HostEvent};
use crate::host::brain::{wire_brain, Brain};
use crate::host::notifier::{HostNotifier, SharedNotifier};
use crate::host::runtime::HostRuntime;

/// The fan-out notifier: every host event lands on the engine's broadcast
/// bus so any number of `host_events()` subscribers see it (the GUI
/// equivalent forwards each variant to its webview channel).
struct BusNotifier {
    tx: broadcast::Sender<HostEvent>,
}

impl HostNotifier for BusNotifier {
    fn notify(&self, event: HostEvent) {
        // No subscribers yet → the event drops; a late subscriber sees
        // later events (the bus is lossy by design).
        let _ = self.tx.send(event);
    }
}

/// A headless engine (§5.3 :295): the brain + the shared host core,
/// started from explicit options. Cheap to hold; `spawn` (3a.5) opens
/// sessions on it.
pub struct Engine {
    /// The brain, held for the engine's lifetime so its Drop-managed
    /// pieces (the CodeGraph file watcher) stay alive.
    _brain: Brain,
    /// The shared host core (3a.2): manager, loop map, spawner, fan-in
    /// receiver, bookkeeping. `spawn` (3a.5) reads it.
    _host: HostRuntime,
    /// The host-event bus — `host_events()` subscribes here.
    host_events_tx: broadcast::Sender<HostEvent>,
    /// The default permission policy for spawned sessions (§5.4); a
    /// `SpawnOptions::permissions` (3a.5) overrides it per session.
    _default_permissions: PermissionPolicy,
    /// The runtime this engine owns when started via
    /// [`start_blocking`](Engine::start_blocking) — `None` when the
    /// caller's runtime hosts it. Held purely for its lifetime
    /// (background loads), like the brain's graph watcher.
    _owned_runtime: Option<tokio::runtime::Runtime>,
}

impl Engine {
    /// Start an engine on an EXPLICIT workspace + config (§5.3 :297).
    /// Never blocks and never shows a picker (§7 row 9): an unusable
    /// workspace — the root missing, or `require_git` unmet on a non-repo
    /// root — is [`EngineError`] code `no_project`.
    pub async fn start(opts: EngineOptions) -> Result<Engine, EngineError> {
        // The workspace gates — row 9 + the §5.9.3 :664 require_git rule.
        if !opts.workspace.root.is_dir() {
            return Err(EngineError::no_project(format!(
                "workspace root '{}' is not a directory",
                opts.workspace.root.display()
            )));
        }
        if opts.workspace.require_git && !opts.workspace.is_git_repo() {
            return Err(EngineError::no_project(format!(
                "workspace root '{}' is not a git repository (require_git)",
                opts.workspace.root.display()
            )));
        }

        // The config dir: explicit → $MNEMO_HOME → ~/.mnemo (§5.3 :287).
        // G4: the engine resolves it itself — `global_config_dir()` stays
        // app-only (Setup::open(None) + the app).
        let config_dir = match opts.config_dir {
            Some(dir) => dir,
            None => default_config_dir(),
        };
        let config = Config::load(&config_dir).map_err(|e| {
            EngineError::start_failed(format!(
                "config load from '{}' failed: {e}",
                config_dir.display()
            ))
        })?;

        // The host-events bus: the engine ALWAYS has one (the GUI forwards
        // to webview channels; a headless host reads host_events()).
        let (host_events_tx, _) = broadcast::channel(256);
        let notifier: SharedNotifier = Arc::new(BusNotifier {
            tx: host_events_tx.clone(),
        });

        // The brain — the SAME wiring tail the app uses, with the
        // engine's explicit inputs (never the app's discovery). A
        // headless start is never a "switched open" (no picker flow).
        let rt = tokio::runtime::Handle::current();
        let workspace = Arc::new(opts.workspace.clone());
        let brain = wire_brain(config_dir, config, workspace, false, Some(notifier), rt)
            .map_err(|e| EngineError::start_failed(format!("brain build failed: {e}")))?;

        // The shared host core — the same assembly the console delegates
        // to (manager, spawner, the main agent, bookkeeping).
        let host = HostRuntime::start(brain.factory.clone())
            .await
            .map_err(EngineError::start_failed)?;

        Ok(Engine {
            _brain: brain,
            _host: host,
            host_events_tx,
            _default_permissions: opts.permissions,
            _owned_runtime: None,
        })
    }

    /// The engine's host-event bus (§5.3 :300): every background event
    /// (the memory reconcile, the code-graph index progress) reaches
    /// every subscriber.
    pub fn host_events(&self) -> EventReceiver<HostEvent> {
        EventReceiver {
            rx: self.host_events_tx.subscribe(),
        }
    }

    /// Shut the engine down (§5.3 :301): ends the sessions and runs the
    /// session-end consolidation. The session bookkeeping lands in 3a.5;
    /// this frame already drops the pieces in dependency order (the
    /// brain's watcher-managed lifetime, then the host core).
    pub async fn shutdown(self) -> Result<(), EngineError> {
        // 3a.5: end the sessions + run the session-end consolidation
        // (the kernel's existing shutdown path).
        Ok(())
    }

    /// Start from a plain (non-async) context — the engine owns a static
    /// tokio runtime, so it works from a plain `fn main` (§7 row 15).
    /// Dropping the engine drops the runtime.
    pub fn start_blocking(opts: EngineOptions) -> Result<Engine, EngineError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| EngineError::start_failed(format!("runtime build failed: {e}")))?;
        let engine = runtime.block_on(Self::start(opts))?;
        Ok(Engine {
            _owned_runtime: Some(runtime),
            ..engine
        })
    }
}

/// The engine's config-dir default (§5.3 :287): `$MNEMO_HOME`, else
/// `~/.mnemo`. Deliberately NOT [`crate::config::global_config_dir`] — G4
/// (:676-677) keeps that for `Setup::open(None)` and the app.
fn default_config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("MNEMO_HOME") {
        return PathBuf::from(home);
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(".mnemo");
    }
    PathBuf::from(".mnemo")
}
