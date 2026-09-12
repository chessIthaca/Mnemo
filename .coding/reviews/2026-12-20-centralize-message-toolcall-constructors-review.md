## Verdict: FINDINGS (0 high, 1 low)

The centralization refactoring is correct and complete for the 10 in-scope files: all 7 `Message` constructors and `ToolCall::new` set every field correctly with doc comments, struct-update syntax is valid at all ~15 override sites, the `name: None` vs `name: Some(...)` behavioral split is preserved exactly, the 5 removed helpers have zero remaining callers, and all import removals are safe. One low-severity gap: 5 `Message` struct literals in `tests/integration/` were not migrated, leaving the centralization goal ("across the codebase") incomplete for those sites.

---

### L1 — 5 unmigrated `Message` struct literals in `tests/integration/` (incomplete migration)

The plan goal states "migrate **all** ~90 Message … struct-literal sites **across the codebase**", but 5 full struct literals remain, each spelling out all 7 fields by hand:

- `tests/integration/provider_integration.rs:46-54` (`streams_text_completion`)
- `tests/integration/provider_integration.rs:81-89` (`accumulates_tool_call_deltas`)
- `tests/integration/provider_integration.rs:133-141` (`tool_choice_forces_specific_tool`)
- `tests/integration/workflow_integration.rs:203-211` (`run_turn` happy path)
- `tests/integration/workflow_integration.rs:288-296` (`run_turn` second site)

These are exactly the pattern the plan eliminates — they are the reason a future field addition to `Message` would force touching every call site. Leaving them unmigrated means a future field addition breaks compilation in 5 more places that the constructors were meant to protect. They are all `#[ignore]`'d / live-provider tests, so they compile and don't run in the default suite — hence low severity, not a correctness bug.

**Fix (trivial):** replace each with `Message::user_text("…")` (all five set `role: Role::User` with plain text and every other field at the constructor default, so the migration is behavior-preserving with no struct-update needed).

---

### Verification checklist (all pass)

**Constructor correctness** — Read `src/provider/mod.rs:181-263` and `:355-369`. All 6 `Message` constructors and `ToolCall::new` set every field (`role`, `content`, `tool_calls`, `tool_call_id`, `name`, `reasoning_content`, `provider_meta` for Message; `id`, `name`, `arguments`, `provider_meta` for ToolCall). `tool_result` correctly sets `tool_call_id: Some(...)` and `name: Some(...)`. Every public fn carries a doc comment (constitution requirement met).

**Struct-update syntax** — All ~15 override sites verified: overridden fields precede `..base`, no trailing comma after `..base`. Spot-checked `context.rs:855`, `turn.rs:1232/1401/1467/1489`, `anthropic.rs:1654/1680/1707`, `openai.rs:3062/3104/3143/3233/3263/4091/4138/4152`, `runtime/agent.rs:642`. Each preserves the original field values exactly (e.g. `Message { reasoning_content: X, provider_meta: Y, ..Message::assistant_text(t) }` reproduces the old 7-field literal).

**`name: None` vs `name: Some("")` behavioral split** — Correctly handled. The 3 `openai.rs` validate tests that need `name: None` on tool messages use `Message { tool_call_id: Some(..), ..Message::text(Role::Tool, ..) }` (base sets `name: None`), NOT `Message::tool_result` (which sets `name: Some(..)`). All `Message::tool_result` call sites (`context.rs` ×4, `turn.rs` ×4, `openai.rs` ×1) originally had `name: Some(..)` and preserve it. The `anthropic.rs` tool-message tests use `Message::text(Role::Tool, ..)` + field assignment, matching the old `msg(..)` helper's `name: None` — pre-existing test behavior, no regression.

**Removed helpers** — `search` for `plain_msg(`/`tool_msg(`/`user_message(` across `**/*.rs` and `msg(`/`user(` in `anthropic.rs` returns **no matches**: zero dangling callers. Build cannot fail on unresolved symbols.

**Import hygiene** — `MessageContent` removed from `agent/tests.rs` and `memory/consolidation.rs`: confirmed no remaining `MessageContent` usage in either file (search returns no hits). `Role` moved from production import to test module in `runtime/agent.rs:992`: confirmed `Role` appears only at lines 4035/4160/4578/4587, all inside the `#[cfg(test)]` module; `MessageContent` correctly retained in the production import (used at lines 577/587/640). No unused-import warnings possible.

**Remaining struct literals** — `search` for `Message {` finds only struct-update sites + the struct/impl definitions in `mod.rs` + the 5 integration-test literals above (L1). `search` for `ToolCall {` finds only struct-update sites (`turn.rs:1395`, `openai.rs:3264`) + definitions; `tool::ToolCall` (`src/tool/mod.rs`) is a different struct, correctly left unmigrated per plan.

**Test coverage** — 6 new unit tests (`mod.rs:768-827`) verify role, content, tool_calls, tool_call_id, name, reasoning_content, provider_meta defaults. `message_user_text_sets_defaults` and `message_tool_result_sets_id_and_name` assert all 7 fields; `system`/`assistant_text` assert the role-specific subset (acceptable — identical pattern, and the compiler enforces all fields in the constructor body). A future field addition breaks the constructors at compile time (struct-literal exhaustiveness), which is the intended centralization guarantee; call sites using `Message::user_text(..)` won't break.

**Constitution** — Pure refactoring, no docs to sync. No platform-specific code (multi-platform neutral). No security implications. Claimed `cargo test` = 1790 tests / 0 failures / zero warnings under `#![deny(warnings)]` is consistent with the verified absence of unused imports/dead code.
