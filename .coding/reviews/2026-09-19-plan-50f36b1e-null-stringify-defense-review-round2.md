## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of plan 50f36b1e (backlog 9118714a) — the FULL uncommitted changeset (`git diff HEAD` across 7 files + untracked `.coding/plans/50f36b1e.md` and the round-1 report).

**Summary:** Both round-1 LOWs are correctly landed — the backlog_status error now names `note` with the test asserting both the new prefix and the note mention, and the mod.rs doc names the file_edit anchor exception while the empty-old_string error carries the batch-mode/use_regex hint (whose batch-mode claim I verified against the `edits` item schema: per-item `old_string`/`new_string` are required, hence never dropped). The fixes introduce no new issues: no other assertion touches the old message texts, and the round-2 edits (doc comment + two error strings) don't touch advertised schemas, so the context-budget test is unaffected. Re-confirming the overall change set found one new LOW: the doc's accepted-exception paragraph claims "a self-correcting trap, not silent corruption," which is true for `old_string` anchors but not for the mirror case — a genuine `new_string` of exactly `"null"` is dropped and silently becomes a deletion, with no error to self-correct on.

---

## Findings

### LOW-1 (round 2): the doc's "self-correcting trap, not silent corruption" claim covers only the anchor side — a genuine `new_string` of exactly `"null"` is silently turned into a deletion

`src/tool/mod.rs:207-212` (the `drop_stringified_nulls` doc paragraph added by the round-2 fix) scopes the accepted exception to "`file_edit` anchors … The dropped anchor surfaces as the tool's empty-old_string error, which names the batch-mode and use_regex workarounds — a self-correcting trap, not silent corruption." That is accurate for `old_string`. But the exception the paragraph acknowledges ("file content can legitimately be exactly `null`") has a replacement side it doesn't name:

- `new_string` is optional in the advertised schema (`required: ["path"]` only — `src/tool/agent/file_edit.rs:1554`), so the defense drops a genuine `"null"` value for it.
- `FileEditArgs.new_string` is `#[serde(default)] String` (`file_edit.rs:58-62`); the dropped key becomes `""`, and "An empty `new_string` in single mode deletes `old_string`" (struct doc `:58-60`; the schema description at `:1545` says the same).
- Concrete case — the mirror of the doc's own JS→C++ example: a C++→JS port, `file_edit(path, old_string="nullptr", new_string="null")` → `new_string` dropped → the edit **deletes** `nullptr` instead of replacing it with `null`. No error fires (the empty-old_string guard doesn't apply; the no-op guard at `:1603-1609` doesn't either); the model-visible output is just `edited <path>` — the diff rides in `result.data` (`:1684`), which nothing in `src/agent` reads (it is UI-facing only).
- Round-1's severity rationale for LOW-2 ("the failure is a clean error with no data corruption") therefore does not extend to this manifestation: it is a wrong write with no error signal. The landed paragraph's "not silent corruption" framing overstates the exception's coverage in the same way round 1 flagged the original claim.

**Why LOW, not HIGH:** the trigger is narrow (the replacement must be EXACTLY the 4-char string `"null"` — any replacement with surrounding context, e.g. `return null;`, is untouched); the wrong write is deterministic and visible in the UI's rendered diff; the workarounds are trivial (batch mode — per-item `new_string` is REQUIRED, `file_edit.rs:1551`, so never dropped — or include surrounding context in `new_string`); and not-dropping would reintroduce the HIGH-severity corruption class round 1 killed (title literally set to "null"). Same class as round-1 LOW-2, same severity.

**Suggested fix (doc-only, one sentence):** extend the exception paragraph to name the replacement side — e.g. "The mirror case (`new_string` exactly `"null"`) is dropped too, and an empty `new_string` deletes the anchor — the returned diff shows a pure deletion; use batch mode (per-item `new_string` is required, never dropped) or include surrounding context." Optionally mirror the clause in the `new_string` schema description.

---

## Round-1 fix verification

**LOW-1 (stale error message) — LANDED.**
- `src/tool/workflow/backlog.rs:367-371`: the validation is `args.status.is_none() && args.deferred.is_none() && args.note.is_none()`, and the error reads exactly "either 'status' (a status transition), 'deferred' (set/clear the skip-Run-All flag), or 'note' (amend the item's note) is required" (line-continuation joins verified — single spaces, no stray whitespace).
- The `status_tool_requires_status_or_deferred` test (`backlog.rs:1232-1249`) asserts BOTH the new prefix (`.contains("either 'status' (a status transition), 'deferred'")`, `:1240`) and the note mention (`.contains("'note' (amend the item's note)")`, `:1244-1248`).
- Assertion sweep: the only other occurrence of the old text is the historical plan doc `.coding/plans/803d2be0.md` (prose, not an assertion). The schema description ("At least one of status, deferred, or note is required" + the "note alone … amends the item's note" sentence) is consistent with the error.

**LOW-2 (overstated doc claim + silent trap) — LANDED.**
- (a) `src/tool/mod.rs:207-212`: the exception paragraph reads verbatim as claimed — "One accepted exception (review LOW-2): `file_edit` anchors — file content can legitimately be exactly `null` (e.g. a JS→C++ port). The dropped anchor surfaces as the tool's empty-old_string error, which names the batch-mode and use_regex workarounds — a self-correcting trap, not silent corruption." (See LOW-1 above for the replacement-side gap in this paragraph.)
- (b) `src/tool/agent/file_edit.rs:481-489`: the empty-old_string error now carries the hint verbatim as claimed — "… If you meant the literal text 'null' (dropped by the null-stringify defense), use batch mode (an edits item's old_string is required, never dropped) or use_regex (e.g. nul[l])".

**Hint accuracy — VERIFIED.**
- "an edits item's old_string is required, never dropped": the `edits` item schema (`file_edit.rs:1551`) declares `"required": ["old_string", "new_string"]`; the defense keys drops on absence from that schema level's `required` list, so per-item anchors (and replacements) are never dropped. The composition test `batch_mode_with_stringified_null_old_new_applies_the_batch` proves the path end-to-end (top-level "null" strings dropped, the batch applies, file content correct).
- "use_regex (e.g. nul[l])": `nul[l]` ≠ `"null"`, so the defense keeps it; with `use_regex=true` it compiles as a regex matching exactly the text `null`. Accurate.

**No new issues from the fixes.**
- Assertion sweep: the only in-code assertion on the file_edit error is `file_edit.rs:2606` (`contains("old_string is empty")`) — the unchanged prefix; the extended tail breaks nothing. No test anywhere asserts the old full texts of either extended message.
- Budget: the round-2 edits touch a Rust doc comment and two error strings — not advertised schemas — so `tools_array_stays_within_context_budget` is unaffected. The backlog_status description's "note alone" sentence was already in round 1's verified set.

---

## Overall change set — re-confirmed (round-1 checks a–g still hold)

- **Dispatch seam** (`src/agent/dispatch.rs`): tool lookup moved before `ParsedToolCall` construction; `drop_stringified_nulls(&tool.schema().parameters, &mut args)` runs before the parsed call is built, so approval, safety rules, previews, steering, and the tool itself all see clean args; the raw `tc.arguments` string is untouched (UI fidelity). The unknown-tool error path is unchanged (names only `tc.name` + available tools; `args` unused there in both versions).
- **Defense** (`src/tool/mod.rs:223-289`): the recursive walk mirrors `normalize_in_place` — `anyOf`/`oneOf`/`allOf` branches, `items` (single + draft-04 tuple), nested `properties`; drops only the exact string `"null"` on properties absent from that schema level's `required`; JSON null, real values, required fields, and unknown properties are untouched. Keyed on the RAW advertised schema (`tool.schema().parameters`), so `required` is the tool's true contract, not strict mode's rewrite. Five unit tests pin each behavior (optional dropped / required kept / nested array items / union branches / unknown untouched).
- **Nullable advertised schemas** — all `["string","null"]`, each pinned by a schema-assertion test: `backlog_status.status`/`note`, `update_plan.title`/`goal`/`context`/`regression_test`, `create_plan.bug`/`branch`/`base`/`context`/`kind`, `file_edit.old_string`/`new_string`. The enum form matches what `normalize_for_strict` produces, and `is_nullable` treats type arrays as already-nullable, so strict widening is a no-op (idempotence preserved).
- **Note-only backlog_status**: order is arg deserialization → three-way validation → item existence → transition guard (illegal transitions error BEFORE any mutation) → `set_deferred` → note-only `set_note`, all under one store-lock acquisition. `status`+`note` lands via `transition`; `deferred`+`note` now lands via `set_note` (previously silently dropped); note-only lands via `set_note`. The new test `status_tool_stringified_null_status_updates_note_only` composes defense + tool exactly as the seam does and asserts success, untouched status, and the persisted note.
- **Tests**: 13 new (5 mod.rs unit + 1 dispatch-seam `dispatch_drops_stringified_nulls_from_optional_params` + 2 file_edit + 2 backlog + 3 plan) plus the extended `status_tool_requires_status_or_deferred` assertions. The dispatch test pins the SEAM end-to-end through `execute_tool_call` with a purpose-built echo tool (optional "null" dropped, required "null" kept, real value passes).
- **Bookkeeping**: `.coding/backlog.jsonl` item 9118714a flipped to done with an accurate note (central defense, nullable victims, and the deliberate scope decision deferring the full ~50-field advertised-nullability sweep — the central defense covers every tool's optional fields for the stringified form). The untracked plan file `.coding/plans/50f36b1e.md` matches the landed implementation (5/5 steps, context accurate).

## Constitution checks — PASS

- **Multi-platform neutrality**: pure Rust logic and string literals; no platform APIs, paths, or shell syntax anywhere in the diff.
- **File-tools-first**: no shell-based file mutation in the diff; the backlog flip went through the sanctioned bookkeeping path.
- **Documentation**: the defense carries a thorough module doc (now including the round-2 exception paragraph); the backlog_status description documents the note-only shape; README/PLAN.md need nothing (internal robustness, no feature/config surface). The one doc gap is LOW-1 above.
- **Test run**: I am read-only and could not run `cargo test` myself; the 2477-passed / 0-failed / 5-ignored / warning-free claim is the parent's. My static review found all new and extended tests well-formed, covering the changed paths, with the composition tests mirroring the dispatch seam exactly.
