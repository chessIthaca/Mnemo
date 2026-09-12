## Verdict: FINDINGS (0 high, 1 low)

Re-verification of the 6 findings from `.coding/reviews/2026-12-21-reasoning-state-continuity-raw-policy.md`, all fixed in commit 949fed2 on `wt/agenticcoding`. **All 6 fixes are correct and accurately documented; both regression tests exercise the fixed paths and fail without the fix.** One low-severity observation: `raw_is_usable` false-negatives on reasoning-only turns — benign, no data loss (see LOW-1).

### Verification method

Read-only review: examined commit 949fed2 via `git show`, read the changed source (openai.rs, anthropic.rs, turn.rs, mod.rs, stream.rs), and traced each fix path end-to-end. **Could not execute `cargo test`** (read-only reviewer, no shell); build/test execution must be confirmed by the main agent. Code is well-formed by inspection — no unused imports, no dead code (`raw_is_usable` is called at openai.rs:1341), `#![deny(warnings)]` in place at both crate roots.

### H1 — null-turn raw override: FIXED ✓

Three-layer defense, all correct:

1. **`raw_is_usable()` guard** (openai.rs:1797-1820): returns `false` when `!has_content && tool_calls.map(|t| t.is_empty()).unwrap_or(true)` — i.e., no `content` key AND no (or empty) `tool_calls`. This is exactly the null-turn shape (`{role:"assistant"}` with no content key), produced because `merge_scalar` skips null/empty-string content placeholders (stream.rs:224-225, 298-299). The builder falls through to field construction, emitting the "(no output)" placeholder. ✓
2. **Anthropic builder empty-array check** (anthropic.rs:389-402): when raw has a `content` key that is an empty array `[]`, falls through to field construction instead of echoing `[]` (which Anthropic rejects). ✓
3. **turn.rs packaging site** (turn.rs:1511): the null-turn path (`tool_calls.is_empty()` + `text.trim().is_empty()`) sets `raw: None`, so raw is already cleared before reaching the builder — defense-in-depth. ✓

**Regression test** `build_request_json_null_turn_uses_placeholder_not_empty_raw` (openai.rs:5199-5226): constructs `Message { raw: Some({"role":"assistant"}), ..Message::assistant_text("(no output)") }` and asserts `body["messages"][0]["content"] == "(no output)"`. Without the fix, the builder echoes raw → `{"role":"assistant"}` (no content key) → `.as_str().unwrap()` panics. **Fails without the fix, passes with it.** ✓

### H2 — malformed-args raw override: FIXED ✓

1. **`raw_is_usable()` guard** (openai.rs:1808-1818): validates that every tool-call's `function.arguments` parses as JSON (`serde_json::from_str::<Value>`). If any fails, returns `false` → falls through to field construction, which uses the sanitized structured `tool_calls`. ✓
2. **turn.rs packaging site** (turn.rs:1431): the sanitized-args path sets `raw: None`, so the malformed raw never reaches the builder. ✓

**Regression test** `build_request_json_malformed_args_fall_through_to_sanitized` (openai.rs:5229-5267): constructs a Message with raw containing `arguments: "{bad json"` and structured `tool_calls` with sanitized `"{}"`. Asserts `body["messages"][0]["tool_calls"][0]["function"]["arguments"] == "{}"`. Without the fix, the builder echoes raw → `"{bad json"` → assertion fails. **Fails without the fix, passes with it.** ✓

### `raw_is_usable` false-negative analysis (verification point 2)

Traced both example cases through the guard (openai.rs:1797-1820):

- **"content but no tool_calls"** (e.g. `{role:"assistant", content:"hello"}`): `has_content = true` → H1 check short-circuits (`!true && …` = false) → no tool_calls → H2 skipped → returns `true`. **No false negative.** ✓
- **"only reasoning_content, no content"** (e.g. `{role:"assistant", reasoning_content:"…"}`): `has_content = false`, `tool_calls = None` → H1 check: `!false && None.unwrap_or(true)` = `true` → **returns `false`**. This IS a false-negative — the guard treats a raw turn carrying valid `reasoning_content` as "unusable."

**The false-negative is benign — no data loss occurs in any path:**

1. **Fresh turns:** the turn loop's null-turn path (turn.rs:1511) clears `raw = None` whenever there is no text and no tool calls — so a reasoning-only turn never reaches the builder with raw intact; `raw_is_usable` is never invoked. The structured `reasoning_content` field is still populated (turn.rs:1505).
2. **Field construction re-adds reasoning_content** (openai.rs:1412-1414): even when `raw_is_usable` returns false (or raw is `None`), the builder's field-construction path unconditionally adds `reasoning_content` from `m.reasoning_content` on assistant messages. So `reasoning_content` always round-trips via the structured field regardless of the raw-echo path.

→ No `reasoning_content` is dropped in any path. The false-negative is a latent imprecision, not an active bug. See LOW-1.

### M1 — ProviderPolicy params inert: FIXED ✓

- **PLAN.md:198**: "A `ProviderPolicy` data object captures each provider's reasoning contract (which field, whether stateful, whether portable) and drives cross-vendor detection (`is_cross_vendor`); full builder consultation of `include_params`/`template_kwargs`/`reasoning_field` is a follow-up (the values are currently hardcoded correctly in the builders)."
- **README.md:58**: matching softened wording — "captures each provider's reasoning contract and drives cross-vendor detection (full builder consultation of include/template/reasoning-field params is a follow-up)."

Both accurately describe the current state: ProviderPolicy drives cross-vendor detection; builder consultation is a documented follow-up. No overclaiming. ✓

### M2 — provider_meta bridge retained: FIXED ✓

`provider_meta` doc comment (mod.rs:189-198): "**Legacy bridge (M2):** superseded by `Message::raw` (Rule 1 raw echo). No builder reads this field — it is set at packaging sites from `acc.take_message_provider_meta()` (which returns `None` since `capture_provider_meta` was removed) and on `ToolCall` from `tc.provider_meta.clone()` (also always `None`). Retained for serialized-conversation backward compatibility (old saves have the field); removal is a follow-up that requires a migration on load." Accurately documents the dead-at-runtime status and the migration-gated removal. ✓

### L1 — one-way strip not documented: FIXED ✓

`strip_cross_vendor_reasoning` doc (mod.rs:481-486): "**One-way (L1):** stripping is irreversible — once `reasoning_stripped` is set, the reasoning fields are gone from `raw` and cannot be restored, even if the user switches back to the original vendor. … The idempotent guard prevents double-processing but does not undo a prior strip." Accurately documents irreversibility. ✓

### L2 — Responses API response_id assumption: FIXED ✓

`message_to_responses_input` doc (openai.rs:1590-1597): "**Assumption (L2):** the stateful path assumes every assistant turn that precedes the current request received a `response_id` from the server (captured via `LlmEvent::ResponseId` from `response.created` / `response.completed`). If a turn lacks a `response_id` … `build_responses_request_json` cannot anchor on it and falls back to sending full input from that point — which is correct but loses the server-side state benefit for that turn." Accurately documents the assumption and the correct fallback. ✓

### LOW-1: `raw_is_usable` doesn't consider reasoning fields as "usable content"

**Severity:** Low (benign — no data loss in any current path; optional hardening)

**Description:** The `raw_is_usable` guard (openai.rs:1801-1806) defines "usable" as having a `content` key OR non-empty `tool_calls`. A raw turn carrying only reasoning fields (`reasoning_content`, `thought_signature`, `reasoning`) — with no `content` key and no `tool_calls` — is treated as "unusable" and falls through to field construction. Field construction re-adds `reasoning_content` (openai.rs:1412-1414) but does NOT re-add `thought_signature` or other unknown keys.

**Why it's benign today:**
- Fresh turns: the null-turn path (turn.rs:1511) clears raw before it reaches the builder, so the guard is never invoked for this shape.
- `reasoning_content` always round-trips via the structured field regardless of the raw-echo path.
- No current provider produces a turn with `thought_signature` but no content and no tool_calls (signatures arrive alongside content/tool_calls).

**Latent risk:** if a future change stops clearing raw on the null-turn path, or if a loaded (serialized) conversation carries a reasoning-only turn with `thought_signature` in raw, the guard would silently drop `thought_signature` (field construction doesn't re-add it). Tightening the guard to also treat reasoning fields as "usable content" (e.g. `has_content || has_reasoning_fields || has_tool_calls`) would close the gap. **No action required for correctness today** — the finding is recorded so the imprecision is visible if either defense-in-depth layer changes.

### Build & tests

Could not execute `cargo test` (read-only reviewer, no shell). By inspection: the changed code is well-formed, all symbols are used (`raw_is_usable` called at openai.rs:1341; both tests are `#[test]`-attributed), no unused imports or dead code introduced, `#![deny(warnings)]` is active at both crate roots (`src/lib.rs`, `src-tauri/src/main.rs`). The main agent should run `cargo test` to confirm green before finishing.
