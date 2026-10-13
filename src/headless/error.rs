// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The headless-API error type (§5.3) — a machine-readable `code` plus a
//! human message, serde-able so a JS binding (§5.5) can surface both
//! fields. The codes are the stable surface (§7 row 15 pins
//! `session_closed`); the message is detail.

use serde::{Deserialize, Serialize};

/// An engine/session error. Constructed by the engine internals; callers
/// match on [`EngineError::code`] (e.g. `"no_project"`, `"session_closed"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineError {
    code: String,
    message: String,
}

impl EngineError {
    /// The stable machine code — the surface callers branch on.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The human-readable detail.
    pub fn message(&self) -> &str {
        &self.message
    }

    // The remaining kind constructors (session_closed / spawn_failed /
    // engine_shutdown) land with the 3a.5 session code that calls them —
    // ahead-of-use fns are dead code under #![deny(warnings)].

    /// The engine could not start: an unusable workspace (no project —
    /// §5.3, row 9: never blocks, never shows a picker) or a failed
    /// build.
    pub(crate) fn no_project(message: impl Into<String>) -> Self {
        Self {
            code: "no_project".to_string(),
            message: message.into(),
        }
    }

    /// The engine failed to build (config load, brain wiring, host
    /// runtime).
    pub(crate) fn start_failed(message: impl Into<String>) -> Self {
        Self {
            code: "start_failed".to_string(),
            message: message.into(),
        }
    }

    /// A session spawn failed (the shared path errored).
    pub(crate) fn spawn_failed(message: impl Into<String>) -> Self {
        Self {
            code: "spawn_failed".to_string(),
            message: message.into(),
        }
    }

    /// The session is closed — every `send` on it fails with this code
    /// (§5.3, row 15).
    pub(crate) fn session_closed(message: impl Into<String>) -> Self {
        Self {
            code: "session_closed".to_string(),
            message: message.into(),
        }
    }

    /// The history export is mid-flight — the turn holds the conversation
    /// while a tool blocks (a pending approval/question); retry between
    /// turns (3a.6, G5).
    pub(crate) fn session_busy(message: impl Into<String>) -> Self {
        Self {
            code: "session_busy".to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}
