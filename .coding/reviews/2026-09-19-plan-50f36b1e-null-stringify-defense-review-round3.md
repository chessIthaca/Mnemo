## Verdict: PASS

Round-3 verification of plan 50f36b1e (backlog 9118714a) — the FULL uncommitted changeset (`git diff HEAD` across 8 files + untracked `.coding/plans/50f36b1e.md` and the round-1/round-2 reports). The round-3 delta over round 2's reviewed state is exactly the three claimed items; everything else is byte-identical to what rounds 1-2 verified.

**Summary:** All three round-2 LOW-1 fix items are correctly landed and factually accurate: the mod.rs exception paragraph now names the mirror case with wording that matches the code's actual behavior, the file_edit `new_string` schema description carries the model-facing clause verbatim, and the three context-budget ceilings are raised with the file's dated raise-comment convention naming backlog 9118714a. The ceiling arithmetic — the round-3-specific risk — checks out under independent reconstruction: the uniform +647 growth over the 2027-01-24 baselines is structurally *expected*, not a copy-paste artifact (detail below). The fixes introduce no new issues, and round-1 checks a-g plus round-2's verification still hold on the unchanged remainder of the changeset.

---

## Round-2 LOW-1 fix verification

**1. mod.rs doc mirror-case paragraph — LANDED, accurate.** `src/tool/mod.rs:212-217` continues the exception paragraph verbatim as claimed: "The mirror case (round-2 LOW-1): a genuine `new_string` of exactly `"null"` is dropped too, and an empty `new_string` deletes the anchor — a wrong write with no error signal, visible only in the returned diff; use batch mode (per-item `new_string` is required, never dropped) or include surrounding context in the replacement." Every claim verified against the code:
- `new_string` is schema-optional (`required: ["path"]` only, `file_edit.rs:1554`) → the defense drops a `"null"` value for it.
- `FileEditArgs.new_string` is `#[serde(default, deserialize_with = "null_to_default")] String` (`file_edit.rs:58-62`) → the dropped key deserializes to `""`, and the struct doc's own line ("An empty `new_string` in single mode deletes `old_string`") confirms the deletion semantics.
- "per-item `new_string` is required, never dropped": the `edits` item schema (`file_edit.rs:1551`) declares `"required": ["old_string", "new_string"]`, and the defense keys drops on absence from that schema level's `required` list.
- "no error signal, visible only in the returned diff": matches round-2's verified finding (the model-visible output is just `edited <path>`; the diff rides in `result.data`, UI-facing) — the round-2 suggested wording, landed essentially as proposed.

**2. file_edit `new_string` schema clause — LANDED, accurate.** `src/tool/agent/file_edit.rs:1545` carries verbatim: "A replacement of exactly 'null' is dropped by the transport's null-stringify defense (empty = deletion) — use batch mode or include surrounding context." Factually correct on all three points (exactly-`"null"` replacement dropped as an optional property; empty-after-drop = deletion per the serde default; both workarounds valid — batch per-item is required hence never dropped, and a replacement with surrounding context is not exactly `"null"` so untouched). No test anywhere pins the old description text (searched; the only occurrence of the clause is the schema line itself), so nothing breaks.

**3. Ceiling raises — LANDED, documented, measured, and arithmetically consistent.** All three in `src/agent/factory.rs`, each following the file's dated convention (old → new (2027-01-24): backlog 9118714a (plan 50f36b1e) — cause; measures N chars; Ceiling = measured + headroom, deliberate raise):
- Executing 32_100 → 32_300, measured 32_118 (`:1824-1831`)
- PlanFrozen 33_400 → 33_600, measured 33_404 (`:1854-1859`)
- Reviewing 27_300 → 27_500, measured 27_382 (`:1942-1947`)

**The uniform +647 is structurally expected — independent reconstruction.** All three claimed measured values are exactly +647 over their 2027-01-24 baselines (31_471 / 32_757 / 26_735, all recorded at the plan-21118961 landing). That equality looked suspicious (Reviewing does NOT carry create_plan — `mod.rs:744-745` — while Executing does), so I reconstructed the growth:
- The budget test measures with `Capabilities::openai()` (`factory.rs:1635`), which sets `supports_strict_schema: true` (`provider/mod.rs:69`) → `ToolRegistry::schemas` applies `normalize_for_strict` to STRICT_TOOLS members (`mod.rs:1196`). `is_nullable` (`strict.rs:198-216`) treats a `type` array containing `"null"` as already-nullable and the widening walk skips it (`strict.rs:153-156`) — so the hand-written `["string","null"]` fields on STRICT_TOOLS members (file_edit, create_plan, update_plan) are **byte-stable** and contribute ZERO measured growth.
- What DOES count since the baselines: committed a96041b (file_edit failure classes) + d136026 (shell calling-trap notes, ~+350 per its own raise comment) — both ride all three filters (shell/file_edit are `Agent => true` everywhere; both committed before this plan) — plus this changeset's measured growth: backlog_status +18 (two widenings — backlog_status is NOT a STRICT_TOOLS member, so its widenings count raw) +98 (the "note alone…" description sentence) and file_edit's new_string clause +153 ≈ **+269**, which rides all three filters (backlog_status is named in all three Workflow arms; file_edit is Agent).
- The one filter-asymmetric change — create_plan's five widenings (Executing/PlanFrozen only) — is absorbed by normalization, contributing 0 to every measured array. Hence all three deltas are EQUAL by structure: the uniform +647 is exactly what the code predicts. My independent reconstruction (31_471 + ~378 committed drift + 116 + 153 ≈ 32_118) matches the claimed figure.
- The trip story also checks out: at round-1 state (before the clause) Executing sat at ~31_965 < 32_100 — round 1's "no ceiling raise needed" was correct at the time; the round-3 +153-char clause alone pushed it past 32_100, exactly as the parent reported. The plan file had predicted this contingency ("budget ceilings… may need a documented dated raise", step 3's "if tools_array_stays_within_context_budget trips… raise the affected ceiling(s) with the dated raise-comment convention naming backlog 9118714a") and the raise followed it.
- Ceilings sit above the measured values (headroom 182 / 196 / 118). Thinner than past raises (300-650), but deliberately-thin headroom is precedented and sanctioned in this file's own convention (the 2027-01-07 comment: "the headroom is thin — 43 chars — on purpose").

## No new issues from the fixes

- The schema clause (~153 chars) breaks no assertion (no test pins `new_string`'s description text; the nullable-schema tests assert type arrays only) and the one test it could trip — `tools_array_stays_within_context_budget` — was raised to fit, with the measured values recorded.
- The mod.rs doc growth is a doc comment — no behavioral or test surface.
- The ceiling raises are test-only constants; raising cannot break other tests, and the growth being accommodated is intentional, documented schema content (the guard against *accidental* drift is intact).
- The round-3 delta adds no code capable of warnings (doc comment + string literal + test constants); the parent reports the full suite green after the fixes (2477 passed / 0 failed / 5 ignored, warning-free under `#![deny(warnings)]`) — read-only reviewer's caveat as in rounds 1-2: I could not run `cargo test` myself; static review finds all tests well-formed and the claim consistent with the unchanged test count since round 1 (rounds 2-3 added no tests).

## Overall change set — re-confirmed (round-1 checks a-g still hold)

The changeset outside the three round-3 items is unchanged from round 2's review; I re-verified the load-bearing points in the current diff and code:
- **(a) Defense** (`mod.rs:228-288`): recursive walk (anyOf/oneOf/allOf, single + draft-04 tuple `items`, nested `properties`), drops only the exact string `"null"` on properties absent from that schema level's `required`; JSON null, real values, required fields, unknown properties untouched — pinned by the 5 unit tests. Only the doc comment above it grew.
- **(b) Dispatch seam** (`dispatch.rs`): tool lookup moved before `ParsedToolCall`; `drop_stringified_nulls(&tool.schema().parameters, &mut args)` runs before the parsed call is built; unknown-tool path unchanged; raw `tc.arguments` untouched.
- **(c) Note-only backlog_status**: three-way validation with the round-1-fixed error naming `note`; transition guard before any mutation; `set_note` under the same store lock; `deferred`+`note` now lands the note.
- **(d) Nullability vs strict normalization**: re-verified independently this round (see the ceiling reconstruction above) — hand-written `["string","null"]` is byte-stable under `normalize_for_strict`; the enum form matches the machine-widened shape; backlog_status's raw type array is standard draft-07 for non-strict endpoints.
- **(e) No remaining victim**: `dispatch.rs` remains the only raw-args→execution path; the round-3 delta adds no raw readers.
- **(f)/(g)**: 13 new tests + extended assertions unchanged from round 2's verified set; composition tests mirror the seam; the dispatch echo test pins optional-dropped / required-kept / real-value-passthrough end-to-end.
- **Bookkeeping**: `.coding/backlog.jsonl` item 9118714a done-flip with an accurate note (unchanged since round 2; its "Full suite 2477 passed, warning-free" matches the parent's post-fix claim); the untracked plan file `.coding/plans/50f36b1e.md` (5/5 steps, context accurate) matches the landed implementation including the ceiling-raise contingency.

## Constitution checks — PASS

- **Multi-platform neutrality**: pure Rust logic and string literals; no platform APIs, paths, or shell syntax in the round-3 delta or the wider changeset.
- **File-tools-first**: no shell-based file mutation anywhere in the diff; the backlog flip went through the sanctioned bookkeeping path.
- **Documentation**: the round-2 LOW-1 doc gap is closed (both the developer-facing doc and the model-facing schema description); README/PLAN.md need nothing (internal robustness, no feature/config surface).

## Notes (no action required)

- The Executing raise comment attributes "~+100" to the nullable widenings; under the measured (strict-normalized) mode the widenings contribute only ~+18 (backlog_status — the STRICT members' are absorbed), and the comment omits backlog_status's +98 description sentence from round 1's part of this changeset. The attributed total (~250) is close to the actual ~269 and the load-bearing measured figures are correct, so this stays a note — the next raise measures fresh anyway (the test prints the values).
- "dropped by the transport's null-stringify defense" (schema clause) and "visible only in the returned diff" (doc) are slightly loose shorthand — the defense sits at the dispatch seam, and the diff rides in `result.data` (UI-rendered) rather than the model-visible output text. Both are directionally accurate and the actionable content (workarounds) is exact; not findings.
