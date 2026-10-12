// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `ask_user` — ask the user a question with clickable options.
//!
//! The agent calls this when it needs a decision or a preference from the
//! user. The question is rendered as clickable option buttons plus an
//! always-present "💬 Let's talk about it" freeform text input, and the agent
//! pauses until the user answers (one question at a time — the turn blocks on
//! the answer, so a second question can't be asked until the first is
//! answered).
//!
//! ## How the pause works
//!
//! Unlike most tools, `ask_user` does **not** run its `execute()` to produce
//! the answer. The dispatch layer ([`crate::agent::dispatch`]) detects an
//! `ask_user` call by name and handles the pause itself — it emits an
//! [`AgentEvent::UserQuestion`](crate::runtime::AgentEvent::UserQuestion)
//! carrying a `oneshot::Sender`, awaits the receiver, and returns the user's
//! answer as the [`ToolResult`]. This mirrors exactly how the approval gate
//! works (the tool trait's `execute` has no access to the fan-in channel or
//! agent id, so the pause is driven from dispatch, which does).
//!
//! The tool struct here exists to provide the JSON schema (so the model knows
//! the tool's shape) and to validate the arguments. Its `execute()` is a
//! fallback that returns an error explaining the tool must be dispatched via
//! the question path — it should never be reached in production (dispatch
//! intercepts `ask_user` before calling `execute`).

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Arguments for `ask_user`.
#[derive(Debug, Deserialize)]
struct AskUserArgs {
    /// The question to ask the user.
    question: String,
    /// The clickable options (at least 2; 2–6 recommended). Each has a short
    /// label and an optional longer description.
    #[serde(default)]
    options: Vec<AskUserOption>,
}

/// One option in an `ask_user` question.
#[derive(Debug, Deserialize)]
struct AskUserOption {
    /// The short button label.
    label: String,
    /// An optional longer description shown under the label.
    #[serde(default)]
    description: Option<String>,
}

/// The `ask_user` workflow tool — ask the user a question.
///
/// See the module docs for how the pause is driven from dispatch. `AutoRun` —
/// asking a question is not a mutation, so it is never approval-gated.
pub struct AskUserTool;

impl AskUserTool {
    /// Create the tool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AskUserTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "ask_user",
            "Ask the user a question, shown as one button per option plus a freeform text \
             input. ONLY for a choice between at least two concrete options — do NOT guess. \
             Frame it as a choice (\"Which approach?\", \"A or B?\"), never open-ended \
             (\"What do you think?\") — post those as text in your response instead. You \
             pause until they answer, and can ask only ONE question at a time. The answer \
             comes back as the chosen label or the freeform text.",
            json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user. Keep it concise and self-contained."
                    },
                    "options": {
                        "type": "array",
                        "description": "The clickable options (at least 2; 2–6 ideal).",
                        "minItems": 2,
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": {
                                    "type": "string",
                                    "description": "Short button label (a few words); put detail in `description`."
                                },
                                "description": {
                                    "type": "string",
                                    "description": "Optional longer explanation shown under the label."
                                }
                            },
                            "required": ["label"]
                        }
                    }
                },
                "required": ["question", "options"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Asking a question is not a mutation — never approval-gated.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        // Dispatch intercepts `ask_user` before reaching here. If we ARE
        // reached, it means the tool was called outside the dispatch path
        // (e.g. a test calling execute directly) — validate the args and
        // return a clear error so the caller knows to use the question path.
        match serde_json::from_value::<AskUserArgs>(args) {
            Ok(a) => ToolResult::error(format!(
                "ask_user must be dispatched via the question path (emit UserQuestion + await \
                 the oneshot), not executed directly. Question was: \"{}\" ({} options).",
                a.question,
                a.options.len()
            )),
            Err(e) => ToolResult::error(format!("invalid ask_user arguments: {e}")),
        }
    }
}

/// Parse + validate `ask_user` arguments from a raw JSON value.
///
/// Used by the dispatch layer to extract the question + options before
/// emitting the `UserQuestion` event. Returns the question text + the
/// serializable options, or an error string.
pub(crate) fn parse_ask_user_args(
    args: &serde_json::Value,
) -> Result<(String, Vec<crate::runtime::QuestionOption>), String> {
    let a: AskUserArgs = serde_json::from_value(args.clone())
        .map_err(|e| format!("invalid ask_user arguments: {e}"))?;
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
        .map(|o| crate::runtime::QuestionOption {
            label: o.label,
            description: o.description,
        })
        .collect();
    Ok((a.question, options))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_args_with_options() {
        let args = serde_json::json!({
            "question": "Pick one",
            "options": [
                {"label": "A", "description": "first"},
                {"label": "B"}
            ]
        });
        let (q, opts) = parse_ask_user_args(&args).unwrap();
        assert_eq!(q, "Pick one");
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].label, "A");
        assert_eq!(opts[0].description.as_deref(), Some("first"));
        assert_eq!(opts[1].label, "B");
        assert!(opts[1].description.is_none());
    }

    #[test]
    fn rejects_zero_options() {
        // At least 2 options are required — an open-ended question should skip
        // the tool and be posted as text instead.
        let args = serde_json::json!({"question": "What now?"});
        let err = parse_ask_user_args(&args).unwrap_err();
        assert!(
            err.contains("at least 2"),
            "error should mention 'at least 2', got: {err}"
        );
    }

    #[test]
    fn rejects_single_option() {
        // One option is not a choice — require at least 2.
        let args = serde_json::json!({
            "question": "Pick one",
            "options": [{"label": "A"}]
        });
        let err = parse_ask_user_args(&args).unwrap_err();
        assert!(
            err.contains("at least 2"),
            "error should mention 'at least 2', got: {err}"
        );
    }

    #[test]
    fn accepts_exactly_two_options() {
        let args = serde_json::json!({
            "question": "Pick one",
            "options": [{"label": "A"}, {"label": "B"}]
        });
        let (q, opts) = parse_ask_user_args(&args).unwrap();
        assert_eq!(q, "Pick one");
        assert_eq!(opts.len(), 2);
    }

    #[test]
    fn rejects_empty_question() {
        let args = serde_json::json!({"question": "   "});
        assert!(parse_ask_user_args(&args).is_err());
    }

    #[test]
    fn rejects_missing_question() {
        let args = serde_json::json!({"options": [{"label": "A"}]});
        assert!(parse_ask_user_args(&args).is_err());
    }

    #[tokio::test]
    async fn execute_fallback_returns_clear_error() {
        let tool = AskUserTool::new();
        let res = tool
            .execute(serde_json::json!({"question": "hi", "options": []}))
            .await;
        assert!(!res.success);
        assert!(res.output.contains("question path"));
    }

    #[test]
    fn schema_has_question_and_options() {
        let tool = AskUserTool::new();
        let schema = tool.schema();
        // The schema serializes with the expected name + properties.
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(json["name"], "ask_user");
        let props = &json["parameters"]["properties"];
        assert!(props["question"]["type"] == "string");
        assert!(props["options"]["type"] == "array");
    }
}
