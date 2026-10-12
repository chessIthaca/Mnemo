// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]
//! Mnemo — a coding-first agentic harness in Rust.
//!
//! See `PLAN.md` for the full design. The library is organized so that the
//! "brain" (config, project, provider, tool, memory, workflow, runtime, agent)
//! is fully usable without the TUI.

pub mod agent;
pub mod app;
pub use crate::app::backlog; // job 2b: relocated; the alias keeps every existing path resolving
#[cfg(feature = "browser")]
pub mod browser;
pub mod codegraph;
pub mod config;
pub mod error;
pub mod host;
pub mod index_staleness;
pub mod instance_marker;
pub mod mcp;
pub mod memory;
pub mod model_resolver;
pub mod project;
pub mod provider;
pub mod runtime;
pub mod safety_rules;
pub mod shell_path;
pub use crate::app::skill; // job 2b: relocated; the alias keeps every existing path resolving
pub mod thread_util;
pub mod tool;
pub mod webview_args;
pub use crate::app::workflow; // job 2b: relocated; the alias keeps every existing path resolving
