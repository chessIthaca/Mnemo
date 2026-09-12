+++
title = "Message/ToolCall construction via typed constructors — use constructors not struct literals"
created = "2026-12-20"
status = "superseded"
+++

DECISION: Message/ToolCall construction via typed constructors (plan ff17566f, merged on wt/agenticcoding commits d6a3995 + 6d66bd8).

After the thought_signature provider_meta field landed and required hand-updating ~110 struct-literal sites with `provider_meta: None`, 7 typed constructors were introduced in src/provider/mod.rs to centralize construction:

- `Message::text(role, content)` — dynamic-role text message
- `Message::user_text(content)` — user-role text message
- `Message::system(content)` — system-role text message
- `Message::assistant_text(content)` — assistant text message (no tool calls)
- `Message::assistant(content, tool_calls)` — assistant message with tool calls
- `Message::tool_result(tool_call_id, name, content)` — tool-result message
- `ToolCall::new(id, name, arguments)` — tool call with no provider metadata

Convention: ALL new Message/ToolCall construction MUST use these constructors, not struct literals. For sites that override one or two fields (reasoning_content, provider_meta, tool_calls), use struct-update syntax: `Message { reasoning_content: Some(..), ..Message::assistant_text(text) }` — overridden fields before the base, no trailing comma after `..base`.

Future field additions to Message (7 fields) or ToolCall (4 fields) touch only the constructors, not every call site. The 5 old local helpers (plain_msg, tool_msg, msg, user, user_message) were removed — their logic lives in the constructors now.

Note: `tool::ToolCall` (src/tool/mod.rs) is a DIFFERENT struct and was intentionally NOT migrated.
