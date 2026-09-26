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
pub mod backlog;
#[cfg(feature = "browser")]
pub mod browser;
pub mod codegraph;
pub mod config;
pub mod error;
pub mod instance_marker;
pub mod mcp;
pub mod memory;
pub mod model_resolver;
pub mod project;
pub mod provider;
pub mod runtime;
pub mod safety_rules;
pub mod shell_path;
pub mod skill;
pub mod thread_util;
pub mod tool;
pub mod webview_args;
pub mod workflow;
