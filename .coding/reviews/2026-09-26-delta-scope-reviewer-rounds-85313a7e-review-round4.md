## Verdict: FINDINGS (0 high, 1 low)

Round-4 delta-scoped verification of the three round-3 LOW fixes for plan f4636852 / backlog 85313a7e, base 4a2bd99 (nothing committed). Delta = `src/agent/review_scope.rs` (truncation-line form, budget comments + 3200→3100 bound, helper + four test updates), `src/tool/agent/spawn_agent.rs` (`with_review_scope` doc), `src/agent/factory.rs` (wiring comment), `docs/FEATURES.md` (three spots), `PLAN.md` (one spot) — wording/docs/test-precision. Round 1-3-verified material was not re-reviewed, only swept for the two claim-classes.

**All three round-3 LOWs are fixed and verified at source level. The LOW 1 and LOW 2 sweeps are clean: no remaining preamble-carries-`git diff` claim anywhere, and no retired-tool name in any live doc or non-comment source prose.** The one new low is the sibling class one tier down: five pre-existing source comments still describe the reviewer's tool surface as including the retired `git_diff` tool — outside both sweeps' literal letter (source comments, not docs/prose; tool-surface claims, not preamble claims), reported so the fourth-round sibling hunt ends here. Details below.
## 1. Round-3 LOW 1 — FIXED (verified)

**The two sibling comments** — both now describe exactly what `render_review_preamble` emits:

- `src/tool/agent/spawn_agent.rs:184-194` (`with_review_scope` doc): enumerates the preamble as "the verdict contract, the one-line constitution checks, the `.coding/**` bookkeeping rule, and, for round >= 2, the delta scope (the delta since `<base>`, named via the reviewer's own `git_read` ops, plus the mechanically-derived changed file set)" — precisely the renderer's round-2 output (review_scope.rs:133-148: base line, delta instruction naming `git_read` ops, changed set). No `git diff <base>` claim; nothing invites a future agent to "restore" a shell instruction.
- `src/agent/factory.rs:1489-1495` (`register_spawn_tool` wiring comment): "…for round >= 2 the delta scope (the delta since `<base>`, named via the reviewer's own `git_read` ops, + the changed file set)" — same corrected enumeration; the enclosing `register_spawn_tool` doc (:1456-1464) is also accurate.

**LOW 1 sweep — complete; no (a)-class claim remains.** Every remaining `git diff` mention in src/, src-tauri/, frontend/, agent.md, PLAN.md, docs/**, README.md classified (`.coding/plans/**` + `.coding/reviews/**` hits are dated history — category (c), never findings):

- **(a) claims the preamble carries/instructs `git diff <base>`: NONE remain.**
- **(b) app-derivation / scope-semantics descriptions — accurate, not findings:** agent.md:145 ("so `git diff <base>` is exactly the fix" — the step-boundary commit convention; the main agent does run git via the `git` tool); PLAN.md:1293 ("derived from `git diff --name-status <base>` + untracked files" — the exact commands `changed_paths_since` runs); README.md:60 and docs/FEATURES.md:10 ("the app scopes it/the review to `git diff <base>`" — the app is the actor, the same derivation); src/workflow/plan_file.rs:142 and :155 (stamp semantics: "`git diff <base>` is exactly what changed since the previous round"). I concur with rounds 2/3's identical judgments on these same spots.
- **(c) intentional removal-documentation / negative assertions:** review_scope.rs:150-153 (source comment explaining why the raw form was removed), :256-258, :271-273, :290 (test prose + `!contains("git diff …")` assertions).
- **(d) real shell commands / implementation:** git.rs (the `git` tool's read queries), git_diff.rs (the `GitDiffTool` delegate running real `git diff HEAD`), cmd_class.rs:630 (`classify("git diff")`), src-tauri files.rs `git_diff_head` (the DiffViewer IPC helper), frontend DiffViewer.tsx:438 ("Re-run git diff HEAD" — the UI label for that IPC command) and tauri.ts:1696 (`invoke("git_diff_head")` — the IPC command name, matching the real function).
- PLAN.md:1284-1293 and agent.md's preamble bullets accurately describe the preamble as naming `git_read` ops. **The LOW 1 class is closed across the tree.**

## 2. Round-3 LOW 2 — FIXED (verified) + sweep clean

**The four spots read accurately:**

- `docs/FEATURES.md:32` — "…plus read-only `git_read`" (names a tool that exists).
- `docs/FEATURES.md:46` — "a strict read-only reviewer role (reads + `git_read` (op `diff`/`log`/`show`/`status`), `web_fetch`, graph tools, and memory/backlog queries + `write_review_report`…)" — matches `REVIEWER_BASE_TOOLS` (src-tauri/src/ipc/spawn.rs:436-470), the spawn tool's role schema (spawn_agent.rs:249-258), agent.md, and this reviewer's actual live surface.
- `docs/FEATURES.md:71` — "`git diff` output is never truncated … mirroring the read-only `git_read` tool" — `git diff` here is the shell command (run by the `git` tool's read queries and by `git_read`'s `op="diff"`, both genuinely never truncated); no retired tool named.
- `PLAN.md:523` — "verify with `git_read` when it matters" — positively confirmed; the retired names are gone.

**LOW 2 sweep — clean.** The trio (`git_log`/`git_show`/`git_diff`) appears in **no** live doc: zero matches in agent.md, PLAN.md, README.md, docs/** (each positively re-verified with a clean walk). Non-comment source prose: the production reviewer lists carry only `git_read` (`REVIEWER_BASE_TOOLS` spawn.rs:459, `ALWAYS_SAFE_ROLE_TOOLS` spawn.rs:475, the factory's expected-tools test factory.rs:2795, the registration itself :1080-1086); the spawn tool's role schema is accurate. The remaining trio strings in source are the retired delegates' own definitions (git_read.rs / git_diff.rs — internal delegates constructed inside `GitReadTool::new`, git_read_tool.rs:28-29/:52-57, never registered, never surfaced to any agent) and review_scope.rs's forbidden-names list (intentional) — not availability claims.
## 3. NEW LOW (this round) — five pre-existing comments still name the retired `git_diff` on the reviewer's surface

Same class as round-2 F1's "likely source of the wrong names" (the spawn schema, since fixed to `git_read`): the drift survives in five sibling comments, each naming a tool that does not exist:

1. `src/tool/agent/spawn_agent.rs:56` — `role` arg doc: "a read-only agent that can read code + run `git_diff` + write its report via `write_review_report`"
2. `src/tool/agent/spawn_agent.rs:284` — execute() role-validation comment: "constrains the spawned agent to a read-only tool surface (read tools + git_diff + write_review_report)"
3. `src/workflow/mod.rs:159` — `tool_allowlist` field doc: "a `role: "reviewer"` spawn is limited to read tools + `git_diff` + `write_review_report`"
4. `src/workflow/mod.rs:426` — `allowed_tools` doc: "This is what constrains a spawned reviewer to read tools + `git_diff` + `write_review_report`"
5. `src-tauri/src/ipc/spawn.rs:972` — test comment: "the read-only role tools (git_diff, write_review_report) ARE kept even though the parent lacks them" — while the test it annotates asserts `git_read` (spawn.rs:983-984) and the constant it describes is `["git_read", "write_review_report"]` (spawn.rs:475)

`git_diff` is not a registered tool: the factory registers only `GitReadTool` (factory.rs:1080-1086), and git_read's own module doc records it "Replaces `git_log`, `git_show` and `git_diff`" (git_read_tool.rs:5-10, delegates constructed at :52-57). All five predate this delta — baseline drift from before the git_read unification (round 2 established the identical drift at spawn_agent.rs:249 predates the delta; the 2027-01-11 git-read SPEC record's "Known stale (pre-unification, future doc pass)" list covers only agent.md and the 2026-08-23 strict-reviewer-surface SPEC record, not these five). They sit outside both sweeps' literal letter (source comments, not live docs/non-comment prose; tool-surface claims, not preamble claims), but they are exactly the trap that bred F1: a future agent reading spawn_agent.rs:56 or mod.rs:159 to learn the reviewer's surface gets a nonexistent tool name. Fix: five one-line `git_diff` → `git_read` replacements (for :972, "(git_diff, write_review_report)" → "(git_read, write_review_report)") — do them in the closing commit, or park as backlog if you want this delta minimal; either way the sibling chain should end here, not in a round 5.

## 4. Round-3 LOW 3 — FIXED (verified)

`assert_no_retired_git_tool_names` (review_scope.rs:365-372) is called by all four renders that reach a tool-name-emitting branch: the contract test (:240), the round-2 test (:269 — replacing the former inline trio check; the comment :263-268 records exactly that), the git-failure test (:330), and the huge-set test (:450).

**Branch map** — every render branch that can emit a tool name is covered:

- contract Empty-diff rule (:110-114) → contract test: positive pin :235-238 + helper :240
- delta intro (:139-144) → round-2 and git-failure renders: pins :260-262 + helper :269/:330
- **truncation line** (:177-181) → huge-set test: positive pin :446-449 + helper :450
- **unavailable-set branch** (:154-158) → git-failure test: positive trailing pin :322-325 + helper :330
- un-truncated changed set (:171-175) + carry-over (:186-192) → inside the round-2 test's helper-checked render (those lines emit no tool names themselves); the no-base fallback (:121-130) and empty-set (:163-169) branches emit no tool names and are pinned by their own tests.

**The two previously-unreached branches are both positively pinned AND covered by the negative check.** Reverting either branch's wording to the pre-F1 trio now fails the suite twice:

- truncation line → review_scope.rs:446-449 (`contains("enumerate the rest with `git_read` (`op=\"log\"`)")` fails) and :450 (the helper panics on `git_log`);
- unavailable trailing mention → :322-325 (`contains("then `git_read` (`op=\"diff\"`) for the uncommitted remainder")` fails — unique to that branch: the delta intro says "take the uncommitted remainder with `git_read`", so the pin can no longer be satisfied by the intro, which was round 3's exact complaint about the old bare `op="diff"` check) and :330 (the helper panics on `git_diff`).

**The comments no longer overclaim.** The helper doc (:358-364) and the round-2 test comment (:266-268) say "every render whose branch can emit a tool name" — true in the branch-coverage sense: every tool-name-emitting branch sits inside a helper-checked render. (Precision note, not a finding: the third-round / empty-set / unchanged-tree / budget unit renders don't call the helper, but they traverse only branches already covered by the round-2 and git-failure renders; the branches unique to them emit no tool names.) Test renamed `a_git_failure_renders_an_instruction_instead_of_a_file_list` (:307) with an accurate history note (:319-321).
## 5. Budgets — unchanged, still honest

- **The reasoning holds.** The only rendered-string change since round 3 is the truncation line (review_scope.rs:179). The budget renders are (a) `ReviewScope::default()` → round < 2 → returns at :118-120 with the contract block only, and (b) round 2 with one changed file (`hidden = 1 - 40 = 0` → no truncation line). Neither render contains the truncation line, so both are byte-identical to what round 3 measured — **1791 / 2909 stand** (round 3 independently re-derived 1794/2910, ±3 hand-count noise; nothing in those renders changed since).
- **3100 + the loosening clause is honest and still meaningful.** Headroom = 3100 − 2909 = 191 ≈ the comment's "~190 chars of tweak headroom"; the paragraph-class example the comment names (~390, a LOW-2-style rewording) → 2909 + 390 = 3299 > 3100 → still breaches. Round 3's note (ii) is addressed in both comments ("bounds loosened 1800 → 2000", "Bounds loosened 3000 → 3100 with F1"), note (i)'s 3100 is adopted, and the measured chain (2460 → 2853 → 2909) is stated with causes. The contract bound (2000 vs 1791, +209 headroom) likewise. The odd two-line string literal at :179-180 (raw newline inside the literal) is legal Rust, renders exactly one trailing newline, and is pinned by the huge-set test — noted only because it reads like an editing artifact; not a finding.

## 6. Acceptance (a)-(e) — all still hold (brief)

(a) round-2+ preamble names base + delta instruction + changed set — renderer :133-148, unit test :244-278, spawn-path test untouched by this delta; (b) the contract block is unconditional (:87-120) and pinned at spawn level; (c) the budget pins and the mechanism stand — this round's own dispatch is the live datapoint: a 5-file delta brief instead of the febcd6f5-era ~28-file full-tree re-read; (d) the Bookkeeping exclusion (:106-109) rides every render (this report's `.coding` section is one line, as contracted); (e) the parent's unpiped run is green (2749/0/5 + 19 + 1/1, exit 0; `#![deny(warnings)]` ⇒ warning-free), frontend untouched by the delta — its `git diff` mentions are the `git_diff_head` DiffViewer IPC, legitimate.

## 7. Production behaviour

**None changed in logic, control flow, or tool surface.** The one production-visible edit is the truncation line's wording form (:179): it now uses the parenthetical `` `git_read` (`op="log"`) `` like every other branch — round 3 called the old two-backtick form "cosmetic … both accurate, just inconsistent". It renders only when >40 paths changed, is in neither budget render, and is now positively pinned by the huge-set test. Everything else in the delta: comments, docs, test code, and test-only bound constants.

## 8. `.coding/**` bookkeeping (one line)

Accurate: the plan's Landed-design numbers (1705/2460, bounds <1800/<2600) are the dated as-landed record; the post-review evolution (1791/2909, <2000/<3100) is documented in the code comments and round reports; the backlog row matches state, the untracked HOW record exists, and the plan frame carries no round stamps only because the running binary predates the change (sanctioned since round 1).

## Observations (not findings)

1. **Live knowledge records still name retired tools** (memory, not docs — outside both sweeps' letter): the 2026-08-22 finish-gate HOW record still advises "git_diff shows nothing. Spawn the pass-2 reviewer with the commit sha (git show <sha>)" — fresh and unflagged; the 2026-08-23 strict-reviewer-surface SPEC record says "reads, git_diff/log/show, …" — already self-documented as known-stale by the 2027-01-11 git-read SPEC record ("Known stale (pre-unification, future doc pass)"), which also confirms agent.md's instance has since been fixed. A small memory_amend pass would close the trap class at its memory root.
2. `src/workflow/mod.rs:2427-2435` and `:2499-2512` use `"git_diff"` as an opaque example token in allow-list mechanics tests — not surface claims (the assertions test filter mechanics, not the reviewer's real list); rename to `"git_read"` only if touched for other reasons.
3. The best-effort clauses (spawn_agent.rs:192-193 and :417-421) say "a git failure degrades to the contract-only preamble" — exact only for a first-round spawn; on round ≥ 2 a changed_paths failure renders contract + delta scope + the unavailable-set instruction (the file set degrades, not the delta block). Pre-existing (read unflagged by rounds 1-3), names no wrong tool, invites no restoration; tighten to "degrades the changed-file set to an instruction (contract-only on a first round)" only if you want it literal.

Test evidence acknowledged (2749+19+1 passed, 0 failed, exit 0 — consistent with a delta that adds no `#[test]`: the helper is a plain fn and the git-failure test was renamed, not added). Not docked, per the dispatch: the harness mechanism not firing live (round 1 sanctioned — the running binary predates the change) and round-2's LOW 3 report-half residual (recorded observation).