## Verdict: PASS

Round-2 delta-scoped verification of plan 7d1da8ea (base `bbff566`): the delta is the single commit `7b7691b` (all 20 files, +1373/−16) plus the harness's uncommitted review-stamp update to `.coding/plans/7d1da8ea.md` (expected bookkeeping — process remark only). Working tree otherwise clean. Both round-1 findings are fixed in the committed source, nothing else changed in their blast radius, and the acceptance criteria still hold structurally. What I actually read: the commit (`git show --stat 7b7691b`), `git diff HEAD`, `tool_choice.rs` in relevant slices (lines 80–130, 195–246, 248–439, 636–755), the steering blocks in `search.rs:1300–1429` and `search_read.rs:195–274`, both `steering_handle` test helpers (`search.rs:1676–1694`, `search_read.rs:551–567`), the app wiring at `src-tauri/src/main.rs:2026–2050`, and the round-1 report. I could NOT execute tests (read-only); the parent's green claims (cargo test 2507 passing warning-free, npm 1297) rest on its runs.

## L1 — LowConfidence confidence in the training row: FIXED

- `log_decision` (`tool_choice.rs:373–382`): the `FellBack` arm now maps `ToolChoiceFallback::LowConfidence(c) => Some(*c)` with a comment explaining the calibration signal; every other fallback reason falls to `_ => None`. Exactly as required.
- `ToolChoiceLogRow.confidence` doc (`tool_choice.rs:335–337`): "when it steered — or when it answered below the threshold (the calibration signal a fine-tune wants). `None` when the classifier never answered at all" — matches the implementation precisely; the previous overstatement is gone.
- The **Steered arm is untouched** (`:371`): `(Some(choice.as_str()), Some(*confidence), None)` — risk focus confirmed, Steered rows still record the classifier's confidence (`a_steered_decision_logs_one_row_with_the_class` asserts `Some(0.9)` at `:696`).
- The test `a_fallback_row_carries_the_reason_slug` (`:703–730`) now asserts `rows[0].confidence == Some(0.5)` with the message "a below-threshold answer still records its confidence", alongside the existing `low_confidence` slug and escaped-signal asserts.
- Serde round-trip intact: `ToolChoiceLogRow` derives `Serialize` + `Deserialize` + `PartialEq` (`:323`); the test helper `read_rows` (`:668–674`) deserializes exactly what `log_decision` serialized, so both log tests exercise the full round-trip.

## L2 — per-module test log paths: FIXED

- `search.rs:1692`: `log_path: Some(std::env::temp_dir().join("mnemo_tool_choice_test_search.jsonl"))`.
- `search_read.rs:567`: `log_path: Some(std::env::temp_dir().join("mnemo_tool_choice_test_search_read.jsonl"))`.
- Distinct names — the two modules no longer collide on one file. The row-count/field asserts live in `tool_choice.rs` (fresh `tempfile::tempdir()` per test), while the search/search_read tests exercise outputs, so no assert depends on the module's shared file.
- The REAL log is never touched by tests: the app handle wires `log_path: None` (`src-tauri/src/main.rs:2035–2040`, i.e. the default `~/.mnemo/laya/training/tool_choice.jsonl` home), and both test helpers construct their own handle with `Some(...)` — the only override path. Confirmed.

## Acceptance criteria re-confirmed (structural, committed state)

- **(a) Inert when off.** `steer_with_handle` (`tool_choice.rs:276–306`): absent handle → `FellBack(NoClassifier)` with zero work; false flag → `FellBack(Disabled)` at `:287` BEFORE the classifier slot is even read at `:295`. `is_live` (`:311–315`) gates the log identically. `choice()` → `None` → the allow-triple collapses to `(true, true, true)` (`search.rs:1352–1357`, `search_read.rs:225–230`), reducing every gate back to the pre-change `escape ||` form — output byte-identical by construction.
- **(b) Class routing.** `Symbol → (false, true, true)` (symbol arm + nudge only), `Memory → (true, false, false)` (memory arm only), `Text → (false, false, false)` (suppresses delegation AND the symbol nudge). Identical mapping in both tools.
- **(c) Threshold.** `TOOL_CHOICE_THRESHOLD = 0.80` (`:43`); `steer` falls back below it (`:234`) with `LowConfidence(confidence)`.
- **(d) Escape first.** In both tools the `escape` check on the bounded `DelegatedKey` set precedes the steer call, and `escape ⇒ steer_decision = None` — the classifier is never asked; the escape-hatch recording on both arms (fast-path return and glob-narrowed prepend) is pre-existing mechanics untouched by this delta.
- **(e) Row on every disposition.** `log_decision` runs inside the `spawn_blocking` closure on both tools AFTER `delegated` is computed and BEFORE any output branch (`search.rs:1416–1426`, `search_read.rs:274+`), so the fast-path return, the glob-narrowed prepend, and the plain-search fall-through all log exactly once. An escaped repeat logs with `decision = None`, `escaped = true` (covered by `an_escaped_repeat_with_no_decision_still_logs_the_correction`).

## Constitution checks (delta)

- **Docs:** README.md and PLAN.md delegation text updated in the commit; the L1 fix itself corrected the module doc comment. In sync.
- **Multi-platform:** log paths via the home-dir helper and `std::env::temp_dir()` — no Windows-only API, path, or shell syntax in the delta. Holds on macOS and Windows.
- **File-tools-first:** no shell-based mutation anywhere in the change.
- **Warning-free:** no `#[allow(...)]` in any read slice; parent's green `cargo test` under `#![deny(warnings)]` covers the rest.
- **Bookkeeping accuracy (one line, not a line review):** the plan file's review stamps (`bbff566`, `7b7691b`) match the actual commits and the round-1 report; the commit message accurately describes the landed change including both fixes.

No findings. The plan may proceed to finish.

Reviewed-state: 7b7691bcc199f05f41a9daf7d4b4dd1bb2654a66
