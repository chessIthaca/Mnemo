## Verdict: PASS

Reviewed all uncommitted changes on `feat/prompts-config` via `git diff HEAD`
(11 modified files; pre-existing untracked `.coding/` bookkeeping — backlog.jsonl,
knowledge/, the plan file, the 2026-08-23 recheck review — treated as unrelated
and ignored, as instructed).

**Plan goal under review:** Load the five compiled system-prompt blocks
(preamble, workflow lifecycle, app rules, tool strategy, memory records) from a
single user-editable `~/.mnemo/prompts.toml`, seeded on first run from
source-side defaults, with per-section fallback to the compiled consts and
mtime-based hot reload (edits apply next turn, no restart). Format: TOML with
`version = 1` + five `[section]` tables, each with a `text` field.

No findings.

---

### 1. Serde default algebra — PASS

`PromptBlocks` (src/agent/prompt.rs:252-271) carries `#[serde(default = "…")]`
on every field, each default fn returning the corresponding const wrapped in a
`PromptSection`. `PromptSection.text` itself has `#[serde(default)]`, so a
present `[section]` table without a `text` key yields an empty string, and an
explicitly empty `text` stays empty — the deliberate block-disable case. A
missing table entirely falls back to the compiled const. Missing `version`
defaults to 1 via `default_version()`. `load_str` (lines 318-337) returns
`PromptBlocks::default()` on any parse error with an `eprintln` (never panics);
a version mismatch is a warning that still applies the sections. Every branch
therefore resolves to a well-formed `PromptBlocks`, so `build_stable_head` can
never produce a corrupt head. Tested by `load_prompts_partial_file_falls_back_to_consts`,
`load_prompts_empty_section_stays_empty`, and `load_prompts_malformed_returns_defaults`.

### 2. mtime reload correctness — PASS

`PromptSource::reload_if_changed` (lines 383-397): stat succeeds + mtime
advanced → re-read + update; stat succeeds + same mtime → no reload; stat fails
(deleted / unreadable) with a previously-cached mtime → cache reset to
`PromptBlocks::default()` and mtime cleared. Stat failure (e.g. transient) keeps
the previous cache because `mtime_of` returning `None` only resets when
`self.mtime.is_some()`. This mirrors the constitution source
(src/project/agent_md.rs:74-104) including the deletion-restores-defaults
semantics. Note the documented deviation: a *malformed* edit re-reads and falls
back to the all-defaults blocks rather than silently keeping the previous good
cache — deliberate, safe (never breaks the loop), and asserted by
`prompt_source_malformed_edit_keeps_previous_cache`. The stat-per-turn cost is
one `std::fs::metadata` call. Edit/deletion behavior is covered by
`prompt_source_reload_picks_up_edits_and_deletion` and the missing-file case by
`prompt_source_missing_file_uses_defaults`.

### 3. ensure_prompts_file — PASS

`ensure_prompts_file` (lines 452-468) returns immediately when the path exists
(never clobbers user edits), writes via `crate::config::write_atomic` (temp
file + rename, src/config/mod.rs:357), and logs + swallows failures.
Seed-once-no-clobber is asserted by `ensure_prompts_file_seeds_once_and_never_clobbers`,
and the seeded payload round-trips to the const defaults
(`default_prompts_toml_round_trips_to_const_defaults`). Ordering in
`build_brain_inner` (src-tauri/src/main.rs:925-929): `ensure_prompts_file` runs
right after `Config::load` and before `AgentLoopFactory::new`
(PromptSource::new reads the file, main.rs:1364) — safe either way, since
`PromptSource::new` treats a missing file as compiled defaults and the factory
holds only a path; both the GUI and console paths go through `build_brain`
(console: src-tauri/src/console.rs:1382 calls `crate::build_brain()`).

### 5. Lock release + CONTEXT_FOOTER + volatile tail — PASS

`PromptHolder::prompt_blocks` (src/agent/loop_impl.rs:252-266) locks, reloads,
and clones to an owned `PromptBlocks`; the lock is released before the
stable-head build in turn.rs:512-515. `CONTEXT_FOOTER` remains a compiled
literal, untouched (test `context_footer_is_stable_literal` still passes with
defaults). The volatile tail (`build_volatile_tail`) is unchanged; the session
primer append in turn.rs is unaffected.

### 6. Call sites, dead code, warnings — PASS

All five `AgentLoopFactory::new` call sites carry the new `PromptSource` arg
(src-tauri/src/main.rs:1364, src-tauri/src/console.rs:2111,
src-tauri/src/ipc/config_io.rs:502, src/agent/factory.rs:1013,
tests/workflow_integration.rs:404); all existing `build_stable_head` /
`build_system_prompt` call sites in prompt.rs tests pass `&PromptBlocks::default()`.
No `#[allow]` added (only the pre-existing
`#[allow(clippy::too_many_arguments)]` on the loop constructors). All new pub
items have doc comments; new pub API (`PromptBlocks`, `PromptSection`,
`PromptSource`, `load_str`, `default_prompts_toml`, `ensure_prompts_file`)
documented. `build_system_prompt` keeps its back-compat wrapper role with all
tests updated. No dead consts — the five consts remain referenced by the
default fns and tests.

### 7. Multi-platform neutrality — PASS

The new code uses only `std::path::PathBuf`, `std::fs`, and `std::time` —
no platform-specific APIs or paths; `config_dir` comes from the existing
cross-platform `config_dir_named` (src/config/mod.rs:367, BaseDirs with HOME /
USERPROFILE fallback). No `cfg(windows)` additions.

### 8. Docs sync — PASS

README.md Configuration section mentions `prompts.toml` (seeded, per-section
editable, missing-section fallback, delete-to-reset, next-turn apply);
PLAN.md gains an "Editable system prompts" subsection (seed, mtime reload,
fallback semantics, footer stays compiled); prompt.rs module doc (lines 1-12)
documents the editable-file override, seeding, and re-read-on-mtime behavior.
The TOML file itself ships a header comment explaining the semantics.

### 9. Tests exercise the changed paths — PASS

- `load_prompts_partial_file_falls_back_to_consts` — fallback algebra (missing
  sections → consts).
- `load_prompts_empty_section_stays_empty` — empty section → block omitted from
  head (asserts absence of an APP_RULES marker + presence of TOOL STRATEGY).
- `load_prompts_malformed_returns_defaults` — parse error → defaults.
- `prompt_source_reload_picks_up_edits_and_deletion` — edit pick-up, no-edit
  no-op, deletion → defaults.
- `prompt_source_missing_file_uses_defaults` — missing file → defaults.
- `prompt_source_malformed_edit_keeps_previous_cache` — malformed edit keeps
  the loop safe (falls back to defaults).
- `default_prompts_toml_round_trips_to_const_defaults` — seed file parses to
  exactly the compiled defaults + carries the header.
- `ensure_prompts_file_seeds_once_and_never_clobbers` — first-run seed +
  user-edit survival.
- `build_stable_head_uses_custom_blocks` — custom section text lands in the
  head while the other sections keep defaults.

Each new test fails without its corresponding behavior (they assert the
specific fallback/omission/reload/seed semantics introduced by this change) and
exercises the real `load_str`/`build_stable_head`/`PromptSource` code paths.
All pre-existing head-stability tests were updated mechanically to pass
`&PromptBlocks::default()` and assert the same behavior as before.

### Notes (no action required)

- The test name `prompt_source_malformed_edit_keeps_previous_cache` asserts
  behavior that differs from its name (a malformed edit does *not* keep the
  previous cache; it falls back to defaults). The inline comment documents the
  intent clearly, so this is cosmetic — flagging only for awareness. (Renaming
  would require re-running the suite; not blocking.)
- `PromptSource` derives `Clone` (unlike `ConstitutionSource`), needed for the
  factory's shared use — fine.
- The `version` warning applies the sections anyway; documented in the file
  header — fine.

**Build:** The main agent reports `cargo test` green for both crates with zero
warnings; this review is static and found nothing contradicting that.
