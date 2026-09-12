# Review — memory access log (Graph tab) + graph-first TOOL_STRATEGY + descriptive graph tool cards

Branch `feat/memory-access-log`, all uncommitted changes vs `HEAD` (git status + git diff HEAD).
Scope per brief: the 11 feature files; `.coding/plans/stack.json` + untracked
`.coding/plans/cb832482-….md` are bookkeeping — noted, not deep-reviewed.

## Correctness — no findings

Verified against source (not just the diff):

- **Ring cap eviction** — `log_access` (src/memory/mod.rs:344–369): pops one front
  entry only when `len >= MAX_ACCESS_LOG_ENTRIES` (100), then `push_back`. Every push
  happens under the mutex and pops at most one, so the invariant `len <= 100` holds
  after arbitrarily many pushes; the `>=` form is the correct one (`>` would reach 101).
  Pinned by `access_log_caps_at_100_evicting_oldest` (105 writes → exactly 100 entries,
  oldest survivor `t005`, newest `t104` — the right survivors for newest-first order).
- **Newest-first snapshot** — ring is newest-last (`push_back`); `access_log()`
  (mod.rs:365–369) collects `iter().rev()` → newest first. Pinned by
  `access_log_records_reads_and_writes` (later read at index 0, earlier write at 1).
- **No poll feedback loop** — `access_log()` only locks + clones; it never calls
  `log_access`. The IPC `memory_access_log` (src-tauri/src/ipc/memory_debug.rs:272–280)
  calls only `store.access_log()`. The complete set of push sites is exactly:
  `write` (mod.rs:820), `recall` (mod.rs:960–965), `strongest` (mod.rs:1099). The 5s
  frontend poll therefore cannot mint entries.
- **No std lock held across an await** — `log_access` and `access_log` are fully
  synchronous (guard dropped at scope end). `write` logs *after* the spawn_blocking
  await resolves; `recall` logs after `batch_access`'s await. The ring's
  `Mutex<VecDeque<…>>` is a dedicated mutex, never held across an await, and the IPC
  command deliberately runs it inline (no SQLite connection touched — consistent with
  the F1 freeze lesson, which concerned the SQLite conn locks).
- **Trait surface untouched** — `MemoryStoreTrait` (mod.rs:150–254) has no new methods;
  `log_access` (private) and `access_log` (pub, inherent) live on `impl MemoryStore`
  (mod.rs:300+). Mock implementors compile unchanged; `IpcState.runtime.memory_store`
  is the concrete `Option<Arc<MemoryStore>>`, so the inherent method resolves.
- **`write` hoist** — `MemoryTier` is `Copy` (types.rs:12); `tier` copied and `title`
  cloned (mod.rs:791–795) before `memory` is moved into the closure. Log fires only on
  success (`??` early-returns on failure) — matches the documented success-only rule.
- **`recall` logs after truncation** — truncate at mod.rs:950–953, then
  `Some(scored.len())` at :964 → hits always equals the returned count. `strongest`
  likewise logs after `memories.truncate(limit)` (:1098–1099).
- **Push-site coverage** — consolidation writes go through `store.write()`
  (consolidation.rs:149, 406, 522) → logged; tool events via `record_tool_event` →
  delegates to `write` → logged; `Project::seed_stores` writes no memories (no
  `Memory::new`/`store.write` in src/project/mod.rs) → no seeding flood. Deliberately
  unlogged paths are sane: `list_by_tier`/`load_all`/`count_by_tier` are bulk/debug
  reads (logging `count_by_tier` would flood the ring via the Memory tab's 2s overview
  poll), and `delete`/`batch_access`/`access` are maintenance/strength mutations, not
  logical reads/writes. The Memory tab's recall test box does log (real `recall`) —
  correct and loop-free.
- **TOOL_STRATEGY order test is sound** — `find("search / search_read")` cannot match
  inside the graph line (`graph_search / graph_context …`), so the byte-index assert
  genuinely pins order; framing strings asserted verbatim. No other prompt test pins
  the removed wording (all `contains(` asserts in prompt.rs target unchanged text).

## Bugs

1. **Minor (UX + wasted IPC) — memory-access section unreachable when codegraph is
   unavailable, while its poll keeps running.**
   frontend/src/components/views/GraphView.tsx:397–408 — the
   `status && !status.available` early return renders only the "Code graph
   unavailable" notice; the memory-access section (:593–620) is skipped entirely in
   that state. But the section shows *memory-store* data, fully independent of
   codegraph availability — on projects with codegraph disabled (or a failed graph
   DB), the feature silently disappears. Meanwhile the 5s poll effect (:240–256) runs
   unconditionally on mount, so the app keeps invoking `memory_access_log` every 5s
   for data that branch can never display. Fix either way: render the memory-access
   section inside the unavailable branch too (notice + log section), or gate the poll
   on `status?.available !== false`.
2. **Note (hygiene, no lasting defect) — CRLF introduced in 8 changed files.**
   `git diff` warns "CRLF will be replaced by LF" for Message.tsx, GraphView.tsx +
   test, toolCardPaths.ts + test, memory_debug.rs, prompt.rs, types.rs — new lines
   were written with CRLF where the repo stores LF. Git normalizes on commit, so the
   committed content is clean; flagged only per the "preserve line-ending style"
   constitution rule (src/memory/mod.rs, tauri.ts, main.rs show no warning).

## Security — no findings

- The one-line render: `formatAccessLine(e)` flows only into React text content and
  the `title` attribute (GraphView.tsx:608–615). React escapes both (text children
  and attribute values), so query/title/detail from the store cannot inject markup;
  no `dangerouslySetInnerHTML` anywhere in the diff. The `detail` cap (120 chars,
  char-boundary safe) bounds the payload.
- `shortSymbolRef` / `graphCallLabel` are pure string splits for display labels — no
   URL/SQL/eval surface; output only ever rendered as React text.

## Constitution compliance — no findings

- **Doc comments on every new pub item**: `MemoryAccessEntry` + all five fields
  (types.rs:96–115), `pub fn access_log`, `pub async fn memory_access_log`
  (memory_debug.rs:262–271, with the no-spawn_blocking rationale), `graphCallLabel`,
  `formatAccessLine`, TS `MemoryAccessEntry` interface + fields + `memoryAccessLog`
  (tauri.ts), and the consts `MAX_ACCESS_LOG_ENTRIES` / `MAX_ACCESS_DETAIL_CHARS` /
  `ACCESS_LOG_POLL_MS`. Private `log_access` and `shortSymbolRef` documented too.
- **No `#[allow(...)]` introduced.** The two
  `// eslint-disable-next-line react-hooks/exhaustive-deps` comments in GraphView.tsx
  are pre-existing context lines, not additions.
- **Regression tests exist for the defect classes**: cap/eviction (105-push test),
  char-boundary detail cap (multibyte `é`×130 → 120), newest-first ordering,
  read/write wire shape, session-primer logging, label logic (incl. malformed JSON +
  missing fields + one-sided path), and the tool-strategy order test. All asserted
  against the constants/units directly, so rewording can't silently break them.
- Warning-free build is implied by green `cargo test` under `#![deny(warnings)]`
  (per the brief; as a read-only reviewer I could not re-execute the suite — the
  commit step should re-run `cargo test` + `npm run build` + `cd src-tauri && cargo
  build` per the usual matrix).

## Verdict

One actionable finding (B1: render or gate the memory-access section/poll on the
codegraph-unavailable branch); one hygiene note (B2, self-healing at commit).
Everything else — ring invariants, ordering, loop-freedom, lock discipline, trait
compatibility, hoist correctness, truncation-before-logging, XSS surface, docs,
no-allow, regression coverage — verified clean.
