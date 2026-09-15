## Verdict: FINDINGS (0 high, 2 low)

Round-6 verification of the same uncommitted change (plan ee65fd4b — "Research plans may write .coding artifacts; close the sub-plan review leak", options A + D; branch `wt/mnemo`, HEAD `0e1d466`, no new commits). The round-5 doc fix itself is **correct**: the four enumerated sync-path callers are exact and complete **for the agent-tool layer**, and I re-derived every production `Sandbox::validate`/`validate_for_creation` call site in the tree without finding a fifth caller there. Two **low, documentation-precision** findings remain (one clause each, zero behavioral impact): (L1) the count sentence is unqualified while the tree holds five further sync-path call sites in the app crate (`src-tauri/src/ipc/files.rs`); (L2) `PLAN.md:133-136` still asserts the blanket Perf-H1 claim round 5 just removed from `sandbox.rs`.

## Q1 — Enumeration accuracy (re-derived from the tree)

**Sync-path callers in the agent-tool layer = exactly the four bullets. No fifth exists.**

| # | caller | evidence |
|---|---|---|
| 1 | `approval::is_project_scoped` | `src/agent/approval.rs:133`, inline; the P3 trade-off is documented at `:114-132` |
| 2 | `shell::resolve_cwd` | `src/tool/agent/shell.rs:217`, called inline from `execute` (`:278-284`); the file contains **no** `spawn_blocking` at all |
| 3 | `dispatch::research_write_verdict` | `src/agent/dispatch.rs:90` (`validate`) + `:108` (`validate_for_creation`), synchronous, called from `execute_tool_call` with the workflow guard held (`:229-241`) |
| 4 | approval-preview hook | `file_edit.rs:1409` (`prepare_for_approval` ← `approval_preview` `:1296`) and `file_write.rs:189/191` (`:98`), reached inline from `dispatch::execute_tool_call` |

**Every other production call site is inside a `spawn_blocking` closure** — verified one by one, including two round 5 did not name:
`convert_line_endings.rs:125 ← :116` · `browser/mod.rs:261 ← :260` and `:886 ← :885` · `file_edit.rs:1335 ← :1334` · `file_read.rs:120 ← :119` · `file_write.rs:140` (`validate_for_write`, i.e. the ladder's internal `validate`/`validate_for_creation` at `sandbox.rs:381/383/398`) `← :132` · **`file_append.rs:110` (`validate_for_write`) `← :100`** · `image_tools/mod.rs:64` (`load_image_data_url`, a sync helper whose only production caller wraps at `:145`) and `:163 ← :162` · `read_files.rs:347` (`read_one_with_count`) with both callers (`read_one` `:240`, `read_one_with_count` `:259`) inside the closure at `:226` · `search_read.rs:381` and `:424` (`read_one`) inside the closure at `:192` · `search.rs` — no `validate` call at all (only `sandbox.root()`); its closure is `:1271-1281`. `sandbox.rs:381/383/398` are the ladder's own internal calls, reachable only through the two wrapped `validate_for_write` sites.

**The fifth caller — app crate, not covered by the sentence.** `src-tauri/src/ipc/files.rs` calls `sandbox.validate` **inline (no `spawn_blocking`), on the async runtime**, at five places: `read_file :37` (its own doc says "the path is sandbox-validated BEFORE the spawn"), `list_files :698`, `repo_relative_pathspec :858 ← git_diff_head :974` (inline before the spawn), `save_conversation :1346`, `load_conversation :1383`. Wrapped in that file: `read_image_data_url_sync :64 ← :110` and `write_sandboxed :128 ← :149`. → finding **L1**.

## L1 (low) — the count sentence is a tool-layer census stated unqualifiedly

`src/tool/agent/sandbox.rs:23-25` now reads "Four sync-path callers remain" with no scope word, immediately after a paragraph whose contract sentence is likewise unqualified (`:15-18`: "callers on the async runtime MUST invoke them from inside a `tokio::task::spawn_blocking` closure"). In this tree there are nine such call sites, not four: the four bullets **plus** the five `src-tauri/src/ipc/files.rs` sites named above. The four bullets are themselves verified exact and complete for the layer they describe (Q1), and each app-crate site is deliberate and documented locally (the N2 pattern: validate before the spawn so `State` never crosses an await) — but a reader auditing "is every async-runtime caller wrapped?" against this sentence gets a wrong answer, and this is the same failure class rounds 4 and 5 corrected twice.

**Fix (one clause or one sentence):** scope it — "Four sync-path callers **in the tool layer** remain" — or add a line acknowledging that the app's IPC file commands also validate inline by design (`src-tauri/src/ipc/files.rs`). If the tool-layer scope was the intent all along, the clause is the entire fix.

**Why this is not a PASS:** the round-5 ask was to re-derive the call-site list from the tree, and the tree's list is nine, not four. I could not find a fifth caller *in the layer the bullets describe*, so nothing about the fix's substance is in doubt.

## Q2 — Each bullet's characterization: all four are right

- **bullet 1 (`is_project_scoped`)** — correct. One `validate` per tool-call approval decision (`approval.rs:133`), inline by construction (the fn is sync and `needs_approval` is a sync seam); the P3 trade-off paragraph at `:114-132` matches the code.
- **bullet 2 (`shell::resolve_cwd`)** — correct. One `validate` + `is_dir` per shell command (`shell.rs:211-217`, called inline at `:284`); the command itself is `tokio::process::Command`, and the file has no `spawn_blocking` at all.
- **bullet 3 (`dispatch::research_write_verdict`)** — correct, including the bound. It does one `validate` and, only when that succeeds, a depth-bounded `lexical_path_is_link_free` walk (`symlink_metadata` per already-existing component, stopping at the first missing one — `sandbox.rs`), and it runs while the workflow mutex guard is held (`dispatch.rs:229-241`). Reachability is exactly as documented: gated on `filter == ToolFilter::ExecutingResearch` and short-circuited on the four `RESEARCH_ARTIFACT_TOOLS` names (`dispatch.rs:18-30`), so no other plan kind or state can reach it.
- **bullet 4 (approval-preview hook)** — correct. `Tool::approval_preview`'s default is `None` (`src/tool/mod.rs:144-146`) and the symbol has exactly three definitions (trait default + `file_edit.rs:1296` + `file_write.rs:98`); `file_append` has no override and `convert_line_endings.rs:97` carries the explicit "No `approval_preview` override (consistent with file_append)" note. Cost and scope match the code: one `canonicalize` plus one preview read (`prepare_for_approval` reads via `read_to_string` — `file_edit.rs:1419`, `file_write.rs:204`) per **prompted** call, in the same approval flow as `is_project_scoped`; `file_write` returns `None` for the append shape (`:102-104`), so "PROMPTED file-tool call" is the accurate phrasing. The claim "their own `execute` paths ARE wrapped" holds for both tools (see Q3).

## Q3 — The "ENTRY-POINT" rewording is truthful

For `file_edit`/`file_write` the `execute` entry point does run **all** of its blocking work in one closure: `file_edit.rs:1328-1335` (validate → protected check → read → `prepare_edit` → write, in the closure by design so the error ordering is unchanged) and `file_write.rs:127-132` (protected check → `validate_for_write`/create-dir → re-validate → write). The only blocking work of those two tools outside a closure is the preview hook — which the same paragraph now names as the fourth caller and designates "the exception rather than the rule". So the rewording *fixes* the old overstatement ("entire blocking work") rather than trading one for another; the same reading holds for the other four tools, whose `execute` bodies are wholly wrapped.

## Q4 — Other live docs: exactly one stale sibling claim

Only **`PLAN.md:133-136`** still carries the blanket Perf-H1 claim that round 5 falsified → finding **L2** below. Every other Perf-H1 comment I swept is per-tool and truthful about its own closure: `file_append.rs:95-98`, `file_write.rs:127-130`, `file_edit.rs:1328-1332`, `convert_line_endings.rs:112`, `file_read.rs:113`, `image_tools/mod.rs:132`, `search.rs:1271`. The `.coding/reviews/**` hits (e.g. `2026-09-15-performance-review.md:38`) are dated historical records — left alone as instructed.

## L2 (low) — PLAN.md still asserts the claim round 5 removed from `sandbox.rs`

`PLAN.md:133-136`: "All synchronous file-system I/O in the async agent tools (`file_read`/`file_edit`/`file_write`/`file_append`/`search`/`describe_image`) runs inside `tokio::task::spawn_blocking` so concurrent multi-agent tool use cannot stall the async runtime." Read literally this is the same overstatement the round-5 fix corrected in the sandbox module doc: `file_edit`/`file_write`'s approval-preview hook (`Tool::approval_preview` → `prepare_for_approval`) performs a `canonicalize` **and** a preview file read inline on the async runtime, so "**All** synchronous file-system I/O in the async agent tools" is not true for those two. `PLAN.md` is a changed file in this very diff, and agent.md's review expectations make documentation sync (PLAN.md technical decisions) part of the change.

**Fix (one clause):** scope the sentence to the tools' execution paths ("…in the agent tools' `execute` paths runs inside …"), or add the preview-hook exception. Severity is low: no behavior depends on it; it is a stale blanket claim sitting next to the doc that was just corrected for saying the same thing.

## Q5 — Nothing except the comment paragraph moved; rounds 3–5 still stand

- `.git/HEAD` → `refs/heads/wt/mnemo`; `git log -5` → HEAD is still **`0e1d466`** ("Fix: the reasoning panel opens only when the user says so"), i.e. **no new commit** — the parent's claim holds.
- `git diff HEAD --stat` = 10 files: the change's six source/test files (`src/agent/dispatch.rs`, `src/agent/tests.rs`, `src/tool/agent/sandbox.rs`, `src/tool/mod.rs`, `src/tool/workflow/plan.rs`, `src/workflow/mod.rs`), the two synced docs (`PLAN.md`, `docs/FEATURES.md`) and two `.coding` bookkeeping files that belong to other sessions (`.coding/backlog.jsonl` — the sandbox-bypass item; `.coding/plans/12284516.md` — a regression-test line).
- The `sandbox.rs` doc hunk is **comment-only** (the paragraph + the fourth bullet). The substantive hunks are unchanged from what rounds 3–5 verified; re-read and confirmed present:
  - HIGH-1 (dangling-link leaf): the canonical branch now requires `lexical_path_is_link_free(&canonical)` before `judge_research_write`, and the lexical fallback arm requires the same on `validate_for_creation`'s result — both fail closed to `NotApplicable`.
  - `judge_research_write` mutual exclusion: `is_artifact_write_target` ends in `!is_protected_write_target`, so `Artifact` and `Protected` can never overlap (the comment is accurate).
  - Gate order + denial text: the workflow filter is consulted first, the verdict computed once, and `denial_message` still has the three arms (protected ≠ research-boundary ≠ generic).
  - `review_required` lifecycle: armed on sub-plan completion (`matches!(kind, Implementation | BugFixing)`), armed on `abandon_plan` of a **non-root** frame, cleared when a fresh root replaces the stack, persisted in `StackSidecar` with `#[serde(default)]`, restored before the skill early-return in `load_latest`; the six new workflow tests cover completion, abandonment, restart, non-leak into the next root, and a pre-field sidecar.
  - The file-tool ladder (`validate_for_write` steps 1–5) and cache invariant #1 are untouched by this round.

## Q6 — State plainly: what remains before commit

1. **L1** — one clause (or one sentence) in `src/tool/agent/sandbox.rs:23-25` to make the count sentence true for the tree (or explicitly tool-layer-scoped).
2. **L2** — one clause in `PLAN.md:133-136` to match the correction just made in the sandbox doc.
3. Then: commit the change **on `wt/mnemo`** (never `main`) including this report; `cargo test` was already green after the round-5 edit (2359 lib + 16 integration, exit 0, and `#![deny(warnings)]` makes that warning-free) — both remaining items are comment/doc text, so no re-test is behaviorally required, though re-running it is harmless.

Nothing else is open: the substantive code, the invariants rounds 3–5 verified, and the enumeration's four bullets are all correct as of this round.
