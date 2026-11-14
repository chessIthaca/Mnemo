# Project Constitution

Project-specific hard rules. Every agent working in this repository follows them.

Mnemo2 reads this file into the prompt of its lead and of every worker. Their role rules (planning, dispatch, review and merge for the lead; implementing, checking and committing for a worker) are built into Mnemo2 and are not repeated here. This file holds this project's policy. The Mnemo2 team section below is wrapped in team-scope markers, so only Mnemo2's lead and workers receive it.

## Environment

- This is a **Windows 11** machine. Use Windows paths and PowerShell syntax for
  shell commands, never Linux paths or bash syntax in the `shell` tool. The
  project root is the folder the session opened.
- Never commit to `main`. Work happens on the lead branch (`wt/mnemo`) and on worker branches (`wt/agent-*`); **Branch policy** says how work reaches `main`.
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
- Before you commit, run `cargo test --workspace` and record it as a check. A root-only `cargo test` builds only the root `mnemo` package and never compiles `src-tauri`, so app-crate breaks pass unnoticed. A change under `src-tauri/src/mnemo2/` or `src-tauri/mnemo2-app/` also needs `cargo test -p mnemo-app --features mnemo2 --lib`; a change under `mnemo2/` also needs `npm test --workspace mnemo2` and `npm run check --workspace mnemo2`. Keep both clippy runs clean: `cargo clippy --workspace --all-targets` and `cargo clippy -p mnemo-app --all-targets --features mnemo2`. CI's `check` job (`.github/workflows/ci.yml`) runs the cargo commands, `npm run build:mnemo2` and `npm test --workspace mnemo2` on every pull request.
- A **feature-gated** dependency bump — the `embeddings` tree (fastembed/ONNX) or the `browser` tree (chromiumoxide) — must be checked by a build that enables the feature. `cargo test --workspace` does, because `src-tauri` depends on `mnemo` with both features; a plain `cargo test` or `cargo test -p mnemo` does not. Precedent: dependabot PR #67 (2026-09-30) passed a no-build CodeQL check while `cargo check --features embeddings` failed (fastembed 7 deprecated `InitOptions`, fatal under `#![deny(warnings)]`).
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
- The build must be **warning-free**: under `#![deny(warnings)]` (set at the
  crate roots `src/lib.rs`, `src-tauri/src/lib.rs` and
  `src-tauri/mnemo2-app/src/main.rs`) any warning fails the build, so a green
  `cargo test` already proves there are zero warnings. Never add `#[allow(...)]`
  to silence a warning — fix the root cause (remove the dead code, drop the
  unused import, drop the unneeded `mut`, etc.).
- **Deletion hygiene — pre-flight import-orphan check.** After any deletion
  edit, for every symbol named in the touched files' `use` statements, run a
  literal search (glob the file, engine: index) for remaining use sites BEFORE
  the first battery run, and trim every orphaned import in the same edit —
  under `#![deny(warnings)]` each orphan is a hard build failure, so one
  search pass replaces N build cycles (2027-01-14, job 2c step 2c.4 on
  wt/mnemo, commit 6fe6768: deleting `SteeringStats::shared()` orphaned
  `OnceLock`, then `Arc`, one at a time — two full cargo cycles burned on a
  mechanically predictable failure).

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

- **Topology.** The lead works on one long-lived lead branch (`wt/mnemo` in this repository). Each worker gets its own `wt/agent-*` branch and `.worktrees/agent-*` checkout, forked from the lead branch tip. Mnemo2 merges a reviewed worker into the lead branch with `--no-ff` and then deletes the worker's branch and checkout. Nothing merges into `main` locally.
- **Landing on `main`.** `main` is protected by two GitHub rulesets: `main-protection` (id 24182945: changes only through a pull request, 0 required approvals, no deletion, no force-push) and `ci-check-required` (id 27026992: the `check` status from `.github/workflows/ci.yml` must pass; only an admin can bypass it, and only when merging a pull request). To land, the lead pushes the lead branch, opens a pull request into `main` and enables auto-merge with the merge method **merge**, so the lead branch's history stays joined with `main`; GitHub merges it once `check` passes. Never force-push, bypass a ruleset or edit one.
- `.coding/` is the mergeable side-car: knowledge files (`.coding/knowledge/`), plans, reviews, and `backlog.jsonl` (git union merge driver) travel with git and merge across instances; `memory.db`/`codegraph.db` (rebuildable caches) and `plans/stack.json` + `instance.json` (per-instance local state) are gitignored — never committed. Mnemo2's per-project state (lead transcript, worker ledger, preferences) lives in `.coding/mnemo2/`, which carries its own `.gitignore` (`*`) and is never committed.
- After a git merge, the memory index converges automatically at the next project open (startup reconciliation re-derives on content-hash drift); to rebuild it by hand, open the Memory tab → Diagnostics → Memory upkeep → **Rebuild index**.

<!-- mnemo:team -->
### Mnemo2 team merge policy (supervisor/worker app)

Mnemo2's **TeamManager** may merge a completed worker branch into the lead's **local working branch** only after all of the following hold:

- the worker's turn finished successfully and its checks passed **on the branch tip being merged** — after any `review_fix` or fixer commit, the affected check scope (for this repo `cargo test --workspace`) is re-run and passes;
- the lead reviewed the branch and **every finding is closed** (fixed) or explicitly marked `wontfix` with a written justification;
- the review carries structured evidence for the exact merged tip: `unanchored_pass: true`, nonempty `intent`, `execution`, `integration`, `test_quality`, and `residual_risk` (see **Mnemo2 review evidence** below);
- for `review_risk: "high"`, a completed read-only audit is linked to the same tip and the lead recorded a nonempty adjudication;
- the lead's working tree is clean and the lead is idle; the merge is serialized (one branch at a time).

It halts on conflicts, never pushes, and never merges into `main`. Landing on `main` is the pull-request path in **Branch policy** above. Worker branches are per-agent `wt/agent-*` worktrees forked from the lead's working-branch tip at dispatch; a base that goes stale while a worker runs is never auto-rebased — the conflict-halt and route-back rule handles it. The TeamManager performs the merge, not the agent's `git` tool. The compiled agent git gate still applies to the agent's own `git merge`/`git push` calls.

### Mnemo2 review evidence and independent audits

The lead is Mnemo2's reviewer of record, but a worker summary plus passing checks is not a review — the lead must record what it actually inspected and concluded. The same policy, written for people, is in `docs/REVIEW-POLICY.md`.

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

**Independent audit for high risk.** A high-risk worker cannot merge on lead review alone. `team_audit_dispatch` takes `{ worker_id, reviewer_id, tier?, model?, reasoning_effort?, brief? }` and creates a fresh read-only **audit worker** against the implementation worker's current branch/tip and plan context, with **no plan step** and **without** the implementation worker's summary, findings, or conclusions. The audit runs on the `high` tier unless `tier` names another. The audit worker is read-only by construction: it cannot edit files, commit, merge, spawn work, ask the user, or tick a plan step. Its completion notice asks the lead to **adjudicate the target review**, never to merge the reviewer. Fresh context is mandatory for independence; a different model is preferred when one is available, but a different model is not required.

**Adjudication and staleness.** The lead links the completed audit with `review.evidence.independent_reviewer_id` and records a nonempty `audit_adjudication`: what the audit found, what the lead accepted or rejected, and why. At both review and merge the host validates that the linked worker is an `audit` worker, targets the same implementation worker, inspected the exact tip, and is terminal non-failed. Worker records carry `kind` (`"implementation"` / `"audit"`, default implementation), `review_risk`, optional `audit_target`, and `audit_tip` (the implementation tip the reviewer inspected). Any fix commit — `review_fix`, a worker fix, or a routed fixer — invalidates the prior lead review and makes the previous audit tip **stale**; the review (and, for high risk, a fresh audit at the new tip) must be repeated. Legacy ledger records deserialize as `kind: "implementation"` / `review_risk: "standard"` with no evidence, so an in-flight worker from before this policy needs a fresh review — plus an audit if its risk is high — before merge.

**Closing verification.** The closing review keeps the structured evidence rather than discarding it at merge, and the lead re-verifies the original user scenarios end to end on the integrated tree — including the plan's `## Acceptance scenarios` when present — before the plan closes. A closing record that only cites passing tests does not close the plan.
<!-- /mnemo:team -->

## Review expectations

The lead checks these in every review, in addition to correctness, bugs and security, and records what it found in the review evidence.

A worker runs the same checklist against its own diff before it commits and reports what it checked.

- **Documentation sync.** A change to behaviour, settings or commands updates the docs that describe them: `README.md`, `docs/FEATURES.md`, `docs/CONFIGURATION.md`, `docs/REVIEW-POLICY.md`, `mnemo2/README.md`, `mnemo2/plugin/README.md` and its skills, module doc comments, and config examples. Missing or stale docs are a finding.
- **Multi-platform neutrality.** The change builds and behaves on both macOS and Windows: no Windows-only APIs, paths or shell syntax in library or app code. The one sanctioned exception is the WebView2 Browser pane and its tools, which are Windows-only by design and gated with `cfg(windows)`. Any other `cfg(windows)`-only addition is a finding.
- **File tools first.** Shell-based file mutation (`Set-Content`, `Out-File`, `Add-Content`, `>>`, `sed -i`, `tee`, python scripts) where `file_edit`/`file_write` would work is a finding, and so is a fallback without a stated justification.
- **Regression test.** A defect fix carries a regression test that fails without the fix and passes with it (see Code style).
- **Checks on the exact commit.** The checks recorded for a review ran on the commit being merged (see the Mnemo2 team merge policy).
