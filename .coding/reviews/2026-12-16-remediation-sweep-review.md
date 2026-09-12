## Verdict: FINDINGS (0 high, 7 low)

**Summary:** The remediation sweep is functionally sound. All 10 commits achieve their stated goals without introducing correctness regressions, security issues, or constitution violations. The `AgentLoopConfig` migration (L2), `stable_head` caching (L4), `bound_repetition_buffer` (L5), and codegraph mtime fast-path (M2) are all correct; the M1a/M1b/M3 extractions are behavior-preserving. The 7 findings are all LOW-severity cosmetic, documentation-sync, or process-discipline nits — none block merge. Three items are explicitly accepted, in-code-documented trade-offs.

---

### Per-commit correctness assessment

- **L1 (2961f1b)** — Clean deletion of dead `MockGit::set_sha`/`calls` + their `#[allow(dead_code)]`. Fully complies the no-`#[allow]` rule. ✓
- **L5 (4e35d3b)** — `bound_repetition_buffer` (openai.rs:1568) is correct: char-boundary safe (`is_char_boundary` loop + `needed.min(text.len())` underflow guard at :1572), called after `detect_repetition` (openai.rs:1136) — though ordering is moot since both are suffix-based and bounding preserves the last `needed` bytes. 5 meaningful regression tests (incl. multibyte-no-panic + semantics-preservation). ✓
- **M2 (d7a0329)** — Fast-path (mod.rs:287) skips only when `stored_mtime == Some(disk_mtime) && has_content && stored_hash.is_some()`; falls through to authoritative hash comparison when mtime advances (mod.rs:296-303). Millis aligned: `mtime_of` uses `as_millis() as i64` (mod.rs:426), search staleness check uses `as_millis() as i64` (search.rs:742) with an explicit "unix millis — the indexer's `mtime_of`" comment (search.rs:727). Same-tick collision accepted/documented. ✓ (one stale doc — finding #3)
- **L2 (e9cd242)** — `from_config` body is byte-identical to the old `new` body; factory.rs field mapping (provider/tools/workflow/sandbox/safety_mode/context_manager/memory/vision) correct; ~96 call sites migrated. But missed `src-tauri/spawn.rs` (finding #6). ✓
- **L4 (b399b48)** — `stable_head` (loop_impl.rs:273) invalidation is correct: rebuilds on `changed || cache.is_none()` where `changed` is the bool from `reload_if_changed()` (verified returns `bool` at agent_md.rs:88, safety_rules.rs:228); the `else`-branch `.expect()` is safe (only reached when `cache.is_none()` is false); lock order source→cache with no reverse acquisition (no deadlock). First call always rebuilds (cache starts `None`). ✓
- **L3 (07b6cb1)** — Well-reasoned `spawn_blocking` rejection (microsecond stat vs task-spawn overhead). Duplicated doc sentences (finding #2). ✓
- **L6 (0757176)** — Well-reasoned feature-gate deferral (deep integration, too invasive for this sweep). ✓
- **M1a (ed23a6c)** — `turn_resolve.rs` (430 lines) is correct: pure brain types, 13 tests moved alongside, `console.rs` + `events.rs` delegate correctly. Duplicated doc lines in events.rs (finding #1). ✓
- **M1b (653c926)** — `validate_and_apply_settings_patch` (settings_dto.rs) is called while holding the config lock (same atomicity as original); DTO types + validation moved cleanly. Incomplete import migration (finding #4) + redundant `parse_safety_mode` (finding #7). ✓
- **M3 (b19c175)** — Correct test-module move: `#[cfg(test)] mod tests;` → `src/memory/tests.rs`, `use super::*` still resolves to `memory/mod.rs`. Over-indentation (finding #5). ✓

---

### Findings (LOW)

**1. Duplicated doc-comment lines in events.rs (M1a extraction artifact)**
`src-tauri/src/ipc/events.rs:96-97` — the line `/// How long a delta batch may sit before it is flushed even if no structural` appears on **two consecutive lines**. `src-tauri/src/ipc/events.rs:747-748` — the line `/// Emit a \`ChildFinished\` event to the frontend, tagged with the child's id so` likewise appears on **two consecutive lines**. Both were introduced when the two extracted functions were deleted: the diff added a `+` copy of the next item's first doc line ahead of the unchanged context line. Cosmetic (valid Rust, no compile impact) but sloppy. Fix: delete the duplicate line at each site.

**2. Duplicated doc sentences in `reload_if_changed` (L3)**
`src/project/agent_md.rs:70-77` — the L3 rationale block was appended *without merging* the pre-existing doc text. "Returns `true` if anything changed" appears at both :71 and :76; the "FS errors keep the previous cache" sentiment appears at :72-73 and :77. Fix: delete the redundant first paragraph (:70-73) and keep the expanded L3 version (:74-87).

**3. Stale "unix seconds" doc in `mtime_of` (M2)**
`src/codegraph/mod.rs:413` — the doc comment reads `/// A file's mtime as unix seconds, or 0 when unavailable` but the implementation now returns **millis** (`as_millis() as i64` at :426). The M2 commit changed the resolution but left the doc stale. Fix: `seconds` → `millis`.

**4. Incomplete `SettingsSaveDto` test-import migration (M1b)**
`src-tauri/src/ipc/settings.rs:1290` and `:1304` still use `use crate::ipc::settings::SettingsSaveDto;` (the old adapter path) instead of `mnemo::config::settings_dto::SettingsSaveDto` (the new brain path, used by the other migrated imports for `ModelsConfigDto`/`ModelRefDto`). These compile only because settings.rs:15 has a private `use ...::{..., SettingsSaveDto}` whose binding is visible to the descendant test module — a fragile coupling that breaks if that import is ever removed. Fix: update both to `mnemo::config::settings_dto::SettingsSaveDto`.

**5. `memory/tests.rs` retains 4-space over-indentation (M3)**
`src/memory/tests.rs` — every line is indented by 4 spaces, carried over from having been the body of the inline `mod tests { ... }` block. Valid Rust (whitespace-insensitive) but inconsistent with the rest of the codebase and harms readability of a 1866-line file. Fix: dedent the file one level.

**6. L2 missed `src-tauri/spawn.rs` migration; test-discipline gap (process nit)**
The L2 commit (e9cd242) migrated factory.rs/tests.rs/runtime/agent.rs but **missed** `src-tauri/src/ipc/spawn.rs:631`, which still used the old 9-arg `AgentLoop::new` signature. L2 only ran `cargo test --lib` (lib crate only), so the broken src-tauri app build went undetected across 3 intermediate commits (L4, L3, L6) until M1a (ed23a6c) migrated it. The **final HEAD state is correct** (M1a fixed spawn.rs and ran the tauri app tests). Process lesson: commits touching `AgentLoop`'s public surface must run the full workspace `cargo test`, not `--lib`, since src-tauri depends on the lib's constructors.

**7. Redundant `parse_safety_mode` call in the settings adapter (M1b)**
`src-tauri/src/ipc/settings.rs` — after `validate_and_apply_settings_patch` (which already parses, validates, and applies the safety mode internally), the adapter re-parses `patch.safety` a second time via `mnemo::config::patch::parse_safety_mode` to derive the typed value for the runtime sync. Correct (the brain's `Err` short-circuits first, so the re-parse never sees invalid input) but redundant. Fix (optional): have the brain return the parsed `SafetyMode` alongside the config to avoid the double parse.

---

### Accepted trade-offs (documented in-code, NOT findings)

- **M2 same-tick mtime collision** — two writes within the same millisecond share an mtime and the fast-path would skip a genuinely changed file. Documented at mod.rs:420-425 as "practically impossible — saves are human-paced and each index pass takes >1 ms"; self-corrects on the next pass when mtime advances. Accepted.
- **L3 blocking-stat on the async runtime** — `std::fs::metadata` issued on the async runtime rather than `spawn_blocking`. Documented at agent_md.rs:79-87: stat is microseconds (OS-cached inode), task-spawn overhead would exceed it, and each turn already does a seconds-long LLM call. Accepted.
- **L6 feature-gate deferral** — fastembed/ONNX, chromiumoxide, tree-sitter not cfg-gated. Documented in `Cargo.toml`: each is a core subsystem deeply integrated into the tool registry/IPC, not an optional plugin; gating would require invasive conditional wiring. Accepted as too risky for this sweep.

---

### Constitution compliance

- **Doc comments on public functions:** ✓ All new public items documented — `file_mtime` (store.rs:142-147), `stable_head` (loop_impl.rs), `validate_and_apply_settings_patch` (settings_dto.rs), `TurnResolveLatch`/`ResolveAction`/`workflow_transition_counts`/suggestion helpers (turn_resolve.rs), `bound_repetition_buffer` (private but documented).
- **No `#[allow(...)]` added:** ✓ L1 *removed* `#[allow(dead_code)]`; L2 *removed* both `#[allow(clippy::too_many_arguments)]`. No new `#[allow]` in any commit.
- **`#![deny(warnings)]`:** ✓ Green per commit test runs (lib: 1723→1737 passed; tauri app: 4 passed). Under deny(warnings), green = zero warnings.
- **Multi-platform neutrality:** ✓ All changes use cross-platform APIs (`std::fs::metadata`, `std::time`, `Mutex`, string ops). No Windows-only APIs in library or app code. The sanctioned WebView2/game exception is untouched.

### Test quality

- **L5:** 5 regression tests — short-text-unchanged, truncation-to-tail, **detection-semantics-preserved** (asserts `detect_repetition` agrees on bounded vs unbounded), bounded-across-many-appends, **multibyte-without-panic** (needed=601 mid-char exercises the `is_char_boundary` fix). Meaningful — the multibyte test panics without the char-boundary loop. ✓
- **M2:** 2 regression tests — fast-path skips unchanged files, fast-path still detects content changes (mtime advance → re-hash). ✓
- **L4:** 1 regression test — cache rebuilds when constitution reloads. ✓
- **M1a/M1b/M3:** tests moved alongside the extracted code (13 for turn_resolve; settings_dto_tests retained; memory tests intact). ✓

### Documentation sync

- `README.md` / `endpoints.toml`: no user-facing behavior changed — no update needed. ✓
- `PLAN.md`: mentions the IPC adapter (`src-tauri/src/ipc/`) at line 27 but does not call out the new brain modules (`src/runtime/turn_resolve.rs`, `src/config/settings_dto.rs`) or the brain/IPC-separation principle that M1a/M1b establish. Minor — PLAN.md is high-level; a one-line note on the extraction would aid future readers but is not required.
- Module doc comments: all new modules have `//!` headers. ✓ (except the stale `mtime_of` line — finding #3, and the duplicated lines — findings #1, #2).
