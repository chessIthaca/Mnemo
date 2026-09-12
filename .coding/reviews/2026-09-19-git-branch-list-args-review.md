## Verdict: FINDINGS (0 high, 5 low)

Review of all uncommitted changes on `wt/agenticcoding` (`git diff HEAD`): `src/tool/agent/git.rs` (+207/-15) and `src/agent/factory.rs` (+7/-0). The change lets `git branch list` (and the default branch action = list) accept an ALLOWLIST of read-only listing flags via `args`, while `branch create`/`delete` and every other write subcommand keep rejecting `args`.

**Bottom line: the security model is sound and the implementation is correct.** The allowlist is genuinely fail-closed — no write/mutate flag can slip through, no positional can create a branch, and the `is_read_sub` change does NOT widen `branch create`/`delete`. The findings are all low-severity doc/message-sync items (the plan already intends to update the knowledge files post-review). I could not run `cargo test` myself (read-only reviewer), but the code is internally consistent with no obvious warning sources, and the plan reports a green test run.


### Correctness & security — PASS

**Allowlist is complete and fail-closed.** `validate_branch_list_args` (git.rs:270-296) uses an allowlist (exact + prefix), not a denylist. Every mutating `git branch` option is correctly ABSENT:
- Delete/move/copy: `-d`/`-D`/`-m`/`-M`/`-c`/`-C`/`--delete`/`--move`/`--copy` — none in `safe_exact`, none a `safe_prefix` → rejected. ✓
- Upstream/description mutators that need NO positional: `--set-upstream-to=`/`-u`, `--unset-upstream`, `--edit-description` — rejected (the test `branch_list_rejects_write_flags` pins all three). ✓
- Force/track/reflog: `-f`/`--force`, `-t`/`--track`, `-l`/`--create-reflog` — rejected. ✓

**No positional can create a branch.** Any arg not starting with `-` is refused first (git.rs:278-283), so `git branch <name>` (create) and `git branch <old> <new>` (move) are impossible. Because no positional can ever be appended, the command is always `git branch <flags-only>`, which git always interprets as a listing. The `--list` flag is itself allowlisted, which *forces* list mode — a defense-in-depth nicety. ✓

**`is_read_sub` does NOT widen create/delete** (git.rs:609-610):
```rust
let is_read_sub = matches!(subcommand.as_str(), "status" | "diff" | "log")
    || (subcommand == "branch" && matches!(action.as_deref(), None | Some("list")));
```
For `branch create`/`delete`, `action` is `Some("create")`/`Some("delete")` → the `matches!` is false → `is_read_sub` false → non-empty `args` errors loudly. Confirmed by the existing `args_rejected_on_write_subcommands` test (git.rs:2023-2043), which already covers both `branch create` + `args` and `action: delete` + `args` — no test gap there. The `None` arm is defensive/dead for branch (action is always `Some` by the time it's reached, or execution already errored) but harmless.

**No-args path is unchanged.** `branch_list_argv(&[])` → `validate_branch_list_args(&[])` returns `Ok(())` (empty loop) → argv `["branch"]` → `git branch`. Identical to the old `self.run_git(&["branch"])`. No regression; `branch_defaults_to_list` still passes. ✓

**No shell.** Args flow as discrete argv through `Command::new("git").args(...)` (git.rs:374-375); the space-joined echo is display-only. The allowlist cannot be bypassed — it runs unconditionally inside `branch_list_argv` before any `run_git`. ✓

**Multi-platform neutral.** Pure argv; the only `cfg(windows)` block is the pre-existing `CREATE_NO_WINDOW` console suppression (git.rs:383-387), untouched by this change. No new platform-specific code. ✓

**Warning-free (by inspection).** Both new fns are used (`validate_branch_list_args` ← `branch_list_argv` ← the list arm); no new imports; `safe_exact`/`safe_prefix` both consumed. No `#[allow(...)]`. I could not run `cargo test` (read-only reviewer) but see no warning source; the plan reports a green run under `#![deny(warnings)]`.

### Tests — PASS (good coverage)
- `branch_list_accepts_verbose_args` — regression: `-vv` forwards and lists feat/main (fails on old code). ✓
- `branch_list_accepts_all_flag` — `-a` via default-action path (no `action` field). ✓
- `branch_list_rejects_positional_arg` — positional refused AND verifies no branch was created (direct `git branch` check). ✓
- `branch_list_rejects_write_flags` — `--set-upstream-to=`/`--unset-upstream`/`--edit-description`/`-d`/`-m`/`--delete`/`--move` all rejected, and verifies still on `feat`. ✓
- Existing `args_rejected_on_write_subcommands` still pins create/delete rejection. ✓

### factory.rs ceiling raise — PASS
20_100 → 20_300 (factory.rs:1433) with a dated 2026-09-19 comment (lines 1425-1429) mirroring the 2026-09-08 `restore` precedent. The test asserts `chars <= ceiling` per filter; ExecutingResearch is the tightest (20_300 < Executing's 23_500), so fitting it fits all. The `args` schema description grew by ~90 chars naming `branch list`; +200 headroom is reasonable. Internally consistent. ✓


### Findings (0 high, 5 low)

**LOW 1 — Error message omits `branch list` from the read-queries list (git.rs:612-615).**
When a write subcommand (incl. `branch create`/`delete`) is called with non-empty `args`, the error reads:
> `args are not supported for git {subcommand} (flags are only forwarded for read queries: status, diff, log)`

The parenthetical now understates the read surface — `branch list` also forwards args. An agent reading this could conclude branch list doesn't accept args and fall back to `shell`. Fix: append `, branch list` to the message (e.g. `"...read queries: status, diff, log, branch list"`). One-line edit.

**LOW 2 — README.md:71 is stale (shipped doc).**
> `The git tool forwards validated extra flags for read queries (status/diff/log) — ...`

Now that `branch list` accepts allowlisted flags, this shipped doc no longer lists it. Fix: add `branch list` to the parenthetical (and optionally note it uses an allowlist / positionals are refused). Docs-sync per the project constitution.

**LOW 3 — HOW knowledge file is stale.**
`.coding/knowledge/how/2026-08-26-git-tool-per-subcommand-parameter-reference.md`:
- Line 14: `args: ... READ queries only (status/diff/log)` — should add `branch list` and note the allowlist (positionals refused).
- Line 26 (branch row): `Optional` column omits `args`; `Runs` says `list: git branch` — should note `list: git branch [read-only flags]` and that `args` is now optional for list.
The file's own header says "verified 2026-09-09 against ... git.rs" — re-verify against the new code. (The plan's step 5.5 already intends this update; flagging so it isn't missed.)

**LOW 4 — DECISION knowledge file is partially superseded.**
`.coding/knowledge/decision/2026-08-27-git-tool-args-is-read-only-writes-reject-free-fo.md` line 6 states `args` is forwarded by "only status/diff/log" and lists `branch` among the write subcommands that "errors loudly on non-empty args." That is now only true for `branch create`/`delete` — `branch list` forwards allowlisted args. Supersede (or update) the record to reflect the allowlist path. (Plan step 5.5 already intends this; flagging per docs-sync.)

**LOW 5 (informational) — Combined short flags and a few bare forms are rejected (fail-closed); acceptable, consider a doc note.**
The allowlist matches exact strings and `--format=`/`--sort=`/`--color=` prefixes, so these valid read-only forms are REJECTED:
- Combined short flags: `-av` (=`-a -v`), `-va`, `-vq` — not exact, not a prefix → refused. Workaround: split into `["-a", "-v"]`.
- Bare `--color` (=`--color=auto`) — refused; `--color=always` and `--no-color` pass.
- `--contains=<commit>` / `--merged=<commit>` — refused; only the bare `--contains`/`--merged` (defaulting to HEAD) pass, so the contains/merged family can't target an arbitrary commit (a positional `<commit>` is also refused).

This is the *correct* posture for a security allowlist (fail-closed; trivial workarounds), so I do NOT consider it a defect. Optional improvement only: add a one-line note to the `args` schema description or module doc that combined short flags must be split, so an agent emitting `git branch -av` self-corrects without a round-trip. If you'd rather keep the surface minimal, leave as-is.

### Notes
- The plan's review focus asked specifically whether `-av`/`-vq` should be allowed: **acceptable to reject** (fail-closed, split workaround). Not a blocker.
- I could not execute `cargo test` (read-only reviewer). The plan reports a green, warning-free run; the code has no obvious warning sources and the new tests are well-formed. The main agent should re-run `cargo test` after addressing LOW 1 (message text change) before committing.
