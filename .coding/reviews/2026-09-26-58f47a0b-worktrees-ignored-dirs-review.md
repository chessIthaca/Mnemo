## Verdict: FINDINGS (0 high, 1 low)

PASS on all six requested verification points: the fix is genuinely ONE const entry repairing exactly the four root-relative consumers of `is_ignored_component` (no second/parallel rule exists); all five regression pins are red-without-the-entry and exercise the changed path; the root-relative semantics and both guard pins hold; no over-reach (user dirs, other consumers, src-tauri landing); the constitution checks pass (warning-free per parent's exit-0 run under `deny(warnings)`, platform-neutral, file-tools-first, no doc enumerates IGNORED_DIRS — the const doc comment is the documentation and it is accurate); and the knowledge amendment is technically accurate on every verifiable claim. One LOW bookkeeping finding: backlog item 39b3bbfb is still `[pending]` in the committed `.coding/backlog.jsonl` and the working tree carries **no** backlog.jsonl modification — the landing commit must carry the pending→done note or the queue re-dispatches an already-fixed item.

## Scope reviewed

ALL uncommitted changes on `wt/mnemo` per `git diff HEAD` (stat: 5 files, +106/−4) plus the untracked plan artifact:
- `src/tool/agent/search.rs` — the const entry (`.worktrees`, :76) + expanded const doc (:54-60); `skips_ignored_dirs` fixture/assert (:2042-2063); `pruned_walk_matches_the_index_engine_and_never_searches_ignored_dirs` fixture + `skipped 3` → `skipped 4` re-pin (:2668-2689); NEW guard `worktree_root_stays_searchable` (:2068-2090).
- `src/tool/agent/search_read.rs` — `skips_ignored_dirs` fixture + assert (:775-789).
- `src/codegraph/walk.rs` — shared `fixture()` gains `.worktrees/runall-abcd12/src/dup.ts` (:231-239); the two exact-list asserts (`walks_source_files_sorted_and_skips_ignored` :263-267, `walks_every_searchable_file_for_the_content_index` :287-299) stay unchanged and exclude dup.ts.
- `src/codegraph/watcher.rs` — `is_indexable_path_accepts_any_file_and_rejects_ignored` gains the worktree-DB + worktree-source rejections (:293-316) and a root-relative guard.
- `.coding/knowledge/bug/2027-01-11-content-index-staleness-root-causes-coding-never.md` (:17) + untracked `.coding/plans/58f47a0b.md`.

I cannot run `cargo test` (no shell in my allow-list). The parent's evidence — post-fix 2718 passed / 0 failed / 5 ignored, +19 integration, +1 doc-test, exit 0; pre-fix all five pins RED + the guard green — is consistent with my static verification at every point, including the exact pre-fix failure shapes ("skipped 3 ignored dirs" with a dup.js match; "2 matches in 2 files"; dup.ts present in both walk lists; the worktree-DB watcher assertion failing).

## 1. One const entry, four consumers — VERIFIED ✓

The code graph shows **exactly four** callers of `is_ignored_component` (search.rs:80-82): `should_search` (search.rs:124), the pruned walk's `descend` (search.rs:323), `walk.rs::visit` (walk.rs:179 — shared by both `walk_project` and `walk_searchable`), and `watcher.rs::under_base` (watcher.rs:55, inside `is_indexable_path`, the sole gate for the debounced pass via `event_is_indexable`, watcher.rs:255-265). No fifth caller exists. A literal search for `IGNORED_DIRS` across the repo finds only the const definition (:61) and its use in `is_ignored_component` (:81) plus doc-comment references — **no second/parallel ignore rule is introduced**, and no consumer bypasses the const. The walk.rs module doc ("Reuses the search tool's ignore rules ([`IGNORED_DIRS`] + [`should_search`])") stays accurate by reference.

## 2. Regression pins genuinely red-without-the-entry — VERIFIED ✓

- `search.rs::skips_ignored_dirs` (:2025): needle planted at `.worktrees/runall-abcd12/src/dup.rs`, asserts `!contains(".worktrees")` (:2063). Pre-fix the walk engine finds dup.rs → RED (parent's run: '.worktrees' present in results).
- `pruned_walk…` (:2654): dup.js needle + `skipped 4 ignored dirs` (:2686). Pre-fix `.worktrees` is descended, not counted → "skipped 3" + a dup.js match → RED (parent's pre-fix output shows exactly that shape).
- `walk.rs`: dup.ts is a `.ts` source that passes both `is_source_file` and `should_search` pre-fix, so both `assert_eq!` exact lists gain it pre-fix → RED, with the expected lists UNCHANGED (the minimal pin).
- `watcher.rs` (:309-316): pre-fix `under_base`'s component walk passes (`.worktrees` not ignored) and `is_coding_data_file` matches only a FIRST-component `.coding` (rel = `.worktrees/…`), so both paths are indexable → both `!` assertions RED (parent's run: the worktree-DB assertion failed).
- `search_read.rs::skips_ignored_dirs` (:768-790): delegates to the same engine → RED pre-fix the same way.

The new `worktree_root_stays_searchable` (:2076) and the watcher inline guard are explicitly documented as NOT regression pins (green before and after — :2068-2074) and are correctly NOT counted as such.

## 3. Root-relative semantics — VERIFIED ✓

All four consumers either strip the root first (`should_search` :124-131; `under_base` via `strip_prefix(base)`) or start AT the root and only test entries beneath it (`descend` :323-353, `visit` walk.rs:179-200). The predicate is never applied to an absolute path, so the new entry cannot match the absolute prefix components of a root that IS `<base>/.worktrees/runall-<id>` — exactly what the two guard pins assert (search side: a hit at `src/own.rs` when the tool root IS the worktree, :2078-2089; watcher side: `is_indexable_path(wt_root/src/own.tsx, &wt_root)` true). The walk side has no dedicated guard test, but `visit` structurally never tests the root itself, and production proves it: run-all worktree agents build their own graphs from their own roots (spawn.rs:139-145). The FSEvents `canonicalize` fallback in `is_indexable_path` retries with the canonical root — still root-relative.

## 4. Over-reach — none found ✓

- **A user's own real `.worktrees` dir**: same policy tier as `.claude`/`.idea`/`.vscode` (hidden bookkeeping dirs excluded by name); `.worktrees` is app-managed per agent.md and gitignored (.gitignore:64-67). The cost is index/search/watch coverage only — `read_files`/`shell` can still read worktree paths directly, so no capability is lost. Documented in the const doc (:54-60), which is accurate.
- **Other consumers of IGNORED_DIRS**: none exist outside `is_ignored_component` (literal search); referencing docs/comments stay accurate by reference.
- **src-tauri run-all landing path**: git/fs-level throughout (`mnemo::project::worktrees`, run_all.rs:4485) — never via search/walk/index tools; the spawn-time seeding (`snapshot_db` VACUUM INTO, spawn.rs:174-177, dest `wt/.coding/codegraph.db`) is direct SQLite, unaffected.
- **Pre-existing `.worktrees/**` rows in live DBs self-heal**: `index_inner` builds the `present` set from `walk_searchable` and `prune_missing(&present)` (mod.rs:540, store.rs:413-427) removes every row outside it on the next full pass. Transition-window nano-note (pre-existing behavior, not introduced by this diff, zero urgency): `reindex_stale_files` (mod.rs:587+) has no `should_search` filter, so an inline repair of a stale pre-fix worktree row could transiently refresh it until the first full pass prunes it.
## 5. Constitution — VERIFIED ✓

- **Warning-free build**: no imports, no dead code, no unused `mut` added — the diff is const entries, doc comments, and test code that executes; the parent's unpiped exit-0 run under `#![deny(warnings)]` at both crate roots proves zero warnings (I cannot run cargo — no shell in my allow-list).
- **macOS/Windows neutrality**: portable std APIs only (`Path::join`, `create_dir_all`, `strip_prefix`, `components`, `to_string_lossy`); forward-slash fixture paths are the established idiom (byte-identical to the pre-existing fixtures); no `cfg` gates, no Windows-only assumptions.
- **File-tools-first**: no shell-based file mutation anywhere in the diff; test `std::fs` writes are the sanctioned test idiom.
- **Docs sync — parent's claim confirmed**: no doc enumerates IGNORED_DIRS. README.md's worktree mention is a feature blurb; PLAN.md:242 is generic pruned-walk prose; docs/FEATURES.md:40's invariant claim ("trigger set matches the content index's coverage") remains true post-fix because BOTH sides now exclude `.worktrees`, and its "only the app's own SQLite stores excluded" qualifier scopes to the `.coding/` family and stays correct. The const's expanded doc comment (search.rs:54-60) IS the documentation and is accurate. No SPEC needed — a one-line boundary change inside the documented architecture; the amended BUG knowledge record carries the invariant.

## 6. Knowledge amendment accuracy — VERIFIED ✓ (one inherited date quirk, not a finding)

- `snapshot_db` claim verified: `spawn_run_all_agent` at spawn.rs:146, dest `wt.join(".coding").join("codegraph.db")` at :174, the `snapshot_db` (VACUUM INTO) call at :177 — the line range and "dest is the worktree's .coding" are exact.
- Four-consumer list, "sole gate for the debounced pass", root-relative claims, and "churn, not a loop" — all verified (§1, §3; the loop argument matches the shipped 2a2ae0aa analysis: a main-tree pass writes only the DATA_FILES family under the main `.coding/`, which the watcher rejects).
- Date note: "(found 2027-01-25 as reviewer LOW-1 of plan 2a2ae0aa)" inherits verbatim from the **committed** backlog item 39b3bbfb ("FOUND 2027-01-25 during the plan-2a2ae0aa review", .coding/backlog.jsonl:200); it conflicts with the LOW-1 review file's own name-date (2026-09-25-…) — a pre-existing repo-wide clock inconsistency (that same review already references the 2027-01-11 knowledge file), not introduced or fixable by this diff. The amendment is consistent with the recorded provenance.
- "commit on wt/mnemo" is an anticipatory branch citation — accurate once the landing commit lands, matching the blessed "cites the branch without claiming main-merged" convention from the 2a2ae0aa round-1 review.

## Bug-plan checks

- **Regression test exercises the changed path**: YES — five pins spanning all four consumers' modules (search, search_read, both walkers, watcher), each failing for the right reason pre-fix (parent's red run + my static analysis of every assert).
- **Root cause documented**: YES — plan Context + Bug field (`.worktrees` absent from IGNORED_DIRS → duplicate index rows since forever; post-2a2ae0aa watcher churn from worktree DB writes; NOT a loop) + the amended knowledge record :17.
- **BUG memory**: not present in the memory store YET, by design — the plan's bookkeeping step says "The BUG: digest auto-captures at finish", and finish follows this review; the parent's searches during execution also show no premature record. No gap at this stage; the auto-captured digest will inherit the plan's accurate Bug field.
- **Test accounting**: 2717 → 2718 passed (+1 = the new guard fn `worktree_root_stays_searchable`) — consistent; the plan Context's "no new test fns" wording was superseded by the planned guard pin in Detailed steps.

## Findings

### LOW-1 — backlog item 39b3bbfb still `[pending]`; no uncommitted backlog.jsonl change exists (bookkeeping completeness)

`.coding/backlog.jsonl:200` — item 39b3bbfb sits `status:"pending"` (note: "interrupted by the user, returned to queue"), and `git status` shows backlog.jsonl **clean** (last commit touching it: 2890ad0). The review-scope note listed it among the "uncommitted" bookkeeping, but there is no modification to commit. The landing commit must carry the pending→done note — the 9201704f precedent from the 2a2ae0aa landing, whose round-2 review explicitly verified that note in the landing commit. Without it the queue keeps a done item pending and a future run-all sweep can re-dispatch an already-fixed item as a duplicate lane.

**Suggested fix**: mark item 39b3bbfb done before or with the landing commit (the backlog tools, note pointing at plan 58f47a0b + the landing commit), mirroring the 9201704f precedent.

**Everything else: PASS — the code change itself is complete, correct, and correctly pinned; no code finding.**