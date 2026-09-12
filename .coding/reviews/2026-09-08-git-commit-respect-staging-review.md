## Verdict: FINDINGS (0 high, 2 low)

### Summary

The core fix is **correct and well-implemented**. The guard `git diff --cached
--quiet` correctly distinguishes "nothing staged" (exit 0 →
`output.status.success()` true → auto-stage) from "something staged" (exit 1 →
success false → skip auto-stage, commit only what's staged). The polarity is
right, backward compatibility is preserved, and the regression test genuinely
fails on the old unconditional-`add -A` code and passes on the new code. No
`#[allow(...)]`, no platform-specific code added.

The two findings are documentation/knowledge-sync items the project
constitution requires before a change ships ("a feature that ships with its
docs not updated is an incomplete change").

---

### LOW 1 — Stale doc comments describe `commit` as unconditionally staging all changes

**Files/lines:** `src/tool/agent/git.rs:8-9` (module doc) and
`src/tool/agent/git.rs:343-344` (schema `description`, agent-facing).

Both still say `commit` "stages all changes (`git add -A`)" unconditionally. The
new behavior is conditional: `commit` auto-stages with `git add -A` **only when
nothing is already staged**; when something IS staged it commits only what's
staged. The module doc (lines 8-9) and the schema description (lines 343-344)
are now misleading — a reader/agent would believe `commit` always sweeps in
everything, which is exactly the behavior this fix removed. The schema
description is the text the agent reads when deciding how to use the tool, so
leaving it stale risks the agent still reaching for the `shell`-for-selective-
commit workaround unnecessarily.

**Fix:** Update both to reflect the conditional staging. Suggested wording:
- Lines 8-9 — append/replace with: ``//! `NeedsApproval`); `commit` auto-stages
  all changes (`git add -A`) only when nothing is already staged — when the
  index has staged changes it commits only what's staged, so a deliberate
  selective `git add` (via `shell`) is respected.``
- Lines 343-344 — change to: ``"`commit` auto-stages all changes (git add -A)
  only when nothing is already staged (otherwise commits only what's staged)
  and REQUIRES `message`."``

---

### LOW 2 — Stale HOW memory record contradicted by the fix

**Memory record:** `a4b3f6e0-966f-5487-8e09-1f28013960e5` — "HOW: git tool
commit auto-stages everything — use shell for selective commits".

This record documents the OLD behavior (commit runs `git add -A`
unconditionally) and advises using `shell` for selective commits as a
workaround. The fix makes the `git` tool's `commit` itself respect selective
staging, so both the stated behavior and the workaround advice are now
obsolete. Per the memory-hygiene rule ("memory_supersede when a new fact
contradicts/obsoletes a stored one — NEVER leave both live"), this record must
be superseded/updated so a future session doesn't act on the stale workaround.

**Fix:** `memory_supersede` (or `memory_update`) record `a4b3f6e0` to state that
the `git` tool's `commit` now respects existing staging (auto-stages only when
nothing is staged); the `shell`-for-selective-commits workaround is no longer
necessary.

---

### Correctness verification (no findings)

1. **Guard polarity — CORRECT.** `run_git_impl` sets `success =
   output.status.success()` (`git.rs:305`), i.e. true only on exit 0. `git diff
   --cached --quiet` exits 0 when the index matches HEAD (nothing staged) →
   `nothing_staged.success` true → auto-stage; exits 1 when staged changes
   exist → `nothing_staged.success` false → skip auto-stage. Polarity is right.
2. **False-positive edge cases — none that cause silent corruption.**
   - *Unborn HEAD (no commits):* `git diff --cached --quiet` compares the index
     to an empty tree; nothing staged → exit 0 → auto-stage. No false positive.
   - *Non-git directory:* `git diff --cached --quiet` exits 128 → `success`
     false → skip auto-stage → `git commit` exits 128 → error returned. The
     error is still surfaced (from `commit` instead of `add -A`); no silent
     wrong commit. In production `project_root` is always a real repo, so this
     path is unreachable in practice.
3. **Backward compatibility — CONFIRMED.** `commit_stages_untracked_files`
   (`git.rs:872`) writes an untracked file with nothing staged → guard sees
   nothing staged → auto-stages → commits. Still passes.
   `commit_requires_message` (`git.rs:863`) returns before the guard.
   Unaffected.
4. **Regression test — genuine.** `commit_respects_existing_staging`
   (`git.rs:913`) stages `staged.txt` via a direct `git add` (realistic shell
   simulation), leaves `untracked.txt` untracked. On OLD code, unconditional
   `git add -A` sweeps in `untracked.txt` → `git ls-files` lists it →
   `!tracked.contains("untracked.txt")` assertion fails. On NEW code, the guard
   skips auto-stage → only `staged.txt` committed → assertions pass. The
   `read_to_string(untracked.txt) == "untracked"` check also confirms the
   untracked file is untouched. Setup is realistic.
5. **Scope — correct.** Only the `commit` arm changed (`git.rs:511-533`). The
   other `git add -A` sites (`git_ops.rs`, `finish_capture.rs`, `plan.rs`,
   `ipc/files.rs`) are app-internal bookkeeping commits that intentionally stage
   everything — out of scope and correctly left alone.
6. **Constitution — compliant.** No `#[allow(...)]`; clear comments on the guard
   and the test; no platform-specific code added (the `#[cfg(windows)]` block at
   `git.rs:293` is pre-existing in `run_git_impl`); multi-platform neutral.
