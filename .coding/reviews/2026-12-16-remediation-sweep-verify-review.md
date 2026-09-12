## Verdict: PASS

**Re-review verification of the Mnemo remediation sweep (commit `d4a0c0d` on `wt/agenticcoder`).** All 7 LOW-severity findings from the first review (`.coding/reviews/2026-12-16-remediation-sweep-review.md`) are resolved. Each fix was verified against the current HEAD state of the affected files. No new issues, regressions, or constitution violations were introduced. The one logic-adjacent change (finding #7) was traced end-to-end and confirmed behavior-preserving.

## Scope & method

- **Branch / commit:** `wt/agenticcoder` @ `d4a0c0d` ("fix: address all 7 review findings").
- **Baseline:** `main` (the full remediation sweep spans commits `b399b48`→`d4a0c0d`).
- **Working tree:** clean — no uncommitted source changes (only an untracked knowledge file for the finding-#6 process lesson). All fixes are committed.
- **Method:** read the current HEAD state of every affected file and confirmed each fix is present and correct. For the one logic-adjacent change (finding #7) I traced the data flow through the brain function end-to-end. Searched all changed files for `#[allow(...)]` (none found). The functional sweep items (codegraph mtime fast-path, `AgentLoopConfig` refactor, `stable_head` caching, `bound_repetition_buffer`, M1a/M1b/M3 extractions) are untouched by the cosmetic fix commit and were already verified correct in the first review; I re-confirmed the three the fix commit borders on (see below).

## Per-finding verification

### Finding 1 — Duplicate doc-comment lines in events.rs ✅ RESOLVED
Two sites had a duplicated `///` line. Both are now single lines:
- `DELTA_FLUSH_INTERVAL` doc — `events.rs:96` reads `/// How long a delta batch may sit before it is flushed even if no structural` → `:97` `/// event arrives...`. No duplicate.
- `emit_child_finished` doc — `events.rs:746` reads `/// Emit a \`ChildFinished\` event to the frontend, tagged with the child's id so` → `:747` `/// the sidebar can mark...`. No duplicate.

### Finding 2 — Redundant doc paragraphs in agent_md.rs ✅ RESOLVED
The `reload_if_changed` doc (`agent_md.rs:70-83`) is now a single coherent block: one "Returns `true` if anything changed. Best-effort: FS errors keep the previous cache…" sentence, followed by the `# Why not spawn_blocking? (review L3)` rationale. The duplicated "Returns `true`…" and "FS errors keep the previous cache" sentiments that previously appeared in both the pre-existing paragraph and the L3 block are gone. The merge achieves the finding's goal (no duplication) cleanly.

### Finding 3 — Stale "unix seconds" in mtime_of doc ✅ RESOLVED
`codegraph/mod.rs:413` now reads `/// A file's mtime as unix millis, or 0 when unavailable (diagnostics only —`. The implementation at `:426` uses `.as_millis() as i64`, so the doc now matches the M2 millisecond resolution. Consistent.

### Finding 4 — Incomplete SettingsSaveDto test-import migration ✅ RESOLVED
Both remaining test modules in `settings.rs` now import from the brain path:
- `:1292` `use mnemo::config::settings_dto::SettingsSaveDto;`
- `:1306` `use mnemo::config::settings_dto::SettingsSaveDto;`

No `use crate::ipc::settings::SettingsSaveDto` remains in the test modules. The migration is complete.

### Finding 5 — memory/tests.rs over-indentation ✅ RESOLVED
The extracted `src/memory/tests.rs` (M3) is now at column 0: `:1` `use super::*;`, `:5` `fn make_store() {`. The 4-space over-indent left over from the `mod tests {` extraction is gone across the whole file (1864 lines). No broken/partial dedent observed.

### Finding 6 — Process nit (L2 missed spawn.rs) ✅ RESOLVED (no code fix; process lesson recorded)
No code change was required — HEAD was already correct. Confirmed `spawn.rs:631` calls `AgentLoop::new(mnemo::agent::AgentLoopConfig { … }, mnemo::project::Constitution::default())`, i.e. the **new** 2-arg `AgentLoopConfig`-struct form, not the old 9-arg positional signature. The process lesson is recorded in memory as `HOW: cross-crate API changes need full-workspace cargo test` (id `4ba26f32`), with the knowledge file `.coding/knowledge/how/2026-08-31-cross-crate-api-changes-need-full-workspace-carg.md` present on disk.

### Finding 7 — Redundant parse_safety_mode in save_settings ✅ RESOLVED (and verified correct)
The adapter (`settings.rs:749-756`) no longer re-parses `patch.safety`; it reads `new_config.general.general.safety` directly under `if patch.safety.is_some()`. **Correctness traced end-to-end:** the brain function `validate_and_apply_settings_patch` (`settings_dto.rs:243`) parses `patch.safety` via the same `parse_safety_mode` (`:315-318`) and applies the result to `general.general.safety` (`:428-430`); that `general` becomes `new_config.general` (`:573-574`). So the field the adapter reads back is exactly the value the brain parsed and applied. Because the brain's `?` short-circuits on a parse error *before* returning `Ok`, the adapter only reaches the read-back branch when the parse succeeded — making the new path strictly equivalent to the old re-parse, with the redundant work removed. Behavior-preserving.

## Cross-cutting checks

- **`#[allow(...)]` / `#![deny(warnings)]`:** searched `events.rs`, `settings.rs`, `memory/tests.rs` for `#[allow` — none. The fix commit is cosmetic/doc/import-only, so it cannot have introduced warnings; the prior green `cargo test` (1737 lib + 4 tauri-app, 0 warnings, per commit message) remains valid.
- **Functional sweep items re-confirmed where the fix commit borders them:**
  - *AgentLoopConfig refactor (L2):* `spawn.rs:631` uses the new struct form — consistent with `factory.rs`/`runtime`/`agent.rs`.
  - *M1b extraction:* `validate_and_apply_settings_patch` is correct and is the single source of truth the adapter now relies on (finding #7).
  - *codegraph mtime fast-path (M2):* doc/impl agree on millisecond resolution (finding #3).
  - *M1a (turn-resolution) / M3 (memory test extraction) / stable_head (L4) / bound_repetition_buffer (L5):* untouched by `d4a0c0d`; remain as verified in the first review.
- **Multi-platform neutrality:** no Windows-only APIs/paths/shell introduced; the changes are doc text and import-path edits.
- **Documentation sync:** the doc fixes *are* the documentation sync — all stale/duplicated doc text is now correct.

## Conclusion

All 7 findings are resolved with no new issues introduced. The remediation sweep is complete and functionally sound. **PASS.**
