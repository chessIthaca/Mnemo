## Verdict: PASS

Round-2 delta re-review of plan 8ff77e5d (base `13128adc`). Scope read: commit `3f286a2` (full diff, 811+/39− across 12 files — the round-1 change set plus the three fixes, now committed), the uncommitted remainder (`git diff HEAD`: one line — the `## Reviews` round-2 stamp in `.coding/plans/8ff77e5d.md`), and the current `src/project/scaffold.rs` in full (all 517 lines, implementation + tests). All three round-1 findings verified fixed; no regressions in the delta.

### LOW 1 — non-UTF-8 skip: FIXED
`ensure_managed_entries` (scaffold.rs:117–131) now does `std::fs::read` → `String::from_utf8`; the UTF-8 error returns `Ok(false)` with no write on that path, so the file stays byte-untouched. Error discrimination is correct: only the UTF-8 conversion is skipped; `NotFound` takes the create-fresh branch and every other io error (`PermissionDenied`, …) propagates via `return Err(e)` — no real error masked. No half-seeded state: `seed_git_files` runs `.gitignore` then `.gitattributes` unconditionally, and `Ok(false)` doesn't short-circuit, so the other file still seeds. New doc bullet present (lines 102–104). Test `a_non_utf8_git_file_is_skipped_untouched` (462–480) writes `b"# caf\xe9 …"` and asserts `Ok(false)` + unchanged byte image — red under the old `read_to_string(...)?` (the `.unwrap()` would panic on `InvalidData`). Genuinely pins the fix.

### LOW 2 — preserving write: FIXED
`write_preserving` (179–199) + `temp_sibling` (203–208), used on BOTH paths (fresh-create line 127, merge line 157). Verified the specific risks named in the dispatch:
- **Confinement:** the temp name is built from `path.file_name()` pushed onto `.seed-tmp` and reattached with `with_file_name` — always a sibling in the target's own directory; it cannot escape the project dir or touch an unrelated file. A no-file-name path returns `None` → plain write (safe degradation).
- **Fallback cannot lose more than before:** on temp-write or rename failure, `let _ = remove_file(&tmp)` then the direct `fs::write(path, content)` — whose error propagates exactly as the pre-fix code's did. The truncate-then-write crash window is gone from the success path.
- **No stale-temp confusion:** the success path consumes the temp via rename; the failure path best-effort removes it. Even a surviving temp (double failure) is inert — seeding only ever reads `.gitignore`, never the `.seed-tmp` sibling, and the next run overwrites it. Same-directory rename ⇒ atomic and same-volume on both Windows (MOVEFILE_REPLACE_EXISTING) and Unix — portable, no Windows-only API.
Test `the_merge_leaves_no_temp_sibling_behind` (501–516) asserts the dir is exactly `[".gitignore"]` after a merge + covered re-run — red if the rename path ever regresses into leaving litter. (One honest note, not a finding: the original defect — a crash between truncate and write — is not unit-observable, so this test pins the new mechanism's invariant rather than reproducing the old failure; that is the correct available proxy.)

### LOW 3 — `*.db` scope wording: FIXED
`GITATTRIBUTES_HEADER` (85–89) now reads "Every SQLite database file — the app's caches under .coding/ and any user-owned .db — is binary", and the `MANAGED_GITATTRIBUTES_ENTRIES` doc comment (62–68) states "deliberately REPO-WIDE rather than scoped to `.coding/`" with rationale — both match the actual entries (`*.db -text` / `-wal` / `-shm`, no path prefix). Searched all shipped surfaces (README.md:96, docs/CONFIGURATION.md:23, docs/FEATURES.md:34, `.coding/skills/*.toml`): no remaining `.coding/`-scoped claim about the db attributes anywhere; the docs describe the merge semantics accurately. (Old scoped phrasing survives only in immutable historical `.coding/plans|reviews` files — history, not shipped docs.)

### Added gap test
`an_empty_existing_git_file_gets_the_full_block` (483–498) drives the `out.is_empty()` branch (no leading blank line, header leads) — the zero-byte case round 1 listed as a minor gap.

### Constitution + process
- **Multi-platform:** std-only fs ops (`read`/`write`/`rename`/`remove_file`); no Windows-only API, path, or shell syntax in the delta. ✓
- **File-tools-first:** no shell-based mutation anywhere in the change. ✓
- **Doc comments:** every pub item (and the private helpers) documented; the fixed behaviors are documented at `ensure_managed_entries` and `write_preserving`, and README/CONFIGURATION/FEATURES match the shipped semantics. Docs sync ✓.
- **Warnings:** no `#[allow(...)]` in the file (read in full); reported green `cargo test` under `#![deny(warnings)]` at both crate roots is consistent with what I read. ✓
- **Bookkeeping accuracy (one line):** plan frame 8ff77e5d, backlog entry, and the round-1 review file in the commit match what shipped; the uncommitted plan-frame `## Reviews` line is the expected round-2 stamp.
- **Process remark (one line):** the non-scaffold hunks of `3f286a2` (SHIPPED_SKILLS/REPO_LOCAL_SKILLS + guard test, `src/project/mod.rs:189` wiring with error mapping, IPC doc, skill toml headers, docs) are the round-1-reviewed material carried into the commit unchanged; I confirmed the wiring call site and skill-test tail rather than re-line-reviewing them.

Reviewed-state: 3f286a2a7a1d7517f822b45d8c485f12efd7c91d
