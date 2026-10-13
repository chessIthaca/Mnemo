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

    // The kind constructors (no_project / session_closed / …) land with
    // the engine + session code that calls them (3a.4/3a.5) — ahead-of-
    // use fns are dead code under #![deny(warnings)].
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}
