## Verdict: PASS

Re-review of the fixes for the 6 findings in
`2026-08-23-knowledge-files-review.md` (plan d16b3c22, branch
`feat/knowledge-files`). All fixes landed in commit **86fd957** (HEAD,
working tree clean — `git status --short` and `git diff HEAD` are empty, so
the fixes were folded into the feature commit rather than sitting as a
follow-up). Each fix was verified against the committed file contents plus
the fix-relevant slice of `git diff 86fd957^ 86fd957`.

---

### H1. `.gitattributes` — FIXED (verified)

`.gitattributes` now carries the **full prior policy**: `* text=auto eol=lf`
(L12), image/media binary guards (L15–28), fonts (L31–35),
exe/dll/pdb (L38–40), and `*.db -text` / `*.db-wal -text` / `*.db-shm -text`
(L43–45) — **plus** `.coding/backlog.jsonl merge=union` appended (L52) with a
rationale comment explaining the union-driver contract. No substitution, no
deletion. `git status` is clean of renormalization noise (empty status), so
the restore introduced no CRLF churn.

### H2. Legacy numeric backlog ids — FIXED (verified)

- `src/backlog.rs`: `BacklogFile.items` is now `Vec<LegacyItem>`;
  `LegacyItem.id: LegacyId` where `LegacyId` is a `#[serde(untagged)]`
  `Num(u64) | Str(String)` whose `as_string()` stringifies numbers
  (L407–448). `BacklogStore::open`'s legacy-not-found path deserializes via
  `from_str::<BacklogFile>` and maps `i.id.as_string()` (L117–142).
- `src/memory/indexer.rs`: the indexer's legacy envelope parse uses
  `BacklogEntry.id` with `#[serde(deserialize_with = "de_backlog_id")]`;
  `de_backlog_id` is the same untagged Num/Str → String shape (L423–459), so
  a pre-upgrade backlog indexes even if the store never opened first.
- Regression test `legacy_json_envelope_is_migrated_on_open`
  (`src/backlog.rs` L845–874) writes a **mixed** file (numeric `"id":1` +
  string `"id":"old-2"`), asserts `items()[0].id == "1"` (numeric
  stringifies) and `items()[1].id == "old-2"`, asserts jsonl persistence
  (2 lines) with the legacy file left for git, and reopens → 3 items
  (no double-migration). The production shape is now exercised.

### H3. Bare front-matter titles — FIXED (verified)

- `KnowledgeStore::bare_title(title, record_type)`
  (`src/memory/knowledge.rs` L501–509) strips the record's **own** typed
  prefix (+ leading space); foreign or absent prefixes pass through
  unchanged (documented: the directory, not the title, defines the type —
  no data loss on odd titles).
- Applied at all four mutation points: `write` (L526), `write_at` (L573),
  `update` (L617), `supersede` successor title (L660). Slugs therefore
  derive from the bare title — no type word in file names (confirmed by the
  migration test's `decision/2026-02-01-storage-engine.md`).
- Wired tool test `knowledge_wired_write_lands_in_a_file_and_derived_row`
  (`src/tool/memory/mod.rs` L1221–1338) asserts, for the **initial write**:
  file front matter contains `title = "storage engine"` (L1256) AND the
  derived row title is `"DECISION: storage engine"` — "prefixed exactly
  once" (L1264–1267); for the **supersede successor**: the same pair
  (L1314–1322). It also re-confirms the old file flips to
  `status = "superseded"` (L1324) and the old row is excluded from recall
  (L1328–1332).
- Non-regressions checked: migration passes pre-stripped bare titles into
  `write_at` (indexer L915–936 — its strip is a no-op); finish_capture's
  BUG path passes `&plan.title` unprefixed (`finish_capture.rs` L175; the
  DB-row fallback prefixes `"BUG: {}"` separately at L187) — unaffected.

### H4. Unbounded knowledge bodies — FIXED (verified)

- `MemoryWriteTool::execute` (`src/tool/memory/mod.rs` L150–164): computes
  `is_knowledge_type` (SPEC/DECISION/BUG/HOW only — PLAN/REVIEW have no
  knowledge home) and skips `budget_violation` when
  `is_knowledge_type && self.knowledge.is_some()`; the knowledge branch then
  writes the file + targeted reindex (L170–201). DB-only rows keep the
  budget.
- `MemorySupersedeTool::execute` (L868–884): the budget applies only when
  `rel_for_id` fails to resolve (DB-only rows keep the budget; file-backed
  successors are unbounded). `rel_for_id` correctly requires a Derived row
  with a knowledge-dir `rel_path` (L322–335).
- Regression test `knowledge_wired_write_accepts_unbounded_bodies`
  (L1341–1393): a ~631-char DECISION body **succeeds** file-backed with all
  600 `x`s on disk and the derived digest ≤300 chars; the **same** body via
  the unwired tool errors with `"DECISION: record over budget"`. Both
  directions covered.
- Docs now match code: README (L42) states the files are unbounded and the
  derived index holds budgeted digests — the H4 contradiction is closed.

### L1. Chat `MemoryEntryCard` link deferral — RESOLVED (verified)

`frontend/src/components/chat/Message.tsx` L95–100 carries an explicit
"review scope note, 2026-08-23" doc block on `MemoryEntryCard`: link chips
are not rendered because the tool-result event payload carries only
tier/title/snippet (no row `data.links`); the wiki-link surface is the
Memory debug view, and extending the chat card would require plumbing row
data through the event. This is exactly the "explicitly amend if deferred"
remedy the original review accepted.

### L2. Migration guard latch — FIXED (verified)

- `migrate_authored_typed_rows` (`src/memory/indexer.rs` L892–952) early-
  returns when the knowledge-dir marker `.migration-authored-v1` exists
  (L899–902) and writes the marker after a completed pass (L946–950,
  best-effort, with row-level idempotency documented as the safety net).
- The marker is a dotfile and every corpus scan filters
  `extension() == "md"` (indexer L382, L403, L555) — it can never be parsed
  as a record.
- Test `migrate_authored_typed_rows_writes_files_and_drops_rows`
  (L1993–2126) asserts the marker is written after run 1 (L2058–2063) and
  that a row inserted **after** migration (`"SPEC: late arrival"`) is NOT
  migrated on the second run (`migrated == 0`, the row stays authored)
  (L2104–2125). Row-level content idempotency (same-slug collision guard,
  partial-failure heal) is retained (L922–935).

---

### Cross-checks requested by the brief

| Check | Result |
|---|---|
| Compile warnings | Not re-runnable here (read-only review); brief reports root `cargo test` 1467 passed, src-tauri 153 + build finished, frontend build ok — under `#![deny(warnings)]` at both crate roots, green builds prove zero warnings. No new `#[allow(...)]` was introduced: a sweep of `src/` and `src-tauri/src/` finds only the two pre-existing `#[allow(clippy::too_many_arguments)]` in `src/agent/loop_impl.rs` (clippy lints, untouched by this change) and one doc-comment mention in `src-tauri/src/main.rs`. |
| Slug derivation uses bare title | Yes — `slug_for(&title, &date)` after `bare_title` (knowledge.rs L526–532, L660–662); migration test's `2026-02-01-storage-engine.md` shows no type word. |
| Migration → `write_at` bare titles (no-op strip) | Yes — `strip_knowledge_prefix` before `write_at` (indexer L915–936). |
| finish_capture BUG path passes `plan.title` (unprefixed) | Yes — `finish_capture.rs` L175; the prefixed `"BUG: {}"` string is built only for the DB-row fallback (L187). |
| Supersede still flips the old file's status | Yes — knowledge.rs L693–704 rewrites the old file with `KnowledgeStatus::Superseded`; tool test asserts `status = "superseded"` (L1324) and recall exclusion (L1328–1332). |
| `.gitattributes` restore → no CRLF churn | Yes — `git status --short` / `git diff HEAD` empty. |

### Notes (non-findings)

- The migration marker travels with git (committed under
  `.coding/knowledge/`), so once merged the one-time migration is latched
  repo-wide across worktrees. This is the planned step-7 semantics and is
  documented in-code; a future intentional re-migration must clear the
  marker or address the files directly.
- `bare_title` intentionally leaves a *foreign* prefix intact (a DECISION
  titled "SPEC: …") — documented; the digest's `typed_title` still prefixes
  with the record's own type exactly once.

All 4 high + 2 low findings are fixed with code, comments, and regression
tests that exercise the exact production shapes. No new findings.
