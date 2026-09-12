# Review: Command-aware shell-output filtering (plan 976423a0)

**Branch:** feat/shell-output-filter (working tree, uncommitted)
**Reviewed:** full uncommitted diff (`git diff HEAD`) + the untracked new files
`src/tool/agent/shell_filter.rs` and `.coding/plans/976423a0-….md`.

## What the plan set out to do

Add a deterministic, regex-based, in-process output filter to the shell tool so
plan-step commands (`cargo build/check/clippy/test`, `npm test`, `npm run build`,
`git status`) collapse noise lines before they reach the LLM transcript, with hard
safety rules (error-marked lines never dropped, unknown commands pass through,
failures always visible), an optional `[shell_filter]` config section (default ON,
`[[shell_filter.override]]` entries), filtering applied only to the `output` field
(raw stdout/stderr untouched in `data`), and >80% measured noise reduction.

## Changed files

Tracked: `.coding/backlog.json`, `.coding/plans/stack.json` (bookkeeping),
`PLAN.md`, `README.md`, `debug.config.toml`, `src-tauri/src/ipc/settings.rs`,
`src-tauri/src/main.rs`, `src/agent/factory.rs`, `src/config/general.rs`,
`src/config/mod.rs`, `src/tool/agent/mod.rs`, `src/tool/agent/shell.rs`.
Untracked: `src/tool/agent/shell_filter.rs` (new, 1314 lines incl. tests),
`.coding/plans/976423a0-….md` (plan file).

---

## Findings

### M1 (medium) — `keep` override cannot veto built-in drops; docs overpromise

`src/config/general.rs:455-458` documents `ShellFilterOverride.keep` as
"Lines matching any of these regexes are always kept, even if a built-in handler
would have dropped them." But the pipeline in `src/tool/agent/shell_filter.rs:269-283`
runs the built-in handler **first** (lines 269-276) and applies the override
(`apply_override`) only to the surviving lines (281-283). A `keep` pattern only
vetoes the same override's own `drop` patterns; it can never resurrect a line the
built-in handler already removed.

Concretely, the shipped example in `debug.config.toml:57`
(`keep = ["Compiling my-crate"]` under `command = "^cargo"`) is silently
ineffective: `cargo build`/`cargo test` drop `Compiling …` lines at
`shell_filter.rs:372/410` (via `cargo_noise_prefix`) before the override runs.
The module header pipeline description (`shell_filter.rs:14-21`) is accurate, so
only the two user-facing texts conflict with the behavior. No safety impact —
`keep` can never hide a failure — but a documented user feature silently no-ops
for its canonical use case.

Fix: either (a) implement `keep` as a pre-pass veto before the built-in drop
(each handler checks `ov.keep` before dropping), or (b) reword the field doc and
the debug.config.toml comment to "keep wins over the same override's drop
patterns" and pick an example that actually works. Given the safety posture
(whitelist-drop, err toward keep), option (a) matches the original intent better;
option (b) is acceptable if (a) is out of scope, but then the docs MUST be fixed.

### L1 (low) — `npm ERR!` headers and node:test's `✖` glyph are not error-marked

`is_error_line` (`shell_filter.rs:102-111`) covers `error`/`failed`/`fail`/
`panicked` (case-insensitive) and the glyphs `✗` (U+2717) / `×` (U+00D7).
Common real failure lines outside that set:

- `npm ERR! code ELIFECYCLE`, `npm ERR! Exit status 1` — "ERR!" contains no
  `error`/`fail` substring.
- node:test spec-reporter failures print `✖` (U+2716), not `✗`/`×`.

These survive the built-in handlers only via the whitelist-drop fallback
(unclassified → keep), which is fine for default configs. But the README's
"errors are never hidden" guarantee does not hold under a user override with a
broad `drop` pattern (`apply_override` vetoes only `is_error_line` + `keep`,
`shell_filter.rs:298-311`), and identical `npm ERR!` lines are also eligible for
dedup collapse. Suggest adding `lower.contains("err!")` and `line.contains('✖')`
to the marker set plus regression-test lines (a substring like "err!" is safe:
"terrible"/"merry" contain "err" but not "err!").

### L2 (low) — `filter_cargo_test` capture mode never exits on real cargo output

The capture-exit condition in the capture branch (`t.starts_with("test result:")`
→ `capture = false`, `shell_filter.rs:432-434`) is unreachable for genuine cargo
output: every real summary line is `test result: ok. N passed; 0 failed; …` or
`… FAILED. …`, so it always contains the marker word "failed" and is caught by
the `is_error_line` branch first (`shell_filter.rs:413-420`), which does not reset
`capture`. After the first failing test binary, `capture` stays true for the rest
of the stream, so later binaries' unclassified lines ("Running unittests …",
"running N tests" headers, blank lines) are kept.

This errs in the safe direction (over-keeping, never failure-hiding — I traced
`test … ok`, `Compiling`, `test result:` and `---- ` lines and they are all still
handled correctly inside capture mode), but it makes the intended block-scoping
dead code and slightly weakens reduction exactly when a run fails. Optional
cleanup: reset `capture` on any `test result:` line in the error branch too.

### L3 (low) — vite/webpack asset-row regexes are not end-anchored

`is_asset_table_line` (`shell_filter.rs:523-535`) matches a prefix only
(`…\s*│` / `…\s` — no `$`). Because `filter_npm_build` checks the structural
asset pattern before the error branch (`shell_filter.rs:547` vs `550`), a
hypothetical line that starts asset-row-shaped but carries failure text
(e.g. `dist/x.js 1.2 kB │ ERROR: hash mismatch`) would be dropped without the
error-marker veto. Realistic vite/webpack builds print the asset table only on
success, so this is theoretical — but anchoring both patterns with a trailing
`\s*$` is cheap defense-in-depth and makes the "truly impossible for a FAILING
line to match" claim airtight.

### I1 (info) — git_status drops conflict-resolution hints outside sections

The `(use "git` drop (`shell_filter.rs:618-619`) applies regardless of section,
so during a rebase/cherry-pick conflict the guidance lines
`(use "git rebase --skip" to skip this patch)` and
`(use "git checkout -- <file>..." to discard changes in working directory)` are
removed even though they appear outside any file-list section. No failure is
hidden — the conflict state itself survives ("You are currently rebasing…",
"Unmerged paths:" and its `both modified:` entries are kept as unclassified
lines) — but actionable escape-hatch hints are lost. Acceptable if intentional;
worth a one-line comment if not.

---

## Correctness verification (no findings)

- **`filter_cargo_test` capture traces:** failure blocks (`test x … FAILED` →
  `failures:` → `---- x stdout ----` → detail → `test result: FAILED.`) are fully
  kept, including unmarked detail lines (`left:`/`right:`), because capture starts
  on the FAILED/failures lines and the `---- ` header independently starts
  capture for doc-test failures that lack a preceding `test … FAILED` line.
  No off-by-one or premature-exit that cuts a failure block short (L2 above is
  the opposite direction: over-keeping).
- **`filter_npm_test` captures:** vitest (`✗` → `→ detail` → `Test Files … failed`
  kept), jest (`FAIL` → `●` block → failing summaries kept; `Snapshots:`/`Time:`
  terminators correctly end capture and are dropped), and TAP (`ok N` dropped,
  `not ok N` + YAML detail kept via whitelist-drop) all trace correctly, including
  capture exit on the next `✓`/summary terminator.
- **Structural-pass exception:** for cargo (`test … … ok`), TAP (`ok <digit>`),
  vitest (`✓`/`✔`/`√`) and jest (`PASS `), a *failing* line cannot take those
  shapes — failures print `FAILED`/`✗`/`×`/`FAIL`/`not ok`, which are all caught
  by error branches. `Compiling thiserror`/`Checking` lines are compile progress
  (failures print `error[E…]` diagnostics, which don't match the noise prefixes).
  The only soft spot in the structural exceptions is L3 (asset rows), noted above.
- **`classify()`:** any `|` → `Unknown` (pipe-reshaped output is never filtered by
  a handler); `&&`/`;` chains resolve to the last segment (verified
  `rsplit(['&',';'])` returns the trailing segment). Since every handler is
  whitelist-drop and error-marked lines are always kept, misclassification can
  only over-keep — never hide a failure. `npm run build:prod`-style prefixes
  over-match into `NpmBuild`, but whitelist-drop keeps that safe.
- **git_status flush logic:** files cannot be lost — entries are collected
  whenever a section is active, and `flush` runs on blank lines, new headers,
  branch lines, and EOF. Porcelain (`--short`) has no headers and passes through
  (tested). Trace of `On branch`/`Your branch`/`nothing to commit` all correct.
- **dedup_lines:** error-marked lines are never collapsed (explicit check +
  regression test); collapse-note lines cannot cascade-collapse each other
  (each note push is always followed by the differing line).
- **Disabled path:** byte-identical passthrough, covered by
  `config_disabled_passthrough_is_byte_identical` and the shell.rs integration
  test.
- **shell.rs wiring:** `data.stdout`/`data.stderr` hold the *raw* unfiltered
  originals (`shell.rs:241-245`); the `[exit code: N]` line is appended to the
  filtered combined text before `cap_tool_output` (`shell.rs:225-232`), so it can
  never be filtered away; the `RwLock` guard is dropped at the end of the `let`
  statement after `.clone()` (`shell.rs:210-214`) — no lock held across the
  filter and no `.await` while the guard is alive.
- **Unicode/char safety:** input is valid UTF-8 (`String::from_utf8_lossy`);
  `strip_ansi`/normalize/dedup operate via `replace_all`/`split`/`trim`, all
  boundary-safe. The TAP slice `t[3..]` (`shell_filter.rs:488`) is guarded by
  `t.len() > 3` and the matched prefix `"ok "` is ASCII, so index 3 is always a
  char boundary. No panics found.
- **Multi-platform:** the lib module is pure string/regex code — no Windows-only
  APIs, paths, or shell syntax; platform differences appear only in test command
  selection via `cfg!(target_os = "windows")`. The pre-existing
  `#[cfg(windows)] CREATE_NO_WINDOW` block in shell.rs is untouched.

## Security (no findings)

- The Rust `regex` crate is guaranteed linear-time (no backreferences/lookaround),
  so neither the shipped patterns nor user-configured override regexes can cause
  catastrophic backtracking (ReDoS), even when matched per line against
  untrusted command output.
- Shipped patterns use bounded/disjoint character classes
  (`\x1b\[[0-9;?]*[ -/]*[@-~]`, `[\s#=█▓▒░\-\[\]]*…`, `^[\s./\\|\-…]+$`, asset
  patterns) — no unbounded `^.*$`-style patterns ship; `^.*$` appears only in a
  unit test (`shell_filter.rs:1067`).
- The `command` regex is matched against the user's own command string (not
  attacker-controlled), and `is_error_line` runs before any override drop — the
  hard safety rule cannot be configured away.

## Constitution compliance

1. **Doc comments:** every new `pub`/`pub(crate)` item is documented —
   `CompiledOverride` (incl. all fields), `HandlerKind`, `strip_ansi`,
   `is_error_line`, `is_warning_line`, `is_progress_line`, `classify`,
   `compile_overrides`, `filter_shell_output`, `ShellFilterConfig` (incl. fields),
   `ShellFilterOverride` (incl. fields), `ShellTool::with_filter_config`,
   `AgentLoopFactory::with_shell_filter_config` / `set_shell_filter_config` (incl.
   the new field). Private helpers are also commented. ✔
2. **Regression tests:** one test per handler (cargo build/check/clippy, cargo
   test failure + all-pass, vitest, jest, node:test TAP, vite build, tsc/vite
   errors, git status collapse + clean/short), safety tests (error-line survival
   under a drop-all override across all six handlers, failing build/test still
   show failure, repeated error lines never deduped), pipeline tests (ANSI,
   progress/spinner), config tests (defaults, parse, round-trip, override
   semantics, first-match-wins, invalid-regex degradation, disabled
   byte-identical), a >80% reduction target test, plus 5 config-schema tests,
   2 shell.rs integration tests, and the factory live-rewire test. All would
   fail if the corresponding behavior regressed — notably the factory test's
   second phase fails if `with_filter_config` wiring is removed from
   `register_agent_tools` (the already-built tool would keep its private default
   handle and still dedup after `set_shell_filter_config(false)`). ✔
3. **Warning-free:** no `#[allow(...)]` added; no unused imports; the only
   `#[expect(dead_code)]` (shell.rs `purpose`) is pre-existing and untouched. ✔
4. **Documentation sync:** README feature bullet (README.md:32) is accurate
   (errors never hidden / unknown commands pass through / raw output in the
   Output tab); PLAN.md technical-decision row (PLAN.md:126) matches the
   implementation; `debug.config.toml` example is correctly placed at config.toml
   top level (`GeneralConfig` maps `[shell_filter]` / `[[shell_filter.override]]`
   to the root — verified against the serde structure); module doc comment and
   the shell tool schema description are updated. README.md:91's config summary
   does not contradict the new section. No stale docs found — **except the
   M1 doc/behavior mismatch, which must be fixed.** ✔ (with M1 caveat)
5. **Multi-platform neutrality:** no Windows-only code in lib; test fixtures use
   platform-neutral string data (the `C:\AgenticCoder` strings are inert fixture
   text, not paths). ✔

## Integration checks (no findings)

- `settings.rs:1259-1264`: the `set_shell_filter_config` push mirrors the
  `set_core_operations` pattern and sits inside `if let Some(f) = …` — safe when
  the factory is `None`. `persist_and_reload` re-parses the saved config, so a
  hand-edited `[shell_filter]` section is picked up on the next save, and the
  config-schema round-trip tests prove the `override` rename survives
  save→reload.
- Factory: default handle is `ShellFilterConfig::default()` (enabled, no
  overrides); `with_shell_filter_config` swaps the handle before any registry
  build; `set_shell_filter_config` writes through the shared `Arc<RwLock>`.
- `main.rs` wiring seeds the handle from the loaded config and chains
  `.with_shell_filter_config(shell_filter)` — correct.

## Summary

The filter design is sound: whitelist-drop handlers + error-marker veto +
first-match override semantics deliver the reduction target without hiding
failures, the raw data field stays untouched, and the test suite is unusually
thorough. One medium finding (M1, documented `keep` semantics that silently
no-op against built-in drops — must be fixed or re-documented), three low
findings (error-marker set misses `npm ERR!`/`✖`; unreachable cargo-test capture
exit — safe direction; unanchored asset regexes), and one informational note.
