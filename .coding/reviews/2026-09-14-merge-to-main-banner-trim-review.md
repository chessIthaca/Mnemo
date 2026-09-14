## Verdict: PASS

Review of ALL uncommitted changes on `wt/mnemo` (plan 46b460e3, kind=implementation — "Merge-to-main banner: drop build-verify and steer mentions"). The change is exactly what the plan describes and nothing more: two text removals in `frontend/src/components/layout/MergeToMainDialog.tsx` (one `<li>` clause, one paragraph sentence). The trimmed banner stays factually accurate against the skill it describes, the JSX is well-formed, no test pins the removed strings, no other user-facing surface narrates build verification or the interrupt/steer affordance, and the project's documentation set (README/PLAN/docs) never quoted the dialog's bullets, so nothing is stale. Static review — see the method note at the end.

## Change set (verified complete)

`git diff HEAD --stat` = 2 files, 3 insertions / 4 deletions:

| File | Change | Judgment |
|---|---|---|
| `frontend/src/components/layout/MergeToMainDialog.tsx` | `:76` `<li>• Verify both builds, delete the merged branch, return to Planning</li>` → `<li>• Delete the merged branch, return to Planning</li>`; `:78-81` paragraph `"…even in Autonomous mode. You can interrupt or steer the agent mid-skill to help resolve conflicts."` → `"…even in Autonomous mode."` | Correct, complete, no scope creep |
| `.coding/plans/506b85e2.md` | step-4 checkbox `[ ]` → `[x]` | Pre-existing bookkeeping, out of plan scope (as declared) |
| `?? .coding/knowledge/bug/506b85e2.md` | untracked BUG record for plan 506b85e2 (`regression test: deepseek_head_stays_byte_stable_across_plan_progress_bumps`) | Pre-existing bookkeeping, out of plan scope (as declared) |
| `?? .coding/plans/46b460e3.md` | the plan file under review; both steps `[x]` | Expected |

No unintended file, no incidental edit, no unrelated reformatting.

## Requested checks (1)–(5)

**(1) No other user-facing surface narrates build verification or the steer affordance — clean.**
- `"interrupt or steer"` / `"steer the agent"` occurred repo-wide only in the removed paragraph and in plan/review artifacts; there is now no occurrence in `frontend/` UI copy.
- `"Verify both builds"` survives only in non-user-facing places: `StatusBar.tsx:192` (developer doc comment, deliberately unchanged), `.coding/skills/merge_to_main.toml:16-20` (non-injected rationale comment) and `:54` (the live step-4 rule), `GitView.test.ts:11` (test doc comment naming where the procedure lives), plus plan/review/knowledge records.
- The two other merge affordances carry no procedure text: `GitView.tsx:79-81` `mergeDispatch()` is one line naming the target branch (pinned by `GitView.test.ts:28-67`, which *fails* if any step text like `npm run build`/`cargo build` is re-introduced), and `StatusBar.tsx:789` tooltip is `Merge '<branch>' into main (one atomic, confirmation-gated action)`. `GitView.tsx:596-602` reuses the same dialog, so the trim applies on both entry paths.

**(2) JSX validity after the two removals — valid.** `MergeToMainDialog.tsx:73-77` is a `<ul>` with three closed `<li>` children (the third now a single text node); `:78-81` is a single `<p className="text-slate-400">` whose remaining text node renders "…even in Autonomous mode." with the newline+indent collapsing to one space, which is the intended sentence. No dangling `<li>`, no unclosed paragraph, no duplicated markup, and the `<DialogDescription asChild>` wrapper, header, and action row are untouched. The retained sentences are still accurate against `.coding/skills/merge_to_main.toml`: bullet 1 ↔ step 1 (NEVER stash), bullet 2 ↔ steps 2–3 (sync first, resolve conflicts), bullet 3 ↔ steps 5–7 (delete branch, return to Planning).

**(3) No test asserts on the removed strings — clean.** Repo-wide search for `"Verify both builds"`, `"interrupt or steer"`, `"Delete the merged branch"` finds no assertion anywhere. Surveyed `?raw` source-contract test files (36 of them) plus `frontend/vitest.config.ts:16-92` — no test imports or reads `MergeToMainDialog.tsx`, and no test reads its copy. The plan's own claim in `46b460e3.md:10` ("interrupt or steer occurs only there") reproduces exactly.

**(4) TSX typecheck / suite evidence — claim consistent with the code as read.** The parent reports `npm run build` (tsc && vite build, `frontend/package.json:9`) exit 0 and root `cargo test` 2338 passed / 0 failed / 5 ignored. I cannot re-run either (read-only reviewer allow-list), but the claim is consistent by inspection: the change is text-only inside a JSX text node, it alters no export, prop, type, or import, and **no** Rust file is touched — so the type/compile surface is genuinely zero-impact. The constitution's `cd src-tauri && cargo build` (bin crate) obligation attaches to the merged tree at merge time, not to this Rust-free change.

**(5) Line-ending style — preserved by construction; one honest residual.** `.gitattributes:12` pins `* text=auto eol=lf` for the whole repo (LF is canonical on every platform, overriding machine-global `core.autocrlf`), and the diff is a single 3-line hunk with no whole-file rewrite — the signature an EOL flip would leave behind. Residual: without shell access a reviewer cannot byte-verify the working tree, so this rests on the diff shape + the repo-wide `eol=lf` policy + the file tools' documented line-ending preservation rather than a direct byte check.

## Deliberately-unchanged items — verified NOT stale

- `.coding/skills/merge_to_main.toml:54` (step 4) still verifies BOTH builds, with its rationale preserved at `:16-20` (`npm run build` catches TS-only breaks; `cd src-tauri && cargo build` compiles the bin crate the root build/test never sees; post-merge breaks shipped repeatedly before the rule). The dialog is a user-facing summary, not the procedure — its trim does not contradict the rule, it just stops narrating it. **No contradiction found.**
- `StatusBar.tsx:189-193` and `:779-784` are `/** … */` developer comments mirroring the real flow (commit → checkout main → sync → merge → delete) — accurate, not user-visible, and consistent with the TOML. **No contradiction found.**
- `MergeToMainDialog.tsx:24-30` (the component's own doc comment) lists commit/checkout/sync/merge/resolve/delete and never mentioned build verification, so it needed no update and is consistent with the trimmed banner.
- Historical artifact note (no action, stated explicitly): `.coding/reviews/2026-09-14-merge-to-main-single-source-review.md:23` records that the dialog's summary was accurate "…('never stashes', 'Sync main with origin first', 'Verify both builds')". That is a point-in-time review record, not living documentation; review reports are append-only history here (and `.coding/reviews/` is not editable via the main agent's file tools anyway). Recording it as an observation, not a finding.

## Constitution checks

- **Correctness / bugs:** no defect. Pure presentation-copy change; no logic, state, prop, event, or IPC path touched. The retained approval promise ("Core git operations (merge, push) always require approval, even in Autonomous mode") is the *operative* claim in that dialog and it remains code-enforced (`GitTool::never_auto_for` / `ShellTool::never_auto_for`, consumed at `src/agent/dispatch.rs:339`; `agent.md:22`), so the trim does not weaken any promise the app makes.
- **Security:** no security surface touched. No approval gate, tool filter, sandbox, or secret path involved; the always-approval sentence is retained, not removed. Nothing in the diff increases what the agent may do unattended.
- **Documentation sync:** README.md, PLAN.md and docs/ describe merge *behavior* (topology, sync-first-before-merge, branch deletion) and never quote the dialog's bullet list or the steer sentence — verified by targeted search; `docs/FEATURES.md:34-35,70` and `PLAN.md:323,452` carry no "Verify both builds"/"interrupt or steer" copy. Nothing is missing or stale. Module doc comments: unchanged-and-accurate (see above). No user-facing config/`endpoints.toml` example references the banner.
- **Multi-platform neutrality:** pure TSX text content — no platform-specific API, path, or shell syntax introduced; the change is neutral on macOS and Windows.
- **File-tools-first policy:** the diff shows a minimal 3-line targeted edit with no whole-file rewrite, no shell-surgery fingerprints, and no mixed-EOL artifacts — consistent with the sanctioned `file_edit` vehicle. No evidence of a shell-based file mutation in this change set.
- **Regression test:** not applicable — this is not a defect fix (no behavior to regression-test); the plan is correctly kind=implementation, and no test pinned the removed copy (check 3).

## Method note

Read-only review: full `git diff HEAD` + `git status --short`, the changed file, both deliberately-unchanged files, the skill TOML, `.gitattributes`, the Git-tab/StatusBar entry paths and their tests, the vitest include list, and repo-wide searches for every removed string. Test/build claims are the parent's runs, taken as given (stated per this repo's review convention). Verdict: **PASS** — ship it.
