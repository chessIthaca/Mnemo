# Job 2a seam audit — every kernel→app coupling site (coordinator-direct enumeration)

Purpose: the per-site input for `.coding/specs/headless-free-agent/job-02a-agent-policy-seam.md`
(step file). Per master-spec §14.2 the coordinator enumerates directly when the glm plan pass
produces nothing (agent id 8, 2026-10-12: no file, no plan, no scratch — its session output is
lost). Method: code-graph + literal searches over the post-job-1 tree (branch `wt/mnemo`,
tip `ef65642`); every production site below was probe-verified this session (search/graph hit
with file:line); reads confirm context where noted. Spec line references are to
`.coding/specs/headless-free-agent.md`.

AgentPolicy method legend (master-spec :204-231): register_tools · stable_head · volatile_tail ·
tool_surface · gate_call · route · on_prompt · after_tool · on_turn_end · spend_tags ·
spawn_roles · on_session_end. "stays kernel" cites :247-258.

## 1. The stable-head const blocks (prompt.rs) — the KERNEL_PREAMBLE / KERNEL_RULES split

The head is assembled by `build_stable_head(constitution)` (prompt.rs :505-528, production):
push order (:-508-525): CODING_SYSTEM_PREAMBLE → WORKFLOW_LIFECYCLE → APP_RULES →
TOOL_CALL_DISCIPLINE → TOOL_STRATEGY → MEMORY_RECORDS → global constitution → project
constitution (`push_section` helper :483-488). The spec's :251-254 split targets:

| const | lines | KERNEL split (spec :251-254) |
|---|---|---|
| CODING_SYSTEM_PREAMBLE | :26-55 (doc :19-25; `CORE PRINCIPLES:` :30) | KERNEL_PREAMBLE = this minus the plan/complete_step lines (:31 `create_plan`, :33 `complete_step`) |
| WORKFLOW_LIFECYCLE | :67-98 ("part of the **stable head**" per its own doc :60-61) | stays CONST (whole block) |
| APP_RULES | :113-~200 (:116-117 never-commit-to-main; CLOSING SEQUENCE :129 onward) | KERNEL_RULES = this minus the closing-sequence block (:129…) and minus never-commit-to-main (:116-117) |
| TOOL_CALL_DISCIPLINE | :150-~200 (doc :154) | stays CONST |
| TOOL_STRATEGY | ~:200-260 (doc continues :196) | stays CONST |
| MEMORY_RECORDS | ~:260-320 | stays CONST |
| CONTEXT_FOOTER | :479 (`pub const`) | stays kernel (cache sentinel shared by policy+kernel) |

Note: `build_stable_head` takes the six consts; 2a maps it to
`policy.stable_head(constitution, blocks: KernelPromptBlocks)` where the app wrapper supplies
the app blocks. Existing head-content tests: prompt.rs :918-968 (stable-head rules region
comments; :1547 `tail.contains("closing sequence (APP RULES)")`), :1534
`reviewing_state_block_points_at_app_rules_without_duplicating_them`.

## 2. src/agent/prompt.rs (the head/tail builders)

| site (probe-verified) | what it does | mapping |
|---|---|---|
| :442-460 `append_plan_nudge` (pub(crate) fn, def :446) | the Complete-state plan nudge (WorkflowState::Complete check :451) | on_prompt |
| :550-554+ `build_volatile_tail(caps, workflow: &Workflow, memories)` | THE tail builder — takes `&Workflow` today (the seam point) | volatile_tail |
| :560-598 tail body | recalled-memories block + workflow section call | volatile_tail |
| :626-652 `format_recall_context` / `_entry` | memory-recall formatting | stays kernel (pure formatters) |
| :654-683 `format_recall_context_delta` | recall-delta lever (pure) | stays kernel |
| :697-709+ `format_primer` | the standing project-memory primer (rides the STABLE head) | app (app supplies it into stable_head) |
| :696-709 primer building | `# PROJECT MEMORY (standing context)` header assembly | volatile_tail/static — app | 
| :616-624 `short_context_line`; :600-607 trunc helper; :641-652 entry | pure helpers | stays kernel |
| :731-872 `workflow_section(workflow)` | per-state banner (STATE_* consts; arms Planning :739 / Executing :742 / Reviewing :798 / Complete :801 / Skill :804 / Subagent :829) | app |
| :446-460 (above) | — | on_prompt |
| tests | :1138 (Complete-state), :1204/:1218/:1231 + 2 more (:1204-1248 nudge family), :1372 (head content), :1534-1560 (Reviewing block), :1864-1899 (subagent-state test helpers) | must stay green (:265) |

## 3. src/agent/turn.rs (the turn loop)

| site | what it does | mapping |
|---|---|---|
| :553-561 | schema_filter consult (the tools array built per state via the workflow's allowed tools — comments verified) | tool_surface (advertise) |
| :1200 | `ModelContext::new(...)` for the turn's provider resolution | route |
| :1811-1956 `emit_workflow_state` | emits WorkflowStateChanged (wire event, kernel shape) + :1902 the auto-continue trigger read (`wf_state == WorkflowState::Complete && result.success`) | on_turn_end (the EVENT stays kernel; the gate moves to policy) |
| :2667-2669 | `resolve_turn_provider_routed(wf.state(), skill_name, plan_kind, Some(target))` / `resolve_turn_provider(wf.state(), ...)` | route |
| :3512 | `prompt::append_plan_nudge(...)` call | on_prompt |

## 4. src/agent/dispatch.rs (the tool gate)

| site | what it does | mapping |
|---|---|---|
| :253 `let filter = wf.allowed_tools();` | THE single dispatch gate read (verified: exact line via search) | tool_surface().gate; call-site allow/veto → gate_call |

## 5. src/agent/loop_impl.rs (the loop + resolution)

| site | what it does | mapping |
|---|---|---|
| :1507-1516 `resolve_turn_provider(workflow_state, skill_name, plan_kind)` | provider resolution entry → delegates :1515 | route → RouteKey |
| :1518-1548 `resolve_turn_provider_routed(...)` | the full chain; :1547 skill-only `ModelContext::new` | route |
| :1630 / :1723 / :1747 | the other three production `ModelContext::new` sites (pinned/forced paths) | route |
| :1943-1948 `set_plan_mutations_allowed(allowed: bool)` | pub setter on AgentLoop ("Used by the IPC spawner… main agent drives the authoritative plan"); called kernel-side by factory/host stamping | stays kernel (gate storage); callers map to spawn_roles |
| :2386-2388 `workflow_handle()` | the accessor returning `Arc<Mutex<Workflow>>` (the door the app helpers use) | stays kernel as accessor; its APP callers rewire |
| :2400-2426 `workflow_expects_progress(unattended)` (:2410-2416, reads state ∪ plan + `unattended && state == Planning`) | the auto-continue gate's state read | stays kernel as helper; the GATE calling it is app (runtime/agent.rs :338) |
| :2423-2426 `workflow_state_label` | the six-block label for cache diagnostics | stays kernel (diagnostic) |
| :1548 | WorkflowState::Skill read inside the routed resolver | route |

## 6. src/runtime/agent.rs (auto-continue / unattended — app-owned per :195-196)

| site | what it does | mapping |
|---|---|---|
| :58 `auto_continue_streak: u32`; :59-75 `unattended: bool` (+doc); :87 `MAX_AUTO_CONTINUE = 12`; :92 `CONSOLIDATE_EVERY_N_TURNS = 8` | the gate's storage + consts | after_tool/on_turn_end state (app) |
| :328-338 | the gate: reads `workflow_expects_progress(self.unattended)` :338 | on_turn_end |
| :343-357 | streak + MAX check; consolidation cadence | on_turn_end |
| :365-367 | the synthetic continue note (unattended wording :367) | on_turn_end |
| :795-805 (Prompt arm) | :795 streak reset; :798-805 re-derivation :805 `self.unattended = text.starts_with(UNATTENDED_PROMPT_PREFIX);` | on_prompt |
| :899 | another arm's streak reset (verify exact fn at execution) | on_prompt (secondary) |
| :1324-1331 | buffered-during-compaction re-derivation (:1330 reset, :1331 re-derive) | on_prompt (arm 2) |
| :291 / :404 | streak snapshots into Stat/wire | stays kernel (wire) |
| tests | :6634, :6733 (budget resets), :6861 (unattended_planning_auto_continues), :7048 (plain_prompt_clears_unattended), :7291 (buffered_prompt_during_compaction_rederives_unattended), :6904-7196 chains | must stay green (:265) |

## 7. src/runtime/channels.rs + turn_resolve.rs (wire/labels — stays kernel)

| site | what it does | mapping |
|---|---|---|
| channels.rs :24 `use crate::workflow::WorkflowState;` | import | stays kernel |
| channels.rs :262-263 + :592-593 `WorkflowStateChanged { state: WorkflowState }`; serde :843-844 | wire event variants (+ round-trip) | stays kernel (:233-236) — app OWNS the emit decision |
| channels.rs :128-139 `UNATTENDED_PROMPT_PREFIX` | the dispatch marker const | kern today; the semANTIC consumer (on_prompt) is app — 2b may relocate |
| channels.rs :488 / :712 / :975-981 | auto_continue_streak in wire structs (Signal/Stat) | stays kernel |
| turn_resolve.rs :28 use; :240 `workflow_transition_counts(prev, new)` | WorkflowState as opaque label | stays kernel (:22) |

## 8. src/model_resolver.rs (routing — RouteKey per :241-244)

| site | what it does | mapping |
|---|---|---|
| :68-84 `ModelContext` struct + :86-101 impl | the routing tuple (state, skill, subagent, plan kind) | RouteKey source (route) |
| production resolve slot-chain | the resolver + endpoint chain (def region < :680) | route |
| tests :680-1409 | the routing lattice family incl. the all-states table :847-851 | must stay green; 2a.0's golden table test mirrors :914 |

## 9. src/agent/factory.rs (registration — register_tools)

| site | what it does | mapping |
|---|---|---|
| :73 + :83-86 | imports of app tools (write_review_report, ask_user, plan::{CreatePlanTool,…}, skill::{…}) | (imports) |
| :963 build_inner → :983-987 `Workflow::new(plans_dir)` + `wf.load_latest()` | the production Workflow construction + plan-stack mirror | stays kernel (the instance the policy wraps) |
| :991 `build_registry(&workflow, …)` | registration entry | (entry) |
| :1280-1306 | core+app: write_review_report :1294-1298; list_models :1303-1305 | register_tools |
| :1325-1428 `register_workflow_tools` | plan tools (create :1332-1336, complete+verify handle :1337-1339+, abandon :1358, finish :1365-1384, current_plan :1392-1395), ask_user :1399, backlog trio :1407-1428 | register_tools (app tool registrations) |
| :1431-1439+ | skill tools fn (skill_start/end/abandon/reload/create) | register_tools |
| :1676-1698 `build_write_review_report_tool` | single construction point (provenance wiring) | stays kernel helper |
| tests :1873-3569 | state/allow-list expectations (:1877-1898 asserts, :3558 load_latest mirror) | must stay green |

## 10. src/host/runtime.rs (post-job-1 spawn path — spawn_roles per :196-197)

| site | what it does | mapping |
|---|---|---|
| :151/:154 `set_plan_mutations_allowed(false)` | subagent denial stamp | spawn_roles |
| :163 `enter_subagent_state()` | subagent state stamp | spawn_roles |
| :182 `workflow_handle()` | handles for stamping | spawn_roles |
| :189 `set_reviewer_allowlist(...)` / :191 `set_tool_allowlist(...)` | allow-list install | spawn_roles |
| :374 parent's `wf.allowed_tools()` read (inside compute_subagent_allowlist) | the parent permission snapshot the allow-list intersects | spawn_roles (reads via tool_surface().gate) |

## 11. Golden-test inventory (master-spec :913-914; step 2a.0 adds them FIRST per :1048-1052)

- `workflow_stable_head_bytes_unchanged` (prompt::tests) — ABSENT (search: no matches).
- `workflow_policy_routing_table_unchanged` (model_resolver::tests) — ABSENT.
- The existing characterization family to extend (auto-continue / unattended / nudge /
  dispatch, all must stay unchanged + green per :265): prompt.rs :1138, :1204-1248, :1534;
  model_resolver.rs :680-1409 (esp. the all-states table :847-851); runtime/agent.rs :6634,
  :6733, :6861, :7048, :7291 (+ chains to :7196); factory.rs :1873-3569.

## 12. DRIFT (spec-vs-tree differences the step file must carry)

1. Master-spec line numbers for the auto-continue region (:195-196 "runtime/agent.rs
   ~:58-410, :795-805, :1330") remain ACCURATE against the current tree (fields :58-75, gate
   :328-367, arms :795-805/:1324-1331) — no drift.
2. The golden tests (:913-914) DO NOT exist yet — they are 2a.0's deliverable, not existing
   regressions (the spec's acceptance rows read as if present).
3. The spec :197 places "reviewer role allow-lists" among APP modules; after job 1 that code
   lives in the kernel file src/host/runtime.rs (compute_subagent_allowlist, sets the flags at
   :189/:191). 2a treats it as a spawn_roles (-app-owned logic in a kernel file) the step file's
   2b-move can relocate; 2a only rewires the reads.
4. The six-block head list + assembly discovered at prompt.rs :505-528 — the spec's :251-254
   describes the SPLIT semantically (which lines leave), not the const names; the step file must
   name CODING_SYSTEM_PREAMBLE (:26-55), APP_RULES :113-~200, and the KERNEL_PREAMBLE line
   set (:31, :33) and KERNEL_RULES removal set (:116-117, :129-end) explicitly.
5. TOOL_STRATEGY/MEMORY_RECORDS exact const bounds were not line-verified this session (their
   push order at :512-513 is verified) — the step file marks their bounds `DRIFT:` verified-at-execution.
6. `build_volatile_tail` taking `&Workflow` (:550-553) is THE central seam site — the 2a table
   replaces the `&Workflow` param with the policy's per-state derivation.
7. The `:899` and `:291/:404` arm contexts were not read this session — step file marks them
   `DRIFT:` verify at execution (streak reset arms / Stat snapshots).

## 13. App-module adjacency (today's locations, for 2b)

`Workflow`/state machine (src/workflow/mod.rs :26-78, :145-1322), workflow tools
(src/tool/workflow/{plan,backlog,skill}.rs, review? — finish/complete_step live with the tools),
backlog (src/backlog.rs), skills (src/skill/), memory (src/memory/), the event/forwarder layer
(src/runtime/event.rs? — verify), and the composite UI-free app module list at spec :191-198.
2b moves app code under `mnemo::app`; this audit's stays-kernel rows are the boundary line.
