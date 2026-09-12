// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Typed IPC error — a serializable `{ kind, message }` DTO returned by every
//! Tauri command, so the frontend can branch on `kind` instead of parsing a
//! bare string.
//!
//! Every `#[tauri::command]` returns `Result<T, IpcError>` instead of
//! `Result<T, String>`. The frontend receives a structured `{ kind, message }`
//! payload and can react differently to distinct failure kinds (e.g.
//! `WorkflowNoPlan` vs `WorkflowWrongState`) rather than only displaying a
//! string.
//!
//! `From<String>` and `From<&str>` map free-form error strings to
//! `kind = "error"`, so existing `.map_err(|e| format!(...))?` call sites
//! convert automatically via `?`. `From<mnemo::error::Error>` preserves
//! the variant name as `kind` so structured errors are discriminable on the
//! frontend.

use serde::Serialize;

/// A serializable error returned by IPC commands.
///
/// Carries a machine-readable `kind` (the `mnemo::error::Error` variant
/// name for structured errors, or `"error"` for free-form strings) and a
/// human-readable `message` (the `Display` output).
#[derive(Debug, Serialize)]
pub struct IpcError {
    /// The error kind — the `mnemo::error::Error` variant name for
    /// structured errors, or `"error"` for free-form strings.
    pub kind: String,
    /// The human-readable error message.
    pub message: String,
}

impl IpcError {
    /// Construct a free-form error (`kind = "error"`).
    pub fn msg<S: Into<String>>(message: S) -> Self {
        IpcError {
            kind: "error".to_string(),
            message: message.into(),
        }
    }
}

impl From<String> for IpcError {
    fn from(message: String) -> Self {
        IpcError::msg(message)
    }
}

impl From<&str> for IpcError {
    fn from(message: &str) -> Self {
        IpcError::msg(message)
    }
}

impl From<mnemo::error::Error> for IpcError {
    fn from(e: mnemo::error::Error) -> Self {
        // The Debug representation starts with the variant name (e.g.
        // `WorkflowNoPlan`, `Workflow("...")`, `WorkflowWrongState { .. }`).
        // Extract everything before the first `(` or `{` as the kind.
        let debug = format!("{:?}", e);
        let kind = debug
            .split(['(', '{'])
            .next()
            .unwrap_or("error")
            .trim()
            .to_string();
        IpcError {
            kind,
            message: e.to_string(),
        }
    }
}
