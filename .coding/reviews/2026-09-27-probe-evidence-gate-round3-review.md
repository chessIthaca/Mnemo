## Verdict: PASS

Round-3 delta review of the probe-evidence gate (plan 0011b40b, backlog 5b232b8d) on `wt/mnemo`, base `23ad8fb`. Scope: commits `bc1af55` + `d33e156` plus the uncommitted remainder. The round-2 low finding (rule descriptions omitted the `/`-preceded path-segment exclusion) is fixed correctly, the delta is genuinely docs-only, and the working tree carries nothing but the harness's round-3 review stamp.

## What I read

- `git show d33e156` (full diff): PLAN.md exclusion sentence + SPEC amendment — the `/`-preceded path-segment clause added to both, nothing else.
- `git show --stat bc1af55`: the round-2 review report + a one-line plan stamp — bookkeeping only.
- `git log` (10): delta since base is exactly `bc1af55`, `d33e156`.
- `git status` + `git diff HEAD`: one uncommitted line — `.coding/plans/0011b40b.md` gains `3 d33e156e6e9bcc4c57f03355c03a6090635abbd1`, the harness's round-3 base stamp, accurate against HEAD.
- `src/tool/workflow/plan.rs` lines 360-469: `holds_dotted_call`, `contains_marker`, `carries_assumption_label`, `carries_evidence_marker` — the shipped detector, unchanged by the delta.

## (a) Descriptions now match the shipped detector exactly

The shipped guard (`holds_dotted_call`, plan.rs:367-413) skips an identifier run before `(` when (1) the byte immediately preceding the run is `/` (line 388 — the path-segment exclusion), (2) the run has no dot or its member is <2 chars / non-alpha, or (3) its root lowercases to `self`/`this`/`crate`/`super`.

- **PLAN.md** now reads: "A bare call (`boil()`), a `::` path, a `self.`-rooted call and a `/`-preceded path segment before `(` (`src/kettle.rs(42)`) are ordinary mentions or locators, not claims." Each named category is skipped by a distinct guard arm (no dot / `:` breaks the walk-back / ROOTS / `/` check); the new clause names exactly the line-388 behaviour — neither more nor less. It claims no more: `ext._evaluator.clashOf()` and `doc.getElementById("x")` (dotted, not `/`-preceded) remain claims. It claims no less: nothing excluded by the code is described as a claim.
- **SPEC amendment** (`2027-01-07-plan-resumability-gate-…md`) adds the same clause with two fixtures: `src/kettle.rs(42)` → run `kettle.rs`, preceded by `/` → skipped; `static/scripts/tools.js(862)` → run `tools.js`, preceded by `/` → skipped. Both trace precisely through the guard. The clause still enumerates the four roots (`self`/`this`/`crate`/`super`) matching `ROOTS`, and the five existence phrases (`exists at` / `is callable` / `callable from` / `is reachable` / `reachable from`) remain outside `holds_dotted_call` — untouched by it, as both descriptions state.
- The in-code doc comment (lines 386-387) already carried the same wording; docs, SPEC, and code are now consistent across all three.

The pre-existing `self.`-rooted phrasing in PLAN.md (naming only `self` where the code excludes four roots) is an example list, is factually true of every root it names, and was part of the material rounds 1-2 already verified — noted, not re-line-reviewed.

## (b) Delta is docs-only

`d33e156` touches exactly two Markdown files (`PLAN.md`, the SPEC knowledge record). `bc1af55` adds only the round-2 report and a plan stamp. No source, config, build, or generated file changed since base.

## (c) No drift in the working tree

`git status --short`: `M .coding/plans/0011b40b.md` only; the diff is the single review-stamp line the harness appends at dispatch. Nothing else is uncommitted.

## Constitution checks

- **Documentation sync** — this change *is* the documentation sync; PLAN.md and the SPEC amendment now match the shipped detector. No further doc surface affected.
- **Multi-platform neutrality** — docs-only delta; no platform-specific anything.
- **File-tools-first** — no file mutations in the delta.
- **Warning-free build** — `cargo test --workspace` exit 0 reported at `d33e156`; docs-only change cannot affect the `#![deny(warnings)]` build.
- **`.coding/**` accuracy** — the round-2 review report and both plan stamps (`2 23ad8fb…`, `3 d33e156…`) match the shipped state; the review chain is consistent.

## Evidence

`cargo test --workspace` exit 0 at `d33e156` (per dispatch; the delta contains no source change, so the round-1/round-2 verified material is untouched by construction — confirmed by the empty source diff in the delta).

Reviewed-state: d33e156e6e9bcc4c57f03355c03a6090635abbd1
