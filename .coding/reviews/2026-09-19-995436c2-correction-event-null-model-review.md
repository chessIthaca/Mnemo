## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes on the working branch for bug_fixing plan 995436c2 ("Stop emitting the visible error event for the harness tool-call correction") plus its user-directed follow-on (backlog 3e6f7887, reviewer-spawn null-model artifact). Both fixes are correct, well-commented, and carry real regression tests that fail without the fix. No high-severity issues.

### What was verified

**Main fix (src/agent/turn.rs:1644-1667)** — correct.
- Context injection retained (`messages.push(Message::user_text(correction))` at :1657); only the `AgentEvent::Error` emission was removed; the replacement comment documents the rationale (red transcript box, activity-log row, doom-streak extension).
- No signal is lost: a repo-wide search finds no remaining consumer of the removed event (only plan/review docs and the inverted test reference it); the frontend has no special-casing of "tool-call correction". The correction still reaches the LLM, which is the functional payload.
- No dead-code risk: `fanin_tx`, `agent_id`, `tool_name` all remain in use elsewhere in the function (ToolResult send :1591, emit_workflow_state :1612, synthesize_not_run_results :1622) — no unused-variable warnings under `deny(warnings)`.
- Regression test `repeat_failure_injects_schema_correction_on_second_identical_error` (src/agent/tests.rs:1570-1574) is a true inversion: without the fix the emitted error event is received by the drain loop and the `assert!` fires; with it, no error event may carry the text. The message-content assertions (exactly one `[harness tool-call correction]` user message after the batch's tool results) are retained at :1558.

**Follow-on fix (src/agent/dispatch.rs:1409-1411, src/tool/agent/spawn_agent.rs:177-183, 246-261)** — correct and end-to-end consistent.
- Gate parity: the gate trims then compares (`m.trim() != "null"`), matching the tool's own semantics (`map(str::trim)` then `model_id != "null"` on the trimmed value) — " null " and "null" both count as absent at both sites.
- Caller wiring (dispatch.rs:405-406): `model` is extracted via `as_str()`, so JSON null was already `None`; the stringified "null" now passes the gate, and the call then proceeds into the tool's `execute`, which treats "null" as unset → the reviewer pin resolves the CONFIGURED reviewing model. No path where "null" leaks into model resolution.
- Schema `"type": ["string", "null"]` is valid JSON Schema and is recognized by `strict::is_nullable` (src/provider/strict.rs:198-216, the draft-07 array form) — so the hand-written form is exactly what `normalize_for_strict` would produce and idempotency holds if spawn_agent ever joins STRICT_TOOLS (it is not a member today, so `apply` is a no-op — fine either way).
- Regression tests: tool-level `null_model_string_counts_as_unset` asserts actual spawn success ("id 3") for both JSON null and string "null", and fails without the fix (the fixture wires no model resolver, so "null" previously hit "model selection unavailable"). Dispatch-level `dispatch_treats_null_model_as_absent_for_reviewer_spawns` pins both shapes through `execute_tool_call`; its negative-only assertion follows the established fixture pattern (the sibling test's sanctioned-retry phase asserts the same way, and the gate decision is the unit under test — the tool-level test covers end-to-end success).

**Constitution checks** — clean.
- Doc comments: `reviewer_spawn_gate` (private) and the spawn_agent trait impls carry thorough docs; the schema `model` description now says "omit or pass null".
- Warning-free: no unused variables/imports introduced by the diff (verified by inspection; claimed green `cargo test` 2447+16 under `deny(warnings)`).
- Multi-platform neutrality: nothing platform-specific in the change.
- File-tools-first: no shell-based mutation; all edits are file-tool-shaped.
- Documentation sync: no README.md / PLAN.md / docs mention the removed error event or the old non-nullable `model` param (repo-wide *.md search hits only .coding plans/reviews) — the in-code schema description is the doc surface and it IS updated. No stale docs.
- Bookkeeping: backlog.jsonl additions and the knowledge-spec supersede chain (`status = "superseded"` + successor `-landed-2.md`) are consistent side-car state.

### Findings

**L1 (low) — "null"-as-absent silently ignores a legitimately configured model named "null".** Both the gate and the tool's forced-model resolution now treat the literal string "null" as absent/unset, on the documented assumption "no configured model is ever named 'null'" (dispatch.rs:1407-1408, spawn_agent.rs:244-245). Nothing enforces that assumption: model ids come from user `endpoints.toml`, and a model id literally "null" would be silently dropped — a reviewer spawn would run on the configured reviewing model instead of the user's pick, and a normal spawn would silently fall back to the default subagent model. Pathological, but silent. Suggested hardening (any one): reject/warn on a configured model id of "null" at config load, or have the tool note "model 'null' ignored (reserved artifact sentinel)" in its output when it drops the value.

**L2 (low) — BUG: memory record not found despite plan step 2 checked.** Plan step 2 ("Document root cause — memory_write a BUG: record") is marked complete, but memory_search (semantic, record_type=bug, targeted queries on both the correction-event defect and the null-model defect) surfaces no BUG record for either; the newest matching bug records predate this session. The task accounting says BUG memory is auto-captured at finish, which may cover it — but since step 2 claims a manual write already happened, verify before finish that the record exists (or write/supersede it then); otherwise the checked step overstates what was done.

**L3 (low) — incident-count discrepancy in the new comments.** The new comments in dispatch.rs:1406, tests.rs:5370, and the plan context say "nine identical reviewer-spawn refusals", while backlog item 3e6f7887 (and the incident report) says 8. One of the two counts is wrong; trivial, but incident-reference comments should be accurate — align on one number.

### Notes (no action required)

- The dispatch-level null test does not assert the spawn succeeded (only gate non-denial); this matches the sibling `dispatch_denies_ad_hoc_reviewer_model_pick` fixture pattern, and the tool-level test pins actual success — acceptable as-is, but a positive assertion (`result.success` or sanction-consumption check) would make it stronger if the fixture ever changes.
- The inverted breaker test now exits its drain loop on the first 200ms-quiet recv — fine for a regression test (vacuous-pass is impossible: without the fix the event arrives and trips the assert).
