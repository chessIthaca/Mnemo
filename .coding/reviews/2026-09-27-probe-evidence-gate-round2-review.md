## Verdict: FINDINGS (0 high, 1 low)

Round-2 delta review of plan 0011b40b (probe-evidence gate, branch wt/mnemo). Both round-1 fixes are present and correct — Fix 1 (drift-proof citation) and Fix 2 (the `/` path-segment guard) verify exactly as described, and the red-check evidence is consistent with the code. One low finding: the rule descriptions that enumerate the tightened rule's exclusions (the SPEC amendment and PLAN.md's gate section) were not extended with the new `/`-preceded path-segment exclusion the guard added, so they no longer describe the shipped detection rule completely.

## Scope and what I read

Base `674aabec`; the delta is one commit, `23ad8fb` (the whole feature + both fixes; round 1 verified its pre-fix state), plus one uncommitted line (the round-2 stamp on `.coding/plans/0011b40b.md` — expected harness bookkeeping, not reviewed as content). Read for this round:

- `git log` (delta = 23ad8fb only), `git show --stat 23ad8fb`, `git diff HEAD` (the stamp), `git status`.
- The fix hunks in the tree: the `holds_dotted_call` doc comment + body (plan.rs:358-413), `contains_marker` / both `carries_*` / `reachability_evidence_issue` (415-507, invariants the guard touches), the pin test `validator_ignores_plain_symbol_mentions` (2758-2774) and its neighbours `create_plan_works` / `create_plan_rides_recalled_context` (2777-2833).
- The fix-1 surfaces: the resumability-gate SPEC amendment (full file) and PLAN.md's gate section (470-499).
- Bookkeeping accuracy: the pre-prompt-model-routing SPEC (new file riding this commit), plan files 0011b40b + d69abf72, the round-1 report.

Round-1-verified material (gate wiring, update_plan scoping, the other four tests, budget entries in factory.rs, GOOD_CTX) was not re-line-reviewed; the fixes cannot touch it (a `///` comment, a code guard, and test lines are not schema text, so the round-1 budget verification holds unchanged).

## Fix 1 (round-1 L1, stale citation) — VERIFIED

- plan.rs:366 now cites "pinned by `validator_ignores_plain_symbol_mentions`; see the amendment in plan 0011b40b" — a test name plus a plan-id pointer, both drift-proof (no line numbers). The test exists and does pin exactly what the comment claims (the `boil()` fixture that disproved the broad rule).
- The SPEC amendment's citation is repaired the same way: "disproven mid-implementation by the `boil()` fixture (pinned by `validator_ignores_plain_symbol_mentions`)" — no line pointer remains. The rest of the amendment's rule description, marker vocabularies, root exclusions, word-start matching, append scoping, test names and ceiling figures all match the shipped code — except the gap in L1 below.

## Fix 2 (round-1 L2, path-paren false positive) — VERIFIED

- The guard (plan.rs:386-390) skips a paren candidate whose identifier run is immediately preceded by `/`, via `continue` — only that candidate; other `(`s on the same line are still evaluated. Byte-level `/` is ASCII, so a multi-byte UTF-8 char can never masquerade as the separator. A claim at line start (`start == 0`) is unaffected.
- The pin is genuinely exercising the guard: `static/scripts/tools.js(862)` (plan.rs:2767) — without the guard, run `tools.js` → member `js` (len 2, alphabetic), root `tools` (not in self/this/crate/super) → flagged; with it, skipped. That is the only `(`-candidate in the pin's context that the guard affects (`boil()` has no dot, `self.boiler.running()` is root-excluded, `RoutingGate::with_log_path` has no paren), so the dispatch's red-check failure message — exactly "1 such line(s)" — matches the code's behavior, and the pin fails without the guard and passes with it. The rest of the pin (bare call, `::` path, self-rooted, `src/kettle.rs:42`) still exercises the round-1-verified exclusions.
- **Risk (a) — cannot mask a real claim:** the guard fires only when the byte before the run is `/`, i.e. the run is a path segment. Every real claim shape in the vocabulary — `ext._evaluator.clashOf()`, `doc.getElementById("x")` — never follows a `/`; the five PHRASES are plain substrings on the lowercased line, untouched by the guard; and the ShapeGraph locator in the plan context (`static/scripts/shapegraph/tools.js:862`) uses `:`, not `(`. No marker or phrase shape became invisible. The only theoretically-masked shape is a dotted call literally written after a slash inside a path — not a shape any honest plan produces, and prose claims on the same line still hit the phrase list.
- **Risk (c) — no regression:** `contains_marker` (word-start matching), the assumption-first ordering, the one-issue message shape, and the update_plan append-scoping are all unchanged from the round-1-verified state; the guard only reduces flagging, so GOOD_CTX and the evidenced/labelled/tool-boundary tests are unaffected logically, and the dispatch's `cargo test --workspace` exit 0 + gate suite 120/120 confirms it. Multi-platform neutral (pure ASCII byte handling); no shell mutation in the delta; warning-free per the green workspace pass under `#![deny(warnings)]`.

## Findings

### L1 (low) — the rule descriptions omit the new `/`-path-segment exclusion

The guard added by Fix 2 is a new boundary of the CLAIM shape ("`src/kettle.rs(42)` / `static/scripts/tools.js(862)` is a path locator, not a member call"), but the two places that enumerate the tightened rule's exclusions were not extended:

- PLAN.md:496-497 — "A bare call (`boil()`), a `::` path and a `self.`-rooted call are ordinary mentions, not claims."
- The SPEC amendment (.coding/knowledge/spec/2027-01-07-plan-resumability-gate-…md, amendment dated 2027-01-11) — "…tightened, so bare calls, `::` paths and `self.`-rooted calls are ordinary mentions."

A reader of either would conclude `static/scripts/tools.js(862)` is flagged — contradicting the shipped code and the pin that proves the opposite. The dispatch explicitly asks whether the SPEC still describes the shipped code accurately; on this clause it does not. Fix: one phrase in each place, e.g. append "and a `/`-preceded path segment before `(` (`src/kettle.rs(42)`) is a locator, not a call" to PLAN.md's sentence and the matching clause to the SPEC's exclusion list (the SPEC amendment is the durable knowledge record, so it matters most).

## Constitution checks

- **Documentation sync** — L1 above (PLAN.md gate section + SPEC amendment). README carries no gate documentation (round 1 confirmed; the fixes change nothing user-facing there). Module doc comments on the changed helper are accurate post-fix.
- **Multi-platform neutrality** — pure byte-level ASCII string handling; no Windows-only API, path or shell syntax in the delta.
- **File-tools-first** — the delta shows normal source/bookkeeping edits; no shell-based mutation.
- **Warning-free build** — per the dispatch evidence, `cargo test --workspace` exits 0, which under `#![deny(warnings)]` proves zero warnings; no `#[allow]` added.
- **.coding accuracy (one line)** — bookkeeping matches what shipped: the routing SPEC riding this commit names exactly the commits/reviews below/at base; backlog.jsonl and plan d69abf72 are consistent; plan 0011b40b's landed-design amendment still carries "(plan.rs:2719)", a dated mid-flight pointer that was true when written and drifted when the same branch's later tests landed — plan files are time-stamped process records, noted here only so the parent knows it was seen, not a finding.
- **Uncommitted carry-over** — the only uncommitted change is the plan-frame round-2 stamp; no round-1-PASS hunks reappear in this delta (this plan's round 1 ended in FINDINGS, so none of its material is carry-over).

Reviewed-state: 23ad8fb1045ac205f19d5e235946085558a88ae8
