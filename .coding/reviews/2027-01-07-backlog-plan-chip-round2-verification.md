## Verdict: PASS

Round-2 verification of plan 33383b61 "Backlog plan chip: human-friendly clickable plan identifier" (backlog f45513b2, user-reported) — commit `82466c3` (parent `734265e`) on `wt/agenticcoding`, working tree clean at HEAD. All four round-1 dispositions (LOW 1, LOW 2, Nit 1, Nit 2) are verified fixed in the commit; the two `fn_body` source-contract tests remain valid after the helper move; the line-accounting reconcile shows no unrelated drift; and the security / multi-platform / correctness properties round-1 verified are undisturbed. No new findings.

## The four fixes

### LOW 1 — Doc-comment misattachment: FIXED
`src-tauri/src/ipc/run_all.rs`. `stamp_backlog_in_flight`'s original doc (lines 2098–2112, beginning "Stamp the run-all's in-flight item (or the single-dispatch in-flight item) `Pending` → `InFlight`, preserving its note…") is re-attached directly above the `pub(crate)` signature (2113). `plan_title_from_file` (2164) now sits BELOW the stamp (which closes at 2155), separated by a blank line, carrying only its own six-line doc (2157–2163, "Read a plan's TITLE from its plan file…"). No merged `///` blocks remain: the `pub(crate)` stamp is documented again, and the helper's doc describes the helper.

### LOW 2 — README doc sync: FIXED
The Backlog bullet (README.md:77) now ends: "…pre-planning it stays pending; in-flight items identify the working plan by title + short id as a chip that switches to the working agent's chat tab (inert once done/failed), with the pre-work git checkpoint sha demoted to a copyable detail row — the manual resume/rollback anchor." Verified accurate against the component: the chip renders whenever `item.plan_id` is set (BacklogView.tsx:782); it is clickable only while in flight with a main agent present (`onClick={chipTarget !== null ? () => setActiveAgent(chipTarget) : undefined}`, :786) with `cursor-pointer` styling and the "Switch to the working agent's chat tab" tooltip; done/failed (or agent-less) items get `cursor-default` + the inert tooltip (:790–795) — the "(inert once done/failed)" parenthetical covers exactly this. The checkpoint sha renders as a copyable detail row (`pre-work checkpoint <8-char>…` + a copy button writing the FULL sha) — "demoted to a copyable detail row — the manual resume/rollback anchor" is precise.

### Nit 1 — Inert-chip tooltip wording: FIXED
BacklogView.tsx:795 now reads "The plan dispatched for this item (no agent currently working it)" — the suggested unambiguous phrasing, correct for both done and failed items.

### Nit 2 — Heading-only read: FIXED
`plan_title_from_file` (run_all.rs:2164–2173) now opens the file and reads ONLY the first line via `std::io::BufReader` + `read_line` (`use std::io::BufRead` scoped inside the fn) instead of `read_to_string` of the whole file. Semantics preserved, verified case-by-case:
- missing file → `File::open(...).ok()?` → `None`;
- empty file → `read_line` returns `Ok(0)`, `first` stays `""` → `strip_prefix` → `None`;
- first line without the `# Plan: ` prefix → `None`;
- heading with trailing newline/whitespace → `t.trim()` (the happy-path test writes `"# Plan: Fix the widget\n\n## Goal\nDo it."` and asserts `Some("Fix the widget")` — the trailing `\n` is consumed by trim);
- empty title after trim → `.filter(|t| !t.is_empty())` → `None` (pinned by the `empty.md` case `"# Plan: \n"`).

The unit tests `plan_title_from_file_parses_the_heading` / `plan_title_from_file_handles_missing_or_malformed` (run_all.rs test module, ~978–1006) are in the commit and cover missing file, heading-not-line-1, and empty-title-after-trim. (The zero-byte empty-file input is not a separate test case, but it collapses into the same `strip_prefix` → `None` arm the `nohead.md` case exercises — semantics verified from source; not a coverage gap worth a finding.)

## Source-contract tests after the helper move — VALID
`fn_body` (run_all.rs:526–536) extracts from `fn {name}(` through the first column-0 `\n}\n`. For `stamp_backlog_in_flight` that is lines 2113–2155 (every inner brace is indented), which contains all asserted snippets:
- `in_flight_stamp_only_applies_to_pending_items` (:685–705): `Some((BacklogStatus::Pending, note))` (:2142), `store.transition(&id, BacklogStatus::InFlight, note)` (:2143), `store.set_plan_id(&id, top_plan_id, plan_title.as_deref())` (:2154) — all present.
- `stamp_backlog_in_flight_records_the_plan_title` (:1009–1030): `plan_title_from_file` is found at :2153 — the CALL inside the stamp's body; the helper's own definition (:2164) falls outside the extracted range, so the move did not orphan the assertion. `set_plan_id` at :2154 satisfies `read < set`, and `plans_dir` (:2152) is present.

The needle `fn stamp_backlog_in_flight(` cannot self-match the test module (the tests pass the bare name as a string; the `fn_body` doc comment at :520–525 documents exactly this hazard and why the composed needle avoids it).

## Line-accounting reconcile — CLEAN
The commit's 15 files map exactly to: the round-1-reviewed change set + the four fixes + the round-1 report file itself (`.coding/reviews/2027-01-07-backlog-plan-chip-review.md`, committed with the change per the closing sequence) + bookkeeping (`.coding/backlog.jsonl` item f45513b2 pending→in_flight with the checkpoint note + plan_id; the previously-untracked `.coding/plans/33383b61.md`). Every hunk outside the four fixes matches what round-1's verification detail describes (stamp logic, serde skip-if-none attributes, fixture values `593f4a4e` / `Second task plan`, TS field docs, test assertions). No unrelated drift.

One observation, not a finding: round-1's scope list described `src/project/git_ops.rs` as "(doc comment only)", but the commit's delta there is `plan_title: None` in a test fixture (:1020, the `item()` helper's `BacklogItem` literal). That line is compile-required by the new field (the literal enumerates every field) and therefore must have been present in the round-1 state too — round-1's parenthetical was a prose inaccuracy in that report, not drift smuggled into the commit.

## Spot-checks (round-1 axes re-confirmed)
- **Security:** `plan_title` renders as a React text node (`{item.plan_title}` inside a `<span>`, BacklogView.tsx:799) — auto-escaped; zero `dangerouslySetInnerHTML` occurrences in BacklogView.tsx (searched); the `title={item.checkpoint_sha}` tooltip is attribute-escaped by React; no new IPC surface (both fields ride the existing backlog-changed payload).
- **Multi-platform neutrality:** frontend uses only `navigator.clipboard.writeText` + `window.setTimeout`/`clearTimeout` (the existing prompt-copy pattern in the same component); the Rust side uses `std::fs::File::open`, `std::io::BufReader`/`read_line`, `std::path::Path::join`, and `tempfile` in tests — all cross-platform. No `cfg(windows)`, no Windows-only paths or shell syntax.
- **Correctness:** `set_plan_id`'s signature went 2→3 args; the code graph shows exactly two callers — the stamp (run_all.rs:2154, 3-arg) and the updated `set_plan_id_records_the_item_plan_linkage` test — so no call site was missed. Every `BacklogItem` literal in the tree carries `plan_title` (src/backlog.rs ×4, backlog_cmds.rs, contract_fixtures.rs ×2, git_ops.rs) — compile-complete. The new `.tsx` tests reference only pre-existing helpers (`renderCard` :36, `buttonMarkup` :43) and exported symbols (`planChipTarget`, `BacklogItemCard`).

## Test-claim consistency
The claimed post-fix results (root `cargo test` 2018+16, `cargo test -p mnemo-app` 226+4, `npx tsc --noEmit` clean, vitest 978) are consistent with the diff: no `#![deny(warnings)]` hazards (`use std::io::BufRead` is used by `read_line`; the `AgentId` import is used in `planChipTarget`'s signature; no dead code; no `#[allow]`), all struct literals and call sites updated, and the new tests bind to symbols that exist. As a read-only reviewer I could not execute the suites; nothing in the source contradicts the claims.

## Verdict rationale
All four round-1 dispositions landed exactly as prescribed; the helper move left both `fn_body` source-contract tests valid (their assertions reference only the stamp's body, which still contains the read-then-set sequence); the reconcile found no drift; and the security / platform / correctness properties round-1 verified are undisturbed by the fixes. PASS.
