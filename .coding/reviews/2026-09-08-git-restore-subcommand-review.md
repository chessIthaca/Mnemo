## Verdict: FINDINGS (0 high, 1 low)

Plan 514dcee1 "Add structured `restore` subcommand to the git tool" (branch wt/agenticcoder). The implementation matches the plan's stated design exactly, the flag-injection surface is closed on every input, tests are thorough and exercise the real git code path, and the constitution checks (docs sync, multi-platform, doc comments, no `#[allow]`) pass. One low-severity cosmetic finding in the frontend label mirror.

## Scope reviewed

Full uncommitted diff (6 files, +463/−13): `src/tool/agent/git.rs` (+413), `src/agent/factory.rs` (ceiling raise + justification), `frontend/src/components/chat/Message.tsx`, `frontend/src/components/chat/messageArgLabel.test.ts`, `README.md`, `.coding/backlog.jsonl`. Verified against the plan file `.coding/plans/514dcee1.md`, `valid_branch_name` (src/project/git_ops.rs:153), `run_git_impl` (argv, no shell), `ToolFilter::ExecutingResearch` (src/tool/mod.rs:361), and the `init_repo` test fixture (git.rs:1278).

## Security analysis (flag-injection surfaces) — CLEAN

- **Paths can never parse as flags.** Every pathspec is validated (`non-empty`, no leading `-`, git.rs:651-658) and appended after a mandatory `--` separator; argv is passed as discrete `Command` args (run_git_impl:311-312) — no shell anywhere. Double-guarded: even without the `-` rejection, the `--` separator alone makes flag-parsing impossible.
- **Source is validated like a branch name** (`valid_branch_name`: rejects empty, leading `-`, any whitespace) and is embedded as a single `--source=<src>` argv element, so it cannot split into a separate flag even hypothetically. `--source=a=b` parses as the value `a=b` (git splits on the first `=`). Covers tree-ishes (`HEAD~1`, `abc123`, `v1.0`) — whitespace-free by nature; anything rejected errors loudly.
- **`args` stays rejected for restore.** The write-subcommand args gate (git.rs:537-548) runs before the dispatch match for every non-read subcommand; `args_rejected_on_restore` pins the loud error ("args are not supported for git restore"). Structured `paths` are not a backdoor for free-form flags.
- **Approval flow unchanged:** restore is a normal `NeedsApproval` write (like checkout), not added to the core-operations list — consistent with the plan's stated intent ("not a core operation").
- **Forgiving `path`→`paths` normalization** (git.rs:513-530) runs pre-serde only for the resolved (lowercased) `restore` subcommand and only when no non-empty `paths` array exists — a present non-empty `paths` wins, matching the documented behavior. No mutation of other subcommands' args.

## Correctness vs. git semantics — CLEAN

argv = `restore [--staged] [--worktree] [--source=<src>] -- paths…` matches `git restore` defaults: worktree restores default to the index, staged/both default to HEAD (documented in the code comment, schema description, and verified by `restore_staged_unstages_only` / `restore_both_discards_staged_and_worktree` against real git in a tempdir). `target` is matched case-insensitively (schema enum stays lowercase-strict — fine). Empty `source` is treated as absent (git defaults apply) — good.

Tests: ~12 new tests cover required-paths, invalid target (error lists valid values), flag-shaped source/path rejection, worktree default discard, restore from `HEAD~1` (asserts content AND that status shows the restore as a modification), staged-unstages-only (index clean via `diff --cached --quiet`, worktree preserved), both, singular `path` normalization, args rejection, and subcommand resolution from either field. The regression-test expectation is fully satisfied.

## Constitution checks

1. **Documentation sync — PASS.** Module doc gains an accurate restore paragraph (structured fields, discards work, never args, plan 47472e35 pointer); all three error strings name `restore` (both unknown-subcommand errors + the trailing arm); ToolSchema adds the enum entry + `source`/`paths`/`target` properties whose descriptions match delivered behavior (incl. "A singular `path` string is also accepted"); the main schema description mentions restore; README's git-tool bullet clause is accurate.
2. **Multi-platform neutrality — PASS.** No new `cfg(windows)`; the only one in the file is the pre-existing `CREATE_NO_WINDOW` spawn flag. New code and tests use plain `std::process::Command` git invocations — platform-neutral.
3. **Style — PASS.** All new fields carry doc comments; no new public functions; no `#[allow]`; no warnings (deny(warnings) + reported green `cargo test`).

## factory.rs ceiling raise — SANE

`git` is in both the Executing and ExecutingResearch tool surfaces (Agent category minus file-edit tools, src/tool/mod.rs:361-369), so the ~700-char schema growth (3 properties ≈ 670 chars JSON + enum entry + description sentence — the "~700 chars" claim checks out) hits both. The raise (Executing +500, ExecutingResearch +100) is tight, not slack: ExecutingResearch now has <100 chars headroom, meaning the next schema tweak anywhere trips the guard — which is exactly the documented purpose of this budget test ("fails … at the commit that causes it"), and the test's own doc says deliberate, justified raises are the legitimate outcome. The dated justification comment is accurate.

## Finding

### Low 1 — Frontend label mirror diverges for `paths: []` + singular `path`
`frontend/src/components/chat/Message.tsx:486-492`. The backend normalization treats an **empty** `paths` array as absent and lifts a singular `path` string (`has_paths` requires non-empty, git.rs:521-524). The frontend mirror branches on `Array.isArray(parsed.paths)` alone, so for `{"subcommand":"restore","paths":[],"path":"b.txt"}` the chip shows bare `restore` while the backend actually restores `b.txt` — the comment says "mirror it so the chip still shows the file". Display-only (the executed command is echoed in the tool result body), and pathological input, but the fix is one condition: only take the array branch when it's non-empty (e.g. `Array.isArray(parsed.paths) && parsed.paths.length > 0`), falling through to the singular-`path` branch otherwise. Note the non-empty-array branch must NOT fall back to `path` (backend ignores `path` then) — only the empty-array case diverges.

## Informational notes (not findings)

- `target: ""` (explicit empty string) errors loudly as "unknown restore target ''" while `source: ""` is forgivingly treated as absent. Loud + lists valid values, and the schema enum has no empty string — acceptable, just an asymmetry worth knowing.
- The frontend `sub === "restore"` check is case-sensitive while the backend resolves case-insensitively; `subcommand: "Restore"` gets the plain generic label. Consistent with every other git subcommand label (none lowercase), display-only.
- The diff also prunes an unrelated *done* backlog item (adc689d8) and the commit should sweep in the untracked `.coding/` side-cars (the 2026-08-27 read-only-args decision record, two image-parsing specs from the merged previous plan, `.coding/plans/514dcee1.md`) — normal bookkeeping, flagged only so the commit includes them deliberately.
- Reported test results (workspace 1562 passed; git module 68 ok; frontend tsc clean + 636 passed) are consistent with the code read; reviewer is read-only and did not re-run them.
