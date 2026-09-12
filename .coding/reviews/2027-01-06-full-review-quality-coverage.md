## Verdict: FINDINGS (3 high, 8 low)

Full-codebase maintainability + code-quality + test-coverage review of Mnemo at HEAD 9a0d5b9 (wt/agenticcoding), 2027-01-06. Method: tree walk of src/, src-tauri/src/, frontend/src/; systematic `#[cfg(test)]` / `#[allow(` / `let _ =` / unwrap-expect / TODO sweeps; vitest include-list cross-check against all 64 test files; targeted reads of every flagged site with exact line evidence.

One-line summary: the codebase is in strong shape (prior review's quality findings all landed, test registration airtight, docs exemplary) — the residual debt is one 2,120-line function at the agent loop's core, one verbatim IPC duplication, one dead parallel validator parked on a 9-month-old TODO, and a set of coverage gaps concentrated in the just-shipped effort feature and the big untested components.


## Prior-review follow-up (2026-12-30 quality review) — all landed

- **Provider SSE duplication** → fixed: `src/provider/sse_util.rs` (351 lines) now owns the shared helpers with its own tests; both providers delegate.
- **openai.rs 500-line `complete()`** → fixed: split into `src/provider/openai/{guard,request,sse,stream,tests}.rs`.
- **GLM stop-boundary constants** → fixed: config-driven via `stop_boundary_strings_for` (src/config/endpoints.rs:463) with tests.
- **git_ops doc comments** → verified present.
- **Source-contract anchors** → the run_all.rs contract tests now anchor on code strings (`"interrupted =="`, `"finish_captured_item_done("`, run_all.rs:820-827), not prose comments.
- **`#[allow(dead_code)]` on `extract_checkpoint_sha`** → committed with a spelled-out rationale + tests (residual noted as LOW finding 8 below).

## Positive observations (no finding)

1. **Frontend test registration is airtight.** All 64 test files under frontend/src are registered in vitest.config.ts's include list (cross-checked every directory listing against the include entries) — no stale entries pointing at deleted files, no test file silently skipped. The known "new files aren't auto-discovered" hazard has been consistently managed.
2. **Rust critical-path coverage is strong.** The prompt's named suspects are, in fact, covered: agent loop (src/agent/tests.rs, 303KB, plus per-module test mods in loop_impl.rs:1636, turn.rs:2247+, dispatch.rs:1018, factory.rs:1107, context.rs:882), tool implementations (37 of 38 src/tool files carry `#[cfg(test)]`; mod.rs:941 and steering.rs:274 included), memory consolidation (consolidation.rs:546), console parse layer (console.rs:1678, ~1,200 lines of tests), run_all (run_all.rs:295 + :866), workflow state machine (workflow/mod.rs:1087, plan_file.rs:563), config patch/DTO, provider SSE (sse_util.rs + openai/tests.rs, 130KB). The real gaps are narrower — see findings 2/4 and the coverage section.
3. **Doc comments: exemplary in the IPC layer.** state.rs, models.rs, mcp.rs, memory_debug.rs document every pub item with rationale (e.g. memory_debug.rs:31-35 explains `MAX_CONTENT_CHARS`; mcp.rs:5-15 explains the dual-scope model). The constitution's "doc comments on ALL public functions" rule is met in every module spot-checked.
4. **TODO hygiene:** exactly one real TODO in the tree (patch.rs:298 — finding 3); the safety_rules.rs:579-585 hits are test fixtures.
5. **Error-type layering is coherent:** `crate::error::Result` (lib) → `IpcError` with `From<String>` (IPC) → anyhow (bootstrap). No inconsistency found.
6. **`let _ =` discipline:** the ~100 occurrences are overwhelmingly the documented fire-and-forget pattern (fanin_tx event sends, best-effort temp cleanup) — the two exceptions that drop durable-state errors are finding 6.


## HIGH findings

### H1. `run_turn` — a 2,120-line function (the codebase's worst offender)

**Evidence:** src/agent/turn.rs:54-2174 — a single `pub async fn run_turn` (the next item, `repair_empty_assistant_messages`, starts at :2174). The file is 2,303 lines, so `run_turn` is 92% of it. Nesting reaches ~10 levels — e.g. the plan-completion memory write at :2048-2062 sits at 40-space indentation. The function mixes at least five phases: pending provider swap + pre-swap summarization (:62-93), the request/stream loop, tool-call batch execution with hard-stop "not run" synthesis (:2075-2077), workflow-state emission + plan-completion memory writes (:2048-2062), and stop-reason handling.

**Why it matters:** this is the agent loop's core — every future contributor edits this function. Deep nesting and phase mixing make regressions easy, review diffs hard to reason about, and the magic numbers at :79/:92 (finding 7) invisible. The codebase already demonstrated the fix pattern on openai.rs (500-line `complete()` → guard/request/sse/stream submodules).

**Fix direction:** decompose into named methods on the turn context — `handle_pending_swap`, `execute_tool_batch`, `synthesize_not_run_results`, `emit_workflow_state` — leaving `run_turn` as a ~100-line orchestrator. The existing 303KB test suite (src/agent/tests.rs) pins behavior, making the refactor safe to do incrementally.

### H2. `list_models` / `list_vision_models` — byte-identical command bodies + cross-file helper duplicate

**Evidence:** src-tauri/src/ipc/models.rs:73-105 vs :149-181 — the credential-resolution block (override → saved endpoint → kind-aware env fallback → `"dummy"`) is copy-pasted verbatim, 33 lines each; the delegation block :110-118 vs :186-194 is also verbatim (9 lines). Only the doc comments differ — the file itself admits it (:126: "Resolution is identical to [`list_models`]"). Additionally `kind_wire` (models.rs:22-28) duplicates `endpoint_kind_wire` (src-tauri/src/ipc/settings.rs:638-643) — the same EndpointKind→wire-string mapping under two names in two files.

**Why it matters:** drift risk on a user-facing path — a fix to one env-fallback chain (new env var, new kind, changed precedence) silently misses the other; the Vision tab's picker would keep the stale resolution. models.rs also has no test module, so the drift would be invisible.

**Fix direction:** extract `resolve_endpoint_credentials(state, endpoint_name, base_url, api_key, kind) -> Result<(String, String, String), IpcError>` plus a `fetch_by_kind(kind, base_url, key)` delegate; both commands become ~3-liners. Delete one of `kind_wire`/`endpoint_kind_wire` and import the survivor. Then add the missing tests (see coverage section).

### H3. Unwired parallel settings validator — `validate_settings_patch` is dead code kept alive by its own tests

**Evidence:** src/config/patch.rs:306-406 defines `validate_settings_patch`; the TODO(F2-full) at :298 says "this fn is NOT yet wired into the adapter's `save_settings`" and lists the B1-B5 reconciliation needed. The live save path is `validate_and_apply_settings_patch` (src/config/settings_dto.rs:258-624, called from src-tauri/src/ipc/settings.rs:822). The only callers of `validate_settings_patch` are its own tests (patch.rs:972-990) — pub-ness + tests keep it alive while it validates nothing real. The TODO references the 2026-04-18 review, so this staging has been parked ~9 months.

**Why it matters:** two parallel validation implementations drift — the TODO's own B1-B5 list documents known divergences (theme lowercase, `default_provider` ref check, ref trim checks). A contributor "fixing" validation in the dead copy ships nothing; a reader can't tell which validator is authoritative.

**Fix direction:** either finish the migration (apply B1-B5, make settings_dto delegate to the patch.rs validator, delete the inline copy) or delete `validate_settings_patch` + its tests. Do not keep both.


## LOW findings

### L1. Just-shipped effort mapping helpers have zero test references

**Evidence:** frontend/src/components/settings/types.ts:355-372 — `effortToSelectValue` and `effortFromSelectValue`. A search across all 64 registered test files finds no reference to either (nor to `REASONING_EFFORT`). The per-model effort selector (commit 86b1c21) tested its siblings — `capsAutofillPatch`, `modelConfigFor`, `upsertModelConfig` (types.test.ts:6-14) — but not the sentinel mapping.

**Why it matters:** the `null` ↔ `REASONING_EFFORT_DEFAULT` round-trip is exactly where a regression silently rewrites stored configs (e.g. returning `""` instead of `null` would persist the sentinel string into endpoints.toml; dropping the `"off"` passthrough would break off-mode).

**Test to add:** a `describe("effort select mapping")` block in types.test.ts — `effortToSelectValue(null | undefined | "" | "off" | "high")` and `effortFromSelectValue(REASONING_EFFORT_DEFAULT | "off" | "high")`, plus the round-trip property `effortFromSelectValue(effortToSelectValue(x)) === x` for the stored domain (`null`, `"off"`, `"low"`..`"high"`).

### L2. Significant frontend components with inline logic and no test

**Evidence:** Message.tsx (1,042 lines — `arePropsEqual` memo equality, tool-card branching), EndpointCard.tsx (925 lines — caps discovery, per-model effort rows), StatsView.tsx (19KB), MemoryDebugView.tsx (21KB), FileViewer.tsx (17KB), GitView.tsx (25KB), App.tsx (35KB) — none has a test file. The node-env vitest setup (vitest.config.ts:9) makes source-contract tests the pattern (BacklogView.test.ts style): they pin wiring strings, not behavior.

**Why it matters:** logic embedded in these components (memo equality in Message.tsx, dirty-state/caps logic in EndpointCard.tsx) is regression-blind; source-contract tests are brittle to renames and cannot catch logic regressions — only wiring removal.

**Fix direction:** continue the codebase's own extraction pattern (toolCardPaths / planSteps / traceStats: logic in lib/ or settings/types.ts + behavior tests alongside). Highest value: pull Message.tsx's memo-equality and tool-card branching into a pure helper; pull EndpointCard's caps/dirty logic into settings/types.ts. Keep source-contract tests only for wiring that can't be extracted. Consider adding knip or ts-prune to CI for dead-export detection (not verifiable read-only).

### L3. `agentState.ts` — extracted as "fully unit-testable" but never directly tested

**Evidence:** frontend/src/hooks/agentState.ts:1-485 — the header documents the Maint H3 extraction for unit-testability, but no `agentState.test.ts` exists; coverage is indirect via useAgentStore.test.ts's 2,192-line table-driven suite.

**Why it matters:** the module's contract (`flushStreamingText`, `pushTranscriptEntry`, transcript truncation edges) is only exercised through the store; a direct suite would pin the helpers' boundary cases (empty transcript, `MAX_TRANSCRIPT_ENTRIES` off-by-one) at a fraction of the store suite's setup cost.

**Fix direction:** add `agentState.test.ts` for the helper edges, or amend the header to state that coverage is deliberately via the store suite.

### L4. `main.rs` monolith — `main()` ~830 lines, `build_brain_inner` ~792 lines, 2 trivial tests

**Evidence:** src-tauri/src/main.rs:124-954 (`main`), :954-1746 (`build_brain_inner`). The only tests in the 91.7KB file are `adoptable_path_allows_spaces` / `adoptable_path_rejects_empty_and_control_characters` (:1746-1760).

**Why it matters:** the startup wiring (provider construction, memory store, codegraph, safety rules, fallback ordering) is untested and unextracted. `build_brain_inner`'s `app: Option<tauri::AppHandle>` parameter already shows the testable seam.

**Fix direction:** extract pure decision logic (runtime selection, fallback ordering — path adoption is already tested) into helpers with unit tests; keep `main()` as wiring only.


### L5. `save_endpoints` — a 459-line command

**Evidence:** src-tauri/src/ipc/settings.rs:179-638 (the next item, `endpoint_kind_wire`, starts at :638). It is organized with numbered section comments and delegates to lib-side patch fns, but the step-4 re-sync block is the bulk of the body.

**Why it matters:** long command bodies make the validate → persist → re-sync flow hard to follow; the re-sync (live-state mutation) is the riskiest part and deserves its own named, testable unit.

**Fix direction:** extract the re-sync step into a helper (e.g. `resync_runtime_state(&app, &state, &new_config)`), leaving the command as a four-step orchestrator.

### L6. Swallowed errors on durable writes (no log line)

**Evidence:** (a) src/agent/turn.rs:2061 — `let _ = store.write(memory).await;` inside a spawned task: a plan-completion semantic-memory write whose failure is silently dropped. (b) src/config/mod.rs:214-219 — `let _ = restore_from_backup(done);` during an already-failing multi-file commit: rollback failures leave a possibly half-renamed config set with no trace.

**Why it matters:** fire-and-forget is fine for UI events (the fanin_tx sends are a documented pattern), but these two drop durable-state errors with no log — a lost memory or a corrupted config dir becomes invisible.

**Fix direction:** log on `Err` in both (the codebase's eprintln!/tracing pattern); for (b), collect restore errors into the returned error message so the user sees the half-committed state.

### L7. Magic numbers inline in `run_turn`

**Evidence:** src/agent/turn.rs:79 — `(new_max as f64 * 0.8) as usize` (swap-summarize threshold); :92 — `summarize_with_interrupt(messages, 6, ...)` (keep-recent count on swap). Both unexplained inline; the 0.8 parallels but does not reference the ContextManager's fill-rate concept.

**Why it matters:** the next reader can't tell whether 0.8 / 6 are policy or accident; they duplicate concepts that exist elsewhere under other values.

**Fix direction:** named consts (`SWAP_SUMMARIZE_FILL`, `KEEP_RECENT_ON_SWAP`) with a one-line comment tying them to the fill-rate policy.

### L8. `#[allow(dead_code)]` residual on `extract_checkpoint_sha`

**Evidence:** src-tauri/src/ipc/run_all.rs:852 — `#[allow(dead_code)]` with a spelled-out rationale (manual resume/rollback anchor; no automatic caller since backlog 45dcf577 removed the rollback arm) and tests. The prior review flagged it uncommitted; it has since been committed with documentation.

**Why it matters:** the project constitution says "Never add `#[allow(...)]` to silence a warning — fix the root cause". This is now a documented, tested exception — but it is still a pub fn with zero production callers, kept alive by the allow, and the rule and the code contradict each other.

**Fix direction:** wire it to a debug IPC command (e.g. a run-all checkpoint status query) or move it into the test module; if it stays as a manual anchor, record the exception in the constitution.

## Test coverage — detail

**Rust gaps (named tests to add):**
- **ipc/models.rs — no test module.** After the H2 dedup, the extracted `resolve_endpoint_credentials` is pure-ish (a config + env). Tests: override-wins-over-saved, saved-endpoint fallback, kind-aware env fallback (anthropic vs openai), empty-base_url error, `"dummy"` terminal fallback.
- **ipc/mcp.rs — no test module.** The split-save + union-merge validation (project wins by name, mcp.rs:5-15) is pure logic over two `McpServerDef` sets; add tests for merge precedence and per-source splitting (verify what config::mcp already covers first).
- **ipc/state.rs — no test module.** `set_safety`'s re-evaluation of pending approvals is logic worth a test.
- **src-tauri/src/main.rs** — see L4.

**Frontend:** all 64 test files registered (verified — no stale entries, no silently-skipped files). Untested significant sources: App.tsx, Message.tsx, EndpointCard.tsx, StatsView.tsx, MemoryDebugView.tsx, FileViewer.tsx, GitView.tsx, most settings sections (Appearance/Embedding/Git/Memory/Models/Providers/Safety/Vision), agentState.ts, useBrowserOverlay.ts, useCountUp.ts, lib/tauri.ts (typed invoke wrappers — DTO drift is covered by ipc-contract.test.ts, acceptable), Sidebar/RightPanel/MergeToMainDialog/SafetyToggleDialog/ProjectPicker.

**Test quality:** the reducer/store suites (useAgentStore.test.ts, 2,192 lines, table-driven through the public dispatch path) and the lib suites pin behavior strongly — they would catch regressions. The source-contract suites (BacklogView.test.ts, Conversation.test.ts, InflightBar.test.ts) pin wiring strings — a documented tradeoff of the node env; they catch wiring removal but not logic regressions, and are brittle to renames. The extraction pattern (logic → lib + behavior test) is the codebase's own answer — apply it to the untested components above.


## Summary table

| Severity | Title | Location |
|---|---|---|
| HIGH | `run_turn`: 2,120-line function, ~10-level nesting (worst offender) | src/agent/turn.rs:54-2174 |
| HIGH | Byte-identical `list_models`/`list_vision_models` bodies + `kind_wire`/`endpoint_kind_wire` duplicate | src-tauri/src/ipc/models.rs:73-105, :149-181, :22; settings.rs:638 |
| HIGH | Unwired parallel validator `validate_settings_patch` — dead code on a 9-month-old TODO(F2-full) | src/config/patch.rs:306-406, :298 |
| LOW | Just-shipped effort mapping helpers (`effortToSelectValue`/`effortFromSelectValue`) have zero test references | frontend/src/components/settings/types.ts:355-372 |
| LOW | Significant untested components with inline logic (Message.tsx, EndpointCard.tsx, StatsView, MemoryDebugView, FileViewer, GitView, App.tsx) | frontend/src/components/** |
| LOW | `agentState.ts` extracted as "fully unit-testable" but never directly tested | frontend/src/hooks/agentState.ts:1-485 |
| LOW | `main.rs` monolith: `main()` ~830 ln, `build_brain_inner` ~792 ln, 2 trivial tests | src-tauri/src/main.rs:124-954, :954-1746 |
| LOW | `save_endpoints`: 459-line command | src-tauri/src/ipc/settings.rs:179-638 |
| LOW | Swallowed errors on durable writes (plan-completion memory write; config rollback) | src/agent/turn.rs:2061; src/config/mod.rs:214-219 |
| LOW | Magic numbers inline in `run_turn` (0.8 swap-fill threshold; keep-recent 6) | src/agent/turn.rs:79, :92 |
| LOW | `#[allow(dead_code)]` residual on `extract_checkpoint_sha` (constitution contradiction) | src-tauri/src/ipc/run_all.rs:852 |

**Overall:** the codebase's quality trajectory is strongly positive — every finding from the 2026-12-30 quality review landed, test registration is airtight, and doc coverage is exemplary. The three HIGH findings are all concentrated, mechanical refactors with existing in-repo patterns to follow (openai.rs decomposition for H1, sse_util extraction for H2, delete-or-wire for H3), and the coverage gaps have cheap, named tests to add (L1, models.rs, mcp.rs).
