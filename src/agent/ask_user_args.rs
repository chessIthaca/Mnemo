// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `ask_user` argument parse — kernel-owned (job 2b.3): the dispatch
//! layer (a kernel consumer) extracts the question + options before emitting
//! the `UserQuestion` event, without importing the app's tool module. The
//! app's `AskUserTool` reads the same parse back through this module
//! (app→kernel, the legal direction) so there is exactly ONE validator.

use serde::Deserialize;

use crate::runtime::QuestionOption;

/// Arguments for `ask_user`.
#[derive(Debug, Deserialize)]
pub(crate) struct AskUserArgs {
    /// The question to ask the user.
    pub(crate) question: String,
    /// The clickable options (at least 2; 2–6 recommended). Each has a short
    /// label and an optional longer description.
    #[serde(default)]
    pub(crate) options: Vec<AskUserOption>,
}

/// One option in an `ask_user` question.
#[derive(Debug, Deserialize)]
pub(crate) struct AskUserOption {
    /// The short button label.
    pub(crate) label: String,
    /// An optional longer description shown under the label.
    #[serde(default)]
    pub(crate) description: Option<String>,
}

/// Parse + validate `ask_user` arguments from a raw JSON value.
///
/// Used by the dispatch layer to extract the question + options before
/// emitting the `UserQuestion` event. Returns the question text + the
/// serializable options, or an error string.
pub(crate) fn parse_ask_user_args(
    args: &serde_json::Value,
) -> Result<(String, Vec<QuestionOption>), String> {
    let a: AskUserArgs = serde_json::from_value(args.clone()).map_err(|e| {
        // Backlog 05a0905f (plan 81a1b70d, review round-1 F1): this was
        // the one decode rejection that escaped the sanitizer sweep —
        // this module predates it, lives under src/agent/, and never
        // called the bare-message predecessor. Route through the
        // enriched producer so the model sees the received
        // bytes/keys/tail and the truncation remedy instead of
        // blind-retrying an identical truncated payload.
        crate::tool::error_message::sanitize_arguments_error_with("ask_user", &e, args)
    })?;
    if a.question.trim().is_empty() {
        return Err("ask_user requires a non-empty 'question'".into());
    }
    if a.options.len() < 2 {
        return Err(format!(
            "ask_user requires at least 2 options (got {}) — for an open-ended question, skip \
             this tool and post the question as text in your response",
            a.options.len()
        ));
    }
    let options = a
        .options
        .into_iter()
        .map(|o| QuestionOption {
            label: o.label,
            description: o.description,
        })
        .collect();
    Ok((a.question, options))
}