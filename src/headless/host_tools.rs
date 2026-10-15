// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! G2 host tools (master-spec §5.9.2) — tools whose execution lives
//! OUTSIDE the kernel. The session's owner registers them per spawn
//! ([`HostToolSpec`] on `SpawnOptions.host_tools`); the model sees and
//! calls them like any tool, but the dispatch layer never runs the shim's
//! `execute` — it emits [`AgentEvent::HostToolCall`] and suspends until
//! the host answers with the `HostToolResult` session command (or the
//! per-tool timeout elapses, which resolves to an error result and lets
//! the turn continue — an absent host can never hang the agent, R4). The
//! answer folds back through the normal `ToolResult` path, so output
//! compaction and the read cache treat a host-tool result like any other.
//!
//! The shims ride the registry's shared dynamic slot (the MCP
//! late-registration path — interior-mutable, so a post-build spawn
//! needs no `&mut registry`): a shim has no deferred group, so it is
//! advertised from the very first request and `get` finds it for the
//! dispatch-time interception.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// The per-tool registration of a host tool (§5.9.2) — plain serde data a
/// binding in any language can pass on `SpawnOptions.host_tools`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostToolSpec {
    /// The tool name (matches the model's tool-call name).
    pub name: String,
    /// One line the model sees in the tool index.
    pub description: String,
    /// The JSON schema for the `parameters` object of the tool schema.
    pub parameters: Value,
    /// The safety level — host tools ride the normal safety/approval
    /// machinery like any other tool (a `NeedsApproval` host tool prompts
    /// exactly like a kernel tool with that level).
    pub safety: SafetyLevel,
    /// The per-tool await timeout in milliseconds — `None` uses the
    /// 120 s default ([`HOST_TOOL_DEFAULT_TIMEOUT_MS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// The default host-tool await: 120 s (§5.9.2 — "a per-tool timeout
/// (default 120 s → error result to the model)").
pub const HOST_TOOL_DEFAULT_TIMEOUT_MS: u64 = 120_000;

impl HostToolSpec {
    /// The effective await timeout — the spec's, or the 120 s default.
    pub fn effective_timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(HOST_TOOL_DEFAULT_TIMEOUT_MS)
    }
}

/// The registry shim (§5.9.2): registered into the spawn's registry so the
/// model sees and calls it like any tool. The dispatch layer intercepts
/// the call and runs the kernel↔host round-trip instead, so [`Tool::execute`]
/// NEVER runs — its body is a defensive error, not a code path. The
/// [`Tool::host_tool_timeout_ms`] marker is what dispatch consults.
#[derive(Debug)]
pub struct HostTool {
    spec: HostToolSpec,
}

impl HostTool {
    /// Wrap a spec as the shared registry shim.
    pub fn wrap(spec: HostToolSpec) -> std::sync::Arc<dyn Tool> {
        std::sync::Arc::new(Self { spec })
    }
}

#[async_trait::async_trait]
impl Tool for HostTool {
    fn name(&self) -> &str {
        &self.spec.name
    }

    fn category(&self) -> ToolCategory {
        // The generic coding-tool bucket: host tools are advertised
        // wherever the agent's filter admits kernel tools.
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.spec.name.clone(),
            description: self.spec.description.clone(),
            parameters: self.spec.parameters.clone(),
            strict: None,
        }
    }

    fn safety(&self) -> SafetyLevel {
        self.spec.safety
    }

    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::error(
            "host tools dispatch through the kernel's HostToolCall round-trip — \
             the shim's execute must never run",
        )
    }

    fn host_tool_timeout_ms(&self) -> Option<u64> {
        Some(self.spec.effective_timeout_ms())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The effective timeout: explicit spec value wins, `None` falls back
    /// to the 120 s default (§5.9.2).
    #[test]
    fn host_tool_spec_effective_timeout() {
        assert_eq!(HOST_TOOL_DEFAULT_TIMEOUT_MS, 120_000);
        let spec = HostToolSpec {
            name: "get_weather".into(),
            description: "Get the weather".into(),
            parameters: serde_json::json!({"type": "object"}),
            safety: SafetyLevel::AutoRun,
            timeout_ms: None,
        };
        assert_eq!(spec.effective_timeout_ms(), 120_000);
        let spec = HostToolSpec {
            timeout_ms: Some(250),
            ..spec
        };
        assert_eq!(spec.effective_timeout_ms(), 250);
    }

    /// The shim answers both dispatch questions — it IS a host tool, with
    /// the effective timeout — and its defensive `execute` never runs.
    #[test]
    fn host_tool_shim_marks_itself_and_maps_the_schema() {
        let spec = HostToolSpec {
            name: "get_weather".into(),
            description: "Get the weather".into(),
            parameters: serde_json::json!({"type": "object"}),
            safety: SafetyLevel::NeedsApproval,
            timeout_ms: Some(500),
        };
        let tool = HostTool::wrap(spec);
        assert_eq!(tool.name(), "get_weather");
        assert_eq!(tool.safety(), SafetyLevel::NeedsApproval);
        assert_eq!(tool.host_tool_timeout_ms(), Some(500));
        let schema = tool.schema();
        assert_eq!(schema.name, "get_weather");
        assert_eq!(schema.description, "Get the weather");
    }
}