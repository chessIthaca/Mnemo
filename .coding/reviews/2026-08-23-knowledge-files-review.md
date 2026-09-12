## Verdict: FINDINGS (4 high, 2 low)

Review of uncommitted work on `feat/knowledge-files` against plan
`.coding/plans/d16b3c22-afa1-4ff3-afc1-83872e9b02c6.md` (knowledge-as-files,
rebuildable memory index, working-branch topology, backlog jsonl).

Scope: full `git diff HEAD` (38 tracked paths + untracked
`src/memory/knowledge.rs`), plan steps, docs/skill sync, multi-platform
neutrality, and the checklist items in the review brief. Tests were not
re-run here; the brief states root 1466 / tauri 153 / frontend build were
green.

---

### High

#### H1. `.gitattributes` replaced wholesale — LF/binary policy deleted

**File:** `.gitattributes`

The prior file (backlog #48) set:

- `* text=auto eol=lf` (canonical LF on every platform)
- binary rules for images/fonts/executables
- `*.db` / `*.db-wal` / `*.db-shm` as `-text`

The diff **deletes all of that** and leaves only:

```gitattributes
.coding/backlog.jsonl merge=union
```

Impact:

- Reintroduces Windows CRLF churn (`core.autocrlf` warnings already show in
  the diff output).
- Removes the SQLite binary safety net if any `.db` ever becomes tracked.
- The union-merge addition is correct and wanted; it must be **appended**,
  not substituted for the whole attributes policy.

**Fix:** restore the previous `.gitattributes` body and add the
`.coding/backlog.jsonl merge=union` line.

---

#### H2. Legacy `backlog.json` migration cannot load real numeric ids

**Files:** `src/backlog.rs` (`BacklogItem.id: String`, `open()` legacy path),
test `legacy_json_envelope_is_migrated_on_open`

Production legacy files used monotonic `u64` ids (`"id": 1`). The envelope
deserializes into `Vec<BacklogItem>` where `id` is now `String`. serde_json
rejects number→string without a custom deserializer, so:

```rust
if let Ok(file) = serde_json::from_str::<BacklogFile>(&content) {
    // migrate…
}
```

silently fails on real files and the store starts **empty** — user backlog
loss on first open after upgrade.

The only migration test writes a **string** id (`"old-1"`), so it never
exercises the production shape.

**Fix:**

- Accept both shapes on load (e.g. `#[serde(deserialize_with = …)]` that
  stringifies numbers, or a legacy DTO with `id: u64` mapped to
  `id.to_string()`).
- Add a regression test with `"id": 1` (and preferably a mixed file).
- Optionally rewrite migrated numeric ids to UUIDs only if you intentionally
  want collision-free multi-worktree ids going forward; preserving
  `id.to_string()` is enough to avoid data loss.

---

#### H3. Knowledge-backed `memory_write` stores the typed prefix in the file title

**Files:** `src/tool/memory/mod.rs` (`MemoryWriteTool::execute`),
`src/memory/knowledge.rs` (`KnowledgeRecord::typed_title`)

Agent titles are always prefixed (`"DECISION: storage engine"`). The
knowledge path does:

```rust
support.knowledge.write(record_type, &title, &body)
```

`write` puts that string into front matter `title = "…"`. The indexer then
builds:

```text
DECISION: DECISION: storage engine
```

via `typed_title()` (`"{prefix} {self.title}"`).

Effects:

- Polluted titles in recall / list / debug UI.
- Slugs include the type word (`…-decision-storage-engine`).
- Diverges from migration (`strip_knowledge_prefix` → bare title) and from
  indexer/unit fixtures (bare front-matter titles).

The wired tool test checks `record_type` and body content but **not**
`m.title`, so the double prefix slips through.

**Fix:** strip the typed prefix (and optional following space) before
`KnowledgeStore::write` / `supersede` / finish-capture titles; keep the bare
title in front matter. Add an assertion on the derived row title.

Same class of bug on knowledge-backed `memory_supersede` (successor title is
also passed through raw).

---

#### H4. Knowledge file bodies are still digest-budget capped (contradicts design)

**Files:** `src/tool/memory/mod.rs` (`budget_violation` before the knowledge
branch), plan step 3, README/PLAN claims

Plan / docs: typed knowledge **files are unbounded**; budgets apply to the
**derived digest**.

Implementation still runs `budget_violation(&title, &args.content, …)` on the
tool `content` **before** branching into the knowledge writer. A legitimate
long DECISION/SPEC/BUG body is rejected with the old “budgeted pointers”
error and never reaches disk.

So the core “files are the truth, unbounded body” invariant is not shipped for
the agent write path (only for direct filesystem / migration / finish
`write_at` paths that skip the tool).

**Fix:** skip content budget (or budget only a separately supplied digest
gist) when `is_knowledge_type && self.knowledge.is_some()`. Keep budgets for
DB-only typed rows (PLAN/REVIEW and unwired tests). Add a regression test
that writes a >300-char DECISION body successfully and asserts the digest
row is still ≤ budget.

---

### Low

#### L1. Plan step 4 incomplete: no links UI on chat `MemoryEntryCard`

**Files:** plan step 4; `frontend/src/components/views/MemoryDebugView.tsx`
(done); `frontend/src/components/chat/Message.tsx` (unchanged)

Step 4 required link chips + “referenced by” in **both** MemoryDebugView and
`MemoryEntryCard`. Only the debug view was updated. Chat memory entries still
show tier/title/snippet only.

**Fix:** either port LinkChip/Backlinks (or a lighter variant) into
`MemoryEntryCard` when `data.links` / `rel_path` exist, or explicitly amend
the plan/docs if chat-surface links were deferred.

---

#### L2. Authored-row migration lacks the planned guard-flag row

**Files:** plan step 7; `src/memory/indexer.rs` `migrate_authored_typed_rows`;
`src-tauri/src/main.rs` startup

Step 7 specified a **guard flag row** for one-time migration. The
implementation relies on “delete authored row after file write” for
idempotency. That is mostly sound, but:

- There is no durable “migration completed” marker independent of row
  presence.
- A future path that re-inserts authored typed rows would migrate again
  (usually OK, but not the planned latch).
- Partial failure handling is “best effort” (`delete_memory` errors ignored).

**Fix (optional but plan-faithful):** write a small index_state /
meta flag (e.g. `migration:authored_typed_to_knowledge=v1`) after a successful
pass and skip when set; still keep content-level idempotency as a safety net.

---

### Checklist (brief)

| Area | Assessment |
|------|------------|
| Knowledge lifecycle (write → UUIDv5 row, supersede chain, removal, migration leaves untyped DB-only) | Core indexer/store logic is solid and well-tested; **agent write path broken by H3/H4**. Migration preserves PLAN/untyped/working (tested). |
| Backlog jsonl + sandbox protect both names + UUID IPC | jsonl format, union driver intent, sandbox/IPC/TS fixtures look consistent; **legacy numeric migration broken (H2)**; `.gitattributes` collateral damage (H1). |
| String ids / `Arc<Mutex<Option<String>>>` call sites | backlog_cmds, run_all, state, fixtures updated; no remaining `AtomicU64` backlog ids or `typeof id === "number"` in the diff. |
| Startup reconcile | `corpus_is_stale` + evented `index_derived` in `build_brain_inner`; silent when fresh; delete-DB rebuild covered by unit test. Console path runs without UI events. |
| Docs / `merge_to_main` | README, PLAN, agent.md, skill two-hop + reconcile text aligned; skill still contains `MERGED into main` (matches `src/skill/mod.rs` assertion). |
| Multi-platform | No new Windows-only APIs in lib/app code; rel paths normalized to `/`; hang-watchdog Windows gate untouched. |
| Regression tests | Strong coverage for indexer knowledge roundtrip, supersede visibility, removal, stale/rebuild, backlog union concat, list UUID ordering, develop auto-create, finish knowledge BUG file. Gaps: H1 (no test), H2 numeric legacy, H3 title assertion, H4 unbounded body. |

---

### Non-findings / positives

- `src/memory/knowledge.rs` parser/writer design (graceful TOML degradation,
  collision suffixing, supersede flips old `status`, wiki-link extraction) is
  clear and unit-tested.
- Indexer supersede reconciliation for **unchanged** predecessors after a
  successor appears (git-merge convergence) is correctly implemented and
  tested.
- `knowledge_id` strips `.md` so tool-reported ids match indexer rows.
- Sandbox protects `.coding/knowledge/`, `backlog.jsonl`, and legacy
  `backlog.json`.
- Factory wires `KnowledgeStore` into memory tools + `FinishTool`; finish BUG
  capture has a dedicated knowledge-file test.
- Working-branch topology: `create_plan` default base `develop`, develop
  auto-create from `main`, wt reuse tests, `stack.json` gitignored and
  untracked in fixtures.
- Trait plumbing (`MemoryStoreTrait` indexer delegates +
  `set_derived_metadata`) is class-scoped and tested.

---

### Recommended fix order

1. Restore `.gitattributes` + append union rule (H1).  
2. Numeric-id legacy backlog migration + test (H2).  
3. Strip typed prefix on knowledge writes/supersedes + title assertion (H3).  
4. Disable body budget on knowledge-backed writes; keep digest budget in
   indexer (H4).  
5. Message.tsx links or plan deferral note (L1); optional migration flag (L2).

After fixes: re-run root `cargo test`, `cd src-tauri; cargo test` /
`cargo build`, and `npm run build` under `#![deny(warnings)]`.
