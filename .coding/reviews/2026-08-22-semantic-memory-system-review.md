## Verdict: FINDINGS (3 high, 4 low)

Review of all uncommitted changes on `feat/semantic-memory-system` (28 tracked files + 4 new files: `src/memory/{indexer,finish_capture}.rs`, `src/tool/agent/git_read.rs`, `src/tool/memory/retrieval.rs`), covering the 4 phases: typed records + hygiene tools, derived indexer + Settings Memory section, retrieval tools + git_read bridge, bug_fixing plan kind + finish auto-capture.

The implementation is overall high quality: thorough in-file + integration tests, correct WAL/lock discipline inside the memory store (spawn_blocking hops, read/write conn split), atomic supersede transaction, fail-closed verdict gate, solid git option-injection validation, and correct schema migration ordering (indexes created after the ALTER TABLEs). Findings below.

---

## HIGH 1 — Session primer (`strongest`) does NOT exclude superseded memories

`src/memory/mod.rs:1626-1631` — the `strongest` override selects with only `WHERE tier IN (...)`; there is no `superseded_by IS NULL` clause. `strongest(&[Semantic, Procedural], 8)` is the session-primer source (`src/agent/turn.rs:546`, rendered as `# PROJECT MEMORY (standing context)` by `format_primer`).

Phase 1's headline invariant is "superseded excluded from default recall, retrievable via include_superseded" — `recall`/`list_filtered` honor it at SQL level, but the primer does not. A superseded high-strength record (e.g. `DECISION: storage engine → postgres`, superseded by `→ sqlite`) will keep surfacing in the standing context of EVERY new session at full strength — exactly the stale-knowledge channel supersede-not-delete was built to close. The `superseded_strongest_match_stays_hidden_by_default` test covers recall only; nothing covers the primer path.

Fix: add `AND superseded_by IS NULL` to the `strongest` SQL, plus a regression test that supersedes the strongest semantic memory and asserts it leaves `strongest()` output.

## HIGH 2 — Plan-file section injection / silent truncation via unsanitized newlines in `bug` and `regression_test`

`src/tool/workflow/plan.rs` (create_plan `bug` param: only `str::trim`; update_plan → `src/workflow/mod.rs` `regression_test`: only `rt.trim()`) — neither rejects nor sanitizes embedded newlines before the value is persisted by `PlanFile::serialize` as a raw `## Bug\n{symptom}\n` / `## Regression test\n{test}\n` block (`src/workflow/plan_file.rs:284-289`).

Two consequences on reload (crash-resume parses the file back):

- **Truncation:** the parser keeps only the LAST non-empty line of each section (`plan_file.rs` `Section::Bug`/`Section::RegressionTest` arms assign `bug_symptom = Some(trimmed_line.to_string())` per line). A two-line symptom silently loses its first line.
- **Injection:** a symptom containing `\n\n## Kind\nresearch` is re-parsed as a real `## Kind` section — the later occurrence overwrites `kind`, turning a `bug_fixing` plan into a `research` plan on resume, which SKIPS the review gate (and disables the finish BUG: capture). Embedded `## Steps` + `- [x] 1. fake` lines likewise re-parse as completed plan steps. The `bug` text is model-generated, so this is self-inflicted rather than adversarial, but it undermines the Phase-4 gates that read kind/symptom/regression_test from this file.

Fix: collapse the values to a single line before persisting (e.g. first line, or replace whitespace runs with spaces) in `create_plan`/`update_plan`, and add a regression test that a multi-line symptom round-trips without changing `kind`/`steps`.

## HIGH 3 — `finish` holds the workflow `tokio::Mutex` guard across the capture awaits

`src/tool/workflow/plan.rs:843` acquires `let mut wf = self.workflow.lock().await;` and the guard lives through the whole function — including across `crate::memory::finish_capture::capture_finish(...).await` (~line 905), which itself awaits multiple store round-trips (`write`, `list_filtered`, one `supersede_memory` per crash marker, each a spawn_blocking SQLite hop).

The codebase's strict rule is never hold a lock across an await (the same file's IPC cousins do snapshot-and-drop; `memory_rebuild_index` in `src-tauri/src/ipc/memory_maintenance.rs` snapshots the corpus paths before `start_op` for exactly this reason). No deadlock path exists today (capture touches only the memory store), but any concurrent workflow-lock acquirer — IPC state queries, the crash-marker writer — stalls behind the capture's SQLite latency, and the pattern invites a future re-entrant deadlock.

Fix: extract `plan` (cloned), `plan_id`, `plans_dir` under the lock, drop the guard, run the gates + capture unlocked, then re-lock to call `wf.finish()` (a state re-check on re-lock keeps it safe).

---

## LOW 1 — Documentation sync: README.md / PLAN.md not updated

Constitution review rule: behavior changes ship with docs. Not updated:

- `PLAN.md:99` — "**Memory tools** — `memory_write`, `memory_recall`, `memory_consolidate`" is now stale: 8 new tools (`memory_update`, `memory_supersede`, `memory_delete`, `memory_list`, `plans_search`, `reviews_search`, `past_fixes`, `context_pack`) plus `git_log`/`git_show` are undocumented there.
- `README.md` "Memory" bullets (lines 37-40) — no mention of typed records (SPEC:/DECISION:/BUG:/PLAN:/HOW:/REVIEW:) with budgets, supersede-not-delete, the derived index (plans/reviews/backlog digests) + Settings → Memory rebuild card, or the `[memory]` config section (Settings knobs; the config overview at line ~107 lists sections without `[memory]`).
- `README.md:20/23` + `PLAN.md` workflow sections — the new `bug_fixing` plan kind (locked skeleton, regression-test gate) and the review-report verdict contract (`## Verdict: PASS|FINDINGS`, fail closed) are user-visible behavior and undocumented.

## LOW 2 — Frontend `MEMORY_TOOLS` set missing the 8 new memory tools

`frontend/src/hooks/agentEventReducer.ts:310` — `MEMORY_TOOLS` is still `{"memory_write", "memory_recall", "memory_consolidate"}`. Events from `memory_update`/`memory_supersede`/`memory_delete`/`memory_list`/`plans_search`/`reviews_search`/`past_fixes`/`context_pack` fall back to generic tool cards instead of memory chips (the set gates `parseMemoryEntry` at lines 398/496). The recall-format regex itself was correctly extended for the new `(id: …, score: …)` line with legacy fallback (and has test coverage) — only the set membership lags.

## LOW 3 — Lazy derived-index bootstrap runs outside the maintenance-op guard

`src-tauri/src/main.rs` (build_brain_inner) spawns the bootstrap `index_derived` as a bare task gated only by `count_by_class() == (_, 0)`. The manual "Rebuild derived index" command IS guarded (`start_op` single-running-op guard), but the bootstrap is not. A user opening Settings → Memory and clicking Rebuild while first-open bootstrap is mid-run interleaves `delete_derived` + `index_state_clear` with bootstrap writes/state-upserts; a state row can survive pointing at a deleted memory row, which then skips as "unchanged" on incremental runs (a permanent hole until the source changes or the next rebuild). Self-healing and narrow-window, hence low — but routing the bootstrap through the same guard (or marking bootstrap-in-progress) closes it.

## LOW 4 — Nits

- `src/memory/types.rs` — `pub struct Session {    pub id: String,` got same-line-merged (rustfmt would reformat; currently not `cargo fmt`-clean).
- `src/tool/memory/mod.rs` (tests) — `async fn typed_write_keeps_confirmation_and_data_shape() {        // Frontend parser compat…` — same same-line-merge glitch.
- `src/memory/types.rs` `MemoryFilter::record_type` doc lists "`SPEC:`/`DECISION:`/`BUG:`/`PLAN:`/`HOW:`" — missing `REVIEW:`.
- `finish` tool schema description (`src/tool/workflow/plan.rs:812-817`) doesn't mention the verdict gate or the bug_fixing regression-test requirement; the prompt covers both, but the tool schema is the agent's always-visible contract.

---

## Checks performed (no findings)

- **SQL correctness:** all `memories` SELECTs updated to the 14-column shape matching `parse_memory_row` (verified via repo-wide `FROM memories` scan — no stale 11-column readers; no other module parses memory rows). FTS UPDATE trigger's `WHEN old.title IS NOT new.title OR old.content IS NOT new.content` guard (`schema.rs:148-149`) matches `update_memory`'s re-index claim; INSERT/DELETE triggers keep FTS in sync for write/supersede/delete.
- **Migration ordering:** `record_class`/`record_type`/`superseded_by` ALTERs run before the new indexes are created (legacy DBs covered by `migration_adds_record_columns_to_legacy_db`); `derived_index_state` + `idx_derived_index_state_memory` present on fresh DBs.
- **Supersede atomicity:** single transaction (insert successor + mark old), error names the existing superseder, rollback verified by test. Re-write via deterministic UUIDv5 resets `superseded_by` (idempotent re-finish) — intended.
- **Recall scoring:** derived per-class cap `retain` runs after the score sort (order preserved) and before the limit truncate; explicit filter limit correctly wins over `per_query_cap`; authored exempt from the cap; class weight (0.05) is tie-break-only next to tier weight (0–0.3).
- **git_read security:** no shell (direct argv), leading-`-` rejection kills option injection, `..`/absolute path rejection for `git_log -- <path>`, metacharacter rejection for commit-ish; tests pin each rejection. `cfg(windows)` `CREATE_NO_WINDOW` matches the existing git/shell-tool pattern (sanctioned; compiles on macOS).
- **write_review_report path guard:** verdict check added before the existing canonicalize-under-reviews check; traversal/subdir/absolute rejections intact and re-tested with verdict-carrying content.
- **Verdict fail-closed:** unparseable verdict blocks `finish` (`parse_verdict` → Unparseable → error); FINDINGS blocks; write_review_report rejects verdict-less reports; both sides tested.
- **bug_fixing gates:** locked skeleton forced at create, steps replacement refused, regression_test required at finish, codegraph `resolve` (`src/codegraph/query.rs:193`) exact→CI→substring match — symbol gate + unwired-graph skip note both tested; `graph.view()` error fails closed.
- **finish capture:** deterministic UUIDv5 ids (idempotent re-run), crash-marker candidate list snapshotted before superseding (successor "— COMPLETE" rows can't self-supersede), capture failure never blocks the state transition.
- **IPC/config:** `[memory]` DTO validation bounds (0.1–365 d, 1–500, 1–100, 50–5000) match `MemorySearchConfig::clamped()`; rewire pushes the live snapshot without restart; `MaintenanceOp::Index` serializes kebab-case with a pinned wire-shape test; `MemoryIndexStatus`/get_settings fixtures updated (dto-get-settings.json + contract_fixtures.rs).
- **Warning-free:** no `#[allow(...)]` added anywhere in the change (the two `#[allow(clippy::too_many_arguments)]` hits are in pre-existing, unchanged `src/agent/loop_impl.rs`).
- **Multi-platform:** no Windows-only APIs/paths/shell syntax in library code (indexer/finish_capture use portable fs + display-only `/` pointers).
- **Tests:** not run by this reviewer (read-only — no shell). Coverage by inspection is strong (budgets incl. multibyte chars, FTS re-index on update, supersede visibility/rollback, derived caps, indexer incremental/removal/rebuild, verdict gates, end-to-end bug_fixing pipeline in `tests/workflow_integration.rs`). The main agent must run `cargo test` unpiped (plus `cd src-tauri && cargo build` for the bin crate, and `npm run build` for the frontend) before committing.
