# Review: Fix flaky agent_md mtime test

**Scope reviewed:** `git diff HEAD` — `src/project/agent_md.rs` (only modified file) + untracked `.coding/plans/e2f9a314-8b96-4922-b248-3b3b679d910f.md` (workflow bookkeeping, noted, no findings).

## Summary

**No findings.** The diff is clean: both hunks are inside `#[cfg(test)] mod tests` (module starts at line 153); production code (`ConstitutionSource::reload_if_changed`, lines 70–100; `load_with_mtimes`, `mtime_of`, `read_optional`) is byte-for-byte untouched, matching the plan's stated intent.

## Correctness — verified

1. **`source_rereads_after_mtime_change` (src/project/agent_md.rs:216–252).**
   - The explicit mtime bump (`SystemTime::now() + 60s`, line 242) cannot collide with the mtime recorded at `ConstitutionSource::new` (line 226): the first write's mtime is ≤ wall-clock at that moment, while the bumped value is strictly ~60s in the future. `reload_if_changed` uses `!=` on `Option<SystemTime>` (lines 73, 87), so any unequal future timestamp triggers the reload — no granularity or tick dependency remains.
   - Windows handle requirement is correct: `File::options().write(true).open(...)` (line 240) grants `FILE_WRITE_ATTRIBUTES`, which `SetFileTime` (underlying `File::set_times`) requires; the inline comment (lines 238–239) documents this accurately. On Unix, `futimens` on a write-open fd succeeds for the file owner. The open does not truncate (truncate requires an explicit `.truncate(true)`), so "GLOBAL v2" content is preserved for the line-249 assertion.
   - Ordering is safe: `std::fs::write` completes (and its handle closes) before the bump; the bump handle closes at block end (line 245) before `reload_if_changed` stats the file — no dangling-handle flush can overwrite the future mtime.
   - `unwrap()` on `set_times` is appropriate in a test (fail-loud), consistent with the file's existing test style.
   - `std::fs::FileTimes` is stable since Rust 1.75; toolchain 1.95 — no MSRV concern.

2. **`source_handles_missing_files` (src/project/agent_md.rs:270–288).**
   - Sleep removal is sound: the transition is `project_mtime: None → Some`, and `reload_if_changed` line 87 compares `self.project_mtime != Some(mtime)` — `None != Some(_)` is always true, independent of timestamp ticks. The comment (lines 283–285) explains this correctly.
   - Only the project path is written; global stays `None` → line 80's `is_some()` guard is false, so no spurious `changed` from the global side. Assertion `src.reload_if_changed()` (line 286) holds deterministically.

## Bugs

None. (Noted non-issue: a >60s backwards wall-clock step between the first write and the bump could theoretically place the bumped timestamp near the recorded one, but exact 100ns-granularity equality is not a realistic flake vector — the test is deterministic in practice, and the 20× loop + full-suite verification reported supports this.)

## Security

None. Test-only changes; no unsafe, no new dependencies, no network/process access, tempfiles confined to `tempfile::tempdir()` sandboxes.

## Constitution compliance

- No `#[allow(...)]` suppressions added; no new warnings possible (all-std, fully-qualified paths, no new imports — `FileTimes` needs no MSRV gate).
- No public API changed, so the "doc comments on public functions" rule is untriggered; the new inline comments are accurate and match the file's comment style.
- Diff shows consistent whitespace/style with the surrounding tests; no mixed line-ending evidence.
- Production `reload_if_changed` exact-mtime comparison intentionally preserved per plan scope (documented design at lines 66–69) — the same-tick-edit limitation remains a known, out-of-scope trade-off.
- Untracked `.coding/plans/e2f9a314-*.md` is workflow bookkeeping state — noted, not a finding.
- Verification (reported pre-done, re-verified by inspection of the logic): `cargo test agent_md` ×20 green; full `cargo test` 785 passed / 0 failed / warning-free under `#![deny(warnings)]`.

**Verdict: no findings. Diff is ready to commit.**
