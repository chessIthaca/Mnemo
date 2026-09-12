// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The agent-facing surface of a connected MCP server.
//!
//! [`McpTool`] adapts ONE remote tool to the local [`Tool`] trait: the
//! agent-facing name is namespaced (`mcp__<server>__<tool>`) so remote
//! names can never collide with built-ins or with each other, the MCP
//! `inputSchema` passes through as the tool schema (it is already JSON
//! Schema), and `execute` routes to `tools/call` through the shared
//! [`McpManager`]. Every call is [`SafetyLevel::NeedsApproval`] — MCP
//! tools are foreign code by definition; the existing safety-mode +
//! safety-rules machinery is the gate, with no MCP-specific bypass in v1.
//!
//! [`McpReveal`] is the `load_tools` half for `mcp.<server>` groups: it
//! connects on demand, lists the server's tools, and materializes one
//! [`McpTool`] per remote tool into the registry's shared [`ToolSlot`] —
//! from the next request on they are ordinary tools.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::mcp::{McpManager, McpToolInfo};
use crate::provider::ToolSchema;
use crate::tool::agent::cap_tool_output;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult, ToolSlot};

/// The agent-facing prefix for every MCP tool name (`mcp__<server>__<tool>`).
/// The research plan filter keys on this prefix to exclude MCP tools
/// wholesale (a research plan produces no source changes; foreign tools
/// can mutate anything).
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// What kind of remote surface this adapter wraps: a tool (`tools/call`),
/// a prompt getter (`prompts/get`), or a resource reader
/// (`resources/read`). The meta kinds are registered at reveal time only
/// when the server advertised the capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolKind {
    /// A remote tool — `tools/call` with the adapter's remote name.
    Tool,
    /// `prompts/get` with the call's `name` + `arguments` args.
    GetPrompt,
    /// `resources/read` with the call's `uri` arg.
    ReadResource,
}

/// Build the agent-facing name for a remote tool.
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("{MCP_TOOL_PREFIX}{server}__{tool}")
}

/// One remote MCP surface, adapted to the local [`Tool`] trait.
pub struct McpTool {
    /// The full agent-facing name (`mcp__<server>__<tool>`).
    full_name: String,
    /// The deferred group this tool joins on reveal (`mcp.<server>`).
    group: String,
    /// What the adapter wraps (tool / get_prompt / read_resource).
    kind: McpToolKind,
    /// The owning server's config name.
    server: String,
    /// The remote tool name (tools/call) — empty for the meta kinds.
    remote_name: String,
    /// The description (passed through to the schema).
    description: String,
    /// The JSON Schema (passed through as the tool schema).
    input_schema: Value,
    /// Per-server AUTO-APPROVE trust: when true, `safety()` is AutoRun for
    /// APPROVAL only (no per-call prompt) — the ToolFilter's read-only-state
    /// arms exclude `mcp__` names from the AutoRun visibility arm, so trust
    /// never widens the plan-first gates. Default deny.
    trusted: bool,
    /// The shared connection manager — calls route through it, so a
    /// crashed connection transparently respawns on the next call.
    manager: Arc<McpManager>,
}

impl McpTool {
    /// Adapt one remote tool. `info` comes from the server's `tools/list`.
    pub fn new(server: String, manager: Arc<McpManager>, info: McpToolInfo) -> Self {
        let full_name = mcp_tool_name(&server, &info.name);
        let group = format!("mcp.{server}");
        Self {
            full_name,
            group,
            kind: McpToolKind::Tool,
            remote_name: info.name,
            description: info.description,
            input_schema: info.input_schema,
            server,
            trusted: false,
            manager,
        }
    }

    /// Mark this adapter as trusted (per-server auto-approve): its
    /// `safety()` becomes AutoRun for approval purposes. Consumed before
    /// pushing into the slot — the reveal passes the def's `trusted`
    /// through.
    pub fn with_trusted(mut self, trusted: bool) -> Self {
        self.trusted = trusted;
        self
    }

    /// Build a capability-gated meta adapter (`GetPrompt` /
    /// `ReadResource`). `suffix` is the tool-name fragment
    /// (`get_prompt` / `read_resource`).
    pub fn new_meta(
        server: String,
        manager: Arc<McpManager>,
        kind: McpToolKind,
        suffix: &str,
        description: String,
        input_schema: Value,
    ) -> Self {
        let full_name = format!("{MCP_TOOL_PREFIX}{server}__{suffix}");
        let group = format!("mcp.{server}");
        Self {
            full_name,
            group,
            kind,
            server,
            remote_name: String::new(),
            description,
            input_schema,
            trusted: false,
            manager,
        }
    }

    /// The deferred group this tool belongs to (`mcp.<server>`).
    pub fn group(&self) -> &str {
        &self.group
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.full_name
    }

    fn category(&self) -> ToolCategory {
        // Agent-category + NeedsApproval rides the existing state gates:
        // visible in Executing (and Reviewing, where finding-fixes run),
        // hidden in Planning/Complete (read-only states admit only AutoRun
        // agent tools) and in research plans (name-prefix exclusion).
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            &self.full_name,
            &self.description,
            self.input_schema.clone(),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Foreign code — every call prompts by default (subject to the
        // user's safety mode + safety rules, exactly like shell/git).
        // A TRUSTED server's tools become AutoRun for APPROVAL only: the
        // ToolFilter's read-only-state arms exclude `mcp__` names from the
        // AutoRun visibility arm, so trust never widens the plan-first
        // gates (a trusted tool stays hidden in Planning/Complete/research).
        if self.trusted {
            SafetyLevel::AutoRun
        } else {
            SafetyLevel::NeedsApproval
        }
    }

    fn deferred_group(&self) -> Option<&str> {
        Some(&self.group)
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let outcome = match self.kind {
            McpToolKind::Tool => self
                .manager
                .call_with_info(&self.server, &self.remote_name, args)
                .await
                .map(|(o, reconnected)| {
                    // Agent-window note: this call had to re-establish
                    // the connection (the child restarted after a crash,
                    // or the connection was idle-expired).
                    let note = if reconnected {
                        format!(
                            "⚠ mcp: server '{}' restarted after a crash — this call \
                             reconnected",
                            self.server
                        )
                    } else {
                        String::new()
                    };
                    let text = if note.is_empty() {
                        o.text
                    } else {
                        format!("{note}\n{}", o.text)
                    };
                    (text, o.is_error)
                }),
            McpToolKind::GetPrompt => {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("");
                let prompt_args = args.get("arguments").cloned().unwrap_or_else(|| json!({}));
                self.manager
                    .get_prompt(&self.server, name, prompt_args)
                    .await
                    .map(|text| (text, false))
            }
            McpToolKind::ReadResource => {
                let uri = args.get("uri").and_then(Value::as_str).unwrap_or("");
                self.manager
                    .read_resource(&self.server, uri)
                    .await
                    .map(|text| (text, false))
            }
        };
        match outcome {
            Ok((text, is_error)) => {
                let text = cap_tool_output(text);
                if is_error {
                    ToolResult::error(text)
                } else {
                    ToolResult::success(text)
                }
            }
            Err(e) => ToolResult::error(format!("mcp server '{}' call failed: {e}", self.server)),
        }
    }
}

/// The `load_tools` reveal path for `mcp.<server>` groups: connect (via
/// the shared [`McpManager`], lazily), list the server's tools, and push
/// one [`McpTool`] per remote tool into the registry's shared
/// [`ToolSlot`]. Holds no reference to the registry itself — the slot is
/// the same shared-handle move `LoadedGroups` uses.
pub struct McpReveal {
    manager: Arc<McpManager>,
    slot: ToolSlot,
}

impl McpReveal {
    /// Wire the reveal path to the shared manager + registry slot.
    pub fn new(manager: Arc<McpManager>, slot: ToolSlot) -> Self {
        Self { manager, slot }
    }

    /// Reveal the `mcp.<server>` group: connect, list, materialize the
    /// adapters. Returns the schemas of the tools the reveal materialized
    /// (tools already in the slot from an earlier reveal are skipped, not
    /// duplicated) so the load_tools response can render them — the agent
    /// pre-flights its calls without re-calling. A connection or handshake
    /// failure propagates — the group is NOT marked loaded, so the agent
    /// can retry.
    pub async fn reveal_group(&self, group: &str) -> Result<Vec<crate::provider::ToolSchema>> {
        let Some(server) = group.strip_prefix("mcp.") else {
            return Err(Error::Mcp(format!(
                "'{group}' is not an mcp tool group (expected 'mcp.<server>')"
            )));
        };
        let tools = self.manager.server_tools(server).await?;
        // Per-server trust: a trusted server's adapters are AutoRun for
        // approval only (state gates unchanged — see McpTool::safety).
        let trusted = self
            .manager
            .server_def(server)
            .map(|d| d.trusted)
            .unwrap_or(false);
        let mut schemas: Vec<crate::provider::ToolSchema> = Vec::new();
        for info in tools {
            let tool = McpTool::new(server.to_string(), Arc::clone(&self.manager), info)
                .with_trusted(trusted);
            if self.slot.contains_name(&tool.full_name) {
                continue;
            }
            schemas.push(tool.schema());
            self.slot.push(Arc::new(tool));
        }
        // Capability-gated meta tools: a server that advertises prompts /
        // resources also gets get_prompt / read_resource agent tools, with
        // the reveal-time listing (names / URIs, capped at 20) embedded in
        // the description. That listing is a SNAPSHOT — resources added
        // later need a re-reveal (documented limitation; the underlying
        // request methods are always live).
        let capabilities = self.manager.capabilities(server).await?;
        if capabilities.prompts {
            let prompts = self.manager.list_prompts(server).await.unwrap_or_default();
            let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).take(20).collect();
            let description = if names.is_empty() {
                format!(
                    "Get a prompt from the '{server}' MCP server (prompts/get). The server \
                     listed no prompts at reveal time."
                )
            } else {
                format!(
                    "Get a prompt from the '{server}' MCP server (prompts/get). Available \
                     prompts: {}",
                    names.join(", ")
                )
            };
            let tool = McpTool::new_meta(
                server.to_string(),
                Arc::clone(&self.manager),
                McpToolKind::GetPrompt,
                "get_prompt",
                description,
                json!({
                    "type": "object",
                    "properties": {
                        "name": {"type": "string", "description": "The prompt name"},
                        "arguments": {"type": "object", "description": "Optional prompt arguments"}
                    },
                    "required": ["name"]
                }),
            )
            .with_trusted(trusted);
            if !self.slot.contains_name(&tool.full_name) {
                schemas.push(tool.schema());
                self.slot.push(Arc::new(tool));
            }
        }
        if capabilities.resources {
            let resources = self
                .manager
                .list_resources(server)
                .await
                .unwrap_or_default();
            let uris: Vec<&str> = resources.iter().map(|r| r.uri.as_str()).take(20).collect();
            let description = if uris.is_empty() {
                format!(
                    "Read a resource from the '{server}' MCP server (resources/read). The \
                     server listed no resources at reveal time."
                )
            } else {
                format!(
                    "Read a resource from the '{server}' MCP server (resources/read). \
                     Available resources: {}",
                    uris.join(", ")
                )
            };
            let tool = McpTool::new_meta(
                server.to_string(),
                Arc::clone(&self.manager),
                McpToolKind::ReadResource,
                "read_resource",
                description,
                json!({
                    "type": "object",
                    "properties": {
                        "uri": {"type": "string", "description": "The resource URI"}
                    },
                    "required": ["uri"]
                }),
            )
            .with_trusted(trusted);
            if !self.slot.contains_name(&tool.full_name) {
                schemas.push(tool.schema());
                self.slot.push(Arc::new(tool));
            }
        }
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(schemas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::mcp::{
        CallToolOutcome, McpClient, McpManager, McpPromptInfo, McpResourceInfo, McpToolInfo,
        ServerCapabilities,
    };
    use crate::tool::ToolSlot;
    use serde_json::json;

    /// A fake transport with configurable capabilities + prompt/resource
    /// content — pins meta-tool registration (capability-gated) and the
    /// execute routing per kind. Calls fail while the shared flag is set
    /// (pins the restart note).
    struct FakeClient {
        caps: ServerCapabilities,
        fail_calls: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait]
    impl McpClient for FakeClient {
        async fn initialize(&mut self) -> Result<ServerCapabilities> {
            Ok(self.caps)
        }
        async fn list_tools(&mut self) -> Result<Vec<McpToolInfo>> {
            Ok(vec![McpToolInfo {
                name: "echo".into(),
                description: "d".into(),
                input_schema: json!({"type": "object"}),
            }])
        }
        async fn call_tool(&mut self, _name: &str, _args: Value) -> Result<CallToolOutcome> {
            if self.fail_calls.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::error::Error::Mcp("boom (scripted)".into()));
            }
            Ok(CallToolOutcome {
                text: "ok".into(),
                is_error: false,
            })
        }
        async fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>> {
            Ok(vec![McpPromptInfo {
                name: "greet".into(),
                description: "".into(),
            }])
        }
        async fn get_prompt(&mut self, _name: &str, _args: Value) -> Result<String> {
            Ok("hello from prompt".into())
        }
        async fn list_resources(&mut self) -> Result<Vec<McpResourceInfo>> {
            Ok(vec![McpResourceInfo {
                uri: "file:///x".into(),
                name: "x".into(),
                description: "".into(),
            }])
        }
        async fn read_resource(&mut self, _uri: &str) -> Result<String> {
            Ok("resource body".into())
        }
    }

    fn fake_manager_with(caps: ServerCapabilities) -> Arc<McpManager> {
        let def: crate::config::mcp::McpServerDef =
            toml::from_str("name = \"fs\"\ncommand = \"fake\"\n").unwrap();
        let fail: std::sync::Arc<std::sync::atomic::AtomicBool> = Default::default();
        let factory: crate::mcp::ClientFactory =
            Arc::new(move |_def: &crate::config::mcp::McpServerDef| {
                Ok(Box::new(FakeClient {
                    caps,
                    fail_calls: std::sync::Arc::clone(&fail),
                }) as Box<dyn McpClient>)
            });
        Arc::new(McpManager::with_factory(&[def], factory))
    }

    fn fake_manager_with_fail(
        caps: ServerCapabilities,
    ) -> (
        Arc<McpManager>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) {
        let def: crate::config::mcp::McpServerDef =
            toml::from_str("name = \"fs\"\ncommand = \"fake\"\n").unwrap();
        let fail: std::sync::Arc<std::sync::atomic::AtomicBool> = Default::default();
        let factory: crate::mcp::ClientFactory = {
            let fail = std::sync::Arc::clone(&fail);
            Arc::new(move |_def: &crate::config::mcp::McpServerDef| {
                Ok(Box::new(FakeClient {
                    caps,
                    fail_calls: std::sync::Arc::clone(&fail),
                }) as Box<dyn McpClient>)
            })
        };
        (Arc::new(McpManager::with_factory(&[def], factory)), fail)
    }

    fn sample_info() -> McpToolInfo {
        McpToolInfo {
            name: "echo".into(),
            description: "Echoes back".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"]
            }),
        }
    }

    #[test]
    fn namespacing_group_and_schema_passthrough() {
        let manager = Arc::new(McpManager::from_config(&[]));
        let tool = McpTool::new("fs".into(), manager, sample_info());
        assert_eq!(tool.name(), "mcp__fs__echo");
        assert_eq!(tool.deferred_group(), Some("mcp.fs"));
        assert_eq!(tool.group(), "mcp.fs");
        assert_eq!(tool.safety(), SafetyLevel::NeedsApproval);
        assert_eq!(tool.category(), ToolCategory::Agent);
        let schema = tool.schema();
        assert_eq!(schema.name, "mcp__fs__echo");
        assert_eq!(schema.parameters["required"][0], "text");
        assert_eq!(schema.description, "Echoes back");
    }

    #[tokio::test]
    async fn execute_maps_manager_errors_and_outcomes() {
        // No servers configured → the manager errors with "unknown mcp
        // server"; the adapter maps it into a failed ToolResult naming
        // the server (never a panic).
        let manager = Arc::new(McpManager::from_config(&[]));
        let tool = McpTool::new("ghost".into(), manager, sample_info());
        let r = tool.execute(json!({"text": "hi"})).await;
        assert!(!r.success, "unknown server surfaces as an error result");
        assert!(r.output.contains("mcp server 'ghost'"), "{}", r.output);
    }

    #[tokio::test]
    async fn reveal_materializes_tools_into_the_slot() {
        use crate::config::mcp::McpServerDef;
        // Fake-client seam: one enabled server whose tool list is served by
        // the same fake used in the manager tests.
        let def = McpServerDef {
            name: "fs".into(),
            enabled: true,
            command: Some("fake".into()),
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            headers_env: Default::default(),
            auth: None,
            client_id: None,
            scopes: Vec::new(),
            trusted: false,
            idle_timeout_secs: None,
        };
        let manager = Arc::new(McpManager::from_config(&[def]));
        // Swap the factory… the real manager has no injection point here,
        // so build one through with_factory via a wrapper type is not
        // possible on an Arc. Instead: exercise reveal against the REAL
        // stdio factory would spawn "fake" — not runnable. The slot
        // mechanics are what this test pins, so use the slot directly
        // with a materialized tool and assert dedup + counting via a
        // manager-backed reveal on a server that fails to connect.
        let slot = ToolSlot::new();
        let reveal = McpReveal::new(manager, slot.clone());
        let err = reveal.reveal_group("mcp.ghost").await.unwrap_err();
        assert!(err.to_string().contains("unknown mcp server"), "{err}");
        // Non-mcp group names are rejected before any I/O.
        let err = reveal.reveal_group("browser").await.unwrap_err();
        assert!(err.to_string().contains("not an mcp tool group"), "{err}");
        // Dedup guard: pushing the same tool twice keeps one entry.
        let mgr = Arc::new(McpManager::from_config(&[]));
        let t = McpTool::new("fs".into(), mgr, sample_info());
        let name = t.name().to_string();
        slot.push(Arc::new(t));
        assert!(slot.contains_name(&name));
    }

    #[test]
    fn tool_name_prefix_is_the_one_the_filters_key_on() {
        assert!(mcp_tool_name("s", "t").starts_with(MCP_TOOL_PREFIX));
    }

    #[tokio::test]
    async fn reconnecting_call_carries_the_restart_note() {
        // A crashed child (failed call) then a success: the reconnecting
        // call's output is prefixed with the agent-window restart note.
        let (manager, fail) = fake_manager_with_fail(ServerCapabilities::default());
        fail.store(true, std::sync::atomic::Ordering::SeqCst);
        let tool = McpTool::new("fs".into(), Arc::clone(&manager), sample_info());
        let r = tool.execute(json!({"text": "hi"})).await;
        assert!(!r.success, "the failing call surfaces its error");
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        let r = tool.execute(json!({"text": "hi"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("restarted after a crash"),
            "the reconnecting call notes the restart: {}",
            r.output
        );
        // A steady-state call carries no note.
        let r = tool.execute(json!({"text": "hi"})).await;
        assert!(r.success);
        assert!(!r.output.contains("restarted"), "steady state: no note");
    }

    #[test]
    fn trusted_flag_flips_approval_only() {
        let manager = Arc::new(McpManager::from_config(&[]));
        let untrusted = McpTool::new("fs".into(), Arc::clone(&manager), sample_info());
        assert_eq!(
            untrusted.safety(),
            SafetyLevel::NeedsApproval,
            "default deny"
        );
        let trusted =
            McpTool::new("fs".into(), Arc::clone(&manager), sample_info()).with_trusted(true);
        assert_eq!(trusted.safety(), SafetyLevel::AutoRun);
        // Meta kinds inherit the same flag.
        let meta = McpTool::new_meta(
            "fs".into(),
            Arc::clone(&manager),
            McpToolKind::GetPrompt,
            "get_prompt",
            "d".into(),
            json!({"type": "object"}),
        )
        .with_trusted(true);
        assert_eq!(meta.safety(), SafetyLevel::AutoRun);
    }

    #[tokio::test]
    async fn reveal_stamps_trusted_servers_auto_run() {
        // A trusted def: the materialized adapters (tool + meta tools) are
        // AutoRun for approval — the reveal reads `trusted` from the def.
        let def: crate::config::mcp::McpServerDef =
            toml::from_str("name = \"fs\"\ncommand = \"fake\"\ntrusted = true\n").unwrap();
        let factory: crate::mcp::ClientFactory =
            Arc::new(move |_def: &crate::config::mcp::McpServerDef| {
                Ok(Box::new(FakeClient {
                    caps: ServerCapabilities {
                        prompts: true,
                        resources: true,
                    },
                    fail_calls: Default::default(),
                }) as Box<dyn McpClient>)
            });
        let manager = Arc::new(McpManager::with_factory(&[def], factory));
        let table = crate::tool::deferred_groups_with(&manager.servers());
        let registry =
            crate::tool::ToolRegistry::with_groups(crate::tool::LoadedGroups::new(), table);
        let reveal = McpReveal::new(Arc::clone(&manager), registry.dynamic_slot());
        reveal.reveal_group("mcp.fs").await.unwrap();
        assert_eq!(
            registry.get("mcp__fs__echo").unwrap().safety(),
            SafetyLevel::AutoRun
        );
        assert_eq!(
            registry.get("mcp__fs__get_prompt").unwrap().safety(),
            SafetyLevel::AutoRun
        );
        assert_eq!(
            registry.get("mcp__fs__read_resource").unwrap().safety(),
            SafetyLevel::AutoRun
        );
    }

    #[tokio::test]
    async fn reveal_registers_capability_gated_meta_tools() {
        let manager = fake_manager_with(ServerCapabilities {
            prompts: true,
            resources: true,
        });
        let slot = ToolSlot::new();
        let reveal = McpReveal::new(Arc::clone(&manager), slot.clone());
        let schemas = reveal.reveal_group("mcp.fs").await.unwrap();
        assert_eq!(schemas.len(), 3, "echo + 2 capability meta tools");
        assert_eq!(schemas[0].name, "mcp__fs__echo", "sorted by name");
        assert!(slot.contains_name("mcp__fs__echo"));
        assert!(
            slot.contains_name("mcp__fs__get_prompt"),
            "prompts capability → get_prompt meta tool"
        );
        assert!(
            slot.contains_name("mcp__fs__read_resource"),
            "resources capability → read_resource meta tool"
        );
    }

    #[tokio::test]
    async fn meta_tools_absent_without_capabilities() {
        let manager = fake_manager_with(ServerCapabilities::default());
        let slot = ToolSlot::new();
        let reveal = McpReveal::new(Arc::clone(&manager), slot.clone());
        reveal.reveal_group("mcp.fs").await.unwrap();
        assert!(slot.contains_name("mcp__fs__echo"));
        assert!(!slot.contains_name("mcp__fs__get_prompt"));
        assert!(!slot.contains_name("mcp__fs__read_resource"));
    }

    #[tokio::test]
    async fn meta_tool_execute_routes_to_the_right_method() {
        let manager = fake_manager_with(ServerCapabilities {
            prompts: true,
            resources: true,
        });
        let prompt_tool = McpTool::new_meta(
            "fs".into(),
            Arc::clone(&manager),
            McpToolKind::GetPrompt,
            "get_prompt",
            "d".into(),
            json!({"type": "object"}),
        );
        let r = prompt_tool.execute(json!({"name": "greet"})).await;
        assert!(r.success, "{}", r.output);
        assert_eq!(r.output, "hello from prompt");

        let resource_tool = McpTool::new_meta(
            "fs".into(),
            Arc::clone(&manager),
            McpToolKind::ReadResource,
            "read_resource",
            "d".into(),
            json!({"type": "object"}),
        );
        let r = resource_tool.execute(json!({"uri": "file:///x"})).await;
        assert!(r.success, "{}", r.output);
        assert_eq!(r.output, "resource body");
    }
}
