## Verdict: PASS

Round-2 verification of plan b277ebfc ("MCP servers v1") on `wt/agenticcoder`, against the three round-1 LOW findings (`.coding/reviews/2026-02-13-mcp-servers-v1-review.md`). All three fixes are present, correct, and comment-accurate; the stated regression tests exist and pin the fixed behavior; no new issues found in the fix delta. Read-only review — suites not re-run (main agent's post-fix runs: cargo 1597/0/1, npm 670/50, both builds green; counts consistent: 1595+2 new backend tests, 669+1 new frontend test).

### Fix 1 (LOW 1 — `McpManager::call` panic race) — VERIFIED

`src/mcp/mod.rs` `call()` (323–343) no longer uses `get_mut().expect(...)`. It loops `for _ in 0..2`: `ensure()` → re-acquire the connections lock → on `None` (connection removed in the gap by a concurrent failed call or `replace_servers`) it `continue`s and re-ensures once; after two vanished attempts it returns a clear `Error::Mcp` naming the server and the race ("another agent's failed call or a Settings save is racing it; retry in a moment"). Doc comment (317–322) accurately describes the race and cites review LOW 1.

**Retry semantics checked as instructed:** a genuine `call_tool` failure still removes the connection (`conns.remove(name)`) and surfaces the error on the FIRST attempt — the `Err(e)` arm returns immediately; the loop's `continue` is reachable only from the vanished-connection branch, never from a tool failure. A second-iteration `ensure()` failure (e.g. server removed wholesale by `replace_servers`) propagates via `?` — no retry of connect failures, no panic, no busy-loop. `args` is moved only on the returning path, so the move-in-loop is borrow-checker-legal (green build confirms).

New stress test `concurrent_calls_survive_connection_removal` (446–477): `multi_thread`/4 workers, 24 spawned tasks with interleaved fail/success flags (`i % 3`), every panic propagates through `h.await.unwrap()`, and a clean call succeeds after the dust settles. Matches the claimed shape.

### Fix 2 (LOW 2 — mcp reveal usable-in-state) — VERIFIED

New `pub fn mcp_reveal_allowed(filter, group)` in `src/tool/agent/load_tools.rs` (72–81): non-mcp groups pass through; `mcp.*` groups are gated by `filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, "mcp__probe__probe")` — byte-identical to the probe `hidden_groups_for` uses (`src/tool/mod.rs:722–727`), so the dispatch gate and the index's visibility can never drift. Doc comment (63–71) accurate.

Dispatch wiring in `src/agent/dispatch.rs` (117–128): placed **inside** the workflow lock block (`{ let wf = self.workflow.lock().await; ... }`, lines 99–129), **immediately after** the `filter.allows` re-check (ends 111) and **before** the plan-mutations gate (138) and the approval gate (246) — exactly the required order, so a Planning/research/reviewer `load_tools(group="mcp.*")` is denied with a state-naming error (`wf.state()`) before any connection can spawn. Reads `parsed_call.arguments` — the initial `args` value was moved into `ParsedToolCall` at line 75, so the fix correctly avoids the use-after-move. A repo-wide search confirms `load_tools` is the only tool with a `group` argument, so the gate cannot misfire on another tool.

Test `mcp_reveal_gate_matches_the_tool_filter` (233–250) pins the exact matrix, and each expectation matches the `ToolFilter::allows` arms verified in `src/tool/mod.rs` (Planning/Complete: `Agent => safety == AutoRun || name == "spawn_agent"` → denied; ExecutingResearch: `!name.starts_with("mcp__")` → denied; Reviewer: strict allow-list → denied; Executing/Reviewing: `Agent => true` → allowed; browser/image pass in Planning/Complete).

### Fix 3 (LOW 3 — server-name laxness) — VERIFIED

Backend `McpServerDef::validate` (`src/config/mcp.rs:94–110`) now rejects any whitespace in `name` (unrevealable `mcp.<name>` group) and any `__` (mcp__ alias collisions), with the rationale in comments and the check order (blank-after-trim → whitespace → `__` → transports) sane. Existing `rejects_empty_and_duplicate_names` extended (342–351) with `"fs "` → "whitespace" and `"a__b"` → "__".

Frontend `validateMcpDraft` (`frontend/src/components/settings/types.ts:586–594`) mirrors both checks against the RAW `s.name` (`/\s/.test(s.name)`, `s.name.includes("__")`); trim is used only for the empty check and duplicate keys — since whitespace is rejected before duplicates are compared, backend (raw) vs frontend (trimmed) duplicate detection can never diverge. New test `validateMcpDraft keeps names revealable and collision-free` (`mcpSection.test.ts:75–80`) pins both rejections.

### No new issues

- `call()` retry bounded at 2, no new deadlock (locks never held across the `call_tool` await — the mutex guard drops at the `return` expression's lock scope end), no `#[allow(...)]` added, no platform-specific code (pure tokio/std + TS).
- Minor observation, not a finding: the stress test is probabilistic (the fake's `ensure()` is instantaneous, so the vanish-in-the-gap interleaving is not guaranteed every run) — inherent to concurrency stress tests; the panic-propagation assertions make it a meaningful regression guard regardless.
