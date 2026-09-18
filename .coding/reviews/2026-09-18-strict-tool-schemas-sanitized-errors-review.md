## Verdict: FINDINGS (1 high, 2 low)

Review of all uncommitted changes on `wt/mnemo` for plan 21118961 "Strict tool schemas + sanitized tool errors for OpenAI-compatible providers" (39 modified + 4 untracked files: `src/provider/strict.rs`, `src/tool/error_message.rs`, spec note, review).

**Summary:** The architecture is sound and well-tested (single decision point in `ToolRegistry::schemas`, defensive strip in both OpenAI builders, correct enum+nullable form verified against OpenAI's own documented strict-mode example, no raw-arguments echo, budget ceilings measured under strict caps). One systemic HIGH defect: the nullable widening advertises `null` as the legal "absent" value for optional properties, but four tools' arg structs use non-Option `String`/`bool`/`PlanKind` fields whose `#[serde(default)]` only fires on *absence* — strict mode forces the key present, the model emits `null`, and the tool rejects the call. Two LOW findings in the sanitizer's fallback/parser edges.


---

## Scope reviewed

- `git diff HEAD` (39 tracked files) + untracked `src/provider/strict.rs` (519 lines), `src/tool/error_message.rs` (372 lines), spec note, this review's directory.
- Read in full: strict.rs, error_message.rs, the nine STRICT_TOOLS' `schema()` methods and their args structs, the turn.rs schema-build + correction sites, factory.rs budget test, dispatch.rs site, endpoints.rs/openai request builders (via diff), `try_429_fallback`.
- External verification: OpenAI function-calling + structured-outputs docs (developers.openai.com, fetched 2027-01-24) for the strict-mode schema rules; DeepSeek docs unreachable (fetch failed — noted, not blocking).

---

## HIGH 1 — Strict-mode nullable widening is not matched by the tools' deserializers: non-Option fields reject the `null` that strict mode forces

**Files:** `src/provider/strict.rs` (normalization) vs `src/tool/agent/file_edit.rs:43-109`, `src/tool/workflow/plan.rs:75-110, 127-140, 436-463`.

**The mechanism.** `normalize_in_place` (strict.rs) widens every property not in the raw `required` list to nullable (`type: ["string", "null"]` compact form, `anyOf` fallback) and inserts **all** property names into `required`. That is exactly OpenAI's documented strict-mode contract — their function-calling guide states: *"All fields in `properties` must be marked as `required`. You can denote optional fields by adding `null` as a `type` option."* Under constrained decoding the model **must** emit every key, and `null` is the sanctioned representation of "no value". The schema is strict-legal (verified — see "Verified clean" below); the problem is on the receiving side.

`#[serde(default)]` on a non-Option field only fires when the key is **absent**. When the key is present with `null`, serde fails with `invalid type: null, expected a string` (or `a boolean` / `enum PlanKind`). Strict mode makes absence impossible, so every "optional" non-Option field becomes a latent hard failure whenever the model picks `null` over a placeholder value — and `null` is precisely what the type array invites.

**Affected fields (all optional in their raw schemas, all widened to nullable + required):**

| Tool | Field | Type | Location |
|---|---|---|---|
| `file_edit` | `old_string` | `String` | file_edit.rs:48-49 |
| `file_edit` | `new_string` | `String` | file_edit.rs:53-54 |
| `file_edit` | `replace_all` | `bool` | file_edit.rs:55-56 |
| `file_edit` | `use_regex` | `bool` | file_edit.rs:60-61 |
| `file_edit` | `fuzzy_whitespace` | `bool` | file_edit.rs:85-86 |
| `file_edit` | `append` | `bool` | file_edit.rs:107-108 |
| `create_plan` | `context` | `String` | plan.rs:78-79 |
| `create_plan` | `kind` | `PlanKind` | plan.rs (CreatePlanArgs, `#[serde(default)] kind: PlanKind`) |
| `update_plan` | `append` | `bool` | plan.rs (UpdatePlanArgs, `#[serde(default)] append: bool`) |
| `create_plan`/`update_plan` `steps[]` | `body` (object branch of the `oneOf`) | `String` | plan.rs:127-140 (`StepInput::Map { header: String, #[serde(default)] body: String }`) |

`file_edit`'s schema declares only `"required": ["path"]` (file_edit.rs:1276-1306), so **all nine** other properties are widened. `update_plan`'s schema has **no** `required` key at all (plan.rs:922-981), so every property including `append: bool` is widened. `create_plan`'s `required` is `["title", "goal", "steps"]` (plan.rs:561), leaving `context`, `kind`, `bug`, `branch`, `base` widened (the latter three are `Option` — fine).

**Concrete failure paths (these are the tools' advertised primary modes):**
- Line-range `file_edit` (`start_line`+`end_line` — the schema description says "no `old_string` is needed"): the model must now emit `old_string`; `null` → `invalid type: null, expected a string` → call fails.
- Batch `file_edit` (`edits` — "leave `old_string`/`new_string` empty"): same, twice over.
- `create_plan` with a header-only step: the `oneOf` object branch forces `body` present; `body: null` fails **both** untagged variants → `data did not match any variant of untagged enum StepInput` → the sanitizer's *fallback* branch (error_message.rs:131-138), which names no parameter — the model gets no actionable hint and may loop (the exact failure mode this plan set out to eliminate).
- `update_plan` without append intent: `append: null` → `invalid type: null, expected a boolean`.

**Recoverability:** for top-level fields the sanitizer does name the parameter ("parameter 'old_string' must be a string, not a null" — the `invalid type` branch handles `null` actuals via `actual_word`, error_message.rs:199), so a retry with `""`/`false` recovers. But every such call burns a turn, and the `StepInput` case gives no parameter guidance. On DeepSeek (the plan's primary target, a weaker model more likely to emit `null` when the type array offers it) this is a meaningful reliability regression — the plan's own goal states "the tools' existing forgiving argument forms keep working," and omission was the forgiving form.

**The wrong assumption is codified in the module doc:** strict.rs's doc comment (~lines 28-33) claims "The tool's own `execute` deserialization is untouched — an `Option<T>` field still accepts `null`, and a missing field still behaves the same." The first clause is true only for fields that actually are `Option<T>`; the inventory above is not.

**Fix direction (cheap, local):** make the affected fields null-tolerant — either `Option<T>` + `unwrap_or_default()` at the use sites, or a small `deserialize_with` helper mapping `null` → `Default::default()` (keeps the struct types unchanged). `PlanKind` already has a forgiving parse path (`PlanKind::parse` falls back to `Implementation`), so `Option<PlanKind>` + existing default composes cleanly. **Add a regression test** that deserializes every STRICT_TOOL's args from the strict-mode shape — all keys present, every schema-optional property set to `null` — asserting success; that test fails today and pins the contract the normalization implies. (Note `StepInput::Map.body` → `Option<String>` must also update `to_text`/rendering to treat `None` as empty, matching current absent-body behavior.)

---

## LOW 1 — Sanitizer fallback still leaks serde vocabulary for unmatched shapes, with no parameter guidance for the untagged-enum case

**File:** `src/tool/error_message.rs:131-138.

Shapes serde emits that fall through to the fallback include `duplicate field \`x\``, `invalid length 2, expected ...`, and — the one that matters — `data did not match any variant of untagged enum StepInput` (produced by `create_plan`/`update_plan` `steps` items, and the exact error HIGH 1's `body: null` triggers). The fallback renders `...its arguments do not match its schema (data did not match any variant of untagged enum StepInput)...` — raw serde vocabulary, no parameter named, no hint which step or which branch failed. The doc comment (error_message.rs:33-36) declares this deliberate ("carries serde's own one-line detail"), and it never echoes the arguments blob, so this is a quality gap rather than a security issue. Suggested: a dedicated branch for `data did not match any variant of untagged enum \`X\`` → "one of the items in a list parameter does not match its allowed forms — check each item against the schema" (optionally extracting the enum name and mapping it to the parameter).

## LOW 2 — `invalid type` / `unknown variant` parsers mis-split when model-supplied content contains the anchor substrings

**File:** `src/tool/error_message.rs:157` (`split_actual_expected` splits on the first `", expected "`) and `:102-107` (`raw.find("expected ")`).

A model-supplied string value containing `", expected "` (e.g. `{"path": "a, expected b"}` typed into an integer field) makes serde render `invalid type: string "a, expected b", expected usize`; `split_once`-style first-match splitting cuts inside the quoted value, so `type_word` receives `b", expected usize` and the message leaks raw serde text in the tail. Same class: a variant value containing `"expected "` mis-anchors the allowed-set extraction at `:103`. Model-controlled content only (never user file content — the sanitizer only ever sees the arguments JSON), rare, cosmetic; no injection surface beyond serde vocabulary. If touched, splitting on the *last* `", expected "` before `" at line"` (or trimming to the position suffix first) closes it.

---

## Verified clean (checked, no finding)

1. **Normalization correctness (strict.rs).** Idempotency holds — second-pass `is_nullable` recognizes both the compact `["T","null"]` form and the `anyOf: [..., {"type":"null"}]` wrapper (tested at strict.rs:505-518; I also traced the `required`-rebuild path: `props.keys()` order is stable across passes since widening uses in-place `take()`/reassign, so the rebuilt `required` array is byte-stable). Recursion covers `properties` (before widening, so nested objects inside widened properties are already normalized), `items` (object and tuple-array forms), `anyOf`/`oneOf`/`allOf` branches. Non-object schemas (`{"type":"string"}` branches, enums) untouched. The compact-vs-anyOf split (simple `type` string → array; complex → anyOf wrapper) matches OpenAI's documented forms.
2. **Enum + nullable is strict-legal.** OpenAI's own strict-mode example uses exactly this change's shape — `"type": ["string", "null"], "enum": ["celsius", "fahrenheit"]` (enum without `null`) — verified against developers.openai.com/api/docs/guides/function-calling (Strict mode section, fetched 2027-01-24). So `create_plan.kind` and `file_write.mode` will not be schema-rejected; the residual risk is only the model *emitting* `null` (HIGH 1). `file_write.mode` is read via `args.get("mode")` on raw JSON (file_write.rs:58), not a typed field — `null` mode falls to the overwrite default, safe.
3. **Single decision point / no divergence path.** `ToolRegistry::schemas` (src/tool/mod.rs) is the only applier; both OpenAI builders only strip. The advertised array, the token estimate (turn.rs:393), and the request all share the same `tool_schemas` value built at turn.rs:370 from the iteration provider's caps. The mid-turn 429 fallback records endpoint stickiness (`try_429_fallback`, loop_impl.rs:1714-1788) and the retry re-enters the turn loop, which **rebuilds** `tool_schemas` with the new provider's caps — so array/estimate/correction stay consistent; the builder strip is the correct defensive gate for any array-reusing path (e.g. strict-capable original → non-strict fallback: flag stripped, normalized parameters remain, which is valid JSON Schema everywhere). The correction site (turn.rs:1644-1656) uses the same in-scope provider that served the batch.
4. **Anthropic never sees the flag.** Anthropic caps hardcode `supports_strict_schema: false`; registry sets `strict: None` → `skip_serializing_if` omits it.
5. **The 44 call sites.** No remaining `invalid arguments:` in `src/` (only historical text in `.coding/` knowledge and a frontend test's hardcoded old-shape string, which is frontend-local). All replacements are inside `impl Tool` blocks with `self.name()` in scope (compile-verified by the green build). `file_read`'s literal `"file_read"` matches `name()` (file_read.rs:55-57); `read_files`'s `invalid_args_error` keeps its received-keys + hint context on top of the sanitized base.
6. **Dispatch site.** The malformed-JSON path now routes through the sanitizer's Syntax/Eof branch (error_message.rs:57-63) and **no longer echoes `tc.arguments`** — the prompt-injection surface of the old `(raw: {…})` is gone. The sanitizer never receives the blob at all.
7. **Budget ceilings.** The test measures with `Capabilities::openai()` (factory.rs:1635) — strict-capable — so the measured values (Executing 31,471 / PlanFrozen 32,757 / Reviewing 26,735) include normalization; the raised ceilings (32,100 / 33,400 / 27,300) leave 565-643 headroom, and the dated comments (2027-01-24) follow the file's existing convention.
8. **Docs sync.** `docs/CONFIGURATION.md` documents the new `supports_strict_schema` key; `PLAN.md` Capabilities block updated; `ToolSchema.strict` doc comment present. `README.md` contains no `endpoints.toml` documentation (searched), so no README gap exists.
9. **Constitution.** No platform-specific APIs/paths in the changed library code; all new public items have doc comments; no shell-based file mutation in the diff; `cargo test` reported green (2,440 passed) by the parent — I could not re-run it (read-only reviewer), so the warning-free claim rests on the parent's run plus the absence of `#[allow]` in the diff.

---

## Verdict rationale

The HIGH finding is a goal-level defect: the feature's stated purpose is that strict mode keeps the tools' forgiving argument forms working, and for ten fields across three of the nine STRICT_TOOLS it does the opposite — it removes omission (strict requires the key) and rejects the replacement (null). The fix is small and local (null-tolerant deserializers + one regression test), and everything else in the change set — architecture, gating, sanitization, security, docs, budgets — checks out.
