# Review: Steer agents to semantic tools (uncommitted changes on fix/compact-bugs)

Scope: uncommitted diff vs HEAD only (`git diff HEAD`), per task. Changed files:
`src/agent/prompt.rs`, `src/tool/memory/mod.rs`, `src/tool/agent/codegraph.rs`,
`src/tool/mod.rs`, `README.md` (plus `.coding/` bookkeeping state: backlog.json,
plans/stack.json — runtime bookkeeping, not reviewed as source).

## Verdict

Two **Low** findings; no Medium/High. The implementation is correct and the
constitution checks pass. Safe to fix the two lows and commit.

---

## Findings

### LOW 1 — Test gap: `schemas_semantic_tools_first_then_stable` never exercises 8 of the 9 class-0 names

`src/tool/mod.rs:975-1009` (test), `:436-449` (`priority_class`), `:524-541` (stub `MemoryTool`).

The stub `registry()` registers only **one** class-0 tool (`memory_recall`).
The test's two assertions are (a) the emitted Vec equals its own
`(priority_class, name)`-sorted clone and (b) the first `memory_*` tool precedes
the first class-1 tool. Both pass even if any of the other eight
`SEMANTIC_FIRST` entries were typo'd (e.g. `"graph_serach"`): the misspelled
entry would silently demote that production tool to class 1 and no assertion
here would catch it, because the stub registry contains no graph/search tools
and nothing pins the list's contents. The plan's step-3 text also said the
test would "assert the first 9 names are exactly the class-0 set in
alphabetical order" — that assertion was dropped, not adapted.

The production mapping itself is correct (I verified all 9 names against the
real registrations: `graph_search`/`graph_context`/`graph_impact`/`graph_path`
at codegraph.rs:106/189/269/348; `memory_write`/`memory_recall`/
`memory_consolidate` at memory/mod.rs:77/189/306; `search` at search.rs:280;
`search_read` at search_read.rs:80). This is a guard-strength finding, not a
shipped-code bug.

Suggested fix: add a direct unit assertion on `priority_class`, e.g. loop over
the 9 names asserting `== 0` and assert `priority_class("shell") == 1` (or
register stub graph/search tools and assert the full expected order).

### LOW 2 — Prompt-text accuracy: "The first three rules are MANDATORY habits" vs conditional memory_consolidate

`src/agent/prompt.rs:161-162` (intro) vs `:175-177` (memory_consolidate rule).

The intro sentence says "The first three rules are MANDATORY habits, not
optional suggestions", but the third rule's own text is explicitly
conditional/restrictive: "call this **only** for a long session you want
distilled now". memory_write and memory_recall are genuinely mandatory
triggers; memory_consolidate is a cost-bearing pipeline (runs an LLM
extraction) that the rule deliberately fences. An LLM reading the header
literally could over-call memory_consolidate. Trivial fix: "The first two
rules are MANDATORY habits", or keep "three" and carve out consolidation
("memory_consolidate stays conditional — use it only for long sessions").

---

## Verified correct (no findings)

- **Sort determinism (cache law).** `priority_class` (mod.rs:436-449) is a pure
  function of the tool name; registry keys are unique (HashMap);
  `sort_by_key(|s| (priority_class(&s.name), s.name.clone()))` (mod.rs:400) is
  therefore a total order, byte-stable across restarts. The code comment
  (mod.rs:391-399) correctly states the invariant is DETERMINISM, not
  alphabetical order — no misrepresentation. Note for the committer (not a
  finding): the reorder causes a one-time provider prefix-cache reset on the
  first request after deploy; that is inherent to any order change and no
  comment/test/README claims otherwise.
- **Downstream consumers.** `turn.rs:611-620` applies `retain` (order
  -preserving) to the schemas Vec; providers never re-sort the tools array
  (the only provider-side sort, stream.rs:85, orders stream-chunk indices).
- **prompt.rs line continuations.** TOOL_STRATEGY (prompt.rs:159-199): every
  continuation line ends with `\`; the deliberate real newlines (after
  "not optional suggestions:" :162 and between bullets :170/:174/:177/:186)
  produce the intended paragraph breaks. The const is pinned by two tests
  (:707-743, :745-785) and suites were reported green (1312/0).
- **No stale-wording assertions.** Repo-wide search for "TRY THESE FIRST",
  "proactively persist", "Prefer this over grep": hits are confined to
  historical `.coding/` plan/review/analysis documents (dated snapshots, not
  living documentation) plus one explanatory *comment* at prompt.rs:712 —
  not an assertion. All assertions were updated.
- **README sync.** The new bullet (README.md:46) accurately describes the
  triggers and the semantic-first deterministic order. PLAN.md contains no
  tool-ordering claims (searched), so nothing there goes stale. `.coding/
  analysis/system-prompt-review.md` still says "sorted alphabetically" but it
  is a dated point-in-time analysis, outside the constitution's documentation
  list (README/PLAN/module docs/endpoints.toml).
- **Doc comments.** `schemas()` keeps its doc comment (mod.rs:368-371);
  `priority_class` is private yet fully doc-commented with rationale
  (mod.rs:428-435). Changed schema-description strings are not doc-comment
  surface.
- **No `#[allow(...)]` added** anywhere in the diff; no platform-specific
  code, paths, or shell syntax (pure Rust sort + English prompt text) —
  multi-platform neutrality intact.
- **Security.** Change surface is prompt/description text plus a deterministic
  sort; nothing flaggable.
