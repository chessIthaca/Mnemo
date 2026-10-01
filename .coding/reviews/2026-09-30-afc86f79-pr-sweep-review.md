## Verdict: PASS

Reviewed the full delta of commit 5da4631 on wt/mnemo (base main 3eb81ab): `.coding/skills/pr_sweep.toml` (new), `src/skill/mod.rs` (REPO_LOCAL_SKILLS entry + guard-test addition + new pinning test, read at lines 328-344, 712-777, 869-930), `docs/FEATURES.md:20`, plus the plan file (bookkeeping). Sources checked against the change: `.github/dependabot.yml` (full), `tests/integration/ci_workflow.rs::cargo_lock_keeps_the_vendored_patches` (lines 288-353), `git show 5da4631` (stat), `git status` (only the plan file modified — the harness review stamp), and a live fetch of the GitHub PR list.

### 1. The pinning test is load-bearing, not ceremonial
`pr_sweep_parses_and_carries_the_merge_gate_order` (src/skill/mod.rs:713) builds its registry from disk — `Path::new(env!("CARGO_MANIFEST_DIR")).join(".coding/skills")` → `SkillRegistry::load_dir` — and `.expect`s the spec by name, so a file that stops parsing (load_dir's logged-and-skip) fails the test instead of silently vanishing. Every anchor is `.find(...).expect(...)` on the loaded `spec.prompt`, so the test genuinely fails if the TOML loses:
- the Cargo.lock gate — `find("[[patch.unused]]")` returns None → panic;
- the approve step — `find("gh pr review")`;
- the merge step — `find("gh pr merge")`.

It also pins the ORDER (gate1 < merge, gate2 < merge, approve < merge — all with index-comparison asserts), the workflow states (Complete/Planning yes, Executing no), `target_state == Planning`, and the charter (shell+git present; file_write/file_edit/multi_edit each asserted absent). House shape is identical to its neighbour `post_merge_sync_parses_and_carries_the_closeout_order` (665): same dir construction, same get/expect, same is_available_in triple, same find/expect + `<` pattern, same closing `contains` asserts. This is the neighbour's shape faithfully extended, not ceremony.

### 2. Count-equality guard satisfied, reason clears the floor
`.coding/skills/` now loads 5 specs (merge_to_main, create_skill, new_release, post_merge_sync, pr_sweep) == `SHIPPED_SKILLS.len()` (2) + `REPO_LOCAL_SKILLS.len()` (3), so `every_repo_skill_is_shipped_or_declared_repo_local` (790-822) holds; the per-name membership loop and the `is_shipped_skill("pr_sweep") == false` assert in `shipped_skill_names_are_recognized` both pass. The new reason ("work the open PR queue end to end — this repo's dependabot queue and the pull_request ruleset 23755694 PR path are meaningless in a user's project") is ~140 chars, well over the >20 floor at 817-822.

### 3. Prompt policy claims verified against sources
- **Cargo.lock / vendored tao/wry claim** — TRUE. `.github/dependabot.yml` lines 26-37 say exactly this (no `[[patch.unused]]`, tao/wry path-sourced from vendor/, the tauri-past-2.11.6 → wry 0.57/tao 0.37 consequence), and the machine guard `cargo_lock_keeps_the_vendored_patches` (tests/integration/ci_workflow.rs:304) enforces both halves on the repo tree. The TOML's line citation (26-39) is accurate.
- **"Approving is not a self-approval"** — TRUE. Live fetch of the open PR list (2026, this review): all five (#73, #72, #67, #66, #65) are authored by dependabot[bot]. A chessIthaca approval satisfies ruleset 23755694's required_approving_review_count without self-approval.
- **No forbidden escalation invited** — the prompt's only merge commands are `gh pr review <n> --approve` + `gh pr merge <n> --merge`, with an explicit "NEVER force, never `--admin`, never edit the ruleset; a refusal is reported as-is, never worked around." Nothing force-pushes, nothing touches the ruleset, and step 7's main handling is `git checkout main` + `git pull --no-rebase` only — no commit to main anywhere in the file (header comments included).
- **No source-edit escape hatch** — step 5 forbids merging a PR that needs source/manifest edits and routes it to a plan; the charter is mechanically enforced (no file tools in the allow-list, `target_state = Planning`, all three pinned by the test).

### 4. Style / project checks
- No dead code, no `#[allow(...)]` anywhere in the delta; the new test and the REPO_LOCAL_SKILLS entry carry explanatory comments matching house style. Implementer evidence: unpiped `cargo test` exit 0 for the workspace — under `#![deny(warnings)]` that proves zero warnings (not independently re-run: read-only review).
- Multi-platform: the change is platform-neutral (TOML + Rust test + one doc line; no cfg(windows), no Windows-only paths).
- File-tools-first: the skill was authored via `skill_create` per the commit message and plan step 1; no shell mutation in the delta.
- Documentation sync: docs/FEATURES.md:20 now enumerates `pr_sweep` with an accurate one-clause description matching what shipped; README.md carries no skill enumeration, so no README edit is due.
- Bookkeeping (one-line accuracy): `.coding/plans/afc86f79.md` matches what shipped — all 6 steps describe exactly the committed change; the Reviews stamp is harness-written. Nothing in `.coding/**` is off.

### Risk focus (dispatching agent's three concerns)
1. *Skill silently vanishing when it stops parsing* — closed: the pinning test `.expect`s the load from disk, so CI fails before the skill can rot unnoticed (the same mitigation the neighbour test documents at 716-717).
2. *Mis-declared repo-local classification shipping this workflow into user projects* — closed: the guard test forces the REPO_LOCAL_SKILLS choice, `is_shipped_skill("pr_sweep")` is pinned false, and `seed_skills` only writes SHIPPED_SKILLS entries, so pr_sweep never seeds.
3. *Prompt wording letting the sweep merge a PR needing source edits* — closed: prompt step 5 + the absent file tools + target_state Planning + the test's forbidden-tools assert make the no-edit charter both instructed and mechanically enforced.

Reviewed-state: 5da46317e38a23fd892721efcd8c05de54495be0
