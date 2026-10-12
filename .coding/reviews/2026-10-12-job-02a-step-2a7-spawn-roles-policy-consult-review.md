## Verdict: PASS

Round-1 delta review of step 2a.7 (boundary commit b9f76c6, HEAD; uncommitted remainder = the plan-frame Reviews stamp only). The one-consult flip in `compute_subagent_allowlist` is semantically identical to the raw read it replaces: `WorkflowPolicy::tool_surface`'s gate is `Some(wf.allowed_tools())` on the *same* `Arc<Mutex<Workflow>>` the raw read would lock (factory.rs:1043 wires the policy with `agent.workflow_handle()`), NoPolicy's gate is `None` (ToolSurface::constant) so the fallback preserves the old path verbatim, and the lock discipline is preserved (loops lock dropped before any await; the None arm's wf guard dies at the arm-block close, before the registry snapshot). No findings.

Reviewed-state: b9f76c64992ea1bf823ff9a5573cc958610892b8
## What I read

- `git_read` `show` b9f76c6 (full diff, archived head+tail) + `show --stat` (4 files: src/host/runtime.rs +30/−9, three plan frames) and the uncommitted `git diff HEAD` (only the `## Reviews` base-revision stamp added to `.coding/plans/dc98b5fa.md` — accurate bookkeeping).
- Post-change code in full: src/host/runtime.rs :330-459 (`compute_subagent_allowlist` + the role match) — HEAD == b9f76c6, so the committed state is the live file.
- The seam: src/agent/policy.rs:1-272 (`AgentPolicy`, `ToolSurface::constant()` → gate `None`, `NoPolicy::tool_surface`), src/app/workflow_policy.rs:76-86 (`tool_surface` = one lock, `advertise: Some(wf.schema_filter())`, `gate: Some(wf.allowed_tools())`), src/agent/loop_impl.rs:2397-2417 (`workflow_handle()` / `policy()`), src/agent/factory.rs:1043 (`WorkflowPolicy::new(agent.workflow_handle())` — the policy wraps the SAME Arc the raw read takes).
- The filter under review: src/workflow/mod.rs:425-491 (`Workflow::allowed_tools` — allow-list override branch, Skill arm, **the Subagent-state `panic!`**, Executing-research arm, `from_state` fallback).
- The spec §2a.7 (.coding/specs/headless-free-agent/job-02a-agent-policy-seam.md:749-758) and the sub-plan frame dc98b5fa.md (goal/anchors/steps match what shipped).
- The tests: all seven job-1 host::runtime spawn tests located (:754/:903/:1011/:1059/:1122/:1158/:1171) and the `parent_in_state` harness (:793-899) read in full; `subagent_stamp_replaces_derived_state_keeps_mirror` located (factory.rs:3552).

## Check results

**(a) Semantic identity — confirmed.** `WorkflowPolicy::tool_surface`'s gate is `Some(wf.allowed_tools())` on the same `Arc<Mutex<Workflow>>` as `workflow_handle()` (factory.rs:1043 is the only WorkflowPolicy attach site today; `impl AgentPolicy for` exists for exactly NoPolicy and WorkflowPolicy). Every state is therefore byte-identical: allow-list override (Reviewer/Skill), Skill, Executing-research, and the `from_state` fallback — and the Subagent-state `panic!` (mod.rs:467) is reachable *identically* through both paths (a Subagent-state parent with an allow-list returns the override filter either way; the fail-loud arm is not bypassed or softened by the policy route). NoPolicy is the only gate-`None` producer (`ToolSurface::constant()`), so the None arm's raw read is exactly the pre-2a.7 behavior for the parents that previously took it.

**(b) Lock discipline — confirmed.** `drop(loops)` (:377) precedes every await, so the agent_loops lock is never held across `tool_surface().await` or the wf lock — no nesting. Some arm: the wf lock is acquired and released inside the policy's `tool_surface` (guard dropped at fn return; the `ToolFilter` is cloned out), so the registry snapshot (:395-399) and the intersection (:408-412) run lock-free. None arm: the `MutexGuard` binding `let wf = wf.lock().await` is scoped to the arm block and dropped at the block close (:386-390), strictly before `registered`. No new nested-lock shape exists.

**(c) Test-harness integrity — confirmed structurally.** `parent_in_state` builds parents via raw `AgentLoop::new` (no `set_policy`), so the harness parents carry the default NoPolicy → gate `None` → the seven tests exercise exactly the fallback arm, whose body is the old read verbatim (just block-scoped). All seven test names present; the factory state test present at :3552. Caveat for the record: this read-only reviewer has no shell, so I could not re-run `cargo test` myself — green is established by the boundary commit's battery claim (3011 unit + 24 doc + 327 integration + feature trees) plus the structural identity above.

**(d) Docs sync — accurate.** The :352-361 comment correctly names the policy consult, the None-gate fallback, and the lock-drop property, and matches the spec §2a.7 wording. No README/PLAN.md surface mentions the allowlist internals (searched — only code + `.coding/knowledge` records, which remain accurate); the spec + plan frames are the right home for a seam step.

**(e) Multi-platform neutrality — confirmed.** The diff is pure async-lock/pattern logic in `src/host/runtime.rs`; no Windows-only API, path, or shell syntax.

**(f) Warn-free — confirmed.** No `#[allow]` anywhere in the changed region; no new imports; the diff introduces no dead code or unused bindings.

**.coding/** bookkeeping (accuracy line): the three plan-frame edits in the commit tick step 1, stamp the boundary, and record the reviews base — they match what shipped.

## Non-blocking observations (no action required for this step)

1. **Some-gate arm has no direct unit coverage**: all seven host::runtime tests construct NoPolicy parents, so the `Some(filter)` arm is exercised only indirectly (integration tree / by-construction identity). The spec's own acceptance (:757-758) scopes to job-1's seven + the factory state tests, so this is within the step's contract — but a test spawning from a factory-built (WorkflowPolicy-carrying) parent would pin the delegate identity against future drift (a natural 2b/3a test).
2. **Forward hazard for 3a**: the "gate IS allowed_tools()" identity currently relies on factory.rs:1043 attaching the policy with the loop's own workflow handle. When 3a's engine/session builders gain `set_policy` sites, a policy wrapping a *different* workflow Arc would silently break the identity. Worth a one-line note in the 3a step file, not a change here.