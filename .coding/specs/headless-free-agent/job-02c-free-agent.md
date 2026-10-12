# Job 2c — the free agent (NoPolicy's real semantics; the G1–G4 kernel mechanics)

Master-spec anchors: §5.2 :270-271 ("2c: `NoPolicy` (the free agent). Kernel head/tail, the constant
surface, `["free"]` routing, the `[models.free]` slot."), the G-gap table :602-607 (G1/G2/G3/G4),
§5.9.1 (:615-632, PromptSpec), §5.9.2 (:634-652, ToolSelection), §5.9.3 (:654-669, Workspace),
§5.9.4 (:671-682, no process globals), §5.4 (:356-378, permissions), the §6 2c file row :762, §7
rows 5/6/7/8/11 and 25/26/27/28, §8 (the verify list), §14.4 ("the coordinator writes the step files
for jobs 1 and 2c–3b directly").

Status: NOT STARTED at authoring (2027-01-14, wt/mnemo, tree HEAD 5fe33b8 = post-2b.3 PASS).
This file is the 2c step file; every anchor below is verified fresh this session. SCOPE READING
(the G-row mapping, recorded here as the coordinator's final pick): §6's 2c file row carries
"— G1-G4", so job 2c lands the KERNEL MECHANISMS of G1–G4 — the kernel-side types, the behavior
flips, and kernel-level tests. The G acceptance rows (§7 :859-872) name the ENGINE-flavored
harness (`Engine`/`EngineOptions`/`Engine::start`); that harness re-verifies on the real Engine
in job 3a and again at job 5's final proof. Job 2c's contract = the mechanics + the kernel-side
tests; any drift from this reading is a documented deviation.

## A. The verified census (read fresh 2027-01-14 on wt/mnemo)

### A1. src/agent/policy.rs (372 total lines)

- :19-37 `ToolSurface { advertise: Option<ToolFilter>, gate: Option<ToolFilter> }` +
  `ToolSurface::constant()` (both `None`) — already the free agent's surface (2a).
- :43-49 `GateVerdict { Allow, Block { reason } }`; :55-63 `PromptHook { unattended:
  Option<bool>, tail_rewrite: Option<String> }`; :69-74 `Continuation { Stop, Continue { note } }`;
  :79-93 `TurnSummary { success, stop_reason, unattended }`; :97-100 `SpendTags`; :104-106
  `RoleSpec { role }`; :113-116 `RouteKey { slots: Vec<String> }`.
- :118-138 `PolicyBuildCtx { agent_id, project_root, plans_dir, root, sandbox, step_verify,
  workflow }`; :140-153 `PolicyFactoryCtx { agent_id, project_root, plans_dir, workflow }`.
- :155-165 `PolicyBuildResult { policy: Arc<dyn AgentPolicy>, attach_loop }` — `build_policy` is
  INFALLIBLE today (:191: `fn build_policy(&self, ctx: &PolicyFactoryCtx) -> PolicyBuildResult`).
- :175-198 `AgentPolicyFactory { create_workflow(plans_dir, is_subagent) :181-185, build_policy
  :191, register_tools :197 }`.
- :200-223 `NoPolicyFactory`: `create_workflow → UnwiredWorkflow` (:207-213); `build_policy → {
  Arc::new(NoPolicy), no-op attach }` (:215-220); `register_tools {}` (:222). Doc :200-203:
  "Used when no app factory is wired (tests that never needed one)"
- :225-248 `KernelPromptBlocks { preamble, lifecycle, rules, discipline, strategy,
  memory_records }` + `::workflow() → prompt::kernel_prompt_blocks()`.
- :250-295 the `AgentPolicy` trait: register_tools :254, stable_head :259, volatile_tail :263,
  tool_surface :266, gate_call :270, route :274, on_prompt :278, after_tool :281, on_turn_end
  :284, spend_tags :287, spawn_roles :290, on_session_end :294.
- :297-335 `NoPolicy` (the 2a stub): register_tools {}; stable_head → `assemble_stable_head(k, c)`
  (:304-306) — FLIPS to the PromptSpec composition in 2c.1; volatile_tail → "" (:307-309);
  tool_surface → constant (:310-312); gate_call → Allow; route → `RouteKey { slots: ["free"] }`
  (:316-320); on_prompt → default; after_tool → no-op; on_turn_end → Stop; spend_tags → default;
  spawn_roles → []; on_session_end → no-op.
- :337+ `assemble_stable_head` — the byte-faithful assembler (the 2a.0 golden's body). STAYS for
  the workflow path (the byte-identical contract).

### A2. src/agent/prompt.rs (1931 total lines)

- The consts: CODING_SYSTEM_PREAMBLE :26-55; WORKFLOW_LIFECYCLE :67; APP_RULES :113;
  TOOL_CALL_DISCIPLINE :161; TOOL_STRATEGY :223; MEMORY_RECORDS :271; PLAN_NUDGE_TEXT :305;
  CONTEXT_FOOTER :418 (pub).
- :26-28 the IDENTITY — compiled bytes: "You are a coding agent operating under a plan-first
  workflow. You help the user build software by reading files, writing code, running commands, and
  managing a plan." (the :27-28 `\`-line continuation collapses to one space).
- :435-437 `build_stable_head(constitution)` = `assemble_stable_head(&KernelPromptBlocks::workflow(),
  constitution)` (the workflow back-compat wrapper).
- :444-453 `kernel_prompt_blocks()` → the six-const set.
- :476-510 `build_volatile_tail(caps, app_tail, memories)`: the recall block :483-490; the app
  tail :493; CAPABILITIES :497-507 (kernel-owned).

### A3. src/agent/dispatch.rs (2398 total lines)

- :441-457 the symbol-search redirect gate (reads `SteeringStats::shared()` :451).
- :469-491 the file_edit / multi_edit stale-read gates (shared() :472/:486).
- :494-564 THE APPROVAL BRANCH (the spec's §5.4 anchor ":483-520" drifted to :494-564):
  :494 the live mode read (`*self.safety_mode.read()`); :503 `let force_prompt =
  tool.never_auto_for(&parsed_call.arguments)`; :510-517 `if force_prompt ||
  approval::needs_approval(...)`; :523-541 the safety-rules auto-approve shortcut; :547 the
  approval preview; :551-563 the `AgentEvent::ApprovalRequest` emission (with the oneshot
  responder).
- :731 `let steering = SteeringStats::shared()` (the dispatch path's observer site).
- Tests :1603-2198 build `SteeringStats::new()` directly (the unit fixtures).

### A4. src/runtime/channels.rs

- `AgentEvent` enum :181; `ApprovalRequest` :218; `WorkflowStateChanged` :263.
- `SerializableAgentEvent` :561 (ApprovalRequest) / :593-601 (WorkflowStateChanged + the 2b.3
  mirrors); the forward arms :783 (ApprovalRequest) / :846 (WorkflowStateChanged); tests :1144
  and :1158 (the ApprovalRequest serialization pattern).
- IPC note :11-14: ApprovalRequest's oneshot → the serializable twin forwarded to the frontend.

### A5. src/agent/factory.rs (3447+ lines; the registration + build map)

- :107 the brain/loop builder; :339/:346/:357 the spawner/skills/graph omit switches.
- :946-966 the loop build: `build_registry(&workflow, agent_id, root, &plans_dir)` :948; the
  loop assembly ending `tools: Arc::new(registry)` :966.
- :1133 `build_registry(...)`; the REGISTRATION ORDER :1157-1194 (MUST stay byte-identical):
  :1157 the registry; :1159 `register_agent_tools`; :1166 `register_policy_factory_tools` (the
  policy's OWN registrations — the app's D5 channel); :1167 vision; :1168 memory; :1169
  codegraph; :1178 spawn; :1180 browser; :1186 the McpReveal wiring; :1191-1194 the deferred
  snapshot + register.
- :1218 `register_agent_tools` (the census at 2c.2): the search family :1278; web :1280;
  GitTool :1281; GitReadTool :1291; write_review_report :1296 (always registered,
  dispatch-gated via review_scope — NOT a group member in 2c); list_models :1302.
- :1326-1349 register_policy_factory_tools (routes to `pf.register_tools(registry, &ctx)`);
  :1362-1397 the browser tools (10 offscreen + 6 on-screen); :1418-1444 the image tools;
  :1449-1522 the memory tools; the Laya tool-choice steering comment :1270 (verify the
  load_tools tool's registration + group pick at 2c.2).
- The live setters :159/:191/:221/:427/:447/:565/:576/:659/:671/:684/:696/:707/:763 (the
  tool-selection setter lands as a sibling of these).

### A6. src/agent/steering_stats.rs (1334+ lines)

- `SteeringStats` :422-424; impl :426-746 (incl. `snapshot` :692-712); `Default` :748-752.
- :450 `static SHARED: OnceLock<Arc<SteeringStats>>` + the shared()/setter right after (read
  :450-470 exactly at 2c.4 before deleting).
- ALL ::shared() consumers: src/agent/dispatch.rs :451/:472/:486/:731;
  src-tauri/src/ipc/trace.rs :130 (get_steering_stats → snapshot()). Tests use ::new()
  (steering_stats.rs :825+; dispatch.rs :1603+).
- The GUI mirror: frontend/src/lib/tauri.ts :2238 getSteeringStats + types.ts :625
  SteeringStatsSnapshot (the IPC shape must NOT change — the reroute is lib-internal).

### A7. project/ + instance_marker + the kernel's .coding joins

- src/project/agent_md.rs: `Constitution` :17, `ConstitutionSource` :41, `ConstitutionHolder`
  :81+ (the per-turn load — the G1 constitution-mode filter point).
- src/project/mod.rs (the 2c.3 split's subject): the §5.9.3 app list (`.coding` discovery, the
  projects registry, `pending_project`, `Project::init` + scaffold, the instance marker) —
  full census at 2c.3 execution before splitting.
- src/instance_marker.rs :1-147 (kernel-side; imports crate::project::Project :24; uses
  Project::CODING_DIR_NAME :50/:61; the app caller: the startup aliveness predicate — probe
  src-tauri/src/startup.rs at 2c.3).
- The kernel's `.coding` joins (→ `state_dir` at 2c.3): src/agent/factory.rs
  :103/:143/:319/:826/:864/:867/:885/:898/:1203-1212/:1615; src/agent/loop_impl.rs
  :179-181 (safety.toml) / :391 (the plans-dir doc); the kernel test fixtures (factory.rs
  :3055/:3213/:3447 keep tempdir roots — state_dir = root/.coding by default, unchanged).

### A8. [models.free] — the 2a.6 remainder

- LANDED: src/config/general.rs :855-861 (`pub free: Option<ModelRef>` + the default arms)
  + src/model_resolver.rs :346 (the walk's `"free" => models.free`) + :791-851
  `free_slot_resolves_configured_or_falls_back_to_default` (set/unset/dangling pinned; the
  unset ride = the default, blessed by the 2a.6 review round-1 LOW — the :792 comment cites it).
- REMAINING: src/config/settings_dto.rs gains the `free` slot (today's routing DTO: :64
  planning / :66 executing / :73 reviewing — NO free) + the apply arms (:638-641 reads /
  :927-937 the patch write) + the fixture mirrors (:1315-1358 pattern).
- Row 7's wording ("unset → [models.executing]") vs the landed "unset → the default ride":
  2c.6 verifies the row's CURRENT §7 text against the walked chain and DOCUMENTS the deviation
  (no behavior change — the review blessed it).

### A9. NOT 2c (recorded to keep the scope honest)

- Engine/EngineOptions/SpawnOptions/Guardrails/SessionLimits/history/resume/snapshot — jobs
  3a/3b (§5.3, §5.9.5-.7).
- HostToolSpec / Engine::register_tool / the host surface — 3c (§5.9.2's host bits; the Host
  group remains enum-only in 2c).
- EngineConfig vs UiConfig split (G9) — job 4.
- Rows 27/28's engine-form harness — 3a; job 2c lands the kernel mechanics + kernel tests.## B. Design decisions (the final picks; deviations from the spec's letter documented)

- **D1 (G1 composition — byte-isolation).** The workflow path stays BYTE-IDENTICAL: KernelPromptBlocks::workflow() + CODING_SYSTEM_PREAMBLE + WORKFLOW_LIFECYCLE + APP_RULES are TOUCHED
  NOWHERE; WorkflowPolicy::stable_head keeps delegating to assemble_stable_head
  (src/app/workflow_policy.rs :125-126). The G1 library serves ONLY the NoPolicy composition:
  prompt.rs gains `pub const CODING_IDENTITY` (the :26-28 sentence verbatim), the block enum
  { CoreCodingPrinciples, ToolCallDiscipline, ToolStrategy, MemoryRecords, FileToolRules }
  (§5.9.1 :624's exact names, serde rename_all = "snake_case"), and the block-text map:
  CoreCodingPrinciples → a NEW kernel-safe const (tool-agnostic principles — NO plan-first /
  create_plan / complete_step / WORKFLOW-LIFECYCLE text; row 6's grep is the gate: [verify at
  execution] whether TOOL_CALL_DISCIPLINE/TOOL_STRATEGY/MEMORY_RECORDS contain no plan/workflow
  text — if any does, the free selection drops it or a kernel-safe copy is carved; the pick is
  recorded in the commit), ToolCallDiscipline → TOOL_CALL_DISCIPLINE, ToolStrategy →
  TOOL_STRATEGY, MemoryRecords → MEMORY_RECORDS, FileToolRules → a NEW small const (the
  read-before-write + exact-match + multi_edit + line-endings + file-tools-first discipline,
  kernel-worded, no workflow text).
- **D2 (NoPolicy's spec + the missing-spec error).** PromptSpec { identity: String, blocks:
  Vec<PromptBlock>, extra_stable: Option<String>, constitution: ConstitutionMode } +
  ConstitutionMode { None, Global, Project, Both } + PromptSpec::coding_default() (§5.9.1
  :617-632 verbatim shapes). PolicyFactoryCtx gains `pub prompt: Option<PromptSpec>`.
  `AgentPolicyFactory::build_policy` becomes `-> Result<PolicyBuildResult, KernelError>` with
  KernelError::PromptRequired + `code() -> "prompt_required"` (the spec's :630 code). NoPolicy
  gains `pub spec: Arc<PromptSpec>`; NoPolicy::stable_head composes { identity + selected blocks
  in the FIXED enum order + extra_stable + the constitution per its mode } via the assembler's
  push_section semantics (empty-skip + raw bytes + "\n\n"); NoPolicyFactory: None →
  Err(PromptRequired), Some(s) → the spec-carrying NoPolicy. The constitution-mode filter rides
  a new default trait method `constitution_mode(&self) -> ConstitutionMode` (default Both —
  the workflow path unchanged); the kernel's per-turn constitution load site (the
  ConstitutionHolder caller) drops the excluded source [verify the exact load site + the
  Constitution/ConstitutionSource mechanics at execution]. The kernel test helpers that build
  NoPolicy agents pass PromptSpec::coding_default() (the compile errors enumerate the fixtures).
- **D3 (ToolSelection, §5.9.2).** The verbatim type lives in src/agent/factory.rs —
  ToolGroup { Read, Write, Shell, Git, Web, Browser, Codegraph, Memory, Images, Spawn, AskUser,
  Mcp, Host } (serde rename_all = "snake_case") + ToolSelection { include: Vec<ToolGroup>,
  exclude: Vec<String> } + ::all(). The factory's params gain `tool_selection:
  Option<ToolSelection>` (None = full, today's exact behavior) + a live setter beside the
  existing ones. THE ARRAY GOLDEN FIRST: pin the full set's tool-name array (registration
  order) BEFORE gating — [verify at execution] whether a test already pins it (extend, not
  duplicate). Then build_registry gates per group: Read = the search family + read_files +
  expand_result; Write = file_edit/multi_edit/file_write/convert_line_endings (+file_append
  [verify: separate tool or the file_write mode]); Shell = shell; Git = GitTool + GitReadTool
  AND ONLY when the root is a repo (D4's require_git flip); Web = web_fetch; Browser = the
  browser set (:1362-1397); Codegraph = the graph_* tools; Memory = the memory tools
  (:1449-1522); Images = the image tools (:1418-1444); Spawn = spawn_agent + list_models;
  Mcp = the McpReveal wiring (:1182-1186). AskUser/Host: kernel-side none today (ask_user is
  app-registered post-2b) — the app's own registrations stay UNGATED in 2c (the policy factory
  path is today-identical); the app group-honoring lands with the host surface in 3c. The
  kernel-side AskUser/Host groups stay enum-only (documented).
- **D4 (Workspace + the project split + the marker move, §5.9.3).** NEW kernel module
  src/workspace.rs: `pub struct Workspace { pub root: PathBuf, pub state_dir: PathBuf,
  pub require_git: bool }` + Workspace::new(root) (state_dir = root/.coding) + the git
  detection (.git exists, file or dir — worktree .git files count). [DEVIATION, documented: the
  spec's §6 row bundles Workspace in factory.rs; a dedicated module keeps the 3400-line factory
  from carrying the state-dir policy — the consumption still lands exactly where the row
  points.] The project split: the app-halves of src/project/mod.rs (discovery/registry/
  pending/init+scaffold per §5.9.3 :664-666) → src/app/project.rs; the kernel keeps
  src/project/agent_md.rs (Constitution). The kernel's `.coding` joins (A7's list) derive from
  the state_dir the factory/brain now carries (state_dir defaults to root/.coding — behavior
  identical for the app); [verify at execution] each derivation's exact wiring (factory.rs's
  plans-dir derivations, loop_impl.rs :179-181 safety.toml, the brain's memory/codegraph db
  paths, the trace dir, the read cache). require_git default false: with no repo at root the
  GIT GROUP IS NOT REGISTERED (row 27's kernel form) and the kernel never writes .gitignore
  (the scaffold is app-side [verify]). instance_marker.rs → src-tauri/src/instance_marker.rs
  in the SAME step (it imports crate::project::Project; after the split Project is app-side —
  the kernel module must not dangle; its app callers keep the same API).
- **D5 (steering de-globalization, §5.9.4).** The brain/factory gains `pub steering:
  Arc<SteeringStats>` (built at construction); the loop build threads it; the dispatch funnel's
  struct gains the field (probe the funnel struct + its construction at execution); the FOUR
  ::shared() reads (dispatch.rs :451/:472/:486/:731) flip to the injected handle (the free fns
  already take &SteeringStats); SHARED :450 + shared()/the setter DELETED — any surviving
  ::shared() reference is a compile error (the intended tripwire). src-tauri/src/ipc/trace.rs
  :130 flips to the factory's steering (the IpcState's factory handle — probe the field; the
  IPC shape + the frontend mirror unchanged). The two-factories disjoint-stats test (row 28's
  kernel form). The 3a Engine reroute noted (the spec :672-674's engine-level wiring).
- **D6 (the permission model's mechanism, §5.4).** [Re-read §5.4 :356-378 verbatim at 2c.5
  execution — the variant/serde names below are this census's capture and must match the spec
  byte-for-byte before coding.] The kernel type (policy.rs): PermissionPolicy { #[default]
  OverridePermissions, ApproveViaApi(SafetyMode) } — serde tag = "mode", the default
  OverridePermissions. The dispatcher's struct gains `pub permission: PermissionPolicy`;
  the factory sets ApproveViaApi at build (the GUI/console path stays byte-identical: the
  live safety-mode RwLock read :494 REMAINS the ApproveViaApi behavior). Under
  OverridePermissions the ENTIRE approval branch is skipped (needs_approval, force_prompt
  prompting, the safety-rules shortcut), each call that WOULD have been gated emits
  AgentEvent::ApprovalBypassed { tool, reason } first — reason "core_op" when force_prompt
  (the predicate is still evaluated, silently), else "safety_mode" — and the call dispatches
  directly. Subagents inherit the parent's policy (the spawn param assembly copies it — probe
  the spawn path at execution).
- **D7 (ApprovalBypassed, channels.rs).** AgentEvent::ApprovalBypassed { tool: String, reason:
  String } beside ApprovalRequest (:218); the SerializableAgentEvent twin + the forward arms
  (the :783/:846 patterns) + the test mirror (:1144 pattern). Additive variant — the app-event
  contract fixtures stay green (the existing names unchanged).
- **D8 (settings_dto free).** The `free` slot mirrors planning/executing/reviewing (:64-120)
  + the apply arms + the fixtures. NO walk change (A8's row-7 verdict).
- **D9 (the split rule).** Each 2c.x step ≤ ~400 changed lines; if a step's landed diff
  crosses it, that G-item splits into its own sub-plan BEFORE the close's review — the step
  file records the pick, the review runs delta-scoped.

## C. Steps (in order; each = a boundary commit --no-verify; the goldens + the battery gate each step's complete)

- **2c.1 (G1)** — the prompt library + NoPolicy's real head: prompt.rs + policy.rs per D1/D2;
  the kernel test helpers updated; tests no_policy_requires_identity,
  prompt_spec_blocks_compose_in_fixed_order, kernel_head_has_no_workflow_text + the
  composition-drift guard (CODING_IDENTITY is CODING_SYSTEM_PREAMBLE's prefix); the workflow
  byte-golden green. Probe first: TOOL_CALL_DISCIPLINE/TOOL_STRATEGY/MEMORY_RECORDS's content
  (the row-6 grep), the build_policy call sites, the constitution load site.
- **2c.2 (G2)** — ToolSelection + the factory gating per D3: the array-bytes golden FIRST;
  the group map + the gating; the McpReveal/list_models/spawn gating; tests
  tool_selection_registers_only_groups (row 26's kernel form: include [Read]) +
  register_spawn_group_gates_list_models_and_spawn + the array stays byte-identical.
- **2c.3 (G3)** — src/workspace.rs + the project split + the state_dir rewiring + the
  Git-group skip + instance_marker.rs → src-tauri per D4; tests: a non-git temp-dir build
  (starts; no git tools; no .gitignore written by the kernel) + state_dir-elsewhere (the
  derived files land under state_dir).
- **2c.4 (G4)** — the steering de-globalization + the trace reroute per D5; tests: the
  two-factories disjoint-stats test + the src-tauri IPC shape unchanged.
- **2c.5 (the permissions)** — PermissionPolicy + the dispatch gate + ApprovalBypassed + the
  serialization + the subagent inheritance per D6/D7 (after re-reading §5.4 :356-378 verbatim);
  tests: the override branch + the two reasons + the serialization + the inheritance.
- **2c.6 ([models.free])** — the settings_dto free slot + the row-7 wording verification per
  D8; tests: the settings round-trip fixture.
- **2c.7 (the close)** — the acceptance checklist (rows 5/6/8/11 + the kernel-side 25/26),
  the FULL verify battery (the master's :14 list + clippy -D warnings), the boundary commit,
  the review rounds to PASS, finish, then complete_step on the master.

## D. Acceptance (the row coverage)

- Row 5 (phase 2c): a NoPolicy (free) agent advertises no plan/backlog/skill/review tool; a
  hallucinated call to create_plan/complete_step/finish/start_skill is rejected at dispatch
  (the unknown-tool path; [probe] the exact rejection error at execution).
- Row 6: the free agent's head contains no "plan-first"/"create_plan"/"WORKFLOW LIFECYCLE"
  text; the tool array is byte-identical across turns.
- Row 7: [models.free] set → used; unset → the default ride (LANDED 2a.6 — verify-only).
- Row 8: the free agent writes nothing under .coding/plans/** (the file tools' protected-set
  refusal [verify the coverage at 2c.7; if the row demands a broader block, NoPolicy::gate_call
  denies .coding/plans/** file-write paths — the kernel-side pick]); recall IS injected;
  consolidation happens at session end (kernel-owned U2 — [verify] the loop's session-end).
- Row 11 (the mechanism half): the dispatch gate + ApprovalBypassed land (2c.5); the Engine
  exposure follows in 3c.
- Row 25 (kernel form): the no-spec build errors with the prompt_required code (2c.1).
- Row 26 (kernel form): ToolSelection { include: [Read] } advertises only the read tools (2c.2).
- Rows 27/28: the kernel mechanics land (2c.3/2c.4 + the Git-group skip, the state_dir
  independence, the disjoint stats); the engine-form rows re-verify in 3a.
- The goldens trio (workflow_stable_head_bytes_unchanged + workflow_policy_routing_table_unchanged
  + volatile_tail_section_order_pinned) green at EVERY step (the 2a.0 contract).
- Compile-enforced greps: `SteeringStats::shared()` absent from the tree (the deletion
  tripwire); no kernel `.gitignore` write; `src/instance_marker.rs` absent from the kernel.

## E. Stop rule

Per §14: any verification attempt failing 3 times → STOP, report, re-plan. Quote drift →
resolve for the tree + document in the commit message. NO silent deviations. The battery is
unpiped on PowerShell ($LASTEXITCODE read from the command, not a pipe — the constitution's
pipe trap). Both crates are #![deny(warnings)]: a green cargo test proves zero warnings; never
#[allow].

## F. Report format (the close)

The 2b close-out shape: Landed (the per-step commits) / the test results / the acceptance
check-offs (the row → the test name → the result) / the deviations (each + why) / the
remaining risk.