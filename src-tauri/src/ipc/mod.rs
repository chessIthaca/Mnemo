// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Tauri IPC bridge - connects the brain's channel contract to the
//! frontend via Tauri commands (frontend -> Rust) and events (Rust -> frontend).
//!
//! The brain is fully decoupled: it speaks `AgentCommand`/`AgentEvent` over
//! channels. This module adapts that to Tauri's IPC:
//! - Commands are split by domain: `agent` (agent control/safety/stats),
//!   `settings` (config/endpoints/keys), `files` (browser/conversations),
//!   `backlog_cmds` (backlog + auto-feed), `browser` (headless debug browser),
//!   and `spawn` (the shared spawn path).
//! - The event forwarder (`events.rs`) translates `AgentEvent`s into Tauri events.
//! - The approval map (`approval.rs`) holds the non-serializable `oneshot::Sender`s.
//! - The backlog store (`backlog.rs`) holds pending prompts for later dispatch.
//! - Run-All orchestration + the backlog-resolution callbacks live in `run_all.rs`.

pub mod agent;
pub mod approval;
pub mod backlog_cmds;
pub mod browser;
pub mod browser_webview;
pub mod codegraph_cmds;
pub mod config_io;
pub mod embeddings;
pub mod error;
pub mod events;
pub mod files;
pub mod keys;
pub mod mcp;
pub mod memory_debug;
pub mod memory_maintenance;
pub mod models;
pub mod projects;
pub mod questions;
pub mod rewire;
pub mod run_all;
pub mod settings;
pub mod spawn;
pub mod startup;
pub mod state;
pub mod trace;

#[cfg(test)]
mod contract_fixtures;

pub use approval::PendingApprovals;
pub use events::spawn as spawn_event_forwarder;
pub use questions::PendingQuestions;
pub use state::IpcState;
