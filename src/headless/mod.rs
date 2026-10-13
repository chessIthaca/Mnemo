// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The public headless API (§5.3) — the Rust-only engine surface: any UI
//! (the Tauri GUI, the console, a daemon, a JS binding through §5.5)
//! drives the same sessions through these types. Phase 3 builds it in
//! steps: the types + error (3a.3), `Engine::start` (3a.4), spawn +
//! `AgentSession` (3a.5), the G5 transcript (3a.6), the G7 limits (3a.7).
//!
//! Nothing here reads stdin, writes stdout, or touches a Tauri handle.

pub mod codegraph_api;
pub mod engine;
pub mod error;
pub mod host_tools;
pub mod memory_api;
pub mod session;
pub mod stats;
pub mod types;

pub use codegraph_api::{CodegraphApi, CodegraphGraph, CodegraphStatus};
pub use engine::{Engine, EngineBuilder};
pub use error::EngineError;
pub use host_tools::{HostTool, HostToolSpec, HOST_TOOL_DEFAULT_TIMEOUT_MS};
pub use memory_api::{
    MemoryApi, MemoryCounts, MemoryDebugOverview, MemoryDebugWire, MemoryIndexStatus,
    ScoredMemoryDebugWire,
};
pub use session::AgentSession;
pub use stats::{EngineStats, SessionStats, TraceApi, UsageRates};
pub use types::{
    EngineOptions, EventReceiver, HostEvent, MemoryBackend, PendingInteraction, RecvError,
    SessionCommand, SessionEvent, SessionId, SessionLimits, SessionSnapshot, SpawnOptions,
    Transcript, TurnOutcome,
};

#[cfg(test)]
mod tests {
    /// G4 close-grep (§5.9.4 :676-677, job-03a §E): the engine takes its
    /// directories from `EngineOptions` — NO source under `src/headless/`
    /// may CALL `global_config_dir(`, the process-global fallback that
    /// would leak one engine's config into another. The scan matches the
    /// call-shaped token only: doc comments legitimately NAME the
    /// function to document why it is NOT used (engine.rs), and this
    /// test's own source names it too — hence mod.rs itself is skipped.
    #[test]
    fn headless_never_calls_global_config_dir() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/headless");
        let mut checked = 0;
        fn walk(dir: &std::path::Path, checked: &mut usize) {
            for entry in std::fs::read_dir(dir).expect("src/headless readable") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk(&path, checked);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    if path.file_name().and_then(|n| n.to_str()) == Some("mod.rs") {
                        continue;
                    }
                    let source = std::fs::read_to_string(&path).expect("source readable");
                    assert!(
                        !source.contains("global_config_dir("),
                        "{path:?} calls global_config_dir — the engine must take \
                         its config dir from EngineOptions (G4, §5.9.4 :676-677)"
                    );
                    *checked += 1;
                }
            }
        }
        walk(&dir, &mut checked);
        assert!(checked >= 4, "the scan must cover the module's files");
    }
}
