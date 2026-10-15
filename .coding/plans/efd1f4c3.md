# Plan: Restore the warning-free clippy gate — 1.95 drift sweep

## Goal
Restore the warning-free clippy gate: `cargo clippy --workspace --all-targets -- -D warnings` exits 0 repo-wide again (currently 101, clippy 1.95 toolchain drift across untouched files), by fixing every lint at its root cause — NO `#[allow(...)]` anywhere — while keeping `cargo build --workspace` and `cargo test --workspace` green, committed in small per-lint/per-directory chunks on the working branch.

## Kind
implementation

## Context
FINDINGS (evidence-backed):

- Drift is pre-existing, not a regression: review `.coding/reviews/2026-10-11-job1-step1.1-host-move-review.md` LOW 2 (2026-10-11) — clippy 1.95 fires lints across files untouched by commit 322139e; the moved `done % every == 0` at src/host/maintenance.rs:112 is character-identical to its pre-move original in src-tauri/src/ipc/memory_maintenance.rs. 86+ error lines observed then.
- Known lint families from that observation (STALE — the tree moved on through jobs 2/3a/5, so step 1 must re-inventory): manual_is_multiple_of (src/memory/knowledge.rs:452; src/host/maintenance.rs:112), redundant_field_names (~164), doc_lazy_continuation / doc-list-indent (~26), collapsible_if (~11), map_or simplifications, needless_borrow/deref (~21), too_many_arguments, useless_vec (test fixtures).
- Verify command confirmed at .coding/specs/job1-host-extraction.md:227/515/640: `cargo clippy --workspace --all-targets -- -D warnings` — this is the 4th verify command of every remaining job-1 step file.
- Hard rules: NO `#[allow(...)]` anywhere — root-cause fixes only (agent.md Code style); `cargo test --workspace` (never root-only) before each step completes; commit at step boundaries on the wt/* working branch; Windows/PowerShell — check `$LASTEXITCODE` unpiped, never a piped exit code.
- Pre-existing uncommitted churn, NOT mine, must not ride my commits: src-tauri/src/ipc/embeddings.rs (~70 lines, an on_tauri_runtime test — belongs to the in-flight Muse session), .coding/backlog.jsonl, .coding/plans/3354cbc5.md, .coding/plans/89bbd2a9.md, untracked .coding/knowledge/bug/3354cbc5.md. Stage only clippy-touched files at each commit (git add <paths> + git commit, shell).
- Red/green: the clippy command itself is the failing test — it must exit 101 at step 1 (baseline red) and 0 at step 6 (green); cargo test --workspace stays green throughout.
- The authoritative per-step file lists live in .coding/tmp/clippy-1.95-inventory.md, written by step 1 — every later step reads its section from that file.

Step-5 batch 8 (too_many_arguments, the LAST family — 15 sites) — design (2027-01-20, all call-site counts verified by literal index search this session):

8a standalone fns (7 sites):
- memory/tests.rs:1983 `record` (11p): group the 7 numeric fields (p,c,r,cached,ttft,gen,at) into a local `StatsRow` struct; ~20 in-file callers become `record(&store, sid, model, endpoint, StatsRow { .. })`.
- zoom.rs:107 `analyze_with_zoom` (8p): group data_url/bytes/region/detail_level/max_rounds into a `ZoomRequest<'a>` struct; fn becomes (vision, req, tool, args) — 4p.
- config/patch.rs:30 `validate_endpoint` (11p): group all 11 into `EndpointDraft` (owned strings; body's borrow sites adjust); callers in the patch applier / config_io.
- workflow/mod.rs:652 `create_plan_with_kind_and_detail` (8p incl self): group title/goal/context/steps into `PlanDraft`; callers: the create_plan tool + the simple create_plan wrapper + tests.
- workflow/mod.rs:739 `update_plan` (8p incl self): group the 5 Option field updates (title/goal/context/regression_test/landed_design) into `PlanUpdate<'a>`; keep steps+append separate; callers: the update_plan tool + tests.
- factory.rs:473 `AgentLoopFactory::new` (10p): `FactoryDeps` struct (provider, constitution_source, memory, sandbox, project_root, safety_rules, safety_mode, context_manager, plans_dir, vision). Verified: 7 call sites via literal search `AgentLoopFactory::new(`: src-tauri/src/ipc/config_io.rs:666, src-tauri/src/ipc/events.rs:2269, src-tauri/src/console.rs:2126, src/host/brain.rs:1142, src/agent/factory.rs:2101+2146 (tests), tests/integration/workflow_integration.rs:392.
- host/runtime.rs:73 `spawn_agent_shared` — signature verified by direct read (host/runtime.rs:73-104): 13 params (factory, manager, agent_loops, name, initial_prompt, parent_id, model, role, own_plans_dir, worktree, permission, initial_messages, limits, host_tools). Fix: group the 11 request fields into a host-local `SpawnRequest` struct; keep the 3 shared handles → 4 params. Verified: 7 call sites via literal search `spawn_agent_shared(`: src/headless/engine.rs:301, src/host/runtime.rs:645+741, src-tauri/src/ipc/spawn.rs:42+175+279, src-tauri/src/main.rs:565. A `SpawnOptions` struct already exists at src/headless/types.rs:100-141 (verified by graph search) — headless public API; do NOT couple host/runtime to headless.

8b turn/dispatch plumbing (8 sites, methods on the same loop type in agent/turn.rs + agent/dispatch.rs):
- Recurring params: fanin_tx, agent_id, cmd_rx, session_id, provider (+ stop_signal, buffered in the two dispatch fns). Sites verified by gate output: dispatch_host_tool (dispatch.rs:778, 8p), dispatch_with_interrupt (dispatch.rs:878, 8p), execute_tool_batch (turn.rs:1990, 15p), maybe_compact (turn.rs:2771, 11p), request_stream (turn.rs:4138, 11p), consume_stream (turn.rs:4322, 11p), record_interrupted_output (turn.rs:4664, 9p), handle_bad_json (turn.rs:4816, 15p).
- Design direction (assumption to verify against run_turn's call sites before editing): ONE context struct for the per-turn plumbing — a `TurnCtx<'a>` {agent_id, session_id, fanin_tx, provider} + &mut {cmd_rx, stop_signal, buffered} — or folding values into the existing TurnState (turn.rs:168, verified by graph search). execute_tool_batch + handle_bad_json ALSO share the assistant-message group (text, assistant_reasoning, msg_meta) — an `AssistantTurn` bundle candidate. Threshold: ≤6 explicit params + self (verified: clippy 1.95 counts self — request_stream reported 11/7 with 10 explicit params).

Step-5 batch 8 (too_many_arguments, the LAST family — 15 sites) — design (2027-01-20, all call-site counts verified by literal index search this session):

8a standalone fns (7 sites):
- memory/tests.rs:1983 `record` (11p): group the 7 numeric fields (p,c,r,cached,ttft,gen,at) into a local `StatsRow` struct; ~20 in-file callers become `record(&store, sid, model, endpoint, StatsRow { .. })`.
- zoom.rs:107 `analyze_with_zoom` (8p): group data_url/bytes/region/detail_level/max_rounds into a `ZoomRequest<'a>` struct; fn becomes (vision, req, tool, args) — 4p.
- config/patch.rs:30 `validate_endpoint` (11p): group all 11 into `EndpointDraft` (owned strings; body's borrow sites adjust); callers in the patch applier / config_io.
- workflow/mod.rs:652 `create_plan_with_kind_and_detail` (8p incl self): group title/goal/context/steps into `PlanDraft`; callers: the create_plan tool + the simple create_plan wrapper + tests.
- workflow/mod.rs:739 `update_plan` (8p incl self): group the 5 Option field updates (title/goal/context/regression_test/landed_design) into `PlanUpdate<'a>`; keep steps+append separate; callers: the update_plan tool + tests.
- factory.rs:473 `AgentLoopFactory::new` (10p): `FactoryDeps` struct (provider, constitution_source, memory, sandbox, project_root, safety_rules, safety_mode, context_manager, plans_dir, vision). Verified: 7 call sites via literal search `AgentLoopFactory::new(`: src-tauri/src/ipc/config_io.rs:666, src-tauri/src/ipc/events.rs:2269, src-tauri/src/console.rs:2126, src/host/brain.rs:1142, src/agent/factory.rs:2101+2146 (tests), tests/integration/workflow_integration.rs:392.
- host/runtime.rs:73 `spawn_agent_shared` — signature verified by direct read (host/runtime.rs:73-104): 13 params (factory, manager, agent_loops, name, initial_prompt, parent_id, model, role, own_plans_dir, worktree, permission, initial_messages, limits, host_tools). Fix: group the 11 request fields into a host-local `SpawnRequest` struct; keep the 3 shared handles → 4 params. Verified: 7 call sites via literal search `spawn_agent_shared(`: src/headless/engine.rs:301, src/host/runtime.rs:645+741, src-tauri/src/ipc/spawn.rs:42+175+279, src-tauri/src/main.rs:565. A `SpawnOptions` struct already exists at src/headless/types.rs:100-141 (verified by graph search) — headless public API; do NOT couple host/runtime to headless.

8b turn/dispatch plumbing (8 sites, methods on the same loop type in agent/turn.rs + agent/dispatch.rs):
- Recurring params: fanin_tx, agent_id, cmd_rx, session_id, provider (+ stop_signal, buffered in the two dispatch fns). Sites verified by gate output: dispatch_host_tool (dispatch.rs:778, 8p), dispatch_with_interrupt (dispatch.rs:878, 8p), execute_tool_batch (turn.rs:1990, 15p), maybe_compact (turn.rs:2771, 11p), request_stream (turn.rs:4138, 11p), consume_stream (turn.rs:4322, 11p), record_interrupted_output (turn.rs:4664, 9p), handle_bad_json (turn.rs:4816, 15p).
- Design direction (assumption to verify against run_turn's call sites before editing): ONE context struct for the per-turn plumbing — a `TurnCtx<'a>` {agent_id, session_id, fanin_tx, provider} + &mut {cmd_rx, stop_signal, buffered} — or folding values into the existing TurnState (turn.rs:168, verified by graph search). execute_tool_batch + handle_bad_json ALSO share the assistant-message group (text, assistant_reasoning, msg_meta) — an `AssistantTurn` bundle candidate. Threshold: ≤6 explicit params + self (verified: clippy 1.95 counts self — request_stream reported 11/7 with 10 explicit params).

Step-5 batch 8 progress marker (2027-01-20, session end): parts committed — batch 8 part 1 (d66f4fa): record helper (StatsRow) + analyze_with_zoom (ZoomRequest) DONE, gate 15 -> 13, tests green. REMAINING 13 sites + designs:
(1) workflow/mod.rs pair — IN FLIGHT, ready to execute: PlanDraft<'a> { title: &'a str, goal: &'a str, context: &'a str, steps: Vec<String> } + PlanUpdate<'a> { title/goal/context: Option<&'a str>, steps: Option<Vec<String>>, append: bool, regression_test: Option<&'a str>, landed_design: Option<bool> } with #[derive(Default)]; signatures take the struct; FIRST body line is `let X { .. } = x;` destructure so bodies stay UNCHANGED (verified: create_plan_with_kind_and_detail body uses title/goal/context/steps at PlanFile::new (mod.rs:679); update_plan body uses all seven by name, 749-845). Structs go at module level (impl Workflow starts mod.rs:225; place both right before it). Callers read so far: create_plan_with_kind wrapper (mod.rs:640), tests 1745/1749/1769/1808-1817/1825, 2743-2751/2774/2792-2797; still to read: mod.rs tests 2825-3165 (one-liners + multi-line 2951/2981/3040/3059/3103), plan.rs:960 (create call), plan.rs:1229 (update call), plan.rs:2108 (one-liner), prompt.rs:1825 (create call).
(2) config/patch.rs:30 validate_endpoint (11p) — EndpointDraft { all 11 fields, owned strings, #[derive(Default)] } + destructure; 18 call sites: settings.rs:151, setup.rs:239, patch.rs:314 (ep helper) + 15 patch.rs tests (332-942).
(3) factory.rs:473 AgentLoopFactory::new (10p) — FactoryDeps struct; 7 call sites listed earlier.
(4) host/runtime.rs:73 spawn_agent_shared (13p) — host-local SpawnRequest struct (11 request fields; keep factory/manager/agent_loops); 7 call sites listed earlier.
(5) turn/dispatch 8 sites — TurnCtx-style plumbing struct design in the 8b section above.
Protocol reminder: gate + cargo test --workspace + commit per sub-chunk; never stage the Muse-session churn (src-tauri/src/ipc/embeddings.rs, .coding/plans/3354cbc5.md, .coding/plans/89bbd2a9.md, .coding/backlog.jsonl, untracked .coding/knowledge/*).

Step-5 batch 8 progress (2027-01-20, continued session): parts 1-4 committed — part 1 d66f4fa (record StatsRow + analyze_with_zoom ZoomRequest), part 2 3e90540? (workflow pair PlanDraft/PlanUpdate, 30 call sites), part 3 85d0793 (validate_endpoint EndpointDraft, 18 call sites), part 4 ceeeee2 (AgentLoopFactory::new FactoryDeps, 7 call sites). Gate: 15 -> 9 unique diagnostics, all too_many_arguments; every part gate-verified + cargo test --workspace green before commit.

REMAINING 9 sites (designs above in the 8b + spawn sections; verified current gate line numbers):
- dispatch.rs:778 dispatch_host_tool (8p incl self), 878 dispatch_with_interrupt (8p incl self)
- turn.rs:1990 execute_tool_batch (15p), 2771 maybe_compact (11p), 4138 request_stream (11p), 4322 consume_stream (11p), 4664 record_interrupted_output (9p), 4816 handle_bad_json (15p)
- host/runtime.rs:73 spawn_agent_shared (14p — 13 explicit + self; SpawnRequest grouping, 7 call sites: headless/engine.rs:301, host/runtime.rs:645+741, src-tauri/ipc/spawn.rs:42+175+279, src-tauri/main.rs:565)

Working recipe (verified 4× this batch): group the params into a struct with #[derive(Default)] where options allow; keep the body UNCHANGED via `let X { .. } = x;` destructure as the first body line; at cross-crate call sites use the full struct path (mnemo::agent::factory::FactoryDeps / crate::agent::factory::FactoryDeps) to avoid import-line hunts; commit per site/sub-chunk with gate + test verification. The turn/dispatch family needs run_turn's call-site reads first (see 8b section: TurnCtx-style plumbing struct OR fold into TurnState at turn.rs:168; execute_tool_batch + handle_bad_json also share the assistant-message group).

## Steps
- [x] 1. **Baseline + full lint inventory (no source edits)** — No source edits. Run `cargo clippy --workspace --all-targets -- -D warnings` (PowerShell, read $LASTEXITCODE unpiped) and capture the COMPLETE error list — confirm baseline red (exit 101). Also confirm baseline `cargo build --workspace` and `cargo test --workspace` are green before any edit. Write the grouped inventory (lint name × file:line) to .coding/tmp/clippy-1.95-inventory.md via file tools — this file is the authoritative worklist for steps 2–5. Cross-check counts against the stale 2026-10-11 list (known: src/memory/knowledge.rs:452, src/host/maintenance.rs:112).
- [x] 2. **Arithmetic + borrow/deref/map_or family** — Fix in src/host/maintenance.rs (line 112: `done % every == 0` → `done.is_multiple_of(every)`), src/memory/knowledge.rs (line 452, same pattern), plus every file listed under needless_borrow / needless_deref / map_or in .coding/tmp/clippy-1.95-inventory.md. No #[allow]. Verify: clippy re-run + `cargo test --workspace` green ($LASTEXITCODE unpiped). Commit only the touched files — never the pre-existing src-tauri/src/ipc/embeddings.rs churn.
- [x] 3. **redundant_field_names sweep (~164 sites)** — Fix `field: field` → field shorthand at every site in the files listed under redundant_field_names in .coding/tmp/clippy-1.95-inventory.md, chunked per directory (src/ files first, then src-tauri/src/ files) so each commit stays reviewable. After each chunk: clippy re-run + `cargo test --workspace` green, then commit that chunk's files.
- [x] 4. **Doc-comment family (~26 sites)** — Fix doc_lazy_continuation and un-indented doc-list items by re-indenting the continuation/list lines in the doc comments of the files listed under doc_lazy_continuation in .coding/tmp/clippy-1.95-inventory.md (known offenders under src/ and src-tauri/src/, ~26 sites) — no meaning changes, keep repo line-length style. Verify: clippy re-run + `cargo test --workspace` green. Commit the touched files.
- [x] 5. **Structural lints until the tree is clean** — Fix the collapsible_if (~11), useless_vec (test fixtures), and too_many_arguments sites listed in .coding/tmp/clippy-1.95-inventory.md. For any pub fn signature change (too_many_arguments → parameter structs): run graph_impact on the symbol first and fix every in-workspace call site (src/ + src-tauri/src/). No #[allow]. Per family: clippy re-run + `cargo test --workspace` green, commit; repeat for any straggler the inventory lists until the full clippy command is clean.
- [x] 6. **Final acceptance gate** — `cargo clippy --workspace --all-targets -- -D warnings` exits 0; `cargo build --workspace` green; `cargo test --workspace` green — each confirmed via unpiped $LASTEXITCODE. Then the closing sequence: pre-flight self-check (docs sync, no cfg(windows) additions, file-tools-first, no #[allow] anywhere), reviewer spawn (report lands under .coding/reviews/), fix findings, final commit including the review report.

## Reviews
1 a89592176ab9b193da39015bebd00ef53f157d95
2 2493572fe722069628e84445accfd1952cc2bb81

## Cost report

Totals: 338 request(s), 52093062 tokens in + 741247 tokens out = 52834309 tokens; 48451264 cached prompt tokens (a subset of tokens in).

Per lane:
- (no lane): 338 request(s), 52834309 tokens
