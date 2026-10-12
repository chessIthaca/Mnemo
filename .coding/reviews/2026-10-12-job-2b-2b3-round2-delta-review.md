## Verdict: PASS

Round-2 delta review of commit d813510 (base aeba3b6, plan 3ce5ebe2). The delta is exactly one commit carrying the whole 2b.3 step plus the three r1 fixes plus the r1 report; the uncommitted remainder is the round-2 plan-frame stamp alone. All three r1 findings are genuinely fixed and traced; my own five-token sweep independently confirms the boundary test's pass condition; the commit's deviation notes are corroborated by tree evidence. Zero new findings.

Reviewed-state: d813510576fc322329b27fe6474c264b9916b466 (base aeba3b657acd05816417826f09d21b3c86fc1452)

## Scope and what I read

- `git log` (10): the delta since base = exactly d813510; `git status`/`git diff`: one uncommitted hunk — the `2 d813510…` line added to `3ce5ebe2.md`'s `## Reviews` (harness bookkeeping, accurate).
- `git show d813510 --stat`: 53 files, +2870/−1713 — the file set matches the task's changed-file set and the step's scope (the "13 more" are tests/integration/*, project/*, model_resolver.rs, runtime/*, tool/agent — all step-scope moves/flips; the only non-source files are the four .coding bookkeeping files).
- Read in full: `src/lib.rs` (268 lines — crate doc, the whole boundary mod, BANNED_TOKENS, kernel_sources, strip_test_items, kernel_does_not_import_app), the r1 report (52 lines, carried in the commit), the plan frame `3ce5ebe2.md`.
- Independent sweeps (search, literal): all five banned tokens over `src/**`; cfg(test) containment checks at every hit site (factory.rs :1629→1634+, prompt.rs :611→615+, tool/mod.rs :1350→1792+, runtime/agent.rs :1478→1508+, agent/tests.rs whole-file exemption); `WorkflowPolicyFactory::new` + `skills_dir` in both bin arms; the goldens trio's existence.

## (a) Each r1 finding genuinely fixed — traced line by line

**L1 — strip_test_items rewrite (src/lib.rs :104-219).** Confirmed against all three of r1's required cases:
- *Case 1* (single-line `#[cfg(test)] use x;`): the braceless guard at :124 (`!opened && ends_with(';') && !starts_with("//")`) now breaks on the attribute line itself — the item is consumed on line one, no swallow of the following production item.
- *Case 3* (doc-comment `/// see foo();` between attribute and item): excluded by the `starts_with("//")` term, which covers `///` as well as `//`.
- *Case 2* (depth skew from literals): the counter is now literal-aware and I traced each branch: regular strings escape-aware with the `\\` skip-pair (:131-146); the `//` comment break (:148-150) sits after the string branch, so `"http://x"` is consumed by the string scanner while a `"` inside a comment is never reached — the interaction is sound both ways; raw strings (:151-179) accept any hash count, require the `"` after the hashes (raw identifiers `r#ident` fall through harmlessly), and the close search is bounds-safe (`m + hashes < cs.len()` before any `cs[m+1+h]` read; unterminated → whole line consumed — the documented multi-line limit); char literals (:180-205) use the ≤4-content escape-aware lookahead with `closed && len <= 4` required, so `'{'`/`'}'` never count depth while lifetimes fall through as plain chars.
- The fn doc (:88-103) documents exactly the limits r1 accepted as the "or document the limitation" branch: multi-line string literals inside stripped items, and cfg-predicate variants (`#[cfg(all(test, …))]`) NOT stripped — the loud direction, matching r1's case-4 framing.

**L2 — BANNED_TOKENS doc (src/lib.rs :40-55).** The covered-spellings note is present: the five spec tokens named, the deliberately-uncovered routes (`super::app`, `mnemo::app`, crate-alias indirections) named with the sweep date and the crate-split rationale. Exactly the one doc line r1 asked for.

**L3 — crate doc (src/lib.rs :8-11).** The kernel brain list no longer contains workflow; the app-side workflow graph is named (`app::workflow`, job 2b). Matches r1's suggested fix shape.

## (b) No drift beyond the fixes

All three fixes are confined to src/lib.rs; the rest of the commit is the step surface r1 reviewed (the severs, the ~70 mechanical flips, the alias retirement, the new kernel files). Spot-verified in the tree: both bin arms pass `Some(skills)` (main.rs :503-508 block_on, console.rs :1228-1230 native await); the goldens trio exists at the claimed sites (prompt.rs :756/:626, model_resolver.rs :896); the crate-root pub-use aliases are gone from lib.rs (module list only). The r1 report is carried in the commit verbatim.

## (c) Battery claim holds — independent confirmation

My own sweep for the five banned tokens over the kernel tree: alias tokens (`crate::workflow/backlog/skill`) appear only inside lib.rs's own `#[cfg(test)] mod boundary` (doc lines + the token table — stripped by the test's own scanner, self-consistent); every `crate::app`/`tool::workflow` hit sits inside a verified `#[cfg(test)]` mod (factory.rs :1629, prompt.rs :611, tool/mod.rs :1350, runtime/agent.rs :1478) or the whole-file-exempt agent/tests.rs. Zero production-kernel hits — the boundary test's green verdict is confirmed independently of the parent's re-run.

## (d) Commit carries the r1 report + fixes + accurate deviation notes

The r1 report (52 lines, opened with the r1 verdict) is in the commit. The deviation notes are corroborated: `.coding/tmp/commit-msg-2b3-boundary.txt` exists (message-file form), and lib.rs :88/:220 show the glued-line doc-comment formatting consistent with the claimed chunked full-file write after the emission-artifact guard rejections. Nothing in the notes contradicts the tree.

## Contract checks

- *Documentation sync*: L3's crate doc, L2's token note, L1's fn-doc limit notes — the change's docs updated in the same commit. In sync.
- *Multi-platform neutrality*: the boundary test uses fs/PathBuf with `replace('\\', "/")` for display only; no platform-conditional code anywhere in the delta; the moves are verbatim.
- *File-tools-first*: no shell-based mutation in the delta; the one file-write fallback (full-file chunks) is documented with its justification in the commit message.
- *Warning-free / no `#[allow]` tricks*: no `#[allow]` additions in the delta; the boundary mod compiles clean under the crate root's deny(warnings) (green battery claim).
- *Bookkeeping accuracy (one line)*: plan frame 3ce5ebe2.md records the landed step + both review stamps (base aeba3b6, round-2 d813510); the commit message's gate numbers and file set match the stat — accurate.

## Residual remarks (no action required — both loud-direction and inert, recorded for the record)

1. The `;`-guard excludes only `//`-prefixed lines; a block-comment line (`/* …; */`) between attribute and item would break the strip early — a false positive (loud) in a contrived shape absent from the tree; not named in the fn doc's known-limits list.
2. Char-literal escapes longer than the ≤4 bound (e.g. `'\u{1F600}'`) fall through the lookahead and count their balanced `{}` — a spurious early break (loud) if it ever appears inside a stripped item; the ≤4 rule is stated in the doc as implemented, but the escape-shape gap is not spelled out in the known-limits list.
3. Cosmetic: the `}    /// …` glued doc-comment formatting at src/lib.rs :88 and :220 (the full-file-write artifact) — valid Rust, attaches correctly, zero warnings; a future formatter pass would tidy it.

## Acceptance checklist

- (a) All three r1 findings fixed and traced — ✓.
- (b) No drift beyond the fixes — ✓ (single commit; fixes confined to src/lib.rs; bookkeeping only in .coding).
- (c) Battery claim holds — ✓ (boundary test present, pass condition independently confirmed).
- (d) Commit carries the r1 report + fixes + accurate deviation notes — ✓.

Reviewed-state: d813510576fc322329b27fe6474c264b9916b466
