## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes (git diff HEAD + untracked `.coding/plans/50f36b1e.md`) for plan 50f36b1e — central `drop_stringified_nulls` defense + nullable victim schemas (backlog 9118714a).

**Summary:** The fix is correct and well-targeted. The defense's traversal mirrors `normalize_in_place` faithfully, the dispatch reorder is behavior-identical on the unknown-tool path, the note-only backlog_status path is side-effect-safe with the transition guard intact, the nullable schema edits are exactly the form `normalize_for_strict` would produce (idempotence preserved via `is_nullable`), and no remaining code path makes correctness decisions on raw `tc.arguments`. Two low findings: a stale error message, and a documented-but-overstated trade-off on legitimate literal `"null"` anchors in file_edit.

---

## Findings

### LOW-1: backlog_status "at least one of" error message omits `note`

`src/tool/workflow/backlog.rs:367-372` — the validation condition was correctly extended to `args.note.is_none()`, but the error text still reads:

> "either 'status' (a status transition) or 'deferred' (set/clear the skip-Run-All flag) is required"

The schema description correctly says "At least one of status, deferred, or note is required", but a model that hits this error is misdirected away from the note-only shape this very plan introduces. One-line fix: include `note` in the message.

### LOW-2: legitimate literal `"null"` anchors are silently dropped — the doc claim is overstated for file_edit

The plan's CHECK (a) asks exactly this; the answer is **yes, it can happen**, and the concrete case is `file_edit`:

- `file_edit`'s top-level `required` is only `["path"]` (`src/tool/agent/file_edit.rs:1551`), so `old_string`/`new_string` are schema-optional.
- A perfectly legitimate call — `file_edit(path="x.js", old_string="null", new_string="nullptr", replace_all=true)` (a JS→C++/Ruby port editing the literal text `null`) — has its `old_string` dropped by the defense → defaults to `""` → the tool errors `"old_string is empty — provide old_string (string matching) or …"` (`file_edit.rs:481-483`).
- A retry re-emits `old_string:"null"` → dropped again → repeat-failure circuit-breaker territory. The model has no signal that the anchor was deliberately removed.
- The doc comment's claim "No tool parameter legitimately wants the literal string `"null"`" is **false for file-edit anchors** — file content can legitimately be exactly `null`. The `spawn_agent` model precedent (no model is named "null") does not extend to file anchors.

**Why LOW, not HIGH:** the case is narrow (the anchor must be *exactly* the 4-char string `null` — any longer anchor containing it is untouched), the failure is a clean error with **no data corruption** (contrast the killed class: title literally set to "null"), and two workarounds exist: batch mode with a single item (per-item `old_string` is REQUIRED inside the item schema, so the defense never touches it — the composition test proves this path) or `use_regex` (e.g. `nul[l]`). The trade-off is defensible: not dropping reintroduces the observed HIGH-severity corruption class. Same class, lower stakes: `memory_search(query="null")` would silently become a browse — negligible.

**Suggested cheap hardening (optional):** extend the `"old_string is empty"` error with a hint — "if you meant to edit the literal text 'null', use batch mode `edits:[{old_string:"null",…}]` or `use_regex`" — converting the silent trap into a self-correcting one. Also soften the doc-comment claim to name file_edit anchors as the accepted exception.

---

## Verification detail (checks a–g)

**(a) Defense traversal + optionality keying — CORRECT.**
- Union branches: each `anyOf`/`oneOf`/`allOf` branch recurses with the same args; a bare-string branch (`{"type":"string"}`) has no `properties` → early return, and the object branch against a string arg fails `as_object_mut` → so a plain-string step `"null"` (e.g. `create_plan` steps) **survives** — verified by reading the code paths, and pinned by the union test.
- Tuple `items` (draft-04) zips schemas↔args; single `items` maps — mirrors `normalize_in_place` (`strict.rs:94-104`) exactly.
- Nested properties recurse via `prop_schema`; drops are collected and removed after iteration (borrow-safe; dropped values are strings, so no recursion is lost).
- Optionality is keyed per schema level against that level's `required`; exact case-sensitive `"null"` match only; JSON `null` untouched; unknown properties untouched — all pinned by the 5 unit tests.
- Theoretical note (not a finding): a property required in one union branch but optional in another gets dropped (the branch-optional wins). No schema in the repo has shared property names across branches (the oneOf branches are disjoint by type), so this is documented-behavior-only.
- The defense correctly uses `tool.schema().parameters` — the RAW schema, not the strict-normalized one — so the `required` list is the tool's true contract, not strict mode's all-required rewrite. Under strict providers the model emits JSON `null` (untouched by the defense, handled by `Option`/`null_to_default`); the stringified form is the non-strict transport bug. Both paths covered.

**(b) Dispatch reorder — NO behavior change.** The old code built `ParsedToolCall` then looked up the tool; the unknown-tool error path returns before `parsed_call` is ever used (it names only `tc.name` + `available_tool_names()`), and `args` was dropped unused on that path in both versions. Identical error, identical ordering.

**(c) Note-only backlog_status — SIDE-EFFECT-SAFE.** Order: arg deserialization → three-way validation error → item-existence error → transition guard (illegal transition errors BEFORE `set_deferred`/`set_note`, no mutation) → `set_deferred` → note-only `set_note`. All under one store-lock acquisition (no TOCTOU). `BacklogStore::set_note` (`src/backlog.rs:684-692`) reloads from disk, replaces the note, persists, and no-ops (returns false, no persist) for unknown ids — and the tool verified existence under the same lock. `status`+`note` → note lands via `transition`; `deferred`+`note` → note now lands via `set_note` (previously silently dropped — correctly fixed); note-only → `set_note`. No way to *clear* a note (pre-existing, not a regression). See LOW-1 for the stale message.

**(d) Nullability vs strict normalization — NO BREAKAGE.** `is_nullable` (`strict.rs:198-216`) treats a `type` array containing `"null"` as already-nullable → the hand-written `["string","null"]` fields are skipped by widening, byte-stable. The enum form (`{type:["string","null"], enum:[…]}`) is *exactly* what `normalize_for_strict` produces for an optional enum (pinned by `enum_survives_widening`), so `create_plan.kind` matches the machine-generated shape. `backlog_status` is not a STRICT_TOOLS member — its type array is standard draft-07 for non-strict endpoints. Deserializers accept JSON null end-to-end: `CreatePlanArgs.context`/`kind` use `null_to_default`; `bug`/`branch`/`base`, `UpdatePlanArgs.title/goal/context/regression_test`, and the backlog status/note fields are `Option<String>` — and strict-mode auto-fill already forced null acceptance on these before this change, so no new failure mode.

**(e) No remaining victim — CLEAN.** `dispatch.rs:192` is the only `from_str(&tc.arguments)` into execution; every downstream consumer (research_write_verdict, load_tools group gate, complete_step finality, ask_user parsing, reviewer_spawn_gate, symbol/file_edit redirects, `never_auto_for`, safety rules, approval preview, execute) reads `parsed_call.arguments` (sanitized). Remaining raw readers are non-correctness paths: `record_raw_tool_calls` (deliberate verbatim trace tap), bad-JSON parse checks, UI event raw args (display fidelity — intended), token counting, and `is_durable_tool`/`record_tool_event` (working-memory capture; `is_durable_tool` only resolves the git subcommand, and git's `subcommand` is required so it's never dropped — worst case a memory record classifies via the other field).

**(f) Constitution checks — PASS.** Multi-platform: pure Rust logic, no platform APIs, no paths/shell syntax. File-tools-first: no shell-based mutation in the diff; `.coding/backlog.jsonl` was updated via the sanctioned backlog bookkeeping (status flip + note). Documentation: the defense carries a thorough module doc; the backlog_status description documents the note-only shape; README/PLAN.md need nothing (internal robustness, no feature/config surface). Budget: ~8 chars per nullable field × ~12 fields ≈ 100 chars across the tools array — negligible; no ceiling raise needed or taken.

**(g) General correctness — PASS.** Tests mirror the seam exactly (`drop_stringified_nulls` + `execute` composition; the dispatch test pins the seam end-to-end through `execute_tool_call` with an echo tool, including the required-field-kept case). The backlog item's done-flip note is accurate. Note: I am read-only and could not run `cargo test` myself; the 2477-passed/warning-free claim is the parent's — my static review found the tests well-formed and covering the changed paths, including regression coverage for all four live incidents.
