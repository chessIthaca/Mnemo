## Verdict: FINDINGS (0 high, 3 low)

Round-3 (delta-scoped) verification of the round-2 F1/F2 fixes for plan f4636852 / backlog 85313a7e, base 4a2bd99, delta = `src/agent/review_scope.rs`, `src/tool/agent/spawn_agent.rs`, `agent.md`, `PLAN.md` (wording/docs only — no production behaviour changed). **Both round-2 findings are fixed and verified at the rendered-text level:** no unregistered git tool name survives any render branch (F1), and all four flagged prose claims read accurately (F2). The sweep the task asked for, however, surfaced two sibling comments still carrying F2's exact stale construction (LOW 1), stale legacy tool names in live docs outside the delta (LOW 2), and two render branches the new negative assertion does not reach (LOW 3). All three are wording/test-precision; nothing found touches production behaviour. Per the task's instruction, the harness mechanism not firing live this session is not held against the round, and round-2's LOW 3 report-half residual is not re-raised.

### F1 — verified fixed (every render branch checked)

All branches of `render_review_preamble` (review_scope.rs:87-192):

| Branch | Rendered tool naming | Ops valid |
|---|---|---|
| Contract Empty-diff rule (:112-114) | `git_read` (`op="log"`), `git_read` (`op="show"`) | log, show ✓ |
| Round ≥ 2, missing/blank base (:121-130) | none emitted | — ✓ |
| Delta intro (:140-142) | `git_read` (`op="log"`), (`op="show"`), (`op="diff"`) | log, show, diff ✓ |
| Unavailable-set branch (:156-158) | `git_read` (`op="log"`), `git_read` (`op="diff"`) | log, diff ✓ |
| Empty-set branch (:163-169) | none emitted | — ✓ |
| Non-empty set truncation line (:179) | `git_read` `op="log"` | log ✓ |
| Carry-over (:186-192) | none emitted | — ✓ |

- Every op named exists in `git_read`'s schema enum `["diff","log","show","status"]` (git_read_tool.rs:63-81); `git_read` is the registered name (:63-65). Direct witness: my own live surface exposes exactly `git_read` with those four ops — no `git_log`/`git_show`/`git_diff` tool exists to call.
- None of the forbidden strings appears in any rendered text (the `` `git_read` (`op="…` `` forms don't contain them as substrings), and no bare `git diff <base>`-style instruction survives (:270-273 and :290 pin the absence).
- The naming drift round 2 traced is gone from all three spots: agent.md's reviewer-surface bullet now reads `git_read` (op `diff`/`log`/`show`/`status`); the spawn tool's `role:"reviewer"` schema description now reads `git_read (op diff/log/show/status)`; PLAN.md's reviewer-surface paragraph names the `git_read` ops. The `MAX_LISTED_PATHS` comment (:61-64) and the tests' own explanatory prose (:231-234) name only `git_read`. ✓
- Cosmetic, not a finding: the truncation line renders `` `git_read` `op="log"` `` (two backticked tokens, no parens) while every other branch renders `` `git_read` (`op="log"`) `` — both accurate, just inconsistent.

### F2 — verified fixed (four spots) + the sweep

All four flagged spots corrected and accurate:
1. `review_scope.rs:20-25` module doc — "the delta-since-`<base>` instruction (naming the reviewer's own `git_read` ops)" ✓
2. `PLAN.md:1291-1293` — "the delta-scope instruction (naming the reviewer's own `git_read` ops) and the changed file set (derived from `git diff --name-status <base>` + untracked files)" ✓ — and the *retained* `--name-status` parenthetical is accurate: `changed_paths_since_impl` runs exactly `git diff --name-status <base>` plus `status --porcelain -uall` for untracked (git_ops.rs:192-220). Keeping it was right.
3. `spawn_agent.rs:405-415` execute comment — "the delta-since-`<base>` instruction (naming the reviewer's own `git_read` ops)" ✓
4. `agent.md` delta bullet — "the preamble names that base, the delta scope and the mechanically-derived changed file set" ✓

**README.md:60 / docs/FEATURES.md:10 — I agree with round 2's "defensible" judgment.** Both describe what the *app* scopes the review to, not what the preamble instructs — scope concept, not preamble-content claim — and the shorthand names a real mechanic (the app genuinely runs `git diff --name-status <base>` to derive the set). Same class: `plan_file.rs:142`/:155 (what the delta *is*; stamp semantics) and `agent.md:145` (the step-boundary commit convention — the main agent's own surface, which does have the `git` tool). None claim the preamble carries the instruction; leave them.

**Over-/under-correction of the reworded spots — neither.** agent.md now claims exactly what the preamble carries (base, delta scope, changed set), nothing lost; PLAN.md corrected the preamble-instruction claim while correctly *retaining* the app's own derivation command. The distinction round 2 drew is kept precisely where it matters.

**Sweep result** — the sweep for other preamble-carries-`git diff <base>` claims found the identical stale construction in two sibling comments the round-2 list missed → LOW 1 below. A wider sweep for legacy tool *names* in live prose found four more spots outside the delta → LOW 2.
### Findings

**LOW 1 — F2 residual: two sibling comments still describe the preamble's delta scope as `git diff <base>`.**
`src/tool/agent/spawn_agent.rs:189` (the doc comment of `with_review_scope` itself): "…and, for round >= 2, the delta scope (`git diff <base>` plus the mechanically-derived changed file set)". `src/agent/factory.rs:1493` (the `register_spawn_tool` wiring comment): "…and for round >= 2 the delta scope (`git diff <base>` + the changed file set)". Both are the *identical construction* round-2's F2 flagged in agent.md:142 ("the `git diff <base>` scope" — flagged as "softer, same drift"), and they carry the exact risk F2 was filed against: an agent reading `with_review_scope`'s own doc comment or the factory wiring comment believes the preamble names a `git diff <base>` scope and could "restore" it. The fix swept the four listed spots but missed these two siblings (round 2's F2 list didn't include them either). Both predate this delta and are untouched by it — comment-only, no behaviour.
Suggested fix: mirror the corrected execute comment, e.g. "the delta scope (the delta since `<base>`, named via the reviewer's own `git_read` ops, plus the mechanically-derived changed file set)".

**LOW 2 — F1 class, live docs outside the delta (pre-existing): the retired tool trio still named in prose.**
- `docs/FEATURES.md:46` — "a strict read-only reviewer role (reads + `git_diff`/`git_log`/`git_show`, `web_fetch`, graph tools…)" — names the reviewer's tool surface with three unregistered tools. This is the sibling sentence of the two spots F1 *did* fix (agent.md's reviewer-surface bullet and the spawn schema description); fixing two of three siblings leaves the doc set inconsistent about the same fact.
- `docs/FEATURES.md:32` — "…plus read-only `git_log`/`git_show`".
- `docs/FEATURES.md:71` — "mirroring the read-only `git_diff` tool".
- `PLAN.md:523` — "verify with `git_log`/`git_show` when it matters" — contradicts agent.md's own long-updated rule ("verify with `git_read`").
All four predate this delta (nothing in the diff touched them) — documentation-sync debt, not a regression — but they are user-facing docs naming tools that do not exist, squarely the unregistered-names class F1 swept elsewhere. Suggested: PLAN.md:523 → "verify with `git_read` when it matters"; FEATURES.md:46 → "reads + `git_read` (op diff/log/show/status)"; :32/:71 accordingly — or park them as a backlog item if you want this delta minimal (they are not caused by this work).

**LOW 3 — test-precision gap: the new negative assertion does not reach the truncation line or the unavailable branch.**
The negative trio check (review_scope.rs:266-269) is genuine and non-vacuous on the render it runs — but it runs only inside `second_round_names_the_base_…` (:246-278), whose render exercises the contract block, delta intro, un-truncated changed set and carry-over. Two tool-name-emitting branches sit outside its reach, and their positive assertions don't close the gap:
- **Truncation line (:179)** — `a_huge_changed_set_is_capped` (:404-419) asserts only the cap ("… and 5 more", `!contains("M src/f44.rs")`). Reintroducing the pre-F1 truncation wording "(`git_log` since `{base}` lists them)" verbatim passes the entire suite. Zero tool-name coverage on that render.
- **Unavailable branch (:156-158)** — `a_git_failure_renders_the_command_instead_of_a_file_list` (:307-324) pins only the leading mention ("Enumerate it yourself: `git_read`", :315) and `` `op="diff"` `` (:316) — which the delta intro in the same render also satisfies, so it does not uniquely pin the branch's trailing "then `git_read` (`op=\"diff\"`)". Regressing that trailing mention to `git_diff` (the exact pre-F1 text) passes the suite. No negative check runs on this render either.
Consequently the comment :262-265 overclaims: "Pin the absence, so the next wording change cannot reintroduce them" — a wording change to those two branches *can*. Same class as round-1's vacuous-PASS edge, so filed the same way.
Suggested fix: run the negative trio check on the huge-set and git-failure renders too (and/or positively pin "`git_read` `op=\"log\"` enumerates the rest" and "then `git_read` (`op=\"diff\"`) for the uncommitted remainder"), or narrow the comment to the branches actually covered. Optional while there: the git-failure test's name still says "renders the command" — F1-era wording; it renders an instruction now.
### Test assertions — non-vacuity (task item 3)

- **Positive pins match the current text.** The contract's Empty-diff rule is pinned by the exact current rendered string (:235-238, "walk the branch's commits with `git_read` (`op=\"log\"`)"); the round-2 render pins all three op forms (:259-261) plus the base/delta phrasing (:252-258); the third-round render pins "the delta since `def5678`" (:291) and the absence of "`git diff def5678`" (:290). A green suite therefore proves the assertions were updated with the wording, not left behind.
- **The negative check (:266-269) is non-vacuous in both directions.** None of `git_log`/`git_show`/`git_diff` appears in the current render (verified branch-by-branch above; no substring false-positives either), and the pre-F1 wording carried exactly those strings in exactly this render — genuinely red before the fix, green after. The `!contains("git diff abc1234")` (:270-273) and `!contains("`git diff def5678`")` (:290) guards are likewise live, not tautological.
- **Coverage map.** The round-2 render reaches the contract block, delta intro, un-truncated changed set and carry-over — those branches are guarded by the negative check. The truncation line and the unavailable branch are not (LOW 3).

### Budgets — honest, bounds still catch the claimed class (task item 4)

- **Independent count.** I re-derived both rendered texts character-by-character from the source literals (em-dashes counted as 3 bytes, as `.len()` does): contract **1794**, round-2 delta render **2910** (delta section 1116). Within ±3 hand-count noise of the claimed **1791 / 2909** — the measured values in the comments (:358-361, :373-376) are honest against the rendered text.
- **History chain.** 1705/2460 (landing — both figures quoted by round 2's report) → 2853 (round-1 LOW-1/2 rewording — measured and cross-checked by round 2) → 1791/2909 (F1 — verified here). Every recorded point was checked by the round that asserted it; the increments (+86 contract / +56 delta) cohere with the F1 op-form expansion within the uncertainty of the pre-F1 wording, which was never committed and survives only in round 2's abbreviated quotes.
- **Bounds.** Loosened 1800→2000 and 3000→3200 in this delta. The loosening was *not forced* — 1791 and 2909 both fit the old bounds — it restores the tweak-class headroom F1 consumed (the contract had 9 chars of headroom at <1800, which would make the budget test churn on any future comma). The new headroom (209/291 chars) still sits below the paragraph class the budget exists to catch: the +393-char LOW-2-style rewording breaches both (1791+393=2184>2000; 2909+393=3302>3200). Honest and still meaningful. Two notes, not findings: (i) the delta bound would let a ~250-char short paragraph through (3159<3200) — if you want it tighter, 3100 keeps ~190 chars of headroom; (ii) the comments record the measured history but not the bound loosening itself — one clause ("bounds loosened 1800/3000 → 2000/3200 after F1's growth") would spare the next reader the archaeology.

### Acceptance criteria (a)-(e) — still hold (task item 5, brief)

- (a) ✓ — the round≥2 render names the base revision, the delta instruction and the changed set (review_scope.rs:136-148; :252-258 pins it; the spawn-path tests' pinned strings were untouched by this delta and the suite is green).
- (b) ✓ — the contract block renders unconditionally in the reviewer branch (:87-116; `the_contract_carries…` :216-240 is annotated as acceptance (b)); the dispatcher supplies nothing.
- (c) ✓ — round 2 verified the measurement record; this delta adds 86/56 chars to the renders — no material change to the cost model.
- (d) ✓ — the `.coding/**` accuracy-in-one-line rule rides every render (:108-115).
- (e) ✓ — the parent's unpiped evidence (2749 passed / 0 failed / 5 ignored, +19, +1, exit 0) plus `#![deny(warnings)]` at both crate roots means green implies warning-free; the frontend is untouched by this delta. I did not re-run the suite (read-only reviewer); everything I verified statically is consistent with it.

### `.coding/**` bookkeeping (accuracy, one line)

Plan f4636852.md accurately records the design sketch and the as-landed state — its `git diff <base>` mentions are dated design/Landed-design text, superseded in-tree by the round-1/2 fixes documented in .coding/reviews/ — and the frame carries no round stamps only because the running app binary predates the change (sanctioned, not a defect).