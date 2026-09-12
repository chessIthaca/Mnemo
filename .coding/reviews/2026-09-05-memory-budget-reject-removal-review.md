## Verdict: FINDINGS (0 high, 7 low)

Review of the uncommitted change on the working branch: the hard
character-budget reject (`budget_violation`) is removed from
`memory_write`/`memory_supersede`, and shortness is instead encouraged by
(a) wiring the configurable `MemorySearchConfig` budgets into the
indexer/finish_capture auto-truncation and (b) a soft prompt guideline.

### Correctness — sound (no high findings)

- **`MemorySearchConfig` is `Clone + Send + Sync`.** It derives `Clone`
  (`src/memory/types.rs:413`) and every field is `f64`/`usize`, so `Send`
  and `Sync` are auto-derived. It therefore moves cleanly into each of the
  three `spawn_blocking` closures (`index_derived` `:90`, `corpus_is_stale`
  `:841`, `reindex_knowledge_files` `:224`) — captured by `move` and passed
  by `&` to the scan functions. No `Arc`/clone dance was needed.
- **`MemoryStoreTrait::memory_search_config()` exists and returns a clone.**
  Trait default at `src/memory/mod.rs:188` returns
  `MemorySearchConfig::default()`; the concrete `MemoryStore` impl (`:469`)
  clones out of the `RwLock`. Both the indexer and `finish_capture` read it
  per call, so a Settings save takes effect without a restart.
- **No missed call sites.** The 8 changed signatures
  (`scan_sources`/`scan_plans`/`scan_reviews`/`scan_backlog`/`scan_knowledge`/
  `plan_record`/`review_record`/`knowledge_source`) are all private to
  `indexer.rs`; every internal caller passes `&config`/`config`. The tests
  exercise only the public entry points (`index_derived`,
  `reindex_knowledge_files`, `corpus_is_stale`), so no test calls a changed
  private signature.
- **Routing variables retained.** `is_knowledge_type` (`mod.rs:149`) and
  `knowledge_rel` (`mod.rs:829`) were correctly kept — they drive the
  file-backed vs DB-only routing and must not have been deleted with the
  budget blocks.
- **Import drop is safe.** `MemorySearchConfig` was removed from
  `src/tool/memory/mod.rs`'s import list and has zero remaining references
  in that file (the only uses were `budget_violation` + the deleted
  `budget_override_from_live_config_is_honored` test). No unused-import or
  missing-symbol error.
- **`finish_capture` signature unchanged** — `config` is read internally
  (`:81`), so no caller updates were required.

### Dead code — none

- `MemoryRecordType::content_budget()` is still live: it seeds
  `MemorySearchConfig::default()` (`types.rs:449-453`) and is asserted by
  `record_type_content_budgets` (`types.rs:772`) and
  `memory_config_digest_budget_mirrors_record_type` (`general.rs:597`).
- `MemorySearchConfig::digest_budget()` is used at all 6 sites (indexer ×4,
  finish_capture ×2).
- The `*_budget` fields are still meaningful — they now control
  auto-truncation length (clamped to 50–5000 by `clamped()`), not rejection.
- Note (not a finding): the builders' `unwrap_or` fallbacks changed from
  `content_budget().unwrap_or(N)` to literal `unwrap_or(N)`. This is
  functionally equivalent because `digest_budget()` returns `Some` for every
  typed record (Plan/Bug/Spec/Decision/Review) — the fallback is only ever
  reached for `How`/`None`, where it correctly yields 500, matching the old
  behavior.

### Tests — consistent and meaningful

- The 3 removed tests (`write_enforces_typed_record_budgets`,
  `budget_override_from_live_config_is_honored`,
  `supersede_enforces_typed_budgets_on_the_successor`) all asserted the
  removed hard reject; their deletion is complete.
- `knowledge_wired_write_accepts_unbounded_bodies` was trimmed of its
  DB-only-reject second half; the remaining assertions (file carries full
  body, derived digest ≤ budget) still hold.
- `db_only_typed_write_accepts_long_content` directly verifies the removed
  reject: a 600-char DB-only `DECISION:` write now succeeds and stores the
  full content.
- `lowered_config_budget_truncates_derived_digest` meaningfully verifies the
  config-driven truncation. Its correctness depends on the pointer line
  being < 100 chars: `pointer_line()` (`knowledge.rs:177`) yields
  `path .coding/knowledge/decision/2026-09-05-tiny-budget.md` (57 chars),
  so `budgeted_digest` with budget 100 leaves `room = 100 − 57 − 1 = 42` for
  the gist → total ≤ 100, the 2000-char body is truncated away, and the
  pointer survives. Sound.

### Prompt — still complete

The updated `MEMORY_RECORDS` block (`prompt.rs:180-201`) retains every
essential element: all six typed prefixes, the pointer-first rule, the four
hygiene tools (`memory_supersede`/`memory_update`/`memory_delete`/
`memory_search`), the "NEVER leave both live" / "superseded, never deleted"
habits, and the branch-status default. The test
`stable_head_carries_memory_records_block` (`prompt.rs:789`) asserts all of
these plus the new `compact and pointer-first` marker; the old `≤500`/
`≤300`/`≤600`/`≤400` budget assertions were correctly dropped. The
assertion `head.contains("compact and pointer-first")` matches the block
text exactly.

### Multi-platform neutrality — clean

Rust-only change; no frontend, no platform-specific APIs, paths, or shell
syntax. `spawn_blocking` is cross-platform. No findings.

---

## Findings (all LOW — documentation sync)

The change set out to replace "enforced by `memory_write` / over-budget
errors" language with "auto-truncation" language. The *behavior* was
updated everywhere, but several **doc comments were missed** and still
describe the removed hard reject. Per the project constitution ("A feature
that ships with its docs not updated is an incomplete change"), these are
findings. None affect runtime behavior, correctness, or the build.

### L1 — `src/memory/types.rs:99-102` — stale enum doc
The `MemoryRecordType` enum doc still says:
> "Typed records carry content budgets (enforced by `memory_write`) and are filterable via `memory_list`…"

`memory_write` no longer enforces any budget. Suggest: "Typed records are
pointer-first (the gist + a path/commit pointer; the file carries the
detail) and are filterable via `memory_list`…" — mirroring the updated
`content_budget()` doc at `:173-177`.

### L2 — `src/memory/types.rs:116-119` — stale `Review` variant doc
> "Usually indexer-written (Phase 2 derived records), but the prefix is budget-enforced for authored writes too."

Authored `REVIEW:` writes are no longer budget-enforced. Suggest dropping
"but the prefix is budget-enforced for authored writes too."

### L3 — `src/memory/types.rs:428-430` — stale `plan_budget` field doc
> "Digest content budgets (chars) enforced by `memory_write` / `memory_supersede` for typed records. Defaults mirror [`MemoryRecordType::content_budget`]."

These fields now drive the indexer/finish_capture auto-truncation length,
not `memory_write`/`memory_supersede` enforcement. Suggest: "Digest
auto-truncation lengths (chars) for derived/captured digests, read by the
indexer and finish-capture. Defaults mirror
[`MemoryRecordType::content_budget`]."

### L4 — `src/memory/mod.rs:184-187` — stale trait-method doc
The `memory_search_config` trait default doc still says:
> "read by `memory_write`/`memory_supersede` for the digest budgets"

Those tools no longer read the config at all. Suggest: "read by recall
(decay/caps) and the derived-index/finish-capture auto-truncation."

### L5 — `src/memory/mod.rs:413-416` and `:466-468` — stale "per recall/write"
Both the `search_config` field doc and the inherent `memory_search_config`
method doc say "Read per recall/write so a Settings save takes effect
without a restart." The write path no longer reads the config (only recall
and the indexer do). Suggest "Read per recall/index so a Settings save
takes effect without a restart."

### L6 — `src-tauri/src/ipc/settings.rs:963` — stale DTO doc
`MemorySearchDto::plan_budget` is documented:
> "Digest content budgets (chars) enforced by `memory_write`."

This is the user-facing Settings DTO; the doc should reflect that these now
control auto-truncation length. Suggest: "Digest auto-truncation lengths
(chars) for derived/captured digests."

### L7 — `src/tool/memory/mod.rs:145-148` — dangling stale comment
The lead-in comment left behind when the budget-check block was deleted
ends with:
> "The tool-level budget therefore applies only to DB-only rows."

There is no tool-level budget anymore — DB-only typed writes now accept
unbounded content (verified by `db_only_typed_write_accepts_long_content`).
Suggest deleting that final sentence (and optionally merging the now
adjacent duplicate "Knowledge-backed write" comment blocks at `:145-148`
and `:151-155`).

---

### Summary
The core change is correct, complete, and well-tested: the config wiring is
sound (`Clone + Send + Sync`, all call sites updated, no dead code), the
removed-reject tests are replaced by positive tests, the prompt retains all
essential conventions, and the change is platform-neutral. The only
findings are 7 low-severity stale doc comments that still describe the
removed `memory_write`/`memory_supersede` budget enforcement — a
documentation-sync cleanup, not a behavior or correctness issue.
