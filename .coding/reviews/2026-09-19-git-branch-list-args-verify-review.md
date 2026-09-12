## Verdict: PASS

Verification re-review of commit `2b00713` on `wt/agenticcoding` ("git tool: allow read-only args on `branch list` (allowlist)"). All 5 prior low findings are resolved in the committed code; the core implementation (confirmed correct in the prior review) is untouched by the fixes and remains correct. No new issues.

Method: reviewed `git show 2b00713` (full diff) + read the on-disk sections of `src/tool/agent/git.rs` and `src/agent/factory.rs` (they match the committed content exactly). The only uncommitted change is the plan file's step-5 checkbox flip (`[ ]`→`[x]`, trivial bookkeeping); untracked `docs/` files are an unrelated deck effort. Neither touches the implementation under review.

### Finding-by-finding verification (all 5 RESOLVED)

**LOW 1 — Error message now names `branch list`.** RESOLVED.
git.rs:612-615 (on-disk, committed):
```rust
"args are not supported for git {subcommand} (flags are only forwarded for read \
 queries: status, diff, log, branch list)"
```
The parenthetical now lists `branch list` alongside status/diff/log, so an agent hitting this error on a write subcommand no longer under-reads the read surface.

**LOW 2 — README.md now lists `branch list`.** RESOLVED.
README.md:71 (committed) now reads `...forwards validated extra flags for read queries (\`status\`/\`diff\`/\`log\`/\`branch list\`) — precise queries such as \`git diff main..feat --stat\`, \`git log -5\`, or \`git branch -vv\`...(\`branch list\` uses an allowlist of read-only listing flags; positionals are refused since they create branches)...`. Shipped doc is in sync.

**LOW 3 — HOW knowledge file updated.** RESOLVED.
`.coding/knowledge/how/2026-08-26-git-tool-per-subcommand-parameter-reference.md` (committed):
- args property description now: `READ queries only (status/diff/log/branch list)` + full allowlist detail (positionals refused; combined short flags must be split; the exact allowed flag set).
- branch table row: Optional column now `action (default list), args (list only)`; Runs column now `list: git branch [read-only listing flags via allowlist]`.
- Verification date updated `2026-09-09` → `2026-09-19`; title gained the branch-list note; a `restore` row was also added (consistency).

**LOW 4 — DECISION knowledge file superseded; successor exists.** RESOLVED.
- Old file `2026-08-27-git-tool-args-is-read-only-writes-reject-free-fo.md`: `status = "superseded"` added to frontmatter (body otherwise preserved).
- New successor `2026-08-30-git-tool-args-is-read-only-writes-reject-free-fo.md`: created with `supersedes = "2026-08-27-..."` pointer and body reflecting that `branch list` forwards allowlisted read-only listing flags (allowlist, not denylist; positionals refused; combined short flags split), while create/delete and all other writes still reject args.

**LOW 5 — Schema description notes combined short flags must be split.** RESOLVED.
git.rs:474 (on-disk, committed) `args` schema description now ends:
`...branch list: read-only listing flags only (positionals refused — they create branches); split combined short flags (use ["-a","-v"] not "-av").`
An agent emitting `git branch -av` now self-corrects from the schema text without a round-trip.

### Core implementation — still correct (unchanged by the fixes)

All 5 fixes are doc/message-only (error string, README, HOW, DECISION, schema description string). None touch the implementation logic, so the prior review's PASS on correctness/security stands and is re-confirmed against the on-disk code:

- **Allowlist is fail-closed** (git.rs:270-296). `validate_branch_list_args` uses `safe_exact` + `safe_prefix` (an ALLOWLIST, not the status/diff/log denylist). Positionals refused first (line 278: any arg not starting with `-`). Any flag matching neither exact nor prefix is rejected (line 286). No write/mutate flag can slip through: `-d`/`-D`/`-m`/`-M`/`-c`/`-C`/`--delete`/`--move`/`--copy`, `--set-upstream-to=`/`-u`/`--unset-upstream`/`--edit-description`, `-f`/`--force`/`-t`/`--track`/`-l`/`--create-reflog` are all absent from both lists → rejected. No shell: args flow as discrete argv via `Command::new("git").args(...)`, so the `--format=`/`--sort=`/`--color=` prefixes can't be abused.
- **`is_read_sub` does NOT widen create/delete** (git.rs:609-610): `... || (subcommand == "branch" && matches!(action.as_deref(), None | Some("list")))`. For create/delete, action is `Some("create")`/`Some("delete")` → false → non-empty args still errors loudly. Pinned by the existing `args_rejected_on_write_subcommands` test.
- **No-args path unchanged** (git.rs:792-798): `args.args.unwrap_or_default()` → `[]` → `branch_list_argv(&[])` → `validate_branch_list_args(&[])` returns `Ok(())` (empty loop) → argv `["branch"]` → `git branch`. Identical to the old `self.run_git(&["branch"])`.
- **4 tests present and meaningful**: `branch_list_accepts_verbose_args` (regression — fails on old code; asserts success + echo `$ git branch -vv\n` + feat/main), `branch_list_accepts_all_flag` (`-a` via default-action path), `branch_list_rejects_positional_arg` (refused + verifies NO branch created via direct `git branch`), `branch_list_rejects_write_flags` (7 mutate flags rejected + verifies still on `feat`).
- **factory.rs ceiling raise justified** (factory.rs:1425-1433): dated `2026-09-19` comment explains ExecutingResearch `20_100 → 20_300` (args schema description grew to name branch list); `20_300 < Executing 23_500`, so fitting ExecutingResearch fits all filters. Mirrors the restore precedent.
- **Warning-free**: both new fns are used (`validate_branch_list_args` ← `branch_list_argv` ← list arm); `safe_exact`/`safe_prefix` both consumed; no new imports; no `#[allow(...)]`. The commit message reports "72 git tests + budget test green" under `#![deny(warnings)]`. (Read-only reviewer could not execute `cargo test`, but no warning source is present and the prior review's reasoning holds.)
- **Multi-platform neutral**: pure argv; the only `cfg(windows)` block is the pre-existing `CREATE_NO_WINDOW` console suppression, untouched.

### Conclusion
All 5 low findings are resolved in commit `2b00713`; the fixes are doc/message-only and introduce no new issues; the core allowlist implementation remains correct and fail-closed. No further action needed.
