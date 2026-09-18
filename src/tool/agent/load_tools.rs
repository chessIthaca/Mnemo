// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `load_tools` — the reveal half of progressive tool disclosure.
//!
//! Occasional tool families are kept out of the `tools` array and
//! summarized as one line each in the stable head's index. When the agent
//! actually needs one it calls `load_tools`, and the family joins the
//! array from the next request on.
//!
//! Why a round-trip instead of just shipping everything: the browser family
//! (16 schemas on Windows, 10 elsewhere — roughly 900 tokens) is one a
//! typical coding turn never calls, paid on every single request. One extra
//! call per session, only when the tools are genuinely wanted, is a far
//! better trade.
//!
//! The reveal response carries the schemas the agent needs to pre-flight
//! its calls: every revealed tool's name + required parameters with types,
//! plus descriptions and optional params for small groups (≤ 8 tools —
//! image renders full; the browser family renders compact). The
//! already-loaded retry stays short.
//!
//! The group table is runtime-configured: the static base (browser, image)
//! plus one `mcp.<server>` group per enabled MCP server (built by the
//! factory via [`crate::tool::deferred_groups_with`]). Revealing an `mcp.*`
//! group connects to that server through the shared
//! [`McpManager`](crate::mcp::McpManager), lists its tools, and materializes
//! one [`McpTool`](crate::mcp::McpTool) adapter per remote tool — they are
//! ordinary tools from the next request on.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::{LoadedGroups, SafetyLevel, Tool, ToolCategory, ToolResult};

/// Reveals a deferred tool group for the rest of the session.
pub struct LoadToolsTool {
    groups: LoadedGroups,
    /// The deferred-group table this install advertises (static base +
    /// `mcp.<server>` entries). The schema enum and unknown-group
    /// validation are generated from it, so they can never drift.
    table: Vec<(String, String)>,
    /// The MCP reveal path for `mcp.<server>` groups (connect + list +
    /// materialize adapters). `None` when no McpManager is wired — an mcp
    /// group then errors clearly instead of pretending to load.
    mcp: Option<crate::mcp::McpReveal>,
    /// The static deferred tools (browser/image families), Arc clones from
    /// the registry at construction. Static tools never change at runtime,
    /// so a snapshot is safe; MCP tools arrive via `reveal_group`'s return
    /// value instead. The reveal response renders their schemas so the
    /// agent can pre-flight its calls without re-calling load_tools.
    deferred: Vec<std::sync::Arc<dyn Tool>>,
}

impl LoadToolsTool {
    /// Wire the tool to the registry's shared reveal-set, this install's
    /// group table (see [`crate::tool::deferred_groups_with`]), the
    /// optional MCP reveal path, and the static deferred tools whose
    /// schemas the reveal response renders.
    pub fn new(
        groups: LoadedGroups,
        table: Vec<(String, String)>,
        mcp: Option<crate::mcp::McpReveal>,
        deferred: Vec<std::sync::Arc<dyn Tool>>,
    ) -> Self {
        Self {
            groups,
            table,
            mcp,
            deferred,
        }
    }
}

#[derive(Debug, Deserialize)]
struct LoadToolsArgs {
    group: String,
}

/// Whether revealing `group` is allowed under `filter`. An `mcp.*` reveal
/// connects to a foreign server (spawning a process / opening a session
/// that lingers in the manager), so it is gated like the tools it would
/// materialize — Agent category + NeedsApproval: allowed in
/// Executing/Reviewing, blocked in Planning/Complete (safety gate) and
/// research plans (name-prefix exclusion). Non-mcp groups pass through
/// (the browser group stays loadable wherever `load_tools` itself is).
/// Enforced by the dispatch layer right after the workflow filter
/// re-check (review LOW 2).
pub fn mcp_reveal_allowed(filter: &crate::tool::ToolFilter, group: &str) -> bool {
    if !group.trim().starts_with("mcp.") {
        return true;
    }
    filter.allows(
        crate::tool::ToolCategory::Agent,
        crate::tool::SafetyLevel::NeedsApproval,
        "mcp__probe__probe",
    )
}

#[async_trait]
impl Tool for LoadToolsTool {
    fn name(&self) -> &str {
        "load_tools"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        // The enum is generated from the runtime table so the schema can
        // never offer a group that does not exist (static base + the
        // install's configured MCP servers).
        let groups: Vec<&str> = self.table.iter().map(|(g, _)| g.as_str()).collect();
        ToolSchema::new(
            "load_tools",
            "Reveal a tool group listed under AVAILABLE TOOL GROUPS. Its tools join your \
             tool list from your next message on — call this first, then use them (the \
             response lists each tool's required parameters). Loading is permanent for the \
             session and safe to call once per group; calling it for a group you will not \
             use just wastes context.",
            json!({
                "type": "object",
                "properties": {
                    "group": {
                        "type": "string",
                        "enum": groups,
                        "description": "The group to reveal."
                    }
                },
                "required": ["group"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Reveals schemas the agent already has permission to use — the tools
        // themselves keep their own approval levels. Nothing is mutated.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: LoadToolsArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let group = args.group.trim();
        if !self.table.iter().any(|(g, _)| g == group) {
            let known: Vec<&str> = self.table.iter().map(|(g, _)| g.as_str()).collect();
            return ToolResult::error(format!(
                "unknown tool group '{group}' — known groups: {}",
                known.join(", ")
            ));
        }
        if self.groups.contains(group) {
            return ToolResult::success(format!(
                "tool group '{group}' was already loaded — its tools are in your tool list"
            ));
        }
        // MCP groups materialize their tools BEFORE the group is marked
        // loaded, so a failed connect/list can simply be retried.
        if let Some(server) = group.strip_prefix("mcp.") {
            let Some(reveal) = &self.mcp else {
                return ToolResult::error(format!(
                    "tool group '{group}' is unavailable — no MCP manager is wired in this \
                     session (check Settings → MCP)"
                ));
            };
            return match reveal.reveal_group(group).await {
                Ok(schemas) => {
                    self.groups.insert(group);
                    let count = schemas.len();
                    let full = count <= FULL_SCHEMA_GROUP_MAX;
                    let block = render_schemas(&schemas, full);
                    let block = if block.is_empty() {
                        String::new()
                    } else {
                        format!("\n\n{}", block.trim_end())
                    };
                    ToolResult::success(format!(
                        "tool group '{group}' loaded — {count} tool(s) from the '{server}' \
                         MCP server join your tool list from your next message on. Continue, \
                         then call them by their mcp__ names.{block}"
                    ))
                }
                Err(e) => ToolResult::error(format!("failed to load '{group}': {e}")),
            };
        }
        self.groups.insert(group);
        let schemas = group_schemas(&self.deferred, group);
        let full = schemas.len() <= FULL_SCHEMA_GROUP_MAX;
        let block = render_schemas(&schemas, full);
        let block = if block.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", block.trim_end())
        };
        ToolResult::success(format!(
            "tool group '{group}' loaded — its tools appear in your tool list from your next \
             message on. Continue, then call them.{block}"
        ))
    }
}

/// Groups with at most this many tools render FULL schema lines (name +
/// description + required + optional params); larger groups render compact
/// (name + required params only) so the response never undoes the
/// deferral's token savings — the browser family (16 tools on Windows, 10
/// elsewhere) stays a few hundred bytes.
const FULL_SCHEMA_GROUP_MAX: usize = 8;

/// The deferred static tools for `group`: filtered by the same predicate
/// the registry's `load_group` uses (`deferred_group` + `available`), sorted
/// by name, mapped to their schemas.
fn group_schemas(tools: &[std::sync::Arc<dyn Tool>], group: &str) -> Vec<ToolSchema> {
    let mut schemas: Vec<ToolSchema> = tools
        .iter()
        .filter(|t| t.deferred_group() == Some(group) && t.available())
        .map(|t| t.schema())
        .collect();
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    schemas
}

/// The first sentence of a description, capped at 120 chars — enough to
/// pick the right tool without paying the full text.
fn first_sentence(description: &str) -> String {
    let trimmed = description.trim();
    let end = trimmed.find(". ").map(|i| i + 1).unwrap_or(trimmed.len());
    let mut s: String = trimmed[..end].to_string();
    if s.chars().count() > 120 {
        s = s.chars().take(117).collect::<String>() + "...";
    }
    s
}

/// "a: string, b: number" for `names` against a JSON-schema object
/// (`properties`: name → {type}); names without a declared type render bare.
fn param_list(params: &serde_json::Value, names: &[String]) -> String {
    let props = params.get("properties").and_then(|p| p.as_object());
    names
        .iter()
        .map(|n| {
            props
                .and_then(|p| p.get(n.as_str()))
                .and_then(|d| d.get("type"))
                .and_then(|t| t.as_str())
                .map(|t| format!("{n}: {t}"))
                .unwrap_or_else(|| n.clone())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render the revealed tools' schemas for the success response — the agent
/// pre-flights its calls from this block instead of re-calling load_tools.
/// FULL mode (small groups) adds each tool's description first sentence and
/// its optional params; COMPACT mode (large groups) is name + required only
/// — the full schemas join the tools array next request anyway.
fn render_schemas(schemas: &[ToolSchema], full: bool) -> String {
    let mut out = String::new();
    for s in schemas {
        let req: Vec<String> = s
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        if full {
            let desc = first_sentence(&s.description);
            if desc.is_empty() {
                out.push_str(&format!("- {}\n", s.name));
            } else {
                out.push_str(&format!("- {} — {}\n", s.name, desc));
            }
            if !req.is_empty() {
                out.push_str(&format!(
                    "  required: {}\n",
                    param_list(&s.parameters, &req)
                ));
            }
            let opt: Vec<String> = s
                .parameters
                .get("properties")
                .and_then(|p| p.as_object())
                .map(|p| p.keys().filter(|k| !req.contains(k)).cloned().collect())
                .unwrap_or_default();
            if !opt.is_empty() {
                out.push_str(&format!(
                    "  optional: {}\n",
                    param_list(&s.parameters, &opt)
                ));
            }
        } else if req.is_empty() {
            out.push_str(&format!("- {} — no required params\n", s.name));
        } else {
            out.push_str(&format!(
                "- {} — required: {}\n",
                s.name,
                param_list(&s.parameters, &req)
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The static table (no MCP servers configured).
    fn static_table() -> Vec<(String, String)> {
        crate::tool::deferred_groups_with(&[])
    }

    #[tokio::test]
    async fn loads_a_known_group_once() {
        let groups = LoadedGroups::new();
        let tool = LoadToolsTool::new(groups.clone(), static_table(), None, Vec::new());
        assert!(!groups.contains("browser"));

        let r = tool.execute(json!({"group": "browser"})).await;
        assert!(r.success, "{}", r.output);
        assert!(groups.contains("browser"), "the shared set is updated");

        // Idempotent: a second call reports the existing state rather than
        // failing, so a model that retries is not derailed.
        let r2 = tool.execute(json!({"group": "browser"})).await;
        assert!(r2.success);
        assert!(r2.output.contains("already loaded"));
    }

    #[tokio::test]
    async fn unknown_group_errors_and_names_the_valid_ones() {
        let tool = LoadToolsTool::new(LoadedGroups::new(), static_table(), None, Vec::new());
        let r = tool.execute(json!({"group": "nope"})).await;
        assert!(!r.success);
        assert!(
            r.output.contains("browser"),
            "the error lists the loadable groups, got: {}",
            r.output
        );
    }

    #[test]
    fn schema_enum_tracks_the_runtime_table() {
        // A table with an mcp entry: the schema enum must offer it, and
        // nothing else beyond the table.
        let table = vec![
            ("browser".to_string(), "b".to_string()),
            ("mcp.fs".to_string(), "f".to_string()),
        ];
        let tool = LoadToolsTool::new(LoadedGroups::new(), table.clone(), None, Vec::new());
        let schema = tool.schema();
        let en = schema.parameters["properties"]["group"]["enum"]
            .as_array()
            .expect("group has an enum");
        assert_eq!(
            en.len(),
            table.len(),
            "the schema enum is generated from the runtime table, never hand-listed"
        );
        assert!(en.iter().any(|v| v == "mcp.fs"));
    }

    #[test]
    fn mcp_reveal_gate_matches_the_tool_filter() {
        // LOW 2: an mcp.* reveal is allowed exactly where its (Agent +
        // NeedsApproval) tools would be — Executing and Reviewing (where
        // finding-fixes run), never in the read-only states, research
        // plans, or a reviewer's allow-list.
        use crate::tool::ToolFilter;
        assert!(mcp_reveal_allowed(&ToolFilter::Executing, "mcp.fs"));
        assert!(mcp_reveal_allowed(&ToolFilter::Reviewing, "mcp.fs"));
        assert!(mcp_reveal_allowed(&ToolFilter::Executing, " mcp.fs "));
        assert!(!mcp_reveal_allowed(&ToolFilter::Planning, "mcp.fs"));
        assert!(!mcp_reveal_allowed(&ToolFilter::Complete, "mcp.fs"));
        assert!(!mcp_reveal_allowed(
            &ToolFilter::ExecutingResearch,
            "mcp.fs"
        ));
        assert!(!mcp_reveal_allowed(&ToolFilter::Reviewer(vec![]), "mcp.fs"));
        // Non-mcp groups are none of this gate's business — the browser
        // group stays loadable wherever load_tools itself is allowed.
        assert!(mcp_reveal_allowed(&ToolFilter::Planning, "browser"));
        assert!(mcp_reveal_allowed(&ToolFilter::Complete, "image"));
    }

    /// A minimal deferred tool for the renderer tests: fixed group, schema
    /// with required + optional params, and a description.
    struct FakeDeferredTool {
        name: String,
        group: &'static str,
        description: &'static str,
        schema: serde_json::Value,
    }

    #[async_trait]
    impl Tool for FakeDeferredTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Agent
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(self.name.as_str(), self.description, self.schema.clone())
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: serde_json::Value) -> ToolResult {
            ToolResult::success("ok".to_string())
        }
        fn deferred_group(&self) -> Option<&str> {
            Some(self.group)
        }
    }

    fn fake_tool(name: &str, group: &'static str) -> FakeDeferredTool {
        FakeDeferredTool {
            name: name.to_string(),
            group,
            description: "Does a thing. Longer explanation follows.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "d"},
                    "pattern": {"type": "string", "description": "d"},
                    "max": {"type": "number", "description": "d"}
                },
                "required": ["path", "pattern"]
            }),
        }
    }

    #[test]
    fn render_schemas_names_every_tool_with_its_required_params() {
        let schemas = vec![
            fake_tool("alpha", "g").schema(),
            fake_tool("beta", "g").schema(),
        ];
        let out = render_schemas(&schemas, false);
        assert!(
            out.contains("- alpha — required: path: string, pattern: string"),
            "{out}"
        );
        assert!(
            out.contains("- beta — required: path: string, pattern: string"),
            "{out}"
        );
        // Compact mode omits descriptions and optional params.
        assert!(!out.contains("Does a thing"), "{out}");
        assert!(!out.contains("optional"), "{out}");
    }

    #[test]
    fn render_schemas_full_mode_includes_descriptions_and_optional_params() {
        let schemas = vec![fake_tool("alpha", "g").schema()];
        let out = render_schemas(&schemas, true);
        assert!(out.contains("- alpha — Does a thing."), "{out}");
        assert!(
            out.contains("  required: path: string, pattern: string"),
            "{out}"
        );
        assert!(out.contains("  optional: max: number"), "{out}");

        // An empty description (McpToolInfo documents "may be empty")
        // renders no dangling separator.
        let mut quiet = fake_tool("quiet", "g");
        quiet.description = "";
        let out = render_schemas(&[quiet.schema()], true);
        assert!(out.contains("- quiet\n"), "{out}");
        assert!(!out.contains("quiet —"), "{out}");
    }

    #[test]
    fn group_schemas_filters_by_group_and_sorts_by_name() {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(fake_tool("zeta", "g")),
            Arc::new(fake_tool("alpha", "g")),
            Arc::new(fake_tool("other", "h")),
        ];
        let schemas = group_schemas(&tools, "g");
        assert_eq!(schemas.len(), 2, "only the matching group's tools");
        assert_eq!(schemas[0].name, "alpha", "sorted by name");
        assert_eq!(schemas[1].name, "zeta");
    }

    #[test]
    fn large_group_renders_compact_under_budget() {
        // Browser-sized (15 fakes — the real browser family is 16 tools on
        // Windows, 10 elsewhere): must render compact (name + required only)
        // and stay within a sane byte budget — the deferral's token savings
        // must not be undone by the response.
        let schemas: Vec<ToolSchema> = (0..15)
            .map(|i| fake_tool(&format!("tool{i:02}"), "browser").schema())
            .collect();
        let out = render_schemas(&schemas, false);
        assert!(
            out.len() < 2000,
            "browser-sized compact block must stay small, got {} bytes",
            out.len()
        );
    }

    #[tokio::test]
    async fn reveal_response_names_every_tool_and_its_required_params() {
        // The acceptance contract (backlog cc52264b): the reveal response
        // carries enough schema for the agent to pre-flight its calls —
        // every tool named, every required param named with its type —
        // alongside the existing join notice.
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(fake_tool("alpha", "browser")),
            Arc::new(fake_tool("beta", "browser")),
        ];
        let tool = LoadToolsTool::new(LoadedGroups::new(), static_table(), None, tools);
        let r = tool.execute(json!({"group": "browser"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output
                .contains("its tools appear in your tool list from your next"),
            "the join notice stays: {}",
            r.output
        );
        // Two tools <= FULL_SCHEMA_GROUP_MAX → full mode: the description
        // line, then the required params on their own indented line.
        assert!(r.output.contains("- alpha — Does a thing."), "{}", r.output);
        assert!(r.output.contains("- beta — Does a thing."), "{}", r.output);
        assert!(
            r.output.contains("  required: path: string, pattern: string"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn large_group_response_stays_compact_under_budget() {
        // Browser-sized (15 tools > FULL_SCHEMA_GROUP_MAX): the response
        // renders compact — no descriptions — and stays within a sane byte
        // budget so the deferral's token savings are not undone.
        let tools: Vec<Arc<dyn Tool>> = (0..15)
            .map(|i| Arc::new(fake_tool(&format!("tool{i:02}"), "browser")) as Arc<dyn Tool>)
            .collect();
        let tool = LoadToolsTool::new(LoadedGroups::new(), static_table(), None, tools);
        let r = tool.execute(json!({"group": "browser"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.len() < 2500,
            "browser-sized response must stay small, got {} bytes",
            r.output.len()
        );
        assert!(
            !r.output.contains("Does a thing"),
            "compact mode omits descriptions: {}",
            r.output
        );
        assert!(
            r.output.contains("- tool00 — required: path: string, pattern: string"),
            "{}",
            r.output
        );
    }

    /// A fake transport serving one tool for the "fs" server (same shape
    /// as the manager tests' fake).
    struct FakeClient;

    #[async_trait]
    impl crate::mcp::McpClient for FakeClient {
        async fn initialize(&mut self) -> crate::error::Result<crate::mcp::ServerCapabilities> {
            Ok(crate::mcp::ServerCapabilities::default())
        }
        async fn list_tools(&mut self) -> crate::error::Result<Vec<crate::mcp::McpToolInfo>> {
            Ok(vec![crate::mcp::McpToolInfo {
                name: "echo".into(),
                description: "d".into(),
                input_schema: json!({"type": "object"}),
            }])
        }
        async fn call_tool(
            &mut self,
            _name: &str,
            _args: serde_json::Value,
        ) -> crate::error::Result<crate::mcp::CallToolOutcome> {
            Ok(crate::mcp::CallToolOutcome {
                text: "ok".into(),
                is_error: false,
            })
        }
        async fn list_prompts(&mut self) -> crate::error::Result<Vec<crate::mcp::McpPromptInfo>> {
            Ok(Vec::new())
        }
        async fn get_prompt(
            &mut self,
            _name: &str,
            _args: serde_json::Value,
        ) -> crate::error::Result<String> {
            Ok("prompt text".into())
        }
        async fn list_resources(
            &mut self,
        ) -> crate::error::Result<Vec<crate::mcp::McpResourceInfo>> {
            Ok(Vec::new())
        }
        async fn read_resource(&mut self, _uri: &str) -> crate::error::Result<String> {
            Ok("resource text".into())
        }
    }

    #[tokio::test]
    async fn mcp_group_reveals_and_materializes_tools() {
        use crate::config::mcp::McpServerDef;
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
        let factory: crate::mcp::ClientFactory = Arc::new(|_def: &McpServerDef| {
            Ok(Box::new(FakeClient) as Box<dyn crate::mcp::McpClient>)
        });
        let manager = Arc::new(crate::mcp::McpManager::with_factory(&[def], factory));

        // A registry sharing the slot the reveal pushes into — the full
        // wiring the factory performs.
        let table = crate::tool::deferred_groups_with(&manager.servers());
        let registry = crate::tool::ToolRegistry::with_groups(LoadedGroups::new(), table.clone());
        let reveal = crate::mcp::McpReveal::new(Arc::clone(&manager), registry.dynamic_slot());
        let tool = LoadToolsTool::new(registry.loaded_groups(), table, Some(reveal), Vec::new());

        let r = tool.execute(json!({"group": "mcp.fs"})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("1 tool"), "{}", r.output);
        // The response renders the revealed tool's schema — the agent can
        // pre-flight its calls without re-calling load_tools.
        assert!(
            r.output.contains("- mcp__fs__echo — "),
            "the echo tool's schema line renders: {}",
            r.output
        );

        // The adapter landed in the registry's slot and is retrievable.
        assert!(
            registry.get("mcp__fs__echo").is_some(),
            "adapter materialized"
        );
        // And a retry reports already-loaded (idempotent).
        let r2 = tool.execute(json!({"group": "mcp.fs"})).await;
        assert!(r2.success);
        assert!(r2.output.contains("already loaded"));
    }

    #[tokio::test]
    async fn mcp_group_without_a_manager_errors_clearly() {
        let table = vec![("mcp.fs".to_string(), "f".to_string())];
        let tool = LoadToolsTool::new(LoadedGroups::new(), table, None, Vec::new());
        let r = tool.execute(json!({"group": "mcp.fs"})).await;
        assert!(!r.success);
        assert!(r.output.contains("no MCP manager"), "{}", r.output);
    }

    #[tokio::test]
    async fn mcp_group_failure_is_retryable() {
        use crate::config::mcp::McpServerDef;
        // A failing factory: the first reveal errors, the group is NOT
        // marked loaded, and a second attempt runs the factory again.
        let def = McpServerDef {
            name: "bad".into(),
            enabled: true,
            command: Some("definitely-not-a-real-binary".into()),
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
        let attempts = Arc::new(AtomicUsize::new(0));
        let fail = Arc::new(AtomicBool::new(true));
        let factory: crate::mcp::ClientFactory = {
            let attempts = Arc::clone(&attempts);
            let fail = Arc::clone(&fail);
            Arc::new(move |_def: &McpServerDef| {
                attempts.fetch_add(1, Ordering::SeqCst);
                if fail.load(Ordering::SeqCst) {
                    Err(crate::error::Error::Mcp(
                        "connect refused (scripted)".into(),
                    ))
                } else {
                    Ok(Box::new(FakeClient) as Box<dyn crate::mcp::McpClient>)
                }
            })
        };
        let manager = Arc::new(crate::mcp::McpManager::with_factory(&[def], factory));
        let table = crate::tool::deferred_groups_with(&manager.servers());
        let registry = crate::tool::ToolRegistry::with_groups(LoadedGroups::new(), table.clone());
        let reveal = crate::mcp::McpReveal::new(Arc::clone(&manager), registry.dynamic_slot());
        let tool = LoadToolsTool::new(registry.loaded_groups(), table, Some(reveal), Vec::new());

        let r1 = tool.execute(json!({"group": "mcp.bad"})).await;
        assert!(!r1.success, "{}", r1.output);
        assert!(r1.output.contains("connect refused"), "{}", r1.output);
        assert!(
            !registry.group_loaded("mcp.bad"),
            "failure did not mark loaded"
        );

        fail.store(false, Ordering::SeqCst);
        let r2 = tool.execute(json!({"group": "mcp.bad"})).await;
        assert!(r2.success, "{}", r2.output);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the retry re-ran the factory"
        );
    }
}
