# Project Constitution

Project-specific hard rules for this project. The agent cannot ignore these.
They are loaded before every LLM turn, after the global constitution.

Universal workflow + review + closing-sequence rules (never commit to main,
core-operations approval gate, bookkeeping tools never prompt, never resend a
failed tool call, preserve line endings, final-summary length, the full
test→review→fix→commit→finish closing sequence) are built into the app's
compiled system prompt and apply to every project — they are NOT repeated here.
This file holds only this project's policy: environment, language, and test
command.

## Environment

- This is a **Windows 11** machine. Use Windows paths
  (`C:\AgenticCoder\AgenticCoder\src\...`) and PowerShell syntax for shell
  commands — never Linux paths (`/c/AgenticCoder`) or bash syntax in the
  `shell` tool.
- The project root is `C:\AgenticCoder\AgenticCoder`.
- Never commit to `main` — commit to the current feature branch.
- `merge_to_main` (the only sanctioned way anything reaches `main`) syncs `main` with `origin` first (`git fetch` + `git pull --no-rebase` — on `shell`, the `git` tool having no fetch/pull subcommand), then merges the branch. `git` core operations (`merge`/`push`) MUST go through the approval-gated `git` tool, and a shell-invoked `git merge`/`push` is gated identically (`GitTool::never_auto_for` covers the tool path; since 2027-01-11 `ShellTool::never_auto_for` matches a `git` executable in a command position) — no unprompted run in Autonomous mode either way.
- **Do not pipe a command through a cmdlet and then trust the reported exit
  code** — it's the *pipe's* code, not the command's, and can read `1` even
  when the command succeeded. To check whether a command actually failed,
  run it unpiped and read `$LASTEXITCODE` directly:

  ```powershell
  cargo test
  "exit=$LASTEXITCODE"
  ```

  …or, if you must filter noise, rely on the command's own `test result:` /
  `error:` lines rather than the exit code. A non-zero code from a piped
  invocation is **not** by itself evidence of failure.

## Code style

- Follow the existing code style in this repository.
- All public functions must have doc comments.
- Run `cargo test` before marking a workflow step complete.
- For every defect (bug) you fix, add a regression test that reproduces the
  defect and asserts the fix. The test must fail without the fix and pass with
  it, so the defect can never silently reappear.
- The build must be **warning-free**: under `#![deny(warnings)]` (set at both
  crate roots, `src/lib.rs` and `src-tauri/src/main.rs`) any warning fails the
  build, so a green `cargo test` already proves there are zero warnings. Never
  add `#[allow(...)]` to silence a warning — fix the root cause (remove the dead
  code, drop the unused import, drop the unneeded `mut`, etc.).

## Documentation expectations

- When a bug_fixing fix turns out FEATURE-SCALE mid-flight (new pub types,
  cross-module surface, concurrency invariants — the plan-time steering
  already sends bug-triggered features to kind=implementation; this covers
  the contained-at-plan-time cases that grew), the agent MUST, BEFORE the
  verify step: (1) amend the plan context via `update_plan` append with a
  paragraph starting "Landed design —" carrying the architecture,
  deviations from the sketch, measured numbers, and invariants to
  preserve, and (2) record `landed_design: true` via `update_plan`. At
  finish, in addition to the auto-captured BUG digest: write a SPEC memory
  documenting the architecture + invariants, and amend the BUG record with
  the landed outcome. The finish gate blocks a `landed_design` plan whose
  context carries no "Landed design" amendment.

## File mutation policy

- File mutation goes through the file tools — `file_edit` (targeted change)
  and `file_write` (whole file, or `mode:"append"` for chunks) — not shell
  one-liners (`Set-Content`, `Out-File`, `Add-Content`, `>>`, `sed -i`,
  `tee`, python scripts). The file tools preview diffs for approval, preserve
  line endings, and feed the stale-read gate; shell surgery bypasses all of
  that.
- Shell-based mutation is a last resort, permitted only for (a) paths the
  file tools refuse by design (`.coding/knowledge/**` — use `memory_amend`
  for amendments, or `memory_update`'s `find`/`replace_with` targeted repair
  to fix stray text in a record body) or (b) a verified file-tool freeze
  (repeated drift errors on verified-identical text after a genuine fresh
  read). State the justification when you fall back to it.
- `file_edit` batch mode (`edits`) applies several edits to one file
  atomically — one write, one combined diff; prefer it over N separate calls.

## Branch policy (working-branch topology)

- One `wt/*` working branch per agent directory (worktree), reused across plans — the working line; `main` is protected and only ever receives merge commits (via the `merge_to_main` skill: `wt/*` → `main`; the `new_release` skill performs the same sanctioned merge inline as its step 2). The skill syncs `main` with `origin` first (`git fetch` + `git pull --no-rebase`, before the branch merge — a stale `main` otherwise surfaces later as a rejected push), and only the sync runs on the shell tool; the merge and push stay on the approval-gated `git` tool. The old `develop` integration tier was removed (2026-08-24).
- `create_plan` auto-forks the per-directory `wt/*` branch from `main` when on `main` (no `branch` arg) and reuses the current branch when already on one — work never silently stays on `main`. The `branch` param is reserved for an explicit user request for a specific branch name; the agent never passes it automatically. `merge_to_main` deletes the merged branch after landing it, so no branches accumulate.
- `.coding/` is the mergeable side-car: knowledge files (`.coding/knowledge/`), plans, reviews, and `backlog.jsonl` (git union merge driver) travel with git and merge across instances; `memory.db`/`codegraph.db` (rebuildable caches) and `plans/stack.json` + `instance.json` (per-instance local state) are gitignored — never committed.
- After a git merge, the memory index converges automatically at the next project open (startup reconciliation re-derives on content-hash drift); in-session, Settings → Memory → "Rebuild index from files".
- Never commit to main — commit to the current feature branch.
- Parallel run-all (plan ffd7a86f) adds per-item worktree branches
  (`wt/runall-<item8>`, forked from main into `.worktrees/runall-<item8>`)
  for concurrently dispatched backlog items. These are app-managed: the
  app lands them via serialized `--no-ff` merges into main (the
  merge_to_main skill cannot run from a linked worktree), so main still
  only ever receives merge commits. Spawned items' reviewer reports land
  in the main tree's `.coding/reviews/` (shared by design); they are not
  carried by the item's branch merge.

## Review expectations

The reviewer (the read-only subagent spawned before every implementation plan
completes) must check the things below in addition to code correctness,
bugs, and security. Universal review mechanics (spawning, report location, the
fix-every-finding rule) live in the compiled prompt — this section only adds
project-specific checks.

- **Documentation sync.** The reviewer checks whether the change requires
  documentation updates — `README.md` (feature lists, config examples),
  `PLAN.md` (technical decisions, provider strategy), module doc comments, and
  user-facing config/`endpoints.toml` examples — and reports missing or
  stale docs as findings. A feature that ships with its docs not updated is
  an incomplete change.
- **Multi-platform neutrality.** The reviewer verifies the change builds and
  behaves on **both macOS and Windows** — no Windows-only APIs, paths, or
  shell syntax in library or app code (the one sanctioned exception is the
  existing WebView2 Browser tab / `game_*` tooling, which is Windows-only by
  design and gated accordingly). `cfg(windows)`-only additions outside that
  gate are findings; so is anything that assumes the app runs on Windows.
- **File-tools-first policy.** The reviewer flags shell-based file mutation
  (`Set-Content`/`Out-File`/`Add-Content`/`>>`/`sed -i`/`tee`/python scripts)
  in non-test, non-generated code where `file_edit`/`file_write` would work —
  and missing justification where a fallback was genuinely needed.
- **Reviewer tool surface + failed-reviewer protocol.** A `role:"reviewer"`
  subagent is read-only by construction: reads + `git_diff`/`git_log`/
  `git_show`/`web_fetch` + the graph tools + memory/backlog **query** tools +
  `write_review_report`. It cannot ask questions (`ask_user`), mutate memory
  or backlog, run plan tools, or `finish`. **Reviewer-only authorship:** a
  review report can only ever be authored by a spawned `role:"reviewer"`
  agent — `write_review_report` is visible under no other filter, and
  `.coding/reviews/` is protected from the file tools (the one residual is an
  approval-gated `shell` call — the standing residual, as with
  `.coding/plans/`), so the main agent can never write a review itself. If a reviewer fails **without writing a
  report**, it is NOT done: never respawn it blindly — `ask_user` and let
  the user choose (retry on another model / abandon the review).
  `spawn_agent` refuses a blind reviewer respawn while a reviewer failure is
  pending. An un-completable review is escaped via `abandon_plan` (returns
  to Planning) — the main agent is never stuck in Reviewing.
