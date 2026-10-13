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

pub mod engine;
pub mod error;
pub mod types;

pub use engine::Engine;
pub use error::EngineError;
pub use types::{
    EngineOptions, EventReceiver, HostEvent, MemoryBackend, RecvError, SessionCommand,
    SessionEvent, SessionId, SessionLimits, SpawnOptions, Transcript,
};
