// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `list_models` — list the configured model endpoints + their model ids.
//!
//! A read-only discovery tool: the agent calls it to learn which models are
//! available before picking one for `spawn_agent`'s optional `model`
//! parameter. Returns a human-readable summary (one line per endpoint) plus
//! the structured [`ModelInfo`] list in `ToolResult.data`.
//!
//! Auto-run (no side effects, no approval) — it only reads the live config
//! through the shared [`ModelResolver`]. Omitted from the registry entirely
//! when no resolver is wired (tests / no `[models]` section), since there's
//! nothing to list.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use crate::model_resolver::{ModelInfo, ModelResolver};
use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// The `list_models` tool.
///
/// Holds an `Option<Arc<dyn ModelResolver>>` — the resolver is the config
/// seam. When `None`, `execute` returns a clear "unavailable" error rather
/// than an empty list, so the agent knows model selection isn't configured.
pub struct ListModelsTool {
    resolver: Option<Arc<dyn ModelResolver>>,
}

impl ListModelsTool {
    /// Create the tool. `resolver` is `None` when no per-context model
    /// resolution is configured (tests / no `[models]` section) — in that case
    /// the tool is typically omitted from the registry by the factory.
    pub fn new(resolver: Option<Arc<dyn ModelResolver>>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for ListModelsTool {
    fn name(&self) -> &str {
        "list_models"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "list_models",
            "List the configured model endpoints and the model ids each serves, so you can pick \
             a model for spawn_agent's optional `model` parameter. Returns one line per endpoint \
             (\"endpoint: model1, model2\"). No parameters.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let Some(resolver) = self.resolver.as_ref() else {
            return ToolResult::error("model selection unavailable (no model resolver configured)");
        };
        let infos: Vec<ModelInfo> = resolver.list_models();
        if infos.is_empty() {
            return ToolResult::error("no model endpoints configured");
        }
        // Human-readable summary: one line per endpoint.
        let summary = infos
            .iter()
            .map(|i| {
                let models = if i.models.is_empty() {
                    "(no models)".to_string()
                } else {
                    i.models.join(", ")
                };
                format!("{}: {}", i.endpoint, models)
            })
            .collect::<Vec<_>>()
            .join("\n");
        ToolResult {
            success: true,
            output: summary,
            data: Some(json!(infos)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModelRef;
    use crate::model_resolver::ModelContext;

    /// A mock resolver returning a canned model list (and canned resolves).
    struct MockResolver {
        infos: Vec<ModelInfo>,
    }

    #[async_trait]
    impl ModelResolver for MockResolver {
        fn resolve(&self, _context: ModelContext<'_>) -> Option<ModelRef> {
            None
        }
        fn build_turn_provider(
            &self,
            _model: &ModelRef,
            _fill_rate: f64,
        ) -> Option<(
            Arc<dyn crate::provider::LlmClient>,
            crate::agent::context::ContextManager,
        )> {
            None
        }
        fn list_models(&self) -> Vec<ModelInfo> {
            self.infos.clone()
        }
    }

    #[tokio::test]
    async fn returns_summary_and_data_when_resolver_present() {
        let resolver = Arc::new(MockResolver {
            infos: vec![
                ModelInfo {
                    endpoint: "openai".into(),
                    models: vec!["gpt-4o".into(), "o3".into()],
                },
                ModelInfo {
                    endpoint: "deepseek".into(),
                    models: vec!["deepseek-v4-flash".into()],
                },
            ],
        });
        let tool = ListModelsTool::new(Some(resolver));
        let result = tool.execute(json!({})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("openai: gpt-4o, o3"));
        assert!(result.output.contains("deepseek: deepseek-v4-flash"));
        // The structured data carries the ModelInfo list.
        let data = result.data.expect("data should be present");
        let arr = data.as_array().expect("data should be an array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["endpoint"], "openai");
        assert_eq!(arr[1]["endpoint"], "deepseek");
    }

    #[tokio::test]
    async fn errors_when_no_resolver() {
        let tool = ListModelsTool::new(None);
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(result.output.contains("unavailable"));
    }

    #[tokio::test]
    async fn errors_when_no_endpoints_configured() {
        let resolver = Arc::new(MockResolver { infos: vec![] });
        let tool = ListModelsTool::new(Some(resolver));
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(result.output.contains("no model endpoints configured"));
    }
}
