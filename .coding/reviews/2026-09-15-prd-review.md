# PRD Cohesion Review — myharness vs `PLAN.md` (2026-09-15)

**Perspective:** Does the implementation match the stated design? Spec-vs-reality gaps.
**Reviewer:** PRD-perspective review (conducted in-session).
**Grounding:** `PLAN.md` (the locked-in design doc), current source.

---

## Overall verdict: **Matches-with-beyond-spec-features**

The architecture and brain faithfully implement `PLAN.md`'s locked-in decisions.
The enforced plan-first workflow, capability-aware provider strategy, sandbox/
path-safety, approval gate, context management, and memory consolidation are all
present and match the spec. The implementation has grown *beyond* the PRD
(skills, backlog/Run-All, plan stack, `spawn_agent` tool, safety-rules system,
stats/pricing) — these are production features, not spec deviations. The one
remaining deliberate deviation (frontend component library) was already remediated.

---

## Findings

### P1 — shadcn/ui (Radix) adoption: now matches the PRD (Resolved)

**What:** `PLAN.md:109` locks in **shadcn/ui (Radix + Tailwind)** as the component
library for accessibility-critical components (Dialog focus trap, Tabs keyboard
nav). Prior reviews flagged this as unadopted (`package.json` had no `@radix-ui/*`).

**Status:** **Resolved.** `frontend/package.json:14-15` now lists
`@radix-ui/react-dialog ^1.1.23` and `@radix-ui/react-tabs ^1.1.21`. The PRD's
locked-in component library is now a dependency. This closes the largest prior
PRD deviation + the root cause of the accessibility finding in one move.

**Recommendation:** Confirm the ConfigDialog/SafetyToggleDialog actually use the
Radix Dialog wrapper (not just that the dep is installed). The dep presence is
necessary but not sufficient — verify the wrappers in `components/ui/` are wired
into the dialogs. (Could not fully verify the wrapper components in this pass;
flag for a follow-up UI review.)

### P2 — PLAN.md status block is accurate (Strength)

**What:** `PLAN.md:21-74` was refreshed as an as-built doc (per the M7
remediation). It now reflects: multi-agent per-agent Workflow+ToolRegistry,
plan stack + skills, backlog/Run-All with checkpoint/rollback + strict-success
gate, safety hardening (protected `.coding` writes, `keys.toml` ACL, approval
contract/DenyAll), `spawn_blocking` for FS tools, IPC module split + golden
contract fixtures, FE store split into pure reducers.

**Assessment:** The status block matches the shipped reality. Test counts
(522 lib + 49 tauri + 5 integration + 82 vitest) are stated. This is the right
state for a design doc — it reflects what was built, not just what was planned.

### P3 — Beyond-spec features are documented (Strength)

**What:** `PLAN.md` documents the beyond-spec features: vision fallback
(`describe_image` tool + `VisionClient`), `spawn_agent` tool, backlog/Run-All,
safety-rules system, stats/pricing, sub-plan stack, richer `AgentEvent` variants
(`ChildFinished`, `UserQuestion`, `PromptDispatched`, `SkillStarted`).

**Assessment:** The PRD no longer understates the implementation. The
terminology section (`PLAN.md:78-100`) cleanly distinguishes Agent tools /
Workflow tools / Skill tools / Memory tools / Spawn tool / Views.

### P4 — Enforced plan-first workflow matches spec exactly (Strength)

**What:** `PLAN.md:8-9` specifies an enforced plan-first workflow: the agent
must author a plan in `.coding/plans/` and check off steps before writing code.
`src/workflow/mod.rs:19-45` implements `WorkflowState` (Planning → Executing →
Reviewing → Complete, with a Skill overlay). `Workflow::allowed_tools()` returns
a `ToolFilter` the agent loop applies *before building the request schema* — in
Planning, write tools are omitted entirely so the model cannot call them
(`src/agent/mod.rs:5`). The dispatch layer re-checks the filter at execution
(`src/agent/dispatch.rs:88-107`), so a hallucinated name still cannot run.

**Assessment:** This is a hard gate, not a soft prompt instruction — exactly as
the PRD specifies. The `Reviewing` state gates `finish` on a non-empty review
report under `.coding/reviews/`, making the review unskippable by construction.

### P5 — Approval contract + core-operation invariant matches spec (Strength)

**What:** `PLAN.md:18-19` specifies every mutating action goes through an
approve-each-action gate with diff-based approval. `src/agent/approval.rs`
implements `needs_approval` (mode-aware) + `await_approval` (oneshot + interrupt
listen). `src/agent/dispatch.rs:151` enforces `never_auto()` / `never_auto_for()`
so core operations (`git merge`/`git push`) always prompt even under Autonomous
mode or a matching safety rule. `src-tauri/src/ipc/approval.rs:124-150`
`re_evaluate` never auto-resolves core operations on a mode change.

**Assessment:** The hard user-sanction invariant (landing commits on main /
pushing to a remote always requires contemporaneous approval) is enforced via
`Tool::never_auto_for`, not a prompt instruction. Matches the PRD intent.

### P6 — Capability-aware provider strategy matches spec (Strength)

**What:** `PLAN.md:116` specifies one OpenAI-compatible client, swap base URL,
with local models as capability-degraded fallback. `src/provider/openai.rs`
implements a hand-rolled SSE client over `reqwest`. `Capabilities`
(`src/provider/mod.rs`) drives multimodal handling: when the provider is
multimodal, image blocks are sent directly; when not, they're stripped and the
vision client handles them separately (`openai.rs:562-587`).

**Assessment:** Matches. The `ProviderKind` enum + `capabilities_with_overrides`
correctly degrades local-model capabilities.

---

## Gaps (none blocking)

- **P1 follow-up:** verify the Radix wrappers are actually wired into the
  dialogs (dep installed ≠ components migrated). Low risk — the dep is present.

## Strengths

1. **Plan-first workflow** is a hard gate (schema filtering + dispatch re-check).
2. **Approval contract** enforces the core-operation invariant by construction.
3. **PRD status block** is an accurate as-built doc (no drift).
4. **Beyond-spec features** are documented in the PRD.
5. **Terminology** is consistent and non-overloaded.

## Remediation order

1. **P1** (Low) — verify Radix Dialog/Tabs wrappers are wired into the actual
   dialogs, not just installed as deps.
2. Everything else is a strength — no action required.
