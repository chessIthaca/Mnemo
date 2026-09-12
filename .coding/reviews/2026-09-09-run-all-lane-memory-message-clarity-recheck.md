## Verdict: PASS

Re-review of the findings-fix commit 3774562 on `wt/agenticcoding` (plan b587da79, backlog 0bba3241, "Run-all lane memory-message clarity"), verifying that all three findings from `.coding/reviews/2026-09-09-run-all-lane-memory-message-clarity-review.md` (1 high, 2 low) are resolved and that nothing new broke.

**Summary:** All three findings are fixed exactly as prescribed. The HIGH 1 fix normalizes the stored knowledge-dir-relative `rel_path` to the project-relative form, and the test now seeds the real stored shape — it fails without the fix, closing the test-masking gap. Both low fixes (README clause, root-comparison doc comment) landed as suggested. The rest of the delta is unchanged from the previously-passed review. No new findings.

| # | Prior finding | Resolution (verified in commit 3774562) | Status |
|---|---------------|------------------------------------------|--------|
| 1 | HIGH — `format_recalled_context` emitted `data.rel_path` verbatim (knowledge-dir-relative, unresolvable from the project root); the test fabricated a prefixed value | `src-tauri/src/ipc/run_all.rs:354-365` normalizes: values already starting with `.coding/` pass through, anything else is prefixed `.coding/knowledge/{p}`; the test (`run_all.rs:1079-1091`) seeds the real stored shape `"rel_path": "spec/2026-09-08-login.md"` and asserts the emitted line contains ` — .coding/knowledge/spec/2026-09-08-login.md`; doc comments on `RECALL_BLOCK_GIST_CHARS` (:311-314) and `format_recalled_context` (:317-324) now say "normalized project-relative" | RESOLVED |
| 2 | LOW — README.md:79 recalled-context description stale (no `...` cut marker, no path fourth field) | The clause now reads "each a tier-prefixed title + gist (ending `...` when cut; knowledge rows append their file path) under a verify-against-the-code caveat" — exactly the suggested text | RESOLVED |
| 3 | LOW — lane-note root comparison semantics undocumented | `src/tool/memory/mod.rs:187-195` comment now documents exact component equality and the same-source-spelling assumption ("verbatim-prefixed or 8.3 short-name spellings would compare unequal — unreachable here") | RESOLVED |

## HIGH 1 — verification detail

- **Normalization direction is correct.** Re-confirmed the store premise independently: the only writer of `rel_path` into row data is `src/memory/indexer/knowledge.rs:398-402` (`"rel_path": record.rel_path`), and `src/memory/indexer/tests/knowledge.rs:63` asserts the unprefixed form (`decision/2026-08-23-typed-records.md`). Real rows are knowledge-dir-relative, so the else-branch prefix (`.coding/knowledge/{p}`) is the production path; the `starts_with(".coding/")` pass-through is defensive only.
- **The test now guards the real contract.** Seeding `"spec/2026-09-08-login.md"` without the normalization emits ` — spec/2026-09-08-login.md`, which does not contain the asserted ` — .coding/knowledge/spec/2026-09-08-login.md` — the test fails without the fix. The prior report's core complaint (test asserts a shape the store never produces) is gone.
- **Edge cases checked:** absent/Null/non-string `data` still falls back to the three-field shape (`and_then(as_str)`); an empty `rel_path` would emit `.coding/knowledge/` but is unreachable (the indexer only writes paths produced by directory walks); a hypothetical `.coding/plans/...` value would pass through unchanged — only knowledge rows carry `rel_path`, so this is defensive, not a defect.
- **Ellipsis logic re-verified:** the flatten stays a 1:1 `map` before count/take (char count unchanged); `truncated = count > 300`; exactly-300 is unmarked, 301+ gets 300 chars + `...`. The existing no-301-consecutive-x's assertion still holds alongside the new ellipsis assertion, which is backed by the seeded 350-x hit (`run_all.rs:1055`). Hit cap `RECALL_BLOCK_HITS=5` unchanged.

## Rest of the delta (sanity check — nothing new broke)

- **src/tool/memory/mod.rs:** `agent_root` field + `with_agent_root` builder are additive — both constructors default to `None`, so all existing call sites keep today's behavior. The lane note lives only in the knowledge-backed success branch (the plain-DB arm reports no path and cannot fire it). Store-root derivation `dir().parent().and_then(Path::parent)` verified against the test helper layout (`mod.rs:1132`: knowledge rooted at `<dir>/.coding/knowledge` → store_root = `<dir>`), so the same-root control (`dir.path()`) compares equal and the lane control (`<dir>/.worktrees/runall-x`) differs. The test covers lane / no-root / same-root.
- **src/agent/factory.rs:** `register_memory_tools` gains `root: Option<&AgentRootSpec>`; its sole call site (`build_registry`, factory.rs:846) is updated in the same commit; `root.map(|r| r.project_root.clone())` types cleanly (`Option<&AgentRootSpec>` → `Option<PathBuf>`).
- **Bookkeeping files** (`.coding/backlog.jsonl` status flip, the plan file, the prior review report) — expected, not code.

## Documentation sync — PASS

README.md:79 updated (fix 2). PLAN.md:83 ("gist under a verify-against-the-code caveat") and :227 ("capped at 5 one-line gist hits") re-checked — still accurate as summaries; the README carries the added detail. No other README/PLAN text quotes the hit-line shape or the `memory_write` result message, so no further doc sync is needed.

## Multi-platform neutrality — PASS

- `p.starts_with(".coding/")` is a string prefix check, not a `Path` operation — identical behavior on Windows and macOS.
- The emitted path uses forward slashes, matching the store's own reporting convention (`mod.rs:184/212` emits `.coding/knowledge/{rel}` the same way); file tools accept forward slashes on Windows.
- The `Path` comparison (`Some(agent_root) != store_root`) is component-wise; both sides derive from the same project-root source (factory: `plans_dir.parent()` + `KNOWLEDGE_DIR_NAME`; test: the same tempdir path), so equality holds on both platforms — and the assumption is now documented (fix 3).
- No Windows-only APIs, paths, or shell syntax anywhere in the delta.

## Security — PASS

The normalization only adds a fixed prefix to a store-sourced string or passes through a `.coding/`-prefixed value — the same trust domain as the titles/gists already echoed verbatim into the block. No new approval surface, no injection path beyond what exists.

## Tests

Spawn prompt reports both suites green after the fixes: mnemo lib 2147+16 passed / 0 failed; src-tauri 291+4+2 passed / 0 failed. This reviewer is read-only (no shell tool) and did not re-run them. Statically, the extended format test now exercises the real data shape and fails without the normalization — the specific gap the prior report identified in the green suite is closed.
