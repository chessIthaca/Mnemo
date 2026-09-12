# Review: git tool read-args forwarding (branch fix/git-tool-read-args)

Scope: all uncommitted changes — `src/tool/agent/git.rs`, `frontend/src/components/chat/Message.tsx`,
`frontend/src/components/chat/messageArgLabel.test.ts` (new), `frontend/vitest.config.ts`, `README.md`
(+ `.coding/` bookkeeping, out of scope). Read-only review; no files edited, no shell run.

---

## HIGH

### 1. `git diff --no-index` escapes the project sandbox (arbitrary file read); auto-approved under AutoApproveProject
**Files:** `src/tool/agent/git.rs:68-79` (validate_read_args denylist), `src/agent/approval.rs:146-166` (`is_git_read_only`)

`validate_read_args` blocks flags that *write* files or *run commands* (`--output…`, `-o…`, `--exec…`),
but it does **not** block `--no-index`. `git diff --no-index <pathA> <pathB>` compares two arbitrary
paths **outside any repository** — so this call:

```json
{"subcommand": "diff", "args": ["--no-index", "/dev/null", "C:/Users/<user>/.ssh/id_rsa"]}
```

runs `git diff --no-index /dev/null C:/Users/.../id_rsa` and returns the target file's full contents
as added diff lines (capped only by the 100 KiB output cap). That is an arbitrary-file-read primitive
that bypasses the project-root confinement every other read tool enforces.

This is worse than it first appears because of the approval layer: `is_project_scoped("git", …)` →
`is_git_read_only` (approval.rs:135,146) classifies a call purely by `subcommand` and **never looks at
`args`**. Under `SafetyMode::AutoApproveProject` the call above is auto-approved with **no human
review** (approval.rs:81-87). In that mode the `shell` tool *always* prompts
(`is_project_scoped_shell_always_false`, approval.rs:510), so the forwarded-args path is now strictly
more permissive than the shell it is meant to replace.

Suggested fix (either or both):
- Add `--no-index` to the `validate_read_args` prefix denylist (one line; `--no-index` has no
  read-only variants, so prefix matching has no false positives).
- Defence in depth: make `is_git_read_only` return `false` when a non-empty `args` array is present,
  so flagged read queries still auto-run but arbitrary flag combinations get human review.

A regression test should assert `{"subcommand":"diff","args":["--no-index","a","b"]}` is rejected.

---

## MEDIUM

### 2. `--ext-diff` / `--textconv` are newly reachable and can run external commands from repo config
**File:** `src/tool/agent/git.rs:68-79`

The denylist's stated contract is "read queries may not write files or run commands", but two
command-execution flags slip through:

- `git diff --ext-diff` executes the external diff driver configured via `diff.external` /
  `GIT_EXTERNAL_DIFF`.
- `git diff --textconv` / `git log -p --textconv` force execution of configured
  `diff.<driver>.textconv` filters.

Mitigations: both require cooperating git config, and the git tool itself cannot write config.
However, `.git/` lives *inside* the project root, so if `file_write`'s sandbox does not exclude
`.git/config` (worth verifying), an agent under AutoApproveProject could plant `diff.external=<cmd>`
(auto-approved in-project write) and then trigger it via an auto-approved read query. Note the
`diff.external` half of this chain pre-exists via plain `git diff` — but `--ext-diff` and explicit
`--textconv` are *newly* reachable only through this change's arg forwarding. Recommend adding
`--ext-diff` and `--textconv` prefixes to the denylist for defence in depth (both are obscure for
the tool's intended use, so blocking costs nothing).

---

## LOW

### 3. Stale inline comment for the stash action list
**File:** `src/tool/agent/git.rs:363` — `// action defaults to "push"; supports push/pop/drop.`
The schema description (git.rs:221), error text (git.rs:375), and `GitArgs.action` doc (git.rs:47)
were all updated for the new `list` action, but this comment wasn't. One-word fix: `push/pop/drop/list`.

### 4. `stash list` (read-only) still prompts under AutoApproveProject
**File:** `src/agent/approval.rs:163-164` — `is_git_read_only` returns false for all of `stash`,
so the new read-only `stash list` requires approval in AutoApproveProject while `branch list`
auto-runs. Conservative (safe) direction and consistent with the pre-existing stash treatment, but
worth a follow-up consideration now that a read-only stash action exists. Not introduced by this
diff's semantics; no action required for this plan.

### 5. Empty `args: []` on write subcommands is silently ignored
**File:** `src/tool/agent/git.rs:277` — the guard is `is_some_and(|a| !a.is_empty())`, so
`{"subcommand":"commit","args":[]}` is accepted and the (empty) args are dropped, while the schema
says "Refused for write subcommands" without caveat. Behaviourally identical to omitting `args`
(nothing is dropped), so this is a cosmetic consistency note, not a defect.

---

## No findings / verified clean

**Denylist shape (besides the gaps above):**
- `-o` matching is sound: `short_output = starts_with("-o") && !starts_with("--")` (git.rs:70)
  correctly lets `--oneline` through while blocking `-o` / `-ox.patch` (and the discrete-argv form
  `-o file`). No read-only `-o…` short flags exist on `status`/`diff`/`log`, so no false positives.
- `--output` / `--exec` prefix matching is sound; the only collateral is the obscure read-only
  `--output-indicator-*` family — acceptable.
- Top-level git options cannot sneak through: `--git-dir`, `--work-tree`, `-C`, `-c`, `--config-env`
  placed after the subcommand are rejected by git's own parser as unrecognised subcommand options;
  `-c` after `log`/`diff` means the read-only `--cc`. Verified by construction; no code path lets an
  arg land before the subcommand.
- A `--` separator in args makes git treat subsequent flags as pathspecs, but `validate_read_args`
  scans *all* args regardless of position — stricter, not bypassable.
- Case variants (`--Output`) are unrecognised by git → error, not a bypass.

**Write-subcommand guard (git.rs:276-283):** fails closed — `matches!` is case-sensitive, so
`"Status"`/`"STATUS"` with args hits the guard's error; unknown subcommand + args errors before the
unknown-subcommand arm. Guard runs before any git invocation.

**argv assembly / borrows:** `read_argv`'s single-lifetime signature (git.rs:84) unifies to `extra`'s
borrow and is used within the arm scope — sound. The match scrutinee borrows `args.subcommand` while
arms partially move disjoint fields (`args.args`, `args.message`, `args.branch`) — legal field-level
splitting. Empty-vec args on read subcommands forwards nothing (defaults only). Error paths return
`ToolResult::error` before `run_git` (git.rs:288-305).

**Later-wins:** `git log --oneline -20 -1` resolves to `-1` (last `-n` wins) — the
`log_accepts_read_args` test asserts exactly this against real git. Repeated formatting flags
(`--oneline` + user `--format=`) degrade benignly.

**Serde:** `#[serde(default)] args: Option<Vec<String>>` — absent/`null` → `None`; non-array or
non-string elements → `invalid arguments` error before execution. The original defect (unknown field
silently dropped) is fixed because `args` is now a declared field.

**Regression tests genuinely fail on old code** (verified by reading): `log_accepts_read_args` (old
code drops `-1`, output contains "init" → assert fails), `diff_accepts_ref_and_flag_args` (old code
runs bare `git diff` on a clean tree → no "b.txt"), `read_args_reject_write_exec_flags` (old code
succeeds → `!result.success` fails), `args_rejected_on_write_subcommands` (old code's git-add failure
in a non-repo lacks "not supported"), `stash_list_action` (old code: "unknown stash action"). Both
rejection tests correctly run without `init_repo` (guards fire before git). `stash list` is consistent
across doc comment, schema, error text (except finding 3), and implementation.

**Frontend:** `argLabel` has exactly one definition (Message.tsx:298; internal callers at 454/594) —
exporting it collides with nothing. The args filter/join is type-safe (`Array.isArray` + `(a): a is
string` guard) and null-safe (missing subcommand → null; non-string/blank entries dropped). Message.tsx's
import chain (react, lucide-react, Markdown, CodeBlock, openFile, toolCardPaths) has no top-level
`window`/`document`/`localStorage` usage in chat components, so importing it under vitest's
`environment: "node"` is safe. The 4 tests cover the no-args/args/filtering/missing-subcommand matrix;
the file is registered in `vitest.config.ts:20`. Rejected calls (write subcommand + args) render their
attempted args in the card label — informative, harmless (React escapes text).

**Constitution compliance:** all new Rust fns are private but carry doc comments; the newly exported
TS `argLabel` keeps its doc comment. No new `#[allow]`, no new `cfg(windows)`, no Windows-only APIs
(the `"C:/evil.patch"` test string is denylist input, not path logic — platform-neutral). Docs synced:
README bullet (README.md:56), tool schema description + `args` property (git.rs:201-227), module doc
(git.rs:18-22). The module-doc rewrite ("standard approval flow" replacing "read ops are auto-run")
was checked against `needs_approval`/`is_git_read_only` and is accurate (auto-approval of reads under
AutoApproveProject is part of that flow).

**Build/tests:** I am read-only and could not run `cargo test`/`vitest`; nothing in the diff suggests
warnings under `#![deny(warnings)]` (no unused imports/variables; `is_some_and` is stable since 1.70,
edition 2021). Parent should confirm green runs.

---

## Summary

1 high (denylist gap: `--no-index` sandbox escape, amplified by `is_git_read_only` ignoring `args`),
1 medium (`--ext-diff`/`--textconv` config-driven exec), 3 low (stale comment; stash-list approval
consistency note; empty-args cosmetic). Everything else reviewed clean.
