## Verdict: FINDINGS (0 high, 1 low)

Round-3 delta re-review of plan `2f74e10a` (branch `wt/mnemo`, base `1d68462a`). The delta decomposes into: commit `47a438c` (the round-1 fix set, verified by round 2 while uncommitted — carry-over, not re-line-reviewed here) plus the uncommitted working-tree change, which is exactly the 8 round-2 fixes plus the 5 disclosed sweep fixes. All 13 are present and correct. One new low finding: a dangling doc reference to the deleted `build_classifier` inside the rewritten `state.rs` hunk.

## What I read

- `git_read op=log` (plan file) → delta commits: `1d68462` (fork point / base), `47a438c` (round-1 fixes).
- `git_read op=show 47a438c` (stat) — confirmed it is exactly the round-1 fix set round 2 verified (`laya_steer_tool_choice` save persistence + 8 comment corrections; commit message itself records round-2's verification and round-3's pending fixes).
- `git_read op=diff` — the full uncommitted delta: 10 files, 24+/23−, all comment/doc-string prose plus the test's embedded block string. No code semantics changed anywhere in the uncommitted set.
- Read the `general.rs:1359/1368`, `ClassifierSection.test.ts:1-30/155-179`, and `tauri.ts:458-475/606-617` regions to pin the remaining-reference sweep and the embedded-block lockstep.
- Swept the tree for removed-surface references: `managed[- ]mode|laya_mode|laya_endpoint|LayaMode|multilingual` and `laya_checkpoint|checkpoint radio|mode radio|build_classifier|fn build_classifier`.

## Round-2 fix verification — all 8 present and correct

1. `src-tauri/src/ipc/laya.rs` `spawn_server` doc — now "a catalog checkpoint id, or a fine-tuned artifact directory path…"; the "in managed mode" qualifier is gone. ✔
2. `laya.rs:840` — "(until now it was None — enabled without an install)". ✔
3. `src-tauri/src/ipc/settings.rs` `LayaWire.auto_finetune` — "(managed runtime only; never blocks startup)". ✔
4. `src/config/settings_dto.rs` `SettingsSaveDto.laya_auto_finetune` — "(managed runtime only, opt-in). Absent = keep the current value." ✔
5. `ClassifierSection.tsx:98` — "(`[general.laya] auto_finetune`, managed runtime only)". ✔
6. `ClassifierSection.tsx:370` — "— managed runtime only: when enough new classified failures have accrued…". ✔
7. `ClassifierSection.test.ts:12` — header now reads "the managed-runtime controls (checkpoint catalog + download progress) stay wired"; "mode toggle" is gone. ✔
8. Embedded block / wire lockstep — `ClassifierSection.test.ts:167-168` and `tauri.ts:467-468` both carry "(managed runtime only, opt-in)"; `tauri.ts:613` (save patch) likewise. The diff hunks are byte-symmetric on every changed and context line, and the surrounding block (lines 155-170 test vs 456-470 tauri.ts) matches verbatim, so the `toContain` assertion stays true — corroborated by the reported green `npx vitest run` (92 files / 1302 tests). ✔

## Sweep verification — all 5 disclosed items correct, none misjudged

- `failure_triage.rs:258` "rebuild on save" ✔; `:266` "managed-runtime start" ✔ — both accurate (the Settings-save swap site needs no "external" qualifier; the sidecar start is the managed runtime's).
- `src/config/general.rs:255` — "(managed runtime only)" ✔.
- `state.rs:157` "when Laya is enabled" ✔ — the "with an endpoint" qualifier was endpoint-mode-era residue. `state.rs:165` "Inert until Laya is enabled + set up" ✔ — correct; LayaManager is inert until the flag is on and setup has produced a sidecar.

## Remaining-reference sweep in/around the touched files

- `src/config/general.rs:1359/1368` — inside the plan's own regression test `laya_removed_mode_endpoint_and_checkpoint_keys_are_ignored`; the removed keys appear there deliberately as legacy-config input. Correct to keep, not a stale reference.
- `src-tauri/src/main.rs:1404` — "Laya is managed-only (the external-endpoint mode is gone)": a deliberate historical note stating the mode IS gone, in the carry-over commit. Not a finding.
- Known remaining, deliberately deferred (files this plan never touched; agreed out of delta): `src/memory/classifier.rs:10/:161`, `src/tool/agent/tool_choice.rs:252`, `src/tool/memory/mod.rs:150`, `docs/FEATURES.md:48`. Disclosure for that future pass: `src/memory/classifier.rs:14` also still says "enabled without an endpoint" — same class, same file.
- Test suite results (root `cargo test` 2781/0, `cargo test -p mnemo-app` 328/0, `tsc --noEmit` exit 0, vitest 1302/0) are the dispatcher's reported runs; the uncommitted delta is comment-prose-only plus the test's own assertion string, so no build/test surface is affected beyond the (green) test itself.

## Finding (low)

**L1 — `src-tauri/src/ipc/state.rs:158`: doc comment still references the deleted `build_classifier`.** The hunk this delta rewrote now reads: "`None` while disabled; see `build_classifier`" — but this plan deleted `build_classifier` (no `fn build_classifier` exists anywhere in the workspace; the only surviving mentions are dangling prose in `src/memory/classifier.rs:14/:197/:260/:855` — a deferred file — and here). This is a still-stale reference to a removed surface in a touched file, the exact class this round is cleaning. Fix: drop the parenthetical or point it at the sites that actually build/swap the slot now (the Settings rewire rebuild — `src-tauri/src/ipc/rewire.rs:85` — and the managed sidecar start). Doc-only, no compile impact.

## Bookkeeping accuracy

- `.coding/plans/2f74e10a.md` — `## Reviews` gains round 3's base stamp (`47a438c…`), matching the delta's HEAD at dispatch; accurate.
- Round-2 report (`.coding/reviews/2026-09-26-plan-2f74e10a-round2-fix-verify.md`) ships inside `47a438c` and matches what it verified. Accurate.
- Process remark (one line): commit `47a438c` re-shows hunks round 2 already verified while uncommitted — carry-over per the empty-diff/uncommitted-carry-over rule; not re-reviewed line-by-line here.

Reviewed-state: 47a438c17e9a379e7217a9d446a8e9bffdc4c5f3
