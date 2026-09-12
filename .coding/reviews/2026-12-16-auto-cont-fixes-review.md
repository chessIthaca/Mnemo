## Verdict: FINDINGS (0 high, 0 medium, 2 low)

Review of the auto-continuation/context-safety remediation on the working tree (uncommitted changes vs `HEAD`). 7 files changed (+184/-41): the M1 preflight-wiring fix, the L1/L2/L3 doc + README fixes, the incidental `AgentLoopConfig` test-signature fix, **and a re-added M2 regression test** (`src/runtime/agent.rs` +127) that the task brief incorrectly described as "lost."

The substantive remediation is correct and complete: M1 is fully fixed on both context-manager construction sites, M2 is in fact addressed (the e2e test is present and sound), L3 is resolved, and the integration-test fix is field-correct. Only two LOW doc-accuracy residuals remain.

---

### M1 — preflight not wired through model_resolver cache paths — FIXED ✓

**`src/model_resolver.rs:246-289` (`build_turn_provider`):** The config read is now hoisted to the top of the method (253-258), and `preflight`/`headroom` are extracted once and applied via `.with_preflight(preflight, headroom)` on **both** paths:
- cache-hit (270-271): `ContextManager::new(max_context, fill_rate).with_preflight(preflight, headroom)` ✓
- cache-miss (282-283): identical ✓

Previously the cache-hit path returned a `ContextManager` with default preflight (enabled/32K) regardless of `[context]` config; now per-state model routing honors the knobs. Correct.

**`src-tauri/src/main.rs:1420-1427`:** the default provider's `ContextManager` now chains `.with_preflight(config.general.context.preflight_compact, config.general.context.compact_headroom_tokens)`. This is the second construction site (the fallback/default CM); both are now fixed. ✓

**Lock-hold concern (raised in the brief) — NOT a problem.** `set_config` (188-197) acquires `config.write()` and releases it at the end of that statement *before* acquiring `cache.write()` — it never holds both simultaneously. `build_turn_provider` holds `config.read()` across the cache lookup, but the lock ordering is consistent everywhere (config-before-cache; no path acquires cache then config), so there is no lock-ordering inversion / deadlock. The `config.read()` held across `build_provider_for` on the cache-miss path is **pre-existing** (the old code did the same); the only *new* hold is `config.read()` during the cache-hit HashMap `get`, which is trivially fast. No finding.

### M2 — zero auto-resume tests — ADDRESSED ✓ (brief's "Known gap" is stale)

The brief states the regression test "was lost in a working-tree corruption incident … only the test is absent." **This is no longer accurate.** The diff shows `src/runtime/agent.rs +127`: the test `prompt_resets_auto_continue_budget_after_exhaustion` + its `CountingMockProvider` mock harness are present and intact (memory record 71611004 confirms it was "SUCCESSFULLY re-added on 2026-12-16").

**Test correctness verified:**
- `CountingMockProvider` implements `LlmClient` faithfully — its `complete` signature matches the trait (`src/provider/mod.rs:401`) exactly, identical to the existing `MockProvider` at agent.rs:1023-1036. Each call returns `TextDelta("Done!")` + `Finish::Stop` (no tool calls), so every turn ends with a Stop finish reason.
- The workflow is placed in `Executing` via `create_plan` (one step, never completed) — so Stop + Executing + streak < `MAX_AUTO_CONTINUE` drives the auto-continue loop.
- Phase 1 waits for `MAX_AUTO_CONTINUE + 1` (=13) provider calls (initial + 12 auto-continues); Phase 2 asserts the agent parked (no 14th call in 300 ms); Phase 3 sends a fresh `Prompt` and asserts the streak reset (one more call). This is a sound e2e regression for the budget-exhaustion + prompt-reset behaviors.
- All required imports are present in the test module (agent.rs:986-1005): `ContextManager`, `AgentLoopConfig`, `SafetyMode`, `Workflow`, `ToolRegistry`, `Sandbox`, the provider types, `async_trait`, `BoxStream`, `tempdir`; `Arc`/`mpsc`/`AgentTask`/`AgentCommand`/`MAX_AUTO_CONTINUE`/`AgentLoop`/`Message` arrive via `use super::*`.

**M2 disposition:** acceptable to ship. The core budget-bound + reset-on-real-input behaviors are now covered. The test does *not* exercise the Interrupt-parks or Reviewing/Complete-state-parks branches (secondary auto-continue paths); those remain untested and are a reasonable optional follow-up, not a blocker.

### Incidental fix — `tests/workflow_integration.rs` AgentLoopConfig conversion — CORRECT ✓

Both `AgentLoop::new(...)` call sites converted from 9 positional args to `AgentLoopConfig { … }` struct-literal form with `Constitution::default()` as the second argument. Field names verified against the struct definition (`src/agent/loop_impl.rs:502-519`): `provider, tools, workflow, sandbox, safety_mode, context_manager, memory, vision` — all present and correctly typed.
- **Site 1 (185-197):** `workflow: workflow.clone()` is **necessary** — `workflow` is locked again at line 221 to assert state. Correct (a move here would be a borrow error).
- **Site 2 (270-282):** `workflow` is moved (not cloned) — correct, it is not referenced again in that scope (a fresh `workflow` is constructed at line 300). The move also avoids a would-be unused-clone warning under `#![deny(warnings)]`.

### L3 — README missing `[context]` knobs — FIXED ✓

`README.md:117` now documents `summarize_at_fill_rate`, `preflight_compact`, and `compact_headroom_tokens` (defaults on / 32 000) with the accurate "aggressive keep_recent=3 compaction once a session exceeds its remaining headroom" framing. `README.md:63` adds the aggressive-compaction sentence to the Key-features bullet. Accurate and complete.

### L1/L2 — doc comments overstated "pre-flight guard fires a forced compaction" — MOSTLY FIXED, 2 residuals (findings below)

The `preflight_compact` field docs in both `context.rs:62-66` and `general.rs:267-274` were correctly rewritten to "the existing compaction in `run_turn` uses `keep_recent=3` instead of the normal `6` when the pre-compaction token count exceeds `hard_ceiling`." Verified against the implementation (`turn.rs:424-430`): `keep_recent = if preflight_compact() && token_count > hard_ceiling() { 3 } else { 6 }` — exact match. The `compact_headroom_tokens` field doc in `context.rs:68-70` was also correctly updated.

Two adjacent docs were missed — see findings.

---

## Findings

### L1 (low) — `hard_ceiling()` doc implies an alternative trigger that doesn't exist
**`src/agent/context.rs:120-125`**
The rewritten doc reads: *"The token count above which compaction turns aggressive (`keep_recent=3`) before each provider call **instead of waiting for the normal fill-rate threshold**."*

The phrase "instead of waiting for the normal fill-rate threshold" is inaccurate. The actual implementation (`turn.rs:387`) gates the *entire* compaction block on `token_count >= summarize_at()` (the fill-rate threshold). The hard-ceiling check (`turn.rs:424-425`) only selects `keep_recent=3` vs `6` **within** that already-triggered block. Since `hard_ceiling()` (= `max_tokens − headroom`, e.g. 96 000) is *higher* than `summarize_at` (= `max_tokens × fill_rate`, e.g. 64 000), the hard ceiling is a higher threshold that tunes aggressiveness — not an alternative earlier trigger that fires "instead of" the fill-rate gate. The doc inverts the relationship.

**Fix:** drop "instead of waiting for the normal fill-rate threshold" and state that compaction remains fill-rate-gated, with the hard ceiling selecting the more aggressive `keep_recent` once *also* exceeded. Suggested:
> *"The token count above which an already-triggered compaction turns aggressive (`keep_recent=3` instead of `6`). Compaction itself is still gated by the fill-rate threshold (`summarize_at`); the hard ceiling only selects the more aggressive `keep_recent` once the token count also exceeds `max_tokens − headroom`. Equals `max_tokens − headroom`."*

### L2 (low) — `compact_headroom_tokens` field doc in `general.rs` still uses the old "pre-flight guard compacts" language
**`src/config/general.rs:276-281`**
The sibling `preflight_compact` field (267-274) was rewritten to the accurate "the existing compaction in `run_turn` uses `keep_recent=3`" wording, but this adjacent field was missed. It still reads: *"The pre-flight guard compacts when `prompt_tokens + headroom > max_tokens`."* — the exact overstated "pre-flight guard compacts" framing L2 targeted. The formula is algebraically correct (`prompt_tokens + headroom > max_tokens` ⟺ `prompt_tokens > hard_ceiling()`), but the wording implies a distinct guard that *triggers* compaction, which is the behavior the finding said to correct.

**Fix:** align with the rewritten `preflight_compact` wording, e.g. *"Tokens reserved for the model's output when selecting the aggressive compaction strength. When the pre-compaction token count exceeds `max_tokens − headroom`, compaction uses `keep_recent=3` instead of `6`. Default 32 000 — generous enough for a substantial response while leaving room for the system prompt + tools array the conversation-token count doesn't include."*

---

## Constitution checks

- **Documentation sync:** README updated (L3 ✓). Module docs in `context.rs`/`general.rs` mostly updated (L1/L2 largely resolved; the two residuals above are the only gaps). A repo-wide sweep for the old "forced compaction before the request is sent" / "pre-flight guard fires" language found no remaining hits in *source* — only in the prior review report and a historical plan file (both artifacts, not source docs). The `turn.rs:414` code comment ("Pre-flight hard-ceiling guard … use aggressive keep_recent=3 instead of the normal 6") is accurate and uses "guard" as a feature name, not an overstatement — not a finding. The `context.rs:110` method doc ("Whether the hard-ceiling pre-flight guard is enabled") likewise names the feature — acceptable.
- **Multi-platform neutrality:** All changes are cross-platform Rust + Markdown. No `cfg(windows)` additions, no Windows-only APIs, no platform-specific paths. The `src-tauri/src/main.rs` change is in shared brain-building code that runs on both macOS and Windows. ✓
- **Warning-free build:** `#![deny(warnings)]` at both crate roots. I verified compilation logically — imports present, `AgentLoopConfig` field names/types match, `LlmClient::complete` signature matches the trait, no unused variables (the `workflow.clone()` at site 1 is required; the move at site 2 avoids an unused-clone). I could not run `cargo test` myself (read-only reviewer), but found no warning sources; the reported green run (1757 tests, exit=0) is consistent with that.
- **Security:** No concerns. The config read path uses `std::sync::RwLock` with poison-expect (consistent with the rest of the codebase), no `unsafe`, no injection surface, no untrusted input handling. Lock handling is deadlock-free (verified above). The integration-test changes are test-only.

## Recommendation

Fix the two LOW doc findings (a few lines each in `context.rs` and `general.rs`), then commit. No blockers; M2 is already addressed.
