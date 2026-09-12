# Review — "Reviewing" workflow state + dead-code cleanup

**Date:** 2026-05-20
**Scope:** All uncommitted changes (`git diff HEAD`) — the new `Reviewing`
workflow state (Change B) and the `#[cfg(test)]` dead-code cleanup (Change A).
**Reviewer:** Read-only subagent.

---

## Summary

The change adds a `Reviewing` state between `Executing` and `Complete` so the
review→fix→commit closing sequence has a real home with its tools available,
and makes the review unskippable: a new `finish` tool is the only
`Reviewing → Complete` transition, gated on a non-empty review report under
`.coding/reviews/`. The design is sound and well-tested. Two findings below
(one correctness bug, one constitution violation); both are fixable with
small targeted edits.

---

## Findings

### 1. Correctness — `load_latest` drops `reviewed` when a skill is persisted

**File:** `src/workflow/mod.rs`, `load_latest`, lines 619–642.

When a skill was persisted to the sidecar, `load_latest` takes an early return
BEFORE the line that restores `self.reviewed` from the sidecar:

```rust
// line 619-624 — early return when a skill is persisted
if let Some(skill) = persisted_skill {
    self.active_skill = Some(skill);
    self.state = WorkflowState::Skill;
    return Ok(());          // <-- returns here
}

// line 630 — only reached on the non-skill path
self.reviewed = persisted_reviewed;
```

So when a skill is resumed, `self.reviewed` is left at its `new()` default
(`false`), even though the sidecar may carry `reviewed: true`. The in-memory
value is then stale. The next `persist_stack` (e.g. when the skill ends via
`end_skill`, which calls `persist_stack`) writes that stale `false` back to
`stack.json`, **corrupting the sidecar**.

**Concrete failure sequence:**
1. `complete_step` (last step) → `Reviewing`, `reviewed = false`.
2. `finish()` → `Complete`, `reviewed = true` (sidecar: `reviewed: true`).
3. `start_skill("merge_to_main", …)` → `Skill` (sidecar still `reviewed: true`).
4. **App restarts.** `load_latest` reads `reviewed: true` + skill, takes the
   skill early-return, and **never sets `self.reviewed`** → in-memory
   `reviewed = false`.
5. `end_skill()` → `Complete`. `persist_stack` writes the stale
   `reviewed: false`.
6. **App restarts again.** `load_latest` sees a complete plan with
   `reviewed: false` → derives **`Reviewing`**, even though the review was
   already closed out in step 2. The agent is forced to re-run `finish` (with
   a review report) to recover `Complete`.

**Severity:** Medium. Requires a restart while a skill is active after a
reviewed plan — plausible for `merge_to_main`, which is exactly the skill
started from `Complete` after a review. The recovery path (re-call `finish`)
works but is confusing and re-opens an already-closed review.

**Fix:** Move `self.reviewed = persisted_reviewed;` to BEFORE the skill
early-return (or set it inside the skill branch too), so the persisted flag
is always restored regardless of whether a skill is active.

---

### 2. Constitution compliance — `abandon_plan` lost its doc comment

**File:** `src/workflow/mod.rs`, line 464.

The project constitution requires "All public functions must have doc
comments." The diff inserted the new `finish()` method + its doc comment
directly above `abandon_plan`, and in doing so **deleted `abandon_plan`'s
original doc comment**. Compare:

**Before (HEAD):**
```rust
/// Abandon the active plan without completing it: pop it off the stack and
/// resume the parent (or return to Planning if the stack empties).
///
/// Returns the title of the abandoned plan, or an error if no plan exists.
pub fn abandon_plan(&mut self) -> Result<String> {
```

**After (working tree):**
```rust
    pub fn abandon_plan(&mut self) -> Result<String> {   // <-- no doc comment
```

The `finish()` doc comment now sits where `abandon_plan`'s used to be, and
`abandon_plan` is left bare. This is a regression introduced by this diff
(verified via `git show HEAD:src/workflow/mod.rs`).

**Severity:** Low (constitution compliance). `abandon_plan` is `pub` and
now undocumented.

**Fix:** Restore the doc comment above `abandon_plan`:
```rust
/// Abandon the active plan without completing it: pop it off the stack and
/// resume the parent (or return to Planning if the stack empties).
///
/// Returns the title of the abandoned plan, or an error if no plan exists.
pub fn abandon_plan(&mut self) -> Result<String> {
```

---

## Items checked and found sound

- **`finish` tool path validation (security):** The
  `canonicalize().starts_with(reviews_dir.canonicalize())` pattern is the
  standard Rust path-containment check. `canonicalize` resolves both `..`
  segments and symlinks, so a symlink `.coding/reviews/evil.md -> /etc/passwd`
  canonicalizes to `/etc/passwd` and fails the `starts_with` check; a `..`
  escape like `.coding/reviews/../../etc/passwd` likewise canonicalizes
  outside `reviews_dir` and is rejected. No bypass found. The non-empty check
  (`read_to_string` + `!s.trim().is_empty()`) is robust for markdown reports.
  There is a minor TOCTOU between the `canonicalize` containment check and
  the `read_to_string` (the file could be swapped), but the impact is
  negligible — `finish` only flips a boolean in `stack.json`; it never
  executes or trusts the report's content. Not a finding.

- **`complete_step` root-vs-sub-plan routing:** Correct. Sub-plans
  (`stack.len() > 1`) pop and resume the parent at `Executing`; only the root
  plan (`stack.len() <= 1`) transitions to `Reviewing`. Verified by
  `completing_sub_plan_pops_and_resumes_parent` and
  `completing_last_plan_transitions_to_reviewing` tests.

- **`finish()` double-call guard:** Properly guarded. Both the tool's
  `execute()` and `Workflow::finish()` check `state == Reviewing` and error
  otherwise. After the first call, state is `Complete`, so a second call
  errors. Covered by `finish_errors_outside_reviewing`.

- **`Reviewing` ToolFilter arm:** Correctly exposes `finish` + `ask_user` +
  `current_plan` + all Agent tools + all Memory tools, while hiding
  `complete_step`/`create_plan`/`update_plan`/`abandon_plan`/`skill_start`/
  `skill_end`/`abandon_skill`. Verified by `reviewing_filter_exposes_closing_tools`.

- **Legacy bare-array sidecar:** `#[serde(default)] reviewed: bool` on
  `StackSidecar` handles the object form missing the field (defaults `false`
  → `Reviewing`, which is correct — the review hasn't been closed). The bare
  array path (`else` branch) leaves `persisted_reviewed = false`, also
  correct. A legacy complete plan now re-derives `Reviewing` instead of
  `Complete` — this is the intended behavior change.

- **Exhaustive matches on `WorkflowState`:** All match arms
  (`Display`, `ToolFilter::from_state`, `prompt.rs` state-blurb,
  `model_resolver.rs`, `run_all.rs` strict gate) include a `Reviewing` arm.
  The compiler would reject a non-exhaustive match; confirmed none missed.

- **`run_all` strict gate:** `strict_success_allows_commit` correctly rejects
  `Reviewing` (it only allows `Complete | Planning`). A turn ending in
  Reviewing is unfinished — the review hasn't been closed out. Covered by
  `strict_success_rejects_executing_reviewing_and_skill`.

- **`#[cfg(test)]` dead-code cleanup (Change A):** The `len()` / `is_empty()`
  methods on `PendingQuestions` and `PendingApprovals` are only called from
  their own `#[cfg(test)]` modules (verified by search). Gating them with
  `#[cfg(test)]` correctly removes them from the production binary with no
  dead-code suppression. Clean.

- **Frontend:** `types.ts` adds `"reviewing"` to the `WorkflowState` union;
  `StatusBar.tsx`, `PlanProgress.tsx`, and `BacklogView.tsx` each add a
  "reviewing" style/label case. The `agentEventReducer` casts
  `event.state as WorkflowState` with no state-specific filtering, so
  "reviewing" flows through. The "Merge to main" button (gated on
  `complete | planning`) correctly stays hidden in Reviewing — you shouldn't
  merge mid-review. No missing cases found.

- **Line-ending style:** No mixed `\r\n`/`\n` introduced. The `file_edit`
  changes are targeted; the LF→CRLF git warnings are on pre-existing
  `.coding/` JSON/MD files and `Cargo.toml` (line-ending normalization only,
  no content change — `Cargo.toml` diff is empty).

- **No direct commits to main:** All changes are uncommitted in the working
  tree on the `main` branch (ahead of origin by 42 commits). The constitution
  prohibits committing directly to main except via the `merge_to_main` skill;
  this review does not commit anything.

---

## Verdict

Two findings, both fixable with small targeted edits:
1. **Medium correctness bug** — `load_latest` doesn't restore `reviewed` when
   a skill is persisted (move the assignment before the skill early-return).
2. **Low constitution violation** — `abandon_plan` lost its doc comment
   (restore it).

Everything else (the `finish` tool's path validation, the state-machine
routing, the ToolFilter arm, the strict gate, the frontend, the dead-code
cleanup) is sound.
