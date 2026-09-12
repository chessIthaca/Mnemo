## Verdict: FINDINGS (0 high, 3 low)

All three round-1 findings are verified RESOLVED at HEAD 0b55b23: the per-component trailing-dot/space trim closes the Win32 creation gap (with exactly the regression test round 1 asked for, and no leading-dot/space bypass — confirmed against Microsoft's path-naming rules), the `--no-verify` comment now matches githooks(5) semantics precisely, and the three named doc sites (file_write/file_append ladder comments, README security bullet) are updated. The two-commit split matches the plan, and nothing drifted beyond the described fixes. The 3 new findings are all LOW doc-sync residuals: the round-1 finding-3 fix updated the three sites it named, but the stale-enumeration sweep this round was asked to run surfaces more sites (two code comments, PLAN.md, the deck family) that still describe the pre-fix protection set. The predicate itself and all five write gates are correct and complete — no behavior findings.

### Scope reviewed

- HEAD 0b55b23 on wt/agenticcoding, working tree clean (`git diff HEAD` and `git status --short` both empty — verified).
- f4a2e6d (5 files, +169/−8): src/tool/agent/sandbox.rs (+148 — predicate trim, doc comment, shared refusal message, 7 regression tests), src-tauri/src/ipc/files.rs (+16 — IPC mirror test), src/tool/agent/file_write.rs + file_append.rs (ladder comments), README.md (security bullet).
- 8f435d5 (1 file, +10/−3): src/tool/agent/git.rs only — `--no-verify` on the commit arm (:697) and both merge arms (:718/:720) plus the corrected comment (:690-696).
- 0b55b23 (.coding only, full diff reviewed): the round-1 report verbatim, plan eda38891.md, one checkbox flip in 4422c64c.md, backlog queue +25 items plus two `deleted_at` stamps (3ed2d537, 117844c5 — routine queue maintenance, well-formed JSON, no source impact).
- External semantics verified via web_fetch: learn.microsoft.com "Naming Files, Paths, and Namespaces" and git-scm.com/docs/githooks.
- Tests: not re-run (read-only reviewer); the parent's stated green runs relied upon (root 2158+16 / 0 failed, src-tauri 292+4+2 / 0 failed, exit 0 unpiped). Root is exactly +1 vs round 1's 2157+16 — matching the single new regression test. All new/changed assertions hand-verified against the code.

### Round-1 finding verification

**1. RESOLVED — Windows trailing-dot/space component variant.**

- The trim is per-component, not whole-path: `rel_str.split('/').any(|c| c.trim_end_matches(['.', ' ']) == ".git")` (sandbox.rs:250-252) — the path is split on `/` first, then each component is trimmed before the `.git` comparison.
- Regression test `protected_git_trailing_dot_and_space_variants` (sandbox.rs:807-821) covers exactly the asked variants — ".git./config", ".git /config", ".git./hooks/pre-commit" — through `validate_for_creation` (the lexical creation path, no FS dependence).
- The negative guard is intact: `gitignore_and_gitattributes_stay_writable` (sandbox.rs:849-863) keeps .gitignore/.gitattributes/.gitmodules/src/main.rs writable. No over-block: a hypothetical ".gitignore." trims to ".gitignore" ≠ ".git" — correct, since Win32 would create the real .gitignore, which is writable by design.
- Leading dots/spaces are NOT a bypass: Microsoft's naming rules strip only trailing spaces/periods ("Do not end a file or directory name with a space or a period… it is acceptable to specify a period as the first character of a name"). " .git" and "..git" are distinct names that do not resolve to .git on Windows, so they need no guard. `trim_end_matches(['.', ' '])` (ASCII 0x2E/0x20, any trailing run) models Win32 normalization exactly.
- Scope note: round 1's optional "belt-and-braces" whole-`rel_str` trim for the `.coding` exact matches was not applied — see Observations (defensible).

**2. RESOLVED — the `--no-verify` comment matches githooks(5).**

- git.rs:690-696 now names what the flag bypasses (pre-commit/commit-msg/pre-merge-commit) and the residual (prepare-commit-msg and post-* still run) with the acceptance rationale. Verified against git-scm.com/docs/githooks: all three named hooks are documented "can be bypassed with the --no-verify option" (commit-msg is documented as invoked by both git-commit and git-merge, so covering the commit and merge arms is accurate); prepare-commit-msg "is not suppressed by the --no-verify option"; post-commit/post-merge always run. The comment's rationale (.git writes refused by the sandbox; shell the documented approval-gated residual; the user's own terminal git unaffected) is accurate.

**3. RESOLVED at the three named sites — residual stale sites remain (the new findings below).**

- file_write.rs:135-139 and file_append.rs:103-107 now read "refuses protected paths (.coding state/bookkeeping or the .git control plane)"; README.md:33 now reads "protected `.coding/` live state and the `.git` control plane (planted hooks/`core.fsmonitor` can never be written by the file tools)". The shared refusal message (sandbox.rs:319-325) carries the same wording.
- The follow-up sweep ("no other doc site still claims the old protection set") found residuals → findings 1-3.

### Findings

**1. LOW — file_edit's call-site comment and the IPC mirror's `write_sandboxed` doc still enumerate the old protection set**

- Location: src/tool/agent/file_edit.rs:892-896; src-tauri/src/ipc/files.rs:120-123.
- Symptom: file_edit.rs:892-893 says "Reject edits to protected live-state / bookkeeping files (e.g. the memory DB, safety.toml, backlog.json, the plan stack)" and files.rs:120-123 (`write_sandboxed`'s doc comment) says "protected live-state targets (the memory/codegraph DBs, `safety.toml`, `backlog.jsonl`, the plan stack and `.coding/plans/*.md`) are refused" — both predate the .git protection. The shared message both reference now covers the .git control plane, and files.rs is the very file whose test module gained `write_sandboxed_refuses_git_control_plane` in f4a2e6d — the doc comment 15 lines above it was left stale. Same class as round-1 finding 3.
- Root cause: the round-1 doc-sync fix enumerated its targets by one search phrase ("protected .coding state/bookkeeping"); file_edit's differently-worded comment and the IPC doc comment didn't match it and were missed.
- Suggested fix: reword both to the sibling wording — "refuses protected paths (.coding state/bookkeeping or the .git control plane)" — or replace the enumerations with a pointer to `is_protected_write_target`'s doc comment (the anti-drift option round 1 noted).

**2. LOW — PLAN.md's protection sentence omits the .git control plane**

- Location: PLAN.md:125-128.
- Symptom: "Protected `.coding` write targets (memory DB, `safety.toml`, `backlog.jsonl`, the `knowledge/` corpus, the `plans/` tree) are refused by the file agent tools" — the as-built architecture doc (a constitution-listed doc-sync target) still describes the pre-fix protection set; the .git control plane (any path with a .git component, incl. nested repos and the linked-worktree .git file) is missing.
- Root cause: the round-1 doc-sync finding named README + the two ladder comments; PLAN.md was not checked.
- Suggested fix: extend the sentence — "…are refused by the file agent tools, as is the `.git` control plane (any path containing a `.git` component — planted hooks/`core.fsmonitor`)".

**3. LOW — the deck/narration security story omits the .git protection**

- Location: docs/why-mnemo-deck.md:677-679; docs/deck_content.py:367; docs/narration.py:236-237.
- Symptom: all three carry the "agent can't edit its own guardrails" enumeration (memory DB, safety.toml, backlog.jsonl, knowledge corpus, plans tree, reviews tree — narration.py's variant: "the safety config, the memory database, the plans tree and the reviews tree") with no .git control plane. Each statement remains true as a subset claim, but this is the security-posture story and the .git protection (the agent cannot plant git hooks) is the headline fact from this fix — the same standard round 1 applied to README.md:33.
- Root cause: presentation docs were outside the round-1 fix's named targets.
- Suggested fix: add the .git control plane to the enumeration in all three files (source/rendered/narration forms of the same deck — keep them in sync).

### Observations (no finding — for the parent)

- **Two-commit split confirmed per the plan:** f4a2e6d = the sandbox protection (predicate + doc + shared message + 7 sandbox regression tests + the IPC mirror test + the two ladder comments + the README bullet); 8f435d5 = `--no-verify` only (git.rs, +10/−3: commit arm :697, both merge arms :718/:720, the corrected comment). 0b55b23 = report + bookkeeping only. No source drift in any of the three commits; the working tree is clean at HEAD.
- **Round-1 observations:** the two-commit split → addressed; app-internal git calls (run-all checkpoints, merge_to_main) without `--no-verify` → correctly left as an out-of-scope observation (the planting vector is closed by the sandbox; shell is the documented approval-gated residual); BUG: memory → finish-time auto-capture per the parent (not this review's concern); IPC UX (FileViewer Save refuses .git edits) → informational, consistent with the security decision.
- **Round 1's optional `.coding` exact-match trim not applied — defensible:** the trim lives only in the .git component check. A trailing-dot variant of a `.coding` exact-match target (e.g. ".coding./safety.toml") is caught by `validate()`'s canonicalization whenever the parent exists — and `.coding` plus its state files always exist in a functioning project (created on project open; the app holds an open memory.db connection), so the creation-path residual is theoretical. Consistent with round 1's "belt-and-braces" framing; noted, not flagged.
- **fullreview.md (:19, :224)** also carries the old enumeration — a historical review artifact at repo root (its test counts, 522 lib, date it), not a living doc; noted only so the parent knows the sweep hit it.
- **IPC mirror verified at HEAD:** `write_sandboxed_refuses_git_control_plane` (files.rs:673-684) refuses .git/config with the protected message, asserts the file was NOT created, and asserts .gitignore still writes (no over-block).
- **Test status:** not re-run (read-only); the parent's green runs relied upon (root 2158+16 / 0 failed — exactly one more than round 1's 2157+16, matching the single new regression test; src-tauri 292+4+2 / 0 failed; exit 0 unpiped). All new/changed assertions hand-verified against the code.
- **Multi-platform neutrality:** the trim is pure ASCII string matching (no Windows API); the regression test is lexical (`validate_for_creation`, no NTFS dependence) so it runs green on macOS too; the Windows-specific behavior being modeled (trailing-dot stripping) is matched, not assumed.
