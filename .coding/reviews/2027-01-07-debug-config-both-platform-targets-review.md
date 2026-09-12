## Verdict: PASS

Plan add4bb15 ("debug.config.toml: name both platform copy targets", mac-readiness LOW 3) — the comment-only edit to debug.config.toml is correct, minimal, and fully resolves the finding. No findings.

### Scope reviewed (all uncommitted changes on wt/agenticcoding)

- `debug.config.toml` — 1 line replaced by 2 (the copy instruction now names both platforms). The change under review.
- `.coding/backlog.jsonl` — expected bookkeeping: backlog item 8c2d8ce2 (this very finding) flipped pending → in_flight with plan_id add4bb15. Not a code change.
- `.coding/plans/add4bb15.md` (untracked) — the plan file, a normal workflow artifact to be committed with the change.

### Verification against the seven requested checks

1. **Both platform paths accurate — verified against the implementation.** `global_config_dir()` (src/config/mod.rs:483-485) = `config_dir_named(".mnemo")`, which resolves via `directories::BaseDirs::home_dir().join(dir_name)` (src/config/mod.rs:465-468) with `$HOME` then `$USERPROFILE` fallbacks (:470-475). On Windows `BaseDirs::home_dir()` is `%USERPROFILE%`, so the comment's `%USERPROFILE%\.mnemo\config.toml` (debug.config.toml:4) is exact; on macOS it is `$HOME` (`~`), so `~/.mnemo/config.toml` (debug.config.toml:5) is exact. Both targets land where the app actually reads config.
2. **Windows target unchanged.** The diff replaces `#   Copy this file to  %USERPROFILE%\.mnemo\config.toml` with the same path plus a `  (Windows)` label — byte-identical path, only the label added.
3. **TOML remains valid.** Both new lines are `#` comments; no keys, sections, or values touched (single hunk; the rest of the 59-line file is byte-identical — confirmed by a full read). The file's own contract ("parses with `toml::from_str`", debug.config.toml:8-9) is unaffected.
4. **No drift.** `git diff HEAD --stat`: debug.config.toml 2 insertions / 1 deletion, exactly one hunk — one line replaced by two, as described. Surrounding lines 3 ("Global config lives in `~/.mnemo/`") and 6 ("(or merge the sections…)") unchanged.
5. **Multi-platform neutrality.** The fix *is* the neutrality: a macOS user following the instruction literally now copies to `~/.mnemo/config.toml`, the correct `global_config_dir()` resolution on macOS. No `cfg(windows)` additions, no Windows-only assumptions introduced.
6. **Documentation sync — clean.** Repo-wide literal searches for `%USERPROFILE%`, `Copy this file to`, and `debug.config.toml` show the copy instruction exists only in debug.config.toml itself plus `.coding/` bookkeeping (the mac-readiness report, backlog.jsonl, historical plan files). README.md and PLAN.md do not reference it — no doc updates required. (Historical plan files mentioning `%USERPROFILE%\.myharness` are pre-migration records, not user-facing docs. `.coding/grok.md:679` merely names the sample-config file in a table; no copy instruction.)
7. **Correctness / security / constitution.** Comment-only change — no runtime behavior, no security surface. No test parses debug.config.toml (no Rust/test references repo-wide), consistent with the reported green suites (root 2008+16, src-tauri 216+4, 0 failed). Line endings unchanged (clean diff hunks, no CRLF mixing). The two new lines are column-aligned (paths and platform labels each start at the same column), matching the file's existing indented-comment style.

### Notes (non-findings)

- The formatting choice (two aligned lines rather than the finding's one-line "(Windows) or … (macOS)" suggestion) satisfies the finding's fix direction and reads better under the file's existing comment style.
- Linux is unnamed, but the constitution's platform pair is Windows/macOS, and the `~/.mnemo/config.toml` line covers any unix-like home resolution anyway.
