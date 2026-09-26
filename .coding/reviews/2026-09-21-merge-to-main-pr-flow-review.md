## Verdict: FINDINGS (1 high, 6 low)

Review of ALL uncommitted changes on `wt/macos-fix` for plan 8ad6fa91 (backlog 006ee7d6) — the ruleset-aware PR flow for merge_to_main + new_release, agent.md, memory records, and the worktrees.rs follow-up backlog item.

The core rework is sound: the test-pinned prompt substrings are genuinely first-in-order (traced against the actual prompt text, not just the green test run), the two-path detection → direct/PR logic, existing-PR handling, STOP semantics, the no-bypass rule, the tag-ref answer, agent.md's four edits, the memory amendments, and the follow-up backlog item are all correct and mutually consistent. One high finding: the GH013 recovery path is mechanically broken as written — the direct path deletes the branch *before* the push whose rejection it is supposed to recover from. Six low findings follow (dropped "optional" on the direct-path push, new_release's PR path missing the branch-build gate, stale user-facing docs, memory-record date inconsistencies, an agent.md accuracy nit, and an unpinned-behavior test gap). No Rust source changed; the reported green `cargo test --workspace` is consistent with the diff (the skill TOML rides `include_str!` and its parse/pin tests are green).

Note on evidence: ruleset facts (enforcement active, conditions `~DEFAULT_BRANCH` only, `required_approving_review_count: 1`, `allowed_merge_methods` merge/squash/rebase, PR #3 open) are taken from the task's recorded live `gh` verification — this reviewer is read-only and could not re-run `gh`.


## Verified correct (evidence-based)

**(a) Test-pinned substrings — genuinely first-in-order.** Traced the actual prompt text in `.coding/skills/merge_to_main.toml` (lines 69-75): step 2 (ruleset check) contains only `gh repo view --json nameWithOwner` and `git remote get-url origin` — neither contains `git fetch origin`, `git pull --no-rebase`, or `--no-ff <branch>`. All three pinned strings first occur inside step 3 (DIRECT path), in the asserted order: `git fetch origin` → `git pull --no-rebase` → `git merge --no-ff <branch>`. `MERGED into main` first occurs in step 5; step 4's `MERGED-at-<sha>` does not contain it. `shipped_skill_files_parse_and_merge_to_main_cleans_up_memories` (src/skill/mod.rs:471-513) therefore passes for the right reason, not by accident. The embedded copy is `include_str!("../../.coding/skills/merge_to_main.toml")` (src/skill/mod.rs:286) — one source of truth, no dual-maintenance gap. Both TOMLs are well-formed (multi-line basic strings legal, trailing `\` line-continuation before the closing `"""` preserved from the original).

**(b) Two-path logic.** The detection criterion (an ACTIVE ruleset with a `pull_request` rule, via `gh api repos/<owner>/<repo>/rulesets`) matches the live ruleset shape including enforcement state; gh-fails/no-ruleset → direct path preserves the skill for unprotected installs; existing-PR handling ("A PR already exists for the branch → report its URL") covers the live PR #3 case; STOP semantics are coherent (stop the *merge attempt*, then step 5's PR-path memory supersede, then step 6 `skill_end`); the no-bypass rule is stated consistently in the prompt, the comment block, and agent.md. Failing OPEN to the direct path is the right portability trade-off for a skill that ships to other installs (gh may be absent on unprotected repos) — the GH013 net is the right safety net for that choice, but the net itself is broken (H1).

**(c) new_release step 2.** Structurally consistent with merge_to_main (same detection, same push-branch + `gh pr create` + report + STOP); the two-phase release is coherent: bump → PR → human merge → re-run ("on re-run main already carries the bump → continue to step 3"), and the tag push sits outside the ruleset's `~DEFAULT_BRANCH` conditions (verified live per the task). Gaps: L2.

**(d) agent.md.** The four edits (line 22 Environment, line 85 Branch policy, line 86 branch deletion, lines 90-97 run-all worktrees note) are accurate and consistent with the skill files and the backlog item — except L5's nit.

**(e) Memory.** The two amendments and the new DECISION record match the shipped skill files factually (detection, PR flow, STOP, no-bypass, direct path preserved, tag answer, worktrees follow-up, the test invariant) — except the GH013-recovery claim (H1) and the dates (L4).

**(f) Backlog.** Item b52b041a is well-formed and self-sufficient: headline 79 chars (≤100), body carries problem + repro + fix direction + acceptance criteria + pointers with real paths; the `src/project/worktrees.rs` "mirroring the merge_to_main skill" comments around lines 158-274 exist as pointed to. The 006ee7d6 `in_flight` flip carries the plan linkage — correct bookkeeping.

**(g) Constitution.** Multi-platform: the new prompt commands (`gh`, `git`) are cross-platform; the pre-existing `Invoke-RestMethod` in new_release step 4 is untouched by this diff. File-tools-first: the TOML edits are file-tool-shaped, the knowledge amendments went through `memory_amend` (the sanctioned `.coding/knowledge/**` path), the backlog through the backlog tools — no shell mutation in the diff. Warning-free build: no Rust source changed; the reported green workspace run + 33 skill-module tests are consistent with the diff.

**(h) Security.** gh usage is read-only except `gh pr create` (the intended action); no new injection surface (the agent constructs the commands, same trust level as the rest of the skill); no bypass path introduced. The GitView dispatch line (`mergeDispatch`, GitView.tsx:79-81) was already refactored to never restate the procedure — no third copy of the flow exists.


## Findings

### H1 (high) — GH013 recovery is mechanically broken: the direct path deletes the branch before the push that can fail

File: `.coding/skills/merge_to_main.toml`, prompt step 3 (line 72). The tail of the direct path reads:

> Green → `git branch -d <branch>` (never -D); `git push` on the git tool, approval-gated. A GH013 / protected-ref rejection → the ruleset check missed it: take the PR path (step 4), re-pushing the branch if the failed push left it unpushed.

The GH013 arrives on the push of **main** — *after* `git branch -d <branch>` has already deleted the local branch. Step 4 then instructs "Push the branch on the git tool (`git push -u origin <branch>`, approval-gated)" and `gh pr create --head <branch>` — both impossible with the ref gone (`git push -u origin <branch>` fails: no such refspec). The commits remain recoverable (the merge commit's second parent / reflog), but the prompt never says so, and "re-pushing the branch if the failed push left it unpushed" presumes a ref that no longer exists. Two further defects in the same recovery:

1. **Local main is left diverged.** After GH013, local main holds the unpushed merge commit. When the human later merges the PR, origin/main gets a *different* commit (the ruleset allows merge/squash/rebase), so the next `git fetch` + `git pull --no-rebase` produces a merge-of-merges or conflicts. The recovery should reset local main to origin/main (the merge only existed locally; the PR re-lands it) before taking the PR path.
2. **The "covers gh-missing/unauthed" claim is wrong.** The comment block (line 35) and the DECISION record (point 1) state the GH013 recovery "covers gh-missing/unauthed" — but those are exactly the cases where step 4's `gh pr create` *also* fails, so the recovery dead-ends there too. It genuinely covers only ruleset-check false-negatives with a working, authenticated gh (e.g. a ruleset the list call didn't surface, or a race).

Fix: reorder the direct path's tail to push **before** deleting (…verify builds → `git push` on the git tool → on success `git branch -d <branch>`), and on GH013: reset local main to origin/main, then take the PR path with the branch intact. Mirror the same push-before-delete order in new_release's direct path (it has the identical `git branch -d`, push sequence). Amend the "covers gh-missing/unauthed" claim in the merge_to_main comment block, the DECISION record, and the two decision-file amendments (which repeat it as fact). None of this moves the test-pinned substrings — they all sit earlier in step 3.

### L1 (low) — direct path dropped the "optional" qualifier on `git push`

Old step 5: "optional `git push` on the git tool, approval-gated". New step 3: "`git push` on the git tool, approval-gated." On a no-remote repo (ruleset check fails → direct path; the sync step says "no remote → continue"), the now-unconditional push instruction fails ("no configured push destination") — the old "optional" let the agent skip it. This also deviates from the plan's claim that the direct path is "the existing flow verbatim". Restore "optional" (or "push if a remote exists").

### L2 (low) — new_release's PR path skips the branch-build gate its sibling skill mandates

merge_to_main step 4 (PR path): "verify the BRANCH builds first — `npm run build` (frontend/) AND `cd src-tauri && cargo build`; fix with file_edit until green (PRs get no CI: build.yml triggers only on tags/dispatch)". new_release step 2's PR path: push branch → `gh pr create` → report URL and STOP — **no build verification**, despite resting on the same no-PR-CI reality (verified: build.yml triggers are `workflow_dispatch` + `push: tags: ["v*"]` only). The release PR carries "commit ALL uncommitted work (.coding/ included)", so unverified content can reach main via the human's merge; the first CI proof is then the tag-triggered 30-90 min run, *after* the fact. Secondary: the re-run instruction ("on re-run main already carries the bump → continue to step 3") doesn't say to checkout + sync main first — after the PR-path STOP the agent sits on the working branch, and step 3's literal `git tag -a v<version> -m "…"` tags HEAD, i.e. a branch tip, violating its own "never a branch tip" rule. Fix: add the both-builds gate to the PR path (matching merge_to_main) and a "checkout main + sync" clause to the re-run instruction.

### L3 (low) — stale docs still describe the direct-only merge flow (documentation sync)

- `frontend/src/components/layout/MergeToMainDialog.tsx` — **user-visible copy** (doc comment lines 25-30 and the body bullets, lines 73-77): "Commit the branch's work, then merge it into main … Sync main with origin first, and resolve any conflicts … Delete the merged branch, return to Planning." No ruleset check, no PR path, no "the branch is deleted after the human merges". A user on this repo clicks "Start merge skill" expecting a merge; the skill now opens a PR and stops.
- `frontend/src/components/layout/StatusBar.tsx` lines 779-784 (the merge button's comment): "the agent drives the merge itself: commit, checkout main, sync main with origin (fetch + pull --no-rebase), merge, resolve conflicts, delete branch" — same stale direct-only description.
- `docs/FEATURES.md` line 35: "merged straight into protected `main` (via the `merge_to_main` skill, which syncs `main` with `origin` first … then deletes the branch after landing it)".
- `PLAN.md` line 357 (Branch topology row): describes merge_to_main as sync + merge + delete with no ruleset/PR mention, and describes the run-all app-managed landing as functioning (it is blocked, per the new agent.md note).

README.md is clean (no merge-flow text). The change updated agent.md but not the UI copy or the feature/decision docs — per the constitution's documentation-sync check, an incomplete change.


### L4 (low) — mutually inconsistent dates across the new/amended memory records

- New DECISION record `.coding/knowledge/decision/2027-01-11-merge-to-main-is-ruleset-aware-pr-flow-on-protec.md`: front matter `created = "2027-01-11"` (and the filename date) vs. body "DECISION (2027-02-05, backlog 006ee7d6, plan 8ad6fa91)".
- Both decision-file amendments are headed "Amended 2027-01-11:" (tool-dated) but carry "(plan 8ad6fa91, backlog 006ee7d6, 2027-02-05)" in the body.
- New backlog item b52b041a's text says "discovered 2027-02-05" while its `created_at` (1789989761) decodes to 2026-09-21 (consistent with sibling item 006ee7d6's own 2026-09-21 timestamps and text).

Whichever date is ground truth, the records disagree with each other, and memory recency ranking plus future readers depend on them. Reconcile to one date (the plan/decision bodies consistently say 2027-02-05) — fix the front matter/headings or the body dates accordingly.

### L5 (low) — agent.md: "main only ever receives merge commits" is no longer strictly true on the PR path

Line 85: "main is protected and only ever receives merge commits — directly via the `merge_to_main` skill on unprotected repos, or through an approved PR where a main-protection ruleset applies". The ruleset's `allowed_merge_methods` include squash and rebase (verified live), so a human merging the PR with squash lands a non-merge commit on main. Reword (e.g. "…receives merges — directly via the skill on unprotected repos, or through the human's chosen PR merge method where the ruleset applies"). Same paragraph, secondary nit: "The skill syncs `main` with `origin` first … before the branch merge" is direct-path-only (the PR path never checks out main) — scope it the way the neighboring clauses now do.

### L6 (low) — the new two-path prompt behavior is not pinned by any test

`shipped_skill_files_parse_and_merge_to_main_cleans_up_memories` pins only the OLD invariants (fetch < pull < `--no-ff <branch>`, "MERGED into main") — the PR path and the ruleset check could be silently deleted and the test would stay green. The repo's own convention (DECISION 2026-08-29, terse-skill-prompts; the existing pin test) is to pin load-bearing prompt substrings in `src/skill/mod.rs`. Add assertions that the prompt carries the new load-bearing strings — e.g. contains `gh api repos/<owner>/<repo>/rulesets`, `gh pr create --base main`, `NEVER bypass`, and that the ruleset-check step precedes the direct path's `git checkout main` (detection before action).

## Edge cases assessed — no finding

- **Over-detection:** an ACTIVE ruleset with a `pull_request` rule targeting a *non-default* branch would send the skill to the PR path unnecessarily (direct pushes to main unblocked). Benign direction (fails toward the safer path); not worth complicating the terse prompt.
- **Tag pushes:** the "not blocked" claim holds for the current rulesets (conditions `~DEFAULT_BRANCH` only); a future tag-targeting ruleset would surface as a push failure at new_release step 3 — acceptable, out of scope.
- **Ruleset-check parsing:** the criterion ("ACTIVE ruleset with a `pull_request` rule") correctly excludes `evaluate`-mode rulesets via the ACTIVE qualifier; the gh api response carries `enforcement` and `rules[].type` an agent can parse.
- **No-remote repos:** covered by L1 (the dropped "optional"); the sync step's "no remote → continue" already handles the sync half.
- **Fail-open vs fail-closed:** fail-open to the direct path is correct for portability (the skill ships to unprotected installs where gh may be absent); the GH013 net is the right design for false negatives — it just needs H1's fix to actually work.

## Summary for the parent

Fix H1 (reorder push-before-delete in both skills' direct paths + reset local main on GH013 + correct the "covers gh-missing/unauthed" claim in the comment block, DECISION record, and both amendments), then the six lows. All fixes are prompt/doc/record text — none touch Rust source or the test-pinned substring positions (H1's reorder keeps them earlier in step 3; verify with the skill-module tests after editing). After fixing, re-run `cargo test --workspace` unpiped and confirm the skill-module tests stay green.
