# Review: `git diff` never truncated (branch fix/git-diff-no-truncation)

Scope reviewed: ALL uncommitted changes (`git diff HEAD` + `git status --short`):
`src/tool/agent/git.rs`, `README.md`, `.coding/backlog.json`, `.coding/plans/stack.json`,
plus the untracked plan file `.coding/plans/28e72152-8859-44b0-96d1-247e0c1b5f84.md`.
Cross-checked against `src/tool/agent/git_diff.rs`, `src/tool/agent/mod.rs`
(`cap_tool_output`), and a workspace-wide search for `run_git_uncapped` / `run_git_impl` /
`cap_tool_output` / other truncation points.

## Verdict

The change is correct and the cap bypass is scoped exactly to the `diff` subcommand.
**No critical, high, or medium findings.** Two low nits and two informational notes below.

## Correctness (verified — no findings)

- **Cap-flag wiring.** `run_git` → `run_git_impl(args, true)` (git.rs:97-99);
  `run_git_uncapped` → `run_git_impl(args, false)` (git.rs:105-107). The ONLY caller of
  `run_git_uncapped` in the entire workspace is the `"diff"` arm (git.rs:249) — confirmed
  by search. Every other arm still routes through capped `run_git`: status (:244), log
  (:251), the commit arm's internal `add -A` (:261) and `commit -m` (:265), merge (:286,
  :288), checkout (:305), stash push/pop/drop (:315-317), branch list/delete/create
  (:335, :350, :367), push (:395, :397).
- **`run_git_impl` preserves prior behavior.** Command echo (:114-118), `kill_on_drop`
  (:125), `CREATE_NO_WINDOW` (:129-133), stdout/stderr combination (:142-148), the
  `$ cmd\n…\n[exit code: N]` format (:151), the `data` payload (:156 — full stdout/stderr,
  which was always uncapped pre-change, so no behavior delta there), and the spawn-error
  path (:159-161) are all unchanged. The only delta is the conditional
  `if cap_output { cap_tool_output(full) } else { full }` (:152).
- **No accidental cap bypass elsewhere.** `cap_tool_output` (mod.rs:58-65, 100 KiB +
  "truncated: output exceeded size limit" marker) is still applied by shell (:232) and
  web_fetch (:210); `git_diff` remains uncapped per its own pre-existing mandate.
- **Unbounded-output scoping.** The uncapped invocation passes a fixed literal argv
  `["diff"]` — no user-controlled argument reaches the uncapped path, so it cannot be
  abused to run other commands uncapped. A workspace search for other `truncat`ion points
  found nothing downstream that re-caps `ToolResult.output` before it enters the
  conversation (remaining truncations are prompt-header rendering, memory, and
  provider-trace display caps). The full diff genuinely reaches the LLM context, as
  mandated; the context-growth tradeoff is inherent to the user mandate and identical to
  the already-shipped `git_diff` exception.
- **Branch coherence** (per the branch note): on THIS tree the diff arm is the
  pre-read-args one (`run_git_uncapped(&["diff"])`, git.rs:249) — coherent; no dangling
  references to `read_argv`.

## Bugs (no findings)

- Regression test `diff_output_is_never_truncated` (git.rs:929-960) genuinely fails on the
  old code: the ~349 KB fixture diff would have been cut at 100 KiB with the marker
  appended — failing assertion 1 (`!contains("truncated: output exceeded size limit")`)
  and assertion 2 (`contains("+line 29999")`, the tail). It passes with the fix. (Note:
  assertion 3, `len() > 100*1024`, alone would NOT discriminate — old capped output is
  102,400 + the 44-char note = 102,444 > 102,400 — but it exists to prove the fixture
  exceeds the cap, which it does with ~3.4x margin. Fine as designed.)
- Test fixture is sound: `init_repo` commits a.txt = "one" and sets
  `core.autocrlf=false` (git.rs:651); the rewrite to 30,000 LF lines yields an unstaged
  modification that plain `git diff` (exit 0, differences or not) reports. `result.success`
  assertion is therefore stable. No CRLF/platform flakiness.

## Security (no findings)

- Approval gating untouched: `safety()` remains `NeedsApproval` (:206-213);
  `never_auto_for` for merge/push unchanged (:215-235); all `valid_branch_name` guards
  intact. The uncapped path adds no new argv surface (fixed literal), so no new injection
  surface. Uncapped output cannot be triggered by any write/core subcommand.

## Constitution compliance (verified)

- Pub items documented (the two new fns are private but carry doc comments anyway).
- Regression test present and discriminating (above). `#![deny(warnings)]`-compatible: no
  dead code, unused imports, or `mut`; the intra-doc link `[`super::git_diff::GitDiffTool`]`
  resolves (`pub mod git_diff;` mod.rs:37, `GitDiffTool` is pub).
- Documentation sync: README.md:56 bullet is accurate; git.rs module doc (:18-21),
  `run_git_uncapped` doc (:101-104), and schema description (:185-187) are mutually
  consistent with git_diff.rs's exception doc (:15-19) and schema note (:100-101) — same
  rationale, same "user-mandated" framing, same 100 KiB figure. The cross-reference the
  task asked about is present: git.rs references `[`super::git_diff::GitDiffTool`]` in both
  places, and git_diff.rs:12-13 already back-references `GitTool::run_git`. Two-place
  documentation is consistent and bidirectionally linked.
- Multi-platform neutrality: no new platform-specific code; test uses std::fs + git CLI
  with LF content.

## Low findings (nits — optional)

1. **git.rs:152** — the binding `let capped = if cap_output { cap_tool_output(full) } else { full };`
   holds the *uncapped* output when `cap_output` is false. A neutral name (e.g.
   `displayed`) would read better. Cosmetic only.
2. **git.rs:940** — test comment says the fixture diff is "~280 KB"; it is actually
   ~349 KB (30k added lines ≈ 319 KB content + 30 KB of `+` prefixes + headers). The
   `> 100 KiB` assertion holds regardless; the estimate is just loose (the sibling
   `large_diff_is_not_truncated` comment in git_diff.rs:261 has the same "~270 KB"
   understatement for the same 30k-line file).

## Informational (not defects)

3. **`.coding/backlog.json`** also flips unrelated items 66/68/70 to `done` — app-managed
   bookkeeping from other branches' completed work riding along in this commit. Harmless,
   but the commit will include them.
4. **Merge-time note for later:** when `fix/git-tool-read-args` (which rewrites the diff
   arm to accept user-supplied args) merges, the uncapped call must be carried over to the
   new diff arm. Nothing to do in this change; flagged so it isn't lost at merge time.
