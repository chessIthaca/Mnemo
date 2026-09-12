## Verdict: PASS

**Summary:** The template_kwargs deletion is complete and correct — the struct field, all seven registry entries, the three must-send doc claims, the module-table extras, and the stale test assert are gone; the three kwargs tests were rewritten as vendor-detection tests preserving Alibaba/Moonshot/Zhipu coverage; the source-scan regression guard is the honest red-green design for this doc-drift defect and is cross-platform; PLAN.md, the spec record, and the BUG record are synced; no dead code, orphaned references, or unrelated changes ride the diff. Zero findings.

**Scope reviewed:** `git diff HEAD` on wt/agenticcoding (4 tracked files: backlog.jsonl 1-line dispatch flip, spec amendment, PLAN.md:330, src/provider/policy.rs 70 lines) + the two untracked files (BUG knowledge record, plan d4b951be.md); full read of policy.rs (725 lines); repo-wide sweeps for `template_kwargs`, `clear_thinking|preserve_thinking`, and the renamed-away test names; README/docs checks. Detailed verification follows in the appended sections.

### 1. The deletion (src/provider/policy.rs) — verified complete

- **Struct field + doc comment** (was :200-202): gone. `ProviderPolicy` is now `{ name, reasoning_required, stateful_key, reasoning_field, include_params, retention, signatures_portable_across_models, vendor, fold_volatile_tail }` — matching the amended spec listing exactly.
- **All seven registry entries deleted**: the empty `template_kwargs: HashMap::new()` lines in claude/gemini/openai_responses/deepseek/vllm_default, and the populated `HashMap` constructions in qwen/kimi/glm dropped entirely (those fns now go straight to `include_params: HashMap::new()`).
- **Must-send claims deleted**: qwen/kimi doc comments are now just "Reasoning in `reasoning_content`; silent degradation."; GLM's "Newer GLM clears thinking by default, so `clear_thinking: false` must be set explicitly" is gone. No doc comment anywhere claims unsent behavior — the backlog acceptance criterion is met.
- **Module-doc table**: Qwen/Kimi and GLM 4.7+ extras rows now "—" (:34-35); other rows untouched (OpenAI Responses still documents its `include` param, which is real and live).
- **Stale assert removed**: `assert!(p.template_kwargs.is_empty())` gone from `claude_policy_for_anthropic_kind`; the `include_params.is_empty()` assert is retained.
- **Rewritten tests** (:561-582): `qwen_policy_detected_by_model_id` (Vendor::Alibaba, `!reasoning_required`, `reasoning_field`), `kimi_policy_detected_by_model_id` (Vendor::Moonshot, `reasoning_field`), `glm_policy_detected_by_model_id` (Vendor::Zhipu, `reasoning_field`, `!fold_volatile_tail`). Vendor coverage preserved, and the naming now matches the sibling tests (`gemini/deepseek_policy_detected_by_model_id`).

### 2. Regression test — honest red-green, cross-platform

`policy_source_carries_no_never_sent_template_kwargs` (:695-724):

- **Red-green logic verified by inspection**: pre-fix, the non-test source contained all three needles (field declaration, seven entries, doc claims) — the test necessarily failed; post-fix it passes. For a doc-drift/dead-registry defect the source-scan is the correct design: it reproduces the defect (the identifiers present in shipped source) and guards reintroduction, with a failure message that documents the gate (vendor-policy direction re-decided + param names verified against the proxy first).
- **Self-exclusion correct**: the test's own needles and its doc comment live inside `mod tests`, excluded by the `split_once("mod tests")` cut — no self-trip.
- **Split point verified**: the first textual occurrence of "mod tests" in the file is the real `#[cfg(test)] mod tests {` at :489-490; no earlier occurrence exists in the non-test source, so the scanned region is exactly the non-test source.
- **Cross-platform**: `concat!(env!("CARGO_MANIFEST_DIR"), "/src/provider/policy.rs")` is a compile-time absolute path (cwd-independent), and the forward-slash suffix is legal on Windows (mixed separators are accepted by the OS path APIs) — runs identically on macOS and Windows. No Windows-only assumptions anywhere in the diff.

### 3. No dead code / orphaned references

- `use std::collections::HashMap` (:49) and `use serde_json::Value` (:51) are both still live via `include_params` — and the warning-free build under `#![deny(warnings)]` (both crate roots) is itself proof: an unused import would have failed the build.
- Repo-wide sweeps: the only src/ hits for `template_kwargs`/`clear_thinking`/`preserve_thinking` are the regression test's own needles (:695-721), all inside `mod tests` — exactly as claimed.
- Old test names (`qwen/kimi_policy_carries_preserve_thinking`, `glm_policy_carries_clear_thinking_false`): zero matches repo-wide — no test-list or CI references broke on the rename.
- Removing a `pub` field from `ProviderPolicy`: all construction sites are the seven private fns in policy.rs; a green workspace build proves no external constructor or field reader existed.

### 4. Documentation sync

- **PLAN.md:330**: follow-up mention reduced to `include_params`/`reasoning_field`; no other template_kwargs mention remains in PLAN.md.
- **Spec record** (`.coding/knowledge/spec/2026-12-21-reasoning-state-continuity-raw-payload-source-of.md`): amended with a dated paragraph (the sanctioned append-only `memory_amend` pattern — the original RULE 2 listing is retained and explicitly superseded). The amendment names the deletion, the never-sent root cause, the rolled-back-direction gate, that `include_params` is NOT dead, the regression guard, and a pointer to the BUG record.
- **BUG record**: `.coding/knowledge/bug/2027-01-11-policy-rs-template-kwargs-must-send-claims-never.md` (untracked, to be committed) carries symptom → root cause → fix + regression test name; the BUG memory (f71b6205) is present.
- **README.md / docs/**: zero hits for any of the three identifiers — nothing else claims the params are sent.
- **Remaining references are historical records only** (the 2026-09-09 provider-comms review, the executive summary, the 2026-12-21 reviews/reverify, old plan 70910b7d, and the backlog item's own problem statement) — correct to leave untouched; they describe the bug or the pre-fix state, not current behavior.

### 5. File-tools-first

All four tracked-file edits are clean, targeted changes consistent with `file_edit`; the spec change is an appended dated paragraph (the `memory_amend` signature). No signs of shell-based mutation anywhere in the diff.

### 6. Bug-plan checks

- **Regression test exercises the changed path** — yes (see §2).
- **Root cause documented** — plan Context, BUG record, spec amendment, BUG memory.
- **Backlog bookkeeping**: be85a2a2 flipped pending→in_flight with plan_id d4b951be — the correct dispatch flip (the done-flip is keyed to finish). The backlog.jsonl diff is exactly that one line; the `deleted_at` fields visible on neighboring items are committed context, not part of this change — no unrelated mutations ride this diff (unlike the earlier F3 round's L3).

### 7. Delete-vs-wire decision — holds

The wire path is gated on the vendor-policy config direction the user rolled back 2027-01-09 (memory 3fcae5b3: main + wt/agenticcoding reset to e0ac5dd), and the param names are absent from public vendor docs (verified by the original 2027-01-09 review) — wiring unverifiable names risks 400s and cannot be live-verified unattended. `include_params` correctly retained: `openai_responses()` still populates it and `openai_policy_for_unrecognized_openai_model` asserts it — live data, not dead registry.

### 8. Security / constitution

No security surface: a data-struct field deletion; the new test reads the crate's own source at test time (test-only, no untrusted input). Nothing committed yet (all changes uncommitted, pre-closing-sequence, as expected); no main-branch involvement; code style matches the module; public items retain doc comments.

### Non-blocking notes (no action required)

1. The spec amendment opens "Amended 2027-01-11: AMENDED 2027-01-11 (plan d4b951be…)" — the `memory_amend` auto-prefix plus the content's own opening produce a doubled date. Cosmetic only; the append-only convention means fixing it would require yet another amendment — not worth it.
2. `split_once("mod tests")` keys on the first textual occurrence: if a future doc comment in the non-test source ever mentions "mod tests", the scanned region silently shrinks (the guard weakens; no false failure). Keying on `"#[cfg(test)]"` or `"\nmod tests {"` would harden it if that ever matters. Current file verified clean.

### Test cross-check

Reported: `cargo test policy_source_carries` 1 passed (RED pre-fix per the plan's reproduce step); full `cargo test --workspace` 2309 + 16 + 297 + 4 + 2 + 2 + 0 passed, 0 failed, 18+3 ignored, warning-free under `#![deny(warnings)]`. Read-only review — not re-executed, but nothing found contradicts the numbers; the deletion is compile-clean by inspection (zero remaining field references in src/).
