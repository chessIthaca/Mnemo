## Verdict: FINDINGS (1 high, 2 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan b587da79 (backlog 0bba3241, "Run-all lane memory-message clarity"): `src/tool/memory/mod.rs` (lane note on memory_write), `src/agent/factory.rs` (root threading), `src-tauri/src/ipc/run_all.rs` (gist ellipsis + rel_path fourth field), plus the `.coding/backlog.jsonl` status flip (bookkeeping, expected).

**Summary:** The lane-note feature (Me4) and the gist ellipsis are correct, well-tested, and correctly threaded. The gist **path-pointer feature is broken for every real hit**: `data.rel_path` on knowledge-derived rows is stored **knowledge-dir-relative** (`spec/2026-09-08-login.md`), but the new test fabricates a **project-relative** value (`.coding/knowledge/spec/...`), so the test passes while the shipped code emits an unresolvable path — exactly the not-found detour this change set out to eliminate.

| # | Severity | Finding |
|---|----------|---------|
| 1 | HIGH | `format_recalled_context` emits `data.rel_path` verbatim, but the indexer stores it knowledge-dir-relative (no `.coding/knowledge/` prefix) — the fourth field is unresolvable from the project root on 100% of real knowledge hits; the new test masks this with a fabricated prefixed value |
| 2 | LOW | README.md:79 recalled-context description ("tier-prefixed title + gist") is now stale — no mention of the "..." cut marker or the path fourth field (documentation sync) |
| 3 | LOW | Lane-note root comparison is exact component equality (case-sensitive Normal components on all platforms; verbatim `\\?\`/8.3 forms would compare unequal) — unreachable in practice since both sides share the same project-root source, but worth one doc-comment sentence |

Verification results for the spawn prompt's (a)–(d) checklist, docs sync, and multi-platform neutrality are detailed at the end of this report.
## HIGH 1 — The fourth field emits a knowledge-dir-relative path; the test asserts a shape that never occurs in production

**Location:** `src-tauri/src/ipc/run_all.rs:347-359` (emission), `:1065-1068` (test fabrication).

**The code:** `hit.memory.data.get("rel_path").and_then(|v| v.as_str())` is emitted verbatim as the fourth field, and the new test seeds `rel_path: ".coding/knowledge/spec/2026-09-08-login.md"`.

**What the store actually holds:** `data.rel_path` on knowledge-derived rows is **knowledge-dir-relative** — no `.coding/knowledge/` prefix:

- The only writer of the field into row data is `src/memory/indexer/knowledge.rs:400` (`"rel_path": record.rel_path`), where `record.rel_path` comes from `parse_file(rel_path, …)` — documented at `src/memory/knowledge.rs:277` as "the path **under the knowledge root**", e.g. `spec/2026-09-08-login.md`.
- Definitive: `src/memory/indexer/tests/knowledge.rs:63` asserts `m.data["rel_path"] == "decision/2026-08-23-typed-records.md"` — unprefixed.
- Corroborated by `KnowledgeSupport::rel_for_id` (`src/tool/memory/mod.rs:314-321`), which reads `data.rel_path` and requires it to start with `spec/`/`decision/`/`bug/`/`how/` — if the stored value were prefixed, `memory_update`/`memory_supersede`/`memory_delete` would be broken today (they are not).
- The store's own reporting convention is project-relative: `memory_write` reports `.coding/knowledge/{rel}` (`src/tool/memory/mod.rs:184, 208`).

**Consequence:** every real knowledge hit emits ` — spec/2026-09-08-login.md` (or `decision/…`, `bug/…`, `how/…`). The dispatched model's file tools are rooted at the project/worktree root, so that path resolves to nothing — precisely the not-found detour this change (and Me4) exist to prevent. Note only knowledge-derived rows carry `data.rel_path` (plans/reviews/backlog rows don't have the field, per `mod.rs:315`), so **100% of real fourth fields are affected** — the feature never works as intended. The new test passes only because it fabricates the prefixed shape, asserting a contract the store never produces.

**Fix (no store-behavior change needed):** in `format_recalled_context`, emit the project-relative path — prefix `.coding/knowledge/` when the value lacks it (matching the store's own convention at `mod.rs:184/208`), e.g. `let path = if p.starts_with(".coding/") { p.to_string() } else { format!(".coding/knowledge/{p}") };`. Then fix the test to seed the **real** stored shape (`"rel_path": "spec/2026-09-08-login.md"`) and assert the emitted line contains `.coding/knowledge/spec/2026-09-08-login.md` — that makes the test exercise the actual data shape and fail without the prefixing fix. (Normalizing at the indexer instead would be a store-behavior change, out of this plan's declared scope.)
## LOW 2 — README.md:79 recalled-context description is stale (documentation sync)

README.md:79 describes the dispatch block as "each a tier-prefixed title + gist under a verify-against-the-code caveat". The change adds two visible behaviors to that line shape: a `...` marker on cut gists and a fourth ` — path` field on knowledge rows. Per the project's documentation-sync rule, update the clause, e.g. "each a tier-prefixed title + gist (ending `...` when cut; knowledge rows append their file path) under a verify-against-the-code caveat". PLAN.md:83 and :227 remain accurate ("gist under a verify-against-the-code caveat", "capped at 5 one-line gist hits") — no change needed there. No README/PLAN text quotes the `memory_write` result message, so the lane note needs no doc sync.

## LOW 3 — Lane-note root comparison is exact component equality (informational, one-sentence doc comment suggested)

`Some(agent_root) != store_root` (`src/tool/memory/mod.rs:194-195`) compares `Path`s component-wise: separators normalize on Windows (`/` vs `\` compare equal), but Normal components are case-sensitive on **all** platforms, and verbatim (`\\?\`) or 8.3 short-name forms would compare unequal. This is unreachable in practice — both sides derive from the same project-root source (factory: `plans_dir.parent()` + `KNOWLEDGE_DIR_NAME`, factory.rs:285-291; lane: the worktree spec built under that same root) — so no code change is required. Suggested action: one doc-comment sentence on the comparison noting it assumes both roots share the same source spelling. The derivation `dir().parent().and_then(Path::parent)` itself is correct for the production layout `<root>/.coding/knowledge` and for the test helper's layout; if knowledge were ever rooted shallower, `store_root` becomes `None` and the note fails **open** (fires for any rooted agent) — conservative direction, acceptable.

## Verification results (spawn-prompt checklist)

**(a) Lane-note condition and wording — PASS.** The note lives only in the knowledge-backed success branch (`mod.rs:187-202`); the plain-DB arm reports no path so it cannot fire there. Condition: `agent_root` must be `Some` AND differ from `store_root` — main-tree agents (root `None` at build_registry, or a root equal to the store root) get no note; worktree lanes (root = `<main>/.worktrees/…`) do. Wording matches the intent ("landed in the MAIN project tree, not this worktree (this lane's file tools are worktree-rooted and cannot read that path)"). The new test `lane_agent_write_names_the_main_tree` covers lane / no-root / same-root controls. Store-root derivation verified against the factory construction (see LOW 3).

**(b) Factory threading — PASS.** `register_memory_tools` gains `root: Option<&AgentRootSpec>`; the sole call site is `build_registry` (`factory.rs:846`, confirmed via the code graph — no other callers), where `root` is already a parameter (`:820`) and used by sibling registrations. `root.map(|r| r.project_root.clone())` types cleanly (`Option<&AgentRootSpec>` → `Option<PathBuf>`). `AgentRootSpec.project_root` is the worktree git root (`:112`) — the right field (the sandbox root and project root are the same worktree directory).

**(c) Gist format contract — PASS except HIGH 1.** Control-char flattening is preserved as a 1:1 map before count/take (char count unchanged); hit cap `RECALL_BLOCK_HITS=5` unchanged; gist cap 300 with `...` appended only when cut (a cut line is 303 chars — the existing "no 301 consecutive x's" assertion still holds, and the new ellipsis assertion covers the marker). `data.get("rel_path").and_then(as_str)` is safe on absent/Null/non-string data (three-field fallback). Doc comments on the const and function updated to match.

**(d) Other callers/tests — PASS, no breakage.** `format_recalled_context` is private with exactly two callers (`recalled_context_block` :372 and the test); signature unchanged. `MemoryWriteTool::new` (≈20 call sites in runtime/agent.rs tests, agent/tests.rs, integration tests) and `with_knowledge` (factory + mod.rs test helper) are unchanged — `with_agent_root` is an additive builder defaulting to `None`, so all existing constructions keep today's behavior. `register_memory_tools` has the single caller.

**Security — PASS.** Message-text-only changes; the emitted path comes from the store's own row data (same trust domain as the titles/gists already echoed verbatim into the block); no new approval surface, no injection path beyond what exists.

**Multi-platform neutrality — PASS.** No Windows-only APIs, paths, or shell syntax; the test uses `tempdir()` + `Path::join`; the message text is platform-neutral. Path-comparison semantics across platforms covered in LOW 3 (correct for the actual same-source inputs on both macOS and Windows).

**Tests:** spawn prompt reports mnemo lib 2147 passed / 0 failed and src-tauri 291+4+2 passed / 0 failed, including the new lane test and the extended format test. Not re-run by this reviewer (read-only); note the green suite does not catch HIGH 1 because the test seeds a value shape the store never produces.
