# Review — Graph-vs-search tool-choice guidance (feat/graph-search-guidance)

Scope: ALL uncommitted changes per `git diff HEAD` — `src/agent/prompt.rs`,
`src/tool/agent/codegraph.rs`, plus bookkeeping (`.coding/backlog.json`,
`.coding/plans/stack.json`, untracked `.coding/plans/632a1d02-….md`).

## Correctness

**No findings.** Verified against the actual files:

- **Hint only on empty matches** (`src/tool/agent/codegraph.rs:135-151`):
  `matches.is_empty().then(|| format!(...))` produces `Option<String>`
  (`bool::then` takes `FnOnce` and evaluates eagerly — no lazy-closure borrow
  held across the `json!` below). The closure borrows `args.query`;
  `json!({"query": args.query, ...})` passes it by reference and moves only
  `matches` — no borrow/move conflict. `out["hint"] = json!(hint)` runs only
  inside `if let Some(hint)`, and `IndexMut` on a `Value::Object` inserts the
  key, so a **hit** serializes exactly `query`/`count`/`symbols` — no `hint`
  key, not even `null`. Shape is byte-identical to the pre-change hit payload.
- **No downstream breakage**: `GraphSearchTool` is registered only in
  `src/agent/factory.rs:700`; its output is consumed solely by the model. A
  `hint` key already exists in `graph_context`'s miss payload
  (codegraph.rs:216), so the pattern is consistent module-wide.
- **Prompt continuations** (`src/agent/prompt.rs:161-173`): every physical
  line of the two reworded bullets ends with ` \` except the last, and Rust's
  backslash-newline strips the newline + next line's leading whitespace, so
  each bullet is one logical run. Every test-asserted substring is contiguous
  in the resulting const value: "TRY THESE FIRST" (161), "for SYMBOL
  questions" (162), "string literals" (166), "FALLBACK when the graph"
  (170), "switch to graph_context" (172). The tests use `.contains()` on the
  const, so no per-physical-line requirement exists. The graph-before-search
  ordering assertion (find-index compare) is retained. Wording is coherent:
  "FIRST choice for non-symbol text … and the FALLBACK when the graph tools
  come up empty" is a deliberate, readable dual framing.
- **Schema** (codegraph.rs:104-109): scope clause appended to the description
  string only; parameters unchanged.

## Tests — do they pin the behavior?

**Yes.** `graph_search_unknown_name_returns_empty_not_error`
(codegraph.rs:444-446) `.expect()`s the hint and asserts it contains the
echoed query and `` `search` `` — removing or emptying the hint logic fails
it. `graph_search_hit_carries_no_hint` (codegraph.rs:450-458) asserts
`out.get("hint").is_none()` — correctly using `get()` (indexing a missing key
on `Value` yields `Null`, which the test comment itself explains); an
unconditional-hint regression fails it. The prompt test
(prompt.rs:723-762) fails if any of the new scoping substrings is dropped or
the graph/search ordering flips.

## Security

**No findings.** The model-supplied `query` is interpolated only into a
`String` that goes through `json!`/`serde_json::to_string_pretty` (proper
JSON escaping) inside `run_query` — it never reaches a shell, SQL, or path.
`view.resolve(&args.query)` is pre-existing behavior, unchanged.

## Constitution compliance

- **Doc comments**: no new public items introduced; modified code is a trait
  impl body + consts. No `#[allow]` added anywhere in the diff. Crate is
  `#![deny(warnings)]` and `cargo test` was green (1076 tests), which proves
  warning-free — no dead code introduced.
- **Minor (informational)**: `git diff` emits `warning: in the working copy
  of 'src/tool/agent/codegraph.rs', CRLF will be replaced by LF the next time
  Git touches it`. The edit left CRLF in the working copy of an LF file (the
  repo's LF/CRLF history is backlog #48 / `fix/line-endings`). Git will
  normalize the blob to LF at commit time, so the committed content stays
  consistent — but normalizing the working copy to LF before committing
  (trivial rewrite of the touched lines) avoids recurring churn warnings.
  Not blocking.

## Bookkeeping files (noted, not deep-reviewed)

- `.coding/backlog.json` — three new user backlog items (ids 50-52: config
  dialog placement, reasoning-section triangle when empty, per-model
  reasoning-effort defaults). Expected app state.
- `.coding/plans/stack.json` — stack swapped to the new plan id,
  `reviewed:false`. Expected.
- Untracked `.coding/plans/632a1d02-1ce2-48ae-9683-027ca042e3e8.md` — the
  plan file for this work.

## Verdict

Diff is clean: no correctness, bug, or security findings; one minor
informational note (CRLF in the working copy of codegraph.rs) and the
expected bookkeeping changes.
