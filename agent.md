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
- `merge_to_main` (the only sanctioned way anything reaches `main`) syncs `main` with `origin` first (`git fetch` + `git pull --no-rebase` — on `shell`, the `git` tool having no fetch/pull subcommand), then merges the branch — on repos WITHOUT a main-protection ruleset. Since 2026-09-21 this repo's `main` is protected by ruleset 23755694 (pull_request, required_approving_review_count: 1): the skill detects it up-front (`gh api .../rulesets`), pushes the working branch, opens a PR against `main`, reports the URL, and STOPS — the human approves and merges (the author cannot self-approve); never bypass (no force-push/admin/ruleset edits). The **Mnemo1 agent workflow** keeps the hard gate: `git` core operations (`merge`/`push`) performed by the agent MUST go through the approval-gated `git` tool, and a shell-invoked `git merge`/`push` is gated identically (`GitTool::never_auto_for` covers the tool path; since 2027-01-11 `ShellTool::never_auto_for` matches a `git` executable in a command position) — no unprompted run in Autonomous mode either way. **Mnemo2** is an application-level integration queue, not an Mnemo1 agent workflow; its scoped merge policy is in **Mnemo2 team merge policy** under Branch policy. The skill also writes its landing records BEFORE the final commit — never after the push (see the record-phrasing rule in Branch policy).
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
- Run `cargo test --workspace` before marking a workflow step complete
  (root-only `cargo test` builds only the root package — it never compiles
  src-tauri, so app-crate breaks pass unnoticed; job 4 D9 review round 1,
  commit 3373096).
- A **feature-gated** dependency bump additionally requires the feature-enabled
  run — `cargo test --features embeddings` (fastembed/ONNX) or `--features
  browser` (chromiumoxide), whichever tree the bump touches. Plain `cargo test`
  never compiles those paths, and no **pull-request-triggered** job does
  either: `codeql.yml`'s Rust analysis is no-build-only, and `build.yml` — which
  *does* compile them, via `cargo test --workspace` with src-tauri selecting
  both features — triggers only on manual dispatch and `v*` tags. A PR can
  therefore merge a broken feature tree unnoticed until the next tag build:
  dependabot PR #67 (2026-09-30) showed a green "Analyze (rust)" check while
  `cargo check --features embeddings` exited 101 — fastembed 4.9.1 -> 7.1.0
  turned `InitOptions` into a deprecated alias (fatal under
  `#![deny(warnings)]`) and gave `TextEmbedding::embed` a `&mut self` receiver.
- For every defect (bug) you fix, add a regression test that reproduces the
  defect and asserts the fix. The test must fail without the fix and pass with
  it, so the defect can never silently reappear.
- The red-check must exercise the real artifact where practical. When the
  faithful red-check would require mutating a large or costly artifact (for
  example regenerating `Cargo.lock`), prove red on a **gitignored doctored
  copy** of the real file (e.g. `.coding/tmp/redcheck/`), swapped in via a
  temporary uncommitted pointer edit — a plan-documented deviation with the
  evidence in the commit message. Ratified 2027-01-11; precedent: plan
  f0758c0d (`cargo_lock_keeps_the_vendored_patches`, item 6e071b07).
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
- `file_edit` batch mode (`ops`) applies several edits to one file
  atomically — compact line ops (`i`/`b`/`d`/`r` verbs with ranges and
  payloads) and anchor items in one array, in order, one write, one combined
  diff; prefer it over N separate calls. For edits spanning several files
  under ONE approval, use `multi_edit` (`files: [{path, ops}]`,
  all-or-nothing).

## Branch policy (working-branch topology)

- One `wt/*` working branch per agent directory (worktree), reused across plans — the working line; `main` is protected and only receives merges — directly via the `merge_to_main` skill on unprotected repos, or through the human's chosen PR merge method where a main-protection ruleset applies (this repo since 2026-09-21, ruleset 23755694: the skill detects it up-front, pushes the branch, opens the PR, and stops for the human approving review; the `new_release` skill performs the same ruleset-aware landing inline as its step 2). On the direct path the skill syncs `main` with `origin` first (`git fetch` + `git pull --no-rebase`, before the branch merge — a stale `main` otherwise surfaces later as a rejected push); the sync runs on the shell tool, the merge and push on the approval-gated `git` tool (the PR path's branch push runs on the `git` tool, equally gated). The old `develop` integration tier was removed (2026-08-24).
- **Landing bookkeeping is written BEFORE the commit (2026-09-26).** `merge_to_main` — and `new_release` inline, as its step 2 — writes its status records while still on the branch and commits them into that branch (order: write records → commit → land), because a write issued after the push can never land: protected `main` rejects it and the branch that could have carried it is merged and deleted, so it dangles uncommitted and rides an unrelated branch. For the same reason landing records are phrased on pre-landing facts — title tail `— MERGED into main (branch <name>, <date>)`, body naming the branch's work tip, never the merge sha (a commit cannot contain its own hash) and never the PR number (it exists only after `gh pr create`); on the PR path the record ships inside the PR, so it becomes true exactly when the human merges. The skill's final check is an empty `git status --short`.
- `create_plan` auto-forks the per-directory `wt/*` branch from `main` when on `main` (no `branch` arg) and reuses the current branch when already on one — work never silently stays on `main`. The `branch` param is reserved for an explicit user request for a specific branch name; the agent never passes it automatically. On the direct path `merge_to_main` deletes the merged branch after landing it; on the PR path the branch is deleted after the human merges the PR — no branches accumulate either way.
- `.coding/` is the mergeable side-car: knowledge files (`.coding/knowledge/`), plans, reviews, and `backlog.jsonl` (git union merge driver) travel with git and merge across instances; `memory.db`/`codegraph.db` (rebuildable caches) and `plans/stack.json` + `instance.json` (per-instance local state) are gitignored — never committed.
- After a git merge, the memory index converges automatically at the next project open (startup reconciliation re-derives on content-hash drift); in-session, Settings → Memory → "Rebuild index from files".
- Never commit to main — commit to the current feature branch.
- Parallel run-all (plan ffd7a86f) adds per-item worktree branches
  (`wt/runall-<item8>`, forked from main into `.worktrees/runall-<item8>`)
  for concurrently dispatched backlog items. These are app-managed: the
  app lands them itself (the merge_to_main skill cannot run from a linked
  worktree), ruleset-aware since backlog b52b041a: an UNPROTECTED main
  gets serialized `--no-ff` merges into local main (a local merge only —
  it never pushed; the old "pushes main directly" note here was
  inaccurate), so main still only ever receives merge commits; a
  PROTECTED main (ACTIVE pull_request ruleset, detected up-front via
  `gh api repos/{owner}/{repo}/rulesets`) gets the branch pushed + a PR
  opened (`gh pr create --base main --head <branch>`), the URL reported
  on the item, and the branch kept for the human merge — swept at the
  next run-start once merged into `origin/main` (backlog 64662ef2) —
  never bypass.
  Spawned items' reviewer reports land
  in the main tree's `.coding/reviews/` (shared by design); they are not
  carried by the item's branch merge.

### Mnemo2 team merge policy (supervisor/worker app)

Mnemo2 is a separate application built on the headless engine, not an Mnemo1 agent workflow. Its **TeamManager** may auto-merge a completed worker branch into the lead's **local working branch** only after all of the following hold:

- the worker's turn finished successfully and its checks passed **on the branch tip being merged** — after any `review_fix` or fixer commit, the affected check scope (for this repo `cargo test --workspace`) is re-run and passes;
- the lead reviewed the branch and **every finding is closed** (fixed) or explicitly marked `wontfix` with a written justification;
- the review carries structured evidence for the exact merged tip: `unanchored_pass: true`, nonempty `intent`, `execution`, `integration`, `test_quality`, and `residual_risk` (see **Mnemo2 review evidence** below);
- for `review_risk: "high"`, a completed read-only audit is linked to the same tip and the lead recorded a nonempty adjudication;
- the lead's working tree is clean and the lead is idle; the merge is serialized (one branch at a time).

It halts on conflicts, never pushes, and never merges into `main`. Landing to `main` remains the repository's protected-PR path (ruleset 23755694; the human approves the PR). Worker branches are per-agent `wt/agent-*` worktrees forked from the lead's working-branch tip at dispatch; a base that goes stale while a worker runs is never auto-rebased — the conflict-halt and route-back rule handles it. The TeamManager performs the merge, not the agent's `git` tool. The compiled agent git gate still applies to the agent's own `git merge`/`git push` calls.

### Mnemo2 review evidence and independent audits

This policy is **Mnemo2 only**. Mnemo1's read-only reviewer workflow in **Review expectations** below is unchanged: this section neither narrows nor replaces it. The lead remains Mnemo2's reviewer of record, but a worker summary plus passing checks is not a review — the lead must record what it actually inspected and concluded.

**Acceptance scenarios first.** A new plan that will dispatch `review_risk: "high"` MUST contain a `## Acceptance scenarios` section with at least one **observable** bullet — a command, a persistence state, a concurrency outcome, a security property, or a UI behavior a reviewer can check without trusting the worker. Other plans are encouraged to include the section. `team_dispatch` with `review_risk: "high"` is rejected when that section is missing or has no observable bullet. Scenarios are written before implementation and re-verified at closing integration; they are never restated from the worker's own claims.

**Unanchored pass, then evidence.** For every completion the lead first reads the target diff *unanchored* — `git_read` op `diff`/`show`, the Diff tab, or `git diff <base>..<tip>` — and forms its own opinion **before** reading the worker's review summary, findings, or test claims. It then records one evidence object on the review:

- `unanchored_pass` — `true` only when that raw-diff pass happened for the exact tip under review.
- `intent` — what the change was meant to do and whether the diff matches the plan and its acceptance scenarios. Quote the diff, not the brief.
- `execution` — the real execution path: changed call sites, error handling, and what can now run differently.
- `integration` — interactions with existing contracts, persistence, concurrency, and neighbouring modules — who calls what changed.
- `test_quality` — whether the tests genuinely exercise the change, including a red-before/green-after test for a defect fix; a passing count alone is insufficient.
- `residual_risk` — what remains unverified, out of scope, or dependent on an assumption.

A **standard** review requires `unanchored_pass: true` and all five assessment strings nonempty. A high-risk review additionally requires `independent_reviewer_id` and `audit_adjudication`. A no-findings result still needs a **substantive rationale**: what was inspected, at which tip, and why the delta is believed correct. "Looks good", a restated worker summary, or a test count is not a rationale and does not satisfy the gate.

**Risk classification.** The lead sets `review_risk: "high"` (through `team_dispatch`; default `"standard"`) for **shared contracts, persistence, concurrency, security, and large or cross-cutting work**. These are exactly the changes where the lead's implementation context is most likely to bias the review.

**Independent audit for high risk.** A high-risk worker cannot merge on lead review alone. `team_audit_dispatch` takes `{ worker_id, reviewer_id, model?, reasoning_effort?, brief? }` and creates a fresh read-only **audit worker** against the implementation worker's current branch/tip and plan context, with **no plan step** and **without** the implementation worker's summary, findings, or conclusions. The audit worker is read-only by construction: it cannot edit files, commit, merge, spawn work, ask the user, or tick a plan step. Its completion notice asks the lead to **adjudicate the target review**, never to merge the reviewer. Fresh context is mandatory for independence; a different model is preferred when one is available, but a different model is not required.

**Adjudication and staleness.** The lead links the completed audit with `review.evidence.independent_reviewer_id` and records a nonempty `audit_adjudication`: what the audit found, what the lead accepted or rejected, and why. At both review and merge the host validates that the linked worker is an `audit` worker, targets the same implementation worker, inspected the exact tip, and is terminal non-failed. Worker records carry `kind` (`"implementation"` / `"audit"`, default implementation), `review_risk`, optional `audit_target`, and `audit_tip` (the implementation tip the reviewer inspected). Any fix commit — `review_fix`, a worker fix, or a routed fixer — invalidates the prior lead review and makes the previous audit tip **stale**; the review (and, for high risk, a fresh audit at the new tip) must be repeated. Legacy ledger records deserialize as `kind: "implementation"` / `review_risk: "standard"` with no evidence, so an in-flight worker from before this policy needs a fresh review — plus an audit if its risk is high — before merge.

**Closing verification.** The closing review keeps the structured evidence rather than discarding it at merge, and the lead re-verifies the original user scenarios end to end on the integrated tree — including the plan's `## Acceptance scenarios` when present — before `finish`. A closing record that only cites passing tests does not close the plan.

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
- **Re-review rounds are delta-scoped (backlog 85313a7e).** Every reviewer
  spawn carries a harness-rendered preamble: the verdict contract, the checks
  above as one line each, and the `.coding/**` rule (an accuracy check in one
  line — never a line review; generated/lockfile noise is out of scope). The
  app stamps each round's base revision on the plan frame (`## Reviews` in
  `.coding/plans/<id>.md`) and, from round 2 on, the preamble names that base,
  the delta scope and the mechanically-derived changed file set —
  so a re-review verifies the DELTA, not the whole tree. It narrows only when
  verified work is committed: **commit code at step boundaries on `wt/*`** (the
  closing sequence still commits last, with the report) so `git diff <base>` is
  exactly the fix. Never hand-write the preamble or the scope — the spawn path
  renders it, and re-writing it by hand only bloats the prompt.
- **Pre-flight self-check before ANY reviewer spawn.** Run this section's
  checklist against the diff yourself first — documentation sync, no
  `cfg(windows)`-only additions outside the sanctioned gate, file-tools-first,
  and a bug fix's regression test genuinely red before the fix — and fix what
  it finds before spawning. Minutes of own work beats a whole review round
  (plan febcd6f5 spent three rounds, ~33 min, on a 1-3 file fix). Keep the
  reviewer's `task` to scope + acceptance criteria + risk focus; the preamble
  already carries the boilerplate, and the reviewer can read the plan file.
- **Reviewer tool surface + failed-reviewer protocol.** A `role:"reviewer"`
  subagent is read-only by construction: reads + `git_read` (op `diff`/`log`/
  `show`/`status`), `web_fetch`, the graph tools, memory/backlog **query** tools
  + `write_review_report`. It cannot ask questions (`ask_user`), mutate memory
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
