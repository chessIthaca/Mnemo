## Verdict: PASS

Job 2c (f348992..05eda72, 8 commits on wt/mnemo) verified against the code, not just the tests: the §5.4 gate is correct and the ApproveViaApi branch preserves today's semantics byte-for-byte; serde round-trips with the documented wire shapes; all G rows hold at the kernel form; every §G deviation checked out against the shipped code. Zero high, zero low findings; the (documented, in-code) 3a Engine forwards are enumerated below, none actionable now.

Reviewed-state: 05eda72886a74ed1b62599ff1ac3c50edf39c626


## What I read (empty-diff rule)

Working tree: one uncommitted change, `.coding/plans/65dd6aa0.md` — the harness's step tick (7/8, review pending), bookkeeping, out of line-review scope. The review scope is the committed delta: all 8 commits walked via `git log` + per-commit `git show --stat` (f348992 2c.1/G1, 1993e39 2c.2/G2, 9723ae2+cbdc50c 2c.3/G3, 6fe6768 2c.4/G4, 4e4462b 2c.5/§5.4, fec4314 2c.6, 05eda72 2c.7), then the landed code at HEAD across the full scope surface:

- `src/agent/policy.rs` — PermissionPolicy enum (:75-89), NoPolicyFactory::build_policy → `Err(KernelError::PromptRequired)` (:292-296), `with_coding_default` (:400), NoPolicy::register_tools registers nothing, serde test (:540-566).
- `src/agent/dispatch.rs` — the §5.4 approval branch (:430-698): `force_prompt` + `gated` hoisted, the Override arm, the preserved ApproveViaApi arm, the direct-dispatch fall-through.
- `src/agent/approval.rs` — `needs_approval` (:71): pure SafetyLevel+SafetyMode+project-scope predicate, no SafetyRules read.
- `src/agent/loop_impl.rs` — `permission` field + `with_permission` (:868), `from_config` defaulting to ApproveViaApi, steering field.
- `src/agent/factory.rs` — ToolSelection/free_default/group_allowed/want (:130-260), build_registry gating + exclude prune (:1326-1460, unregister :1394), Git-group repo gate (:1529-1550), Host-group review channel (:1551-1559), Spawn registration with the factory policy (:1895-1919), the build chain `.with_steering`/`.with_permission(ApproveViaApi { safety_mode: *self.safety_mode.read() })` (:1150-1157), the build_policy fallback (:1190-1213), the sanctioned cfg(windows) browser sites (:1666, :3546).
- `src/runtime/channels.rs` — `AgentEvent::ApprovalBypassed` (:579) + SerializableAgentEvent twin (:809-813).
- `src/runtime/mod.rs` — AgentSpawner trait `permission` param (:194+).
- `src/tool/agent/spawn_agent.rs` — with_permission (:217/:221), both spawn arms pass `self.permission.clone()` (:494/:500).
- `src-tauri/src/ipc/spawn.rs` (:250-283) + `src/host/runtime.rs` (:577-604) — the two production spawners.
- `src/workspace.rs` (full, 200 lines), `src/project/mod.rs` + directory listings (the G3 split), `src/app/{project,scaffold,worktrees}.rs` present.
- `src/config/settings_dto.rs` — free slot (:76-82), validation list (:644-657), apply arm (:950-955), `free_patch_set_clear_keep` (:1440-1498).
- `src/tool/agent/sandbox.rs` — protected-set refusal (:373-390).
- `src/agent/prompt.rs` — PromptSpec/ConstitutionMode/coding_default (:520-578), `kernel_head_has_no_workflow_text` (:2168-2183), golden trio member (:946).
- `src-tauri/src/console.rs` render arm (:347-352); `src-tauri/src/instance_marker.rs` (moved, imports `mnemo::workspace::CODING_DIR_NAME`); `src/agent/steering_stats.rs` (singleton gone — search zero hits).
- `src/agent/tests.rs` — both override tests verbatim (:2355-2449).

## Acceptance verification

**1. §5.4 gate — VERIFIED.** `force_prompt = tool.never_auto_for(...)` is evaluated unconditionally (silently under Override, the documented D6 pick); `gated = force_prompt || approval::needs_approval(...)` is hoisted. `gated && OverridePermissions` emits `ApprovalBypassed { tool, reason }` with `reason: if force_prompt { "core_op" } else { "safety_mode" }` — the exact mapping — then falls through to the shared direct-dispatch path (the audit event precedes execution; the `let _ = fanin_tx.send` mirrors the existing ApprovalRequest emission pattern). The only path that can emit `ApprovalRequest` is the `else if gated` arm, i.e. ApproveViaApi — under Override none is ever possible, force_prompt included. That ApproveViaApi arm is today's branch preserved: SafetyRules auto-approve shortcut guarded by `!force_prompt`, approval preview, `ApprovalRequest { core_operation: force_prompt }`, oneshot answer wait; `needs_approval` is pure, so SafetyRules is genuinely not consulted under Override (§5.4 :374-376). GUI/console: `AgentLoopFactory::build_inner` chains `.with_permission(ApproveViaApi { safety_mode: *self.safety_mode.read() })` and `AgentLoop::from_config` defaults to the same — every shipped loop keeps today's semantics, and the live safety-mode RwLock read in dispatch is untouched (the payload is the initial-mode serialization surface only). Tests exercise the real dispatch path (real AgentLoop + real GitTool + scripted provider), asserting exactly one bypass, zero ApprovalRequests, and that the gated call actually ran.

**2. Subagent inheritance — VERIFIED at the letter's level.** The `spawn_agent` tool carries its parent loop's policy (`with_permission`; `self.permission.clone()` through both spawn arms) and `AgentSpawner::spawn`/`spawn_with_parent` take the `permission` param; `spawn_passes_the_parent_permission_policy` pins the pass-through. Both production implementations currently accept `_permission` and discard it — documented in-code and in §G as the 3a Engine forward: today's children are factory-built and the factory wires the same ApproveViaApi it puts on every loop, which equals every possible parent policy today, so no divergence is observable. Not a defect; see "Documented forwards" below.

**3. Serde — VERIFIED.** `#[serde(tag = "mode")]`, `#[default] OverridePermissions`, named-field `ApproveViaApi { safety_mode }`. The wire shapes `{"mode":"OverridePermissions"}` / `{"mode":"ApproveViaApi","safety_mode":"autonomous"}` round-trip in `permission_policy_serde_shape_and_default`, with SafetyMode's kebab-case literal asserted (the §G discovery). The tuple→named-field deviation from §5.4 :358's sketch is documented with the correct rationale (an internally-tagged enum cannot carry a newtype variant whose payload serializes to a bare string).

**4. G rows — ALL VERIFIED.**
- *Row 5:* NoPolicy::register_tools registers nothing; `free_default()` = every group except Host; the reviewer channel (`write_review_report`) rides Host; hallucinated create_plan/complete_step/finish/start_skill calls hit the unknown-tool path (the free registry never contains them). Tests: `free_default_excludes_the_reviewer_channel`, `list_models_and_spawn_require_the_spawn_group`.
- *Row 6:* `kernel_head_has_no_workflow_text` asserts the head starts with the caller's identity and contains no WORKFLOW LIFECYCLE / APP RULES / plan-first / create_plan / complete_step text; the workflow consts stayed byte-verbatim (golden trio green at every step; `workflow_stable_head_bytes_unchanged` at prompt.rs :946); the None-selection registry keeps today's exact registration order (`expected_tool_names_registered_when_fully_wired`), and the exclude prune is post-registration (`unregister` before the deferred snapshot) — the array stays byte-identical across turns.
- *Row 8:* `Sandbox::is_protected_write_target` refuses `.coding/plans|reviews|knowledge` (sandbox.rs :388) — the sandbox has no policy knowledge, so the refusal is policy-independent; the spec's conditional gate_call-deny pick was correctly not needed. Recall injection (volatile-tail kernel half) and session-end consolidation (loop shutdown) checked off in §G.
- *Row 25:* `NoPolicyFactory::build_policy` without `ctx.prompt` → `Err(KernelError::PromptRequired)` (policy.rs :292-296); `no_policy_requires_identity` green.
- *Row 26:* `include: [Read]` advertises only the read family (`tool_selection_registers_only_read_group`).
- *Rows 27/28 (kernel form):* `Workspace { root, state_dir, require_git }`; `root_is_git_repo` accepts `.git` as dir or file; `git`/`git_read` register only when the root is a repo (factory :1529-1550); `ensure_minimal_project` never writes `.gitignore`/`.gitattributes` (idempotence + never-writes test); the steering singleton is gone (zero matches in steering_stats.rs; `two_factories_have_disjoint_steering_stats`; factory threads `Arc<SteeringStats>` through `with_steering`; the trace reroute reads `state.runtime.factory`).
- *Row 7 (verify-only):* the free slot is `Option<Option<ModelRefDto>>` after `complete`, in the validation list and the apply arm, exactly mirroring the lifecycle slots; the resolver walk untouched (2a.6's `free_slot_resolves_configured_or_falls_back_to_default`).

**5. Constitution checks — CLEAN.**
- *Multi-platform:* the only cfg(windows) sites in touched files are the sanctioned WebView2 browser registration and its platform-conditional expected-set test — pre-existing. `workspace.rs` is pure `std::path`; `instance_marker.rs` moved app-side with a platform-neutral doc. No new Windows-only surface.
- *File-tools-first:* no shell-based file mutation in landed code. `std::fs` inside `ensure_minimal_project` is kernel bootstrap runtime code (the same pattern as the app's `Project::init`), not agent tooling.
- *Warning-free:* `#![deny(warnings)]` at both crate roots; §G records the battery green at each step (`-p mnemo` 2714 passing, `--workspace`, `--features browser,embeddings`, `build --workspace`, goldens 3/3). The new settings arm deliberately used the lint-clean `.map(to_ref)` form (zero new clippy sites); the 3 in-hunk clippy hits 2c.5 introduced were fixed in-step. No `#[allow]` added.
- *Docs sync:* no README/PLAN.md change — correct for phase-2 kernel mechanics (GUI wire behavior unchanged; the new console line only renders under OverridePermissions, which no factory-built loop can hold). `SteeringStats::shared` references survive only in specs/reviews/plans as history, which §G explicitly acknowledges. Nothing stale found.
- *Bookkeeping accuracy:* the plan frame (7/8, review pending) matches the 7 feature commits + this review; every §G amendment I cross-checked (the serde variant rationale, the kebab discovery, the fallback comment, the lint-clean arm, the clippy-storm accounting, the 2c.7 check-offs) matches the shipped code. One cosmetic note: §G amendment headers are dated 2027-01-14/15 while the boundary commits carry 2026-10-12 author dates — the repo's clock is globally inconsistent (fictional dates throughout); content accuracy is unaffected.

## Risk-focus items

- *Dispatch approval-branch preservation:* verified above — `gated` hoisting is the only structural change; under ApproveViaApi the auto-approve shortcut, preview, prompt payload, and answer handling are unchanged.
- *settings_dto apply arms:* the double-Option semantics are correct — `Some(Some(m))` replaces (effort included), `Some(None)` clears, outer `None` keeps — and `free_patch_set_clear_keep` drives all three end-to-end through `validate_and_apply_settings_patch`, with the endpoint-existence validation covering the new slot.
- *The unwired-Err fallback (factory.rs :1190-1213):* verbatim-verified. `pf.build_policy(&ctx).unwrap_or_else(|_error| … NoPolicy::with_coding_default() …)` with the in-code comment naming the 3a Engine as the owner of error propagation (step-file D2's documented forward); the `None` arm uses the same explicit default. Today's only production wire (WorkflowPolicy) never errs, so the fallback is unreachable in shipped paths; row 25 is pinned directly against the factory method, not through this wrapper. Acceptable as documented.

## Documented forwards (verified, not findings)

1. `IpcSpawner`/`HostSpawner` accept the spawn `permission` param but discard it — §G 2c.5 + in-code; end-to-end inheritance lands with the 3a Engine's SpawnOptions (no observable divergence today: every parent is factory-built ApproveViaApi).
2. ApproveViaApi named-field variant vs §5.4's tuple sketch — serde constraint, documented.
3. App-side tools are not yet G2-gated (kernel-only test factory isolates the selection) — the app group honoring lands with the Engine surface (3a/3c), per §G 2c.2.
4. The Engine's per-agent SpawnOptions tool-selection supply — 3a-forward, per §G.
5. The pre-existing rustc-1.95 clippy storm (~250 sites) stays out of scope (backlog a439d8c2); zero new sites were added — spot-checked the touched hunks.

## Remaining risk

Engine-form rows (5/6/8/27/28 at the engine level, 11's exposure, row 4's end-to-end) re-verify in 3a/3c; clippy stays red tree-wide until the a439d8c2 sweep; the GUI cannot express the free slot yet (phase-3 by design). Nothing in this delta blocks the plan's finish.
