+++
title = "Strict tool schemas + sanitized tool errors — code anchors (plan 21118961)"
created = "2027-01-11"
+++

Investigation 2027-01 (plan 21118961, branch wt/mnemo) — anchors for strict-schema + tool-error-sanitizer work:

1. STRICT MODE IS PLUMBED BUT NEVER SET. `ToolSchema` (src/provider/mod.rs:690) has `pub strict: Option<bool>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`. Both OpenAI builders already emit it correctly and only when `Some`: chat path src/provider/openai/request.rs:686-707, Responses path :934-952. But `ToolSchema::new` (:807-817) hardcodes `strict: None` and NO call site ever assigns it.

2. THE CAPABILITY GATE ALREADY EXISTS — do not add a new field. `Capabilities.supports_strict_schema: bool` (src/provider/mod.rs:48) is `true` for OpenAI (:68), `false` for Local (:81) and Anthropic (:101). It is already consulted for tool_choice at request.rs:760 (`self.caps.supports_tool_choice`), so the pattern is established. `ProviderKind::capabilities_with_overrides` (:1206-1213, callers: mod.rs only) does NOT accept it — extend it if per-endpoint override is needed. Existing test src/provider/client_factory.rs:487 already asserts the Anthropic client reports `!supports_strict_schema`.

3. RAW ERROR SITES — 44 occurrences of `format!("invalid arguments: {e}")` across 23 files (src/tool/agent/*, src/tool/memory/mod.rs, src/tool/workflow/plan.rs, src/tool/browser/mod.rs). Notable: src/tool/workflow/plan.rs:577/:983/:1228/:1769 (the plan tools), src/tool/agent/file_edit.rs:1307, file_write.rs:115, file_append.rs:81, convert_line_endings.rs:104. src/tool/agent/read_files.rs:95 already has a partial improvement (`invalid arguments: {e} (received keys: {keys}).{hint}`) — a hint mechanism to mirror. Dispatch has its own raw site: src/agent/dispatch.rs:189-200 emits `malformed arguments JSON: {e} (raw: {…})`.

4. Forgiving schemas that normalization must preserve: git (src/tool/agent/git.rs:457-505, only `subcommand` required, others interchangeable via resolve_git_subcommand/resolve_git_action), complete_step (int OR numeric string, dispatch.rs:325-332), file_edit (5 modes).

5. Prefix-cache constraint: the advertised tools array must stay byte-stable per plan (ToolFilter::PlanFrozen, src/tool/mod.rs:310-338; the 2027-01-11 6,016-token cache collapse). Normalize at the ONE place that builds the advertised array (ToolRegistry::schemas), never per-tool.
