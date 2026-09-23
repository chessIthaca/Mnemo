## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes for plan aff95a51 ("Tighten empty-call descriptors + wire the graph_search → graph_context pointer"). The migration is correct and well-tested; one low-severity substance note on the shell descriptor.

### What was verified

**1. Migration correctness — PASS (one low note, below).**
All 7 descriptors (graph_search, graph_context, graph_path, read_files, shell, git_read, memory_write) now lead with `tool_contract::contract(...)` and keep their own substance after it. Checked per tool:
- graph_search: "MANDATORY first step…", language list, 20-candidate cap, "string literals not indexed" — all retained.
- graph_context: 360° view, language list, "call immediately after graph_search" — retained; the either-or id/name semantics survive via `contract("`id` (or `name`)")` and `recovery_hint("`id` (or `name`)")`.
- graph_path: shortest-path purpose, edge kinds, id-or-name acceptance — retained.
- read_files: the "up to 10 {path, start_line?, max_lines?} specs" constraint moved into the substance sentence — not lost.
- git_read: all four op descriptions verbatim.
- memory_write: MANDATORY-immediately guidance, typed prefixes, pointer-first rule — retained.
- Error-path wiring unchanged: every site still routes through `read_files::invalid_args_error` with only the hint text swapped to `recovery_hint(field)`; the graph_context empty-args early-return keeps its comment and structure.
- A repo-wide search confirms the content-first clause ("If you catch yourself") now appears ONLY in `src/agent/prompt.rs` (the universal TOOL_CALL_DISCIPLINE block) and in negative test assertions — no straggler per-tool copies, and none of the six untouched memory tools gained one.

**2. The `out["next"]` pointer — PASS.**
`first_id` is cloned from `matches.first()` BEFORE the `json!` that moves `matches` (codegraph.rs:379), and `hint`/`count` are computed before the move too — no borrow/move issue. Shape: a string field `next` = `graph_context(id="<id>") → callers/callees/imports; graph_impact(id) → blast radius`, set only when `first_id` is Some, so a total miss carries no `next` (its `hint` already redirects to `search`). The embedded id is JSON-escaped by serde, so symbol ids containing quotes cannot break the payload. The new test `search_hits_carry_the_graph_context_pointer` is non-vacuous: the `indexed_graph()` fixture (codegraph.rs:713) writes `pub fn helper()` to `src/lib.rs`, so the asserted prefix `graph_context(id="src/lib.rs::helper::` matches the real id shape (`file::name::line`) and is robust to the line number.

**3. Test quality — PASS.**
The flipped pins assert the new shape positively (opener "Always pass …", "No zero-argument form", example present) AND negatively (no "If you catch yourself"), with the rationale commented at each site. The drift guard `every_migrated_description_leads_with_the_shared_contract` constructs all 7 migrated tools for real (real Sandbox, in-memory CodeGraph, in-memory MemoryStore) and asserts lead + no-clause per tool; `assert_eq!(tools.len(), 7)` matches reality — exactly 7 descriptors carried the block, and `graph_impact` correctly stays out (verified: its description never contained "Always pass"/the block). `tool_contract.rs`'s own unit tests pin the contract text, the multi-field rendering, the clause drop, and a ≤190-char compactness bound that blocks silent regrowth.

**4. Documentation sync — PASS.**
codegraph.rs's module doc now records the `next` pointer with the plan id and the under-chaining rationale. tool_contract.rs's module doc is accurate — "7 descriptors across 5 files" is correct (codegraph.rs, git_read_tool.rs, read_files.rs, shell.rs, memory/mod.rs), and the evidence narrative (description-led block still fumbled; the error-side hint is what recovered) matches the record. README.md/PLAN.md/docs contain no empty-call/tool-contract text that could go stale — no doc updates owed.

**5. Multi-platform neutrality — PASS.**
No Windows-only APIs, paths, or shell syntax in lib code. The shell descriptor's "PowerShell 5.1 on Windows, sh on Unix" is pre-existing dual-platform text. start.bat is a Windows launcher by nature (sanctioned category) and its new cargo-PATH pickup uses only `%USERPROFILE%` and `where` — nothing Unix-hostile was added to shared code.

**6. File-tools-first policy — PASS.**
The diff contains no shell-based file mutation; all source changes are file-tool edits. The untracked bookkeeping files (.coding/plans/aff95a51.md, the amended HOW record) are the sanctioned .coding/ paths.

**7. Security — PASS.**
.npmrc pins the public registry with a clear comment; no tokens or credentials anywhere in the diff. start.bat adds no unquoted-input risk (it only reads `%USERPROFILE%` and `%~1`). backlog.jsonl changes are state bookkeeping (two done items gaining `deleted_at`, one new pending item) — no sensitive content.

**Pre-existing changes (.npmrc, start.bat):** sound as assessed above — the registry pin fixes the ENOTFOUND failure mode it describes, and the cargo PATH pickup + friendly error is a strict improvement over the cryptic "cargo metadata … program not found" death.

### Findings

**L1 (low) — shell descriptor dropped a tool-specific hint along with the boilerplate.**
The old shell description carried "(if you just made a shell call, the next one needs its own command)" — a shell-specific hint aimed at exactly the observed failure pattern (a successful call followed by an empty one). The migration replaced the whole block with the generic contract, losing that sentence; every other tool's substance survived verbatim, this one did not. The generic "No zero-argument form" + the error-side recovery hint cover the rule, so this is guidance-quality, not correctness — but it is a deviation from "only the boilerplate sentence replaced". Suggested fix: restore the parenthetical into shell's substance sentence (e.g. after "Execute a shell command"), or record the deliberate drop in the plan context. Either resolves the finding.
