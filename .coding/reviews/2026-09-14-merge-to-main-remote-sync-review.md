## Verdict: FINDINGS (1 high, 5 low)

Review of ALL uncommitted changes on `wt/mnemo` for plan d826b9ad (backlog: merge_to_main must pull the remote BEFORE the branch merge). Files: `.coding/skills/merge_to_main.toml`, `frontend/src/components/views/GitView.tsx` (+ new `GitView.test.ts`, `frontend/vitest.config.ts`), `frontend/src/components/layout/MergeToMainDialog.tsx`, `docs/FEATURES.md`, `src/skill/mod.rs`, plus the pre-existing dirty `.coding/knowledge/spec/2027-01-11-compaction-*.md`.

**The change is correct and the ordering is the right fix.** The git sequence (`git checkout main` → `git fetch origin` → `git pull --no-rebase` → `git merge --no-ff <branch>`) is sound: `--no-rebase` overrides any `pull.rebase=true` so main's history is never rewritten; a diverged main gets a merge commit with local commits preserved (nothing is lost); the escape hatch for a missing remote/upstream is achievable with the allowed tools (shell reports the fatal on stderr and the prompt pre-authorizes continuing). I verified against history that this is exactly the incident shape: commit `76af04b` is `Merge branch 'main' of https://github.com/chessIthaca/Mnemo` with parents `d88d922` + `bb0d83f` — i.e. the catch-up pull that happened AFTER `d88d922` had already landed the branch. Both SHAs cited in the new comment/amendment are real and accurate. The one high finding is about *which tool* executes the flow, not about the flow.

---

### HIGH 1 — The new step steers git through `shell`, which is outside the always-on core-operation approval gate

The new step 2 says the sync runs "via shell". That is *necessary* — the git tool has no fetch/pull subcommand (its own error text lists the supported set: `status, diff, log, commit, merge, checkout, stash, branch, push, restore`, `src/tool/agent/git.rs:559-567`) — but the prompt names `shell` as the tool for git inside the merge flow while the branch merge itself (`git merge --no-ff <branch>`, new step 3) and the optional push (step 7) are left without a tool anchor. A weaker model reading "git commands via shell" one step earlier can reasonably keep using shell for the merge and the push.

Why that matters, with evidence:
- **Shell has no core-op gate.** `never_auto_for` is implemented only by `GitTool` (`src/tool/agent/git.rs:516`); the trait default is `false` (`src/tool/mod.rs:133`) and `ShellTool` does not override it (full impl read: `name`/`category`/`schema`/`safety`/`execute` only, `src/tool/agent/shell.rs:158-307`). The dispatch layer forces the prompt on that signal alone (`src/agent/dispatch.rs:339`), so in `SafetyMode::Autonomous` a shell-invoked `git merge`/`git push` runs **with no approval prompt**.
- **Config cannot close it.** The core-operations list is a `GitTool` field consulted only from `GitTool::never_auto_for` (`:532-536`) — adding `"pull"`/`"merge"` in Settings → Git does not gate a `shell` invocation.
- **The app promises the opposite to the user.** `MergeToMainDialog.tsx:84-86` tells the user "Core git operations (merge, push) always require approval, even in Autonomous mode" — the same promise is in `agent.md` and in the compiled APP RULES ("no safety mode or skill bypasses this"). Through shell that promise does not hold.
- **Reachable on the unattended path.** Run-all items can invoke `merge_to_main` mid-run (documented in the repo's own bug record for backlog 66c65db9), and that is precisely where Autonomous mode lives.
- **Baseline (stated honestly):** the exposure is pre-existing — `shell` was already allow-listed and the old step 2 named no tool for `git checkout main` → `git merge --no-ff <branch>`. This change makes the shell path the *explicitly instructed* one for git in this flow (and newly routes a main-writing operation, the pull, through it), so it grows the likelihood rather than the capability.

**Minimal fix (both prompt copies):** keep the pull on shell but say why, and re-anchor the gated operations to the git tool.
- `.toml` step 2: `…then `git fetch origin` + `git pull --no-rebase` via shell (the git tool has no fetch/pull subcommand)…`
- `.toml` step 3: `` `git merge --no-ff <branch>` via the **git** tool (approval-gated) ``
- `.toml` step 7 already says "`git push` via the git tool (approval-gated)" — good; make step 3 match it.
- `GitView.mergePrompt` step 3/4: one clause naming the git tool for the merge (`Do NOT push.` stays).

Optional follow-up (not required by this plan — queue as a backlog item rather than extending scope): a command-aware `never_auto_for` on `ShellTool` that detects git core subcommands, which would close the hole for any agent-authored shell git command.

### LOW 1 — The GitView copy's new sync step has no completion path for a pull conflict (the skill copy has the other half)

`GitView.tsx:80` (new step 3): "…nothing new or no remote → continue; conflicts here too: resolve with file_edit taking the UNION of both sides, never drop a side's change)." It never says to conclude the merge. An agent that rewrites the conflict markers with `file_edit` and moves on leaves unmerged index entries + `MERGE_HEAD` in place, and the very next step (step 4, `git merge --no-ff <branch>`) fails with *"You have not concluded your merge (MERGE_HEAD exists)"* — the flow strands on main. The sibling `.toml` step 2 carries `resolve with file_edit, `git add`, `git commit`` but *not* the union rule, so each copy has exactly half of the instruction — the opposite of the drift-consistency this plan set out to achieve (check #3).

**Fix (one line, `GitView.tsx:80`):** "…conflicts here too: resolve with file_edit taking the UNION of both sides, then `git add` + `git commit` to conclude the pull merge — never drop a side's change, never `-X ours`/`-X theirs`."

### LOW 2 — The confirmation dialog contradicts the skill on stashing

`MergeToMainDialog.tsx:74` — bullet "• Stash uncommitted changes if needed" — and the component doc comment at `:27` ("where the agent drives the merge itself (stash, commit, merge, resolve conflicts, delete branch)") directly contradict the skill's hard rule, injected every turn: *"NEVER stash — a stash here is never popped"*, whose rationale comment records ten orphan stashes by 2026-08-24 (`.coding/skills/merge_to_main.toml:14-16`). The bullet list is the exact block this change edited (three bullets replaced), so the contradiction sits adjacent to the change.

**Fix:** "• Commit uncommitted changes on the branch (the skill never stashes)" and drop `stash` from the doc comment. Pre-existing text, flagged under the project's documentation-sync expectation.

### LOW 3 — Sibling statements of the same contract were not updated while `docs/FEATURES.md` was

`docs/FEATURES.md:34` now documents the sync clause, but the two other places that state the same merge-flow contract are untouched:
- `PLAN.md:320` (Branch topology row — the project's technical-decision record): "`merge_to_main` merges the branch into `main` and deletes it (no accumulation)."
- `agent.md:82` (branch policy — and the skill's own header comment points readers there: "Topology … documented in agent.md", `merge_to_main.toml:30-33`).

Neither is *false* now, but both describe the merge flow without its new first precondition, and `agent.md` is the file the skill header defers to. **Fix:** one clause each — "syncing `main` with `origin` first (`fetch` + `pull --no-rebase`) so the landed merge is immediately pushable".

### LOW 4 — The Rust ordering guard pins only half of the sync (`git fetch origin` is unpinned)

The added assertion **is a real guard, not tautological** — I checked it against the OLD prompt text: `merge.prompt.find("git pull --no-rebase")` returns `None` (the old prompt contains no pull at all), so `.expect("…carries the remote-sync step")` panics before any comparison; `find("--no-ff <branch>")` does resolve on the old text, so it is genuinely the *new* half that fails. The frontend guard is real too (old prompt → both indices `-1` → `toBeGreaterThan(-1)` fails), and its `indexOf` guard comment about a naive `-1` compare is a nice touch.

Gap: the Rust test never asserts `git fetch origin`, so an edit that keeps the pull and drops the fetch passes both the Rust test and (for the `.toml`) everything else — the frontend test covers the fetch only for the *GitView* copy. **Fix (one line):** add `let fetch = merge.prompt.find("git fetch origin").expect("merge_to_main prompt carries the fetch step");` and include it in the ordering assert (e.g. `assert!(fetch < sync && sync < branch_merge, …)`). Optional and not required: the host test's name is now broader than memory cleanup, but its inline comment documents the extension, so a rename is cosmetic.

### LOW 5 — `.coding/instance.json` will be committed into main by this flow (one-line `.gitignore` fix)

`src/instance_marker.rs:47-55` writes `<project>/.coding/instance.json` as `{pid, started_at}` on every launch, "last writer wins" — per-instance ephemeral state, exactly the class the repo already gitignores for `plans/stack.json: .coding/plans/stack.json` ("per-worktree live state"), plus `memory.db`, `codegraph.db`, `logs/`, `backlog-images/`. `instance.json` is **not** ignored (`.gitignore` read in full), and it is currently untracked (`?? .coding/instance.json`).

Consequences for this plan specifically: the skill's step 1 commits ALL uncommitted work "including `.coding/`", so the next merge drags a fresh pid into main (churn on every landing); and because the file is plain text with no union driver (only `backlog.jsonl` has `merge=union`), two instances/branches that differ produce a conflict — now surfaceable during the NEW step 2 pull as well. This plan's own closing commit has the same exposure. **Fix (one line):** add `.coding/instance.json` to `.gitignore` (with a short per-instance rationale comment, mirroring the stack.json entry).

---

## Verified — no action required

**Git sequence soundness (check #1).** Cannot lose work: `git pull --no-rebase` merges `origin/main` into local main, never discarding local commits, and `--no-rebase` overrides a repo-level `pull.rebase=true` so main is never rewritten. Cannot rebase main. Detached HEAD: `git checkout main` simply re-attaches before the fetch/pull. Missing remote/upstream: `git fetch origin` / `git pull` fail loudly ("does not appear to be a git repository" / "no tracking information"); the prompt's "no remote/upstream → continue" pre-authorizes ignoring exactly that, and no special tool is needed because `shell` reports any *executed* command as success with `exit_code` as data and full stderr preserved (`src/tool/agent/shell.rs:289-301`) — so the escape hatch is reachable. Not covered but acceptable: an unrelated-history remote (`refusing to merge unrelated histories` is a hard error, not a conflict) — no realistic path to that on this repo's origin; the conflict paths themselves are covered in both copies (modulo LOW 1).

**TOML validity/robustness (check #6).** The `"""` multiline string still terminates (`:58`); the inserted step-2 text contains no backslash, quote, or `"""` hazard; the two line-continuation `\`s are intact; `tools` includes everything the new step needs (`shell`, `git`, `file_edit` — `:38-48`). This file is the `include_str!` source for `SHIPPED_SKILLS` (`src/skill/mod.rs:164-166`), so new projects seed the new step. Existing projects keep their already-seeded copy — `seed_skills` is write-if-missing, the documented self-heal path (DECISION 2026-08-29) — known and accepted, not a finding. Wording is terse but unambiguous for the operative parts: the ordering is stated as "SYNC MAIN WITH THE REMOTE FIRST", the flag rationale is inline, and the stale-main consequence is named.

**The two copies agree where it matters (check #3).** Both put the sync before the branch merge and both name the same command pair (`git fetch origin` + `git pull --no-rebase`), with consistent framing (nothing new / no remote → continue). Numberings are clean and gapless: `.toml` 1–7, `GitView` 1–8. Pre-existing, intentional difference preserved: the GitView override ends "Do NOT push." while the skill keeps the optional push via the approval-gated git tool. The override is not cosmetic: `GitView.tsx:159` passes it as the skill prompt (`enterSkill(agent, "merge_to_main", mergePrompt(target))`), which wins over the registry prompt at `src/tool/workflow/skill.rs:144`.

**Test registration.** `GitView.test.ts` is listed in `frontend/vitest.config.ts` include, and the allow-list guard (`src/lib/vitestInclude.test.ts`) globs `src/**/*.test.{ts,tsx}` and fails on any unregistered file — a miss would have been loud. The new test is pure (string contract, no Tauri/DOM), so it is stable in CI.

**SHAs cited are real and accurate (verified via `git show`).** `76af04b` = "Merge branch 'main' of https://github.com/chessIthaca/Mnemo", parents `d88d922` + `bb0d83f` — literally the post-merge catch-up pull the new rationale comment describes. `d88d922` = the `Merge branch 'wt/mnemo'` landing whose second parent is `21d4f5f` — matching the knowledge amendment's "MERGED into main at d88d922 … pre-merge tip 21d4f5f".

**Committing the pre-existing dirty knowledge file with this change is correct.** `.coding/knowledge/spec/2027-01-11-compaction-guarantees-budgeted-request-sendable.md` carries a 2-line dated amendment recording the earlier `d88d922` landing of this same branch. The `.coding/` corpus is designed to travel with git, the skill's step 1 commits it anyway, and its content checks out against history — leaving it dirty would just hand the same uncommitted residue to the merge.

**Multi-platform neutrality.** Nothing platform-specific was added: `git fetch/pull/checkout/merge/add/commit` behave identically under PowerShell and `sh`, the Rust assertion is platform-neutral, and the frontend changes are DOM-free/pure. No `cfg(windows)` additions, no Windows-only paths or syntax.

**File-tools-first.** The diff introduces no shell-based file mutation; every change is a normal file-tool edit.

**No stale README.** A scoped search over `README.md`, `docs/**`, `agent.md`, `PLAN.md`, `src/agent/prompt.rs` found no merge-flow description in README.md — nothing to update there. `docs/FEATURES.md:35` (merge hygiene) and `PLAN.md:444` ("post-merge step") stay accurate: neither cites a step *number*, so the 5→6 renumbering breaks nothing.

## Out of scope — flagged for awareness, NOT counted as findings

- **`src/project/worktrees.rs` — the app-managed landing path still has the bug this plan fixes.** `land_item_branch_impl` (`:172-227`) does `reset --hard main` + `merge --no-ff <branch>` and never fetches or pulls origin, so a parallel run-all item can be landed onto a stale local main and the later push is rejected — the same failure mode, different entry point. Deliberately out of scope per the plan, and it does **not** invalidate this change; but the inconsistency is real, so I recommend queueing a backlog item rather than extending this plan (the app-side path would need its own fetch/pull before merging, in the landing worktree).
- **Pre-existing prompt shell-chaining, untouched by this diff:** step 4's `cd src-tauri && cargo build` and the GitView copy's `cd frontend; npm run build` (PowerShell `&&`/`;` semantics). Empirically these have worked across many recorded landings and this diff only renumbers them — not a finding for this change.
- **Pre-existing exposure, unchanged:** `shell` can execute arbitrary git commands generally (see HIGH 1 — the recommended follow-up backlog item covers this class, not just this skill).
