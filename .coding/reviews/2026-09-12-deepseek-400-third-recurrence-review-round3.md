## Verdict: PASS (0 high, 0 low)

Round-3 (final) re-review of plan c6cb69f7 ("Fix DeepSeek reasoning_content 400, third recurrence — tail-owner escape") on wt/mnemo, after the round-2 report (.coding/reviews/2026-09-12-deepseek-400-third-recurrence-review-round2.md, 0 high / 1 low) was fixed by the main agent. Scope: all uncommitted changes (git diff HEAD incl. untracked) — the three round-2 LOW-1 sentences are fixed comment-only; the functional code is confirmed byte-identical to the round-2-reviewed state. Read-only static verification per the round-1 methodology note: `cargo test` NOT re-run (spawn instructions: "spot-check by reading the tests; do not run builds"); comment-only deltas cannot alter the reported matrix (root 2306+16 passed, 0 failed; mnemo-app 297+4+2+2 passed, 0 failed).

## (a) The three fixed sentences — accurate and self-consistent (verified in situ)

1. **tests.rs:1749-1752** (inner annotation of `deepseek_tail_owner_keyed_when_system_follows_tool_results`): "text-only historical turns tolerate a missing key (probes A/D/G2), but this one carries tool_calls, so the widened guarantee (2026-09-12 plan c6cb69f7) gives it the bare `""` key below." — The referenced raw (tests.rs:1753-1760) does carry `tool_calls: [{id call_old, read}]` and no `reasoning_content`, and the body's assert (1819-1823) expects exactly `Some(&json!(""))` on messages[0]. The annotation matches its own assertion and the header (1740-1743). **PASS.**
2. **tests.rs:1643-1645** (header of `raw_echo_injects_reasoning_content_when_required_but_absent`): "Text-only historical assistant turns tolerate a missing key (probes A/D/G2), but the age-0 turn AND every older turn carrying tool_calls are validated (third recurrence, 2026-09-12 plan c6cb69f7)." — Matches `needs_reasoning_readd` (request.rs:267: `assistant_age == 0 || raw_has_tool_calls(raw)`) and the test's own age-0 key-presence assertions (1681-1691). **PASS.**
3. **mod.rs:198-204** (`Message::reasoning_content` field doc): text-only historical tolerance scoped; tool-call turns validated; "the builder guarantees the key on the tail turn (age 0, carrying the structured text) and on every older tool-call turn (the bare `""` when the raw echo lacks it) — foreign 429-fallback turns included." — Cross-checked against the builder: age 0 re-add writes `reasoning_content.unwrap_or_default()` (structured text, request.rs:1178-1179); age ≥ 1 tool-call re-add writes `""` (1180-1182); keyed turns with no strip are borrowed with their own key; the synthetic path unconditionally keys assistant turns (631-634) and re-keys bare after a strip on tool-call turns (658-665). Every claim holds; the "when the raw echo lacks it" parenthetical is the re-add case, and does not contradict key-present turns keeping their own value. The pre-existing sentence at mod.rs:192-197 ("validates the request TAIL…", c9b5cbe4) is a correct subset — it does not say "only the tail", and the widened sentences follow immediately. **PASS.**

## (b) Staleness sweep — exhausted (all phrase classes clean)

Literal/regex scans across **/src/**:
- "tolerate a missing key" → **exactly 3 matches** (request.rs:1124, mod.rs:198, policy.rs:86), all text-only-scoped — reproduces the claimed evidence exactly.
- "tolerates a missing key", "age-0 only", "Historical assistant turns tolerate", "ONLY the request tail", "age-0 guarantee" → 0 matches.
- "only the most recent assistant turn" → 1 (request.rs:638: "Age >= 1 only — the most recent assistant turn is never stripped" — the synthetic retention-strip comment, correct and unrelated to the key contract).
- "must NOT get a fabricated" → 0; "must not get a fabricated" → tests.rs:1717 (Gemini continuity-contract exclusion — still correct, Gemini never receives the key).
- "request tail" → 4 (mod.rs:193 tail-subset sentence, correct; request.rs:250, request.rs:1119, policy.rs:80 — all widened, "AND on every older assistant turn that carries tool_calls"; none retains the stale "validates only the TAIL" claim).
- "is validated"/"fabricated" remainder → only mod.rs:199 (the fixed doc) and tests.rs:1700/1906 (Gemini exclusion; regression test's text-only boundary comment — both correct).
- "tail-turn contract" → tests.rs:2301 + 2334 (prefix-cache test) — now properly qualified ("key injected with the structured text (the tail-turn contract)… when it ages to 1 a TOOL-CALL turn keeps the guarantee — the bare key"; assert message scoped "age 0 →"). Consistent.
- The 2026-12-23 SPEC record still carries its December tail-only body, but the two dated 2027-01-11 amendment paragraphs (widened contract + "milk-turn"→"mid-turn" typo correction) supersede it in place — the record's live truth matches the corrected contract on every claim (key on age-0 AND every tool-call turn; text-only tolerance; value split structured@0/`""`@≥1; strip re-add on both paths; shared `needs_reasoning_readd`; fingerprint MODE 0/1/2; both test names). Acceptable amendment hygiene, as round 2.

## (c) No functional change since round 2 — deltas are the three comments only

Every functional element is present and byte-equal to the round-2 review's own quotes, with matching line numbers:
- `needs_reasoning_readd` (request.rs:261-274) — `(assistant_age == 0 || raw_has_tool_calls(raw)) && reasoning_required && reasoning_field == Some("reasoning_content") && (key absent || strip about to fire)`; shared by echo (1162) and fingerprint (291-294).
- `raw_has_tool_calls` (239-243) — non-empty `tool_calls` array, read from raw.
- fingerprint `readd_mode` (295-308) — 0 = none, 2 = age-0 structured, 1 = bare `""` at age ≥ 1; hashed as a byte (307-308); flip 2→1 forces the cache rebuild (pinned by `prefix_cache_rebuilds_when_the_readd_ages_out`, hit_count 0, 2348-2356).
- echo value split (1173-1183) — age 0 `reasoning_content.unwrap_or_default()`, age ≥ 1 `""`; comment documents probe T2 acceptance.
- synthetic path (631-634 unconditional pre-strip re-add → 640-649 strip → 658-665 post-strip bare re-add gated on `role == Assistant && reasoning_required && reasoning_field == Some("reasoning_content") && !m.tool_calls.is_empty() && obj.get("reasoning_content").is_none()`), with the LOW-2 ordering comment.

The current diff's remaining src/ hunks are exactly the three round-2 LOW-1 sentences replaced (tests.rs:1643-1645 header, tests.rs:1749-1752 inner annotation, mod.rs:198-204 field doc), all inside `//`/`///` comments — no statement or expression touched. No new functional hunk exists beyond those the round-1/round-2 reports reviewed.

## (d) bug_fixing extras — all hold

- **Regression test exercises the changed path and predates the fix:** `deepseek_keys_every_tool_call_turn_after_cross_vendor_reentry` (tests.rs:1833-1925) builds the incident shape (keyless two-turn tool-call chain, deepseek-v4-flash + reasoning_effort "max"), asserts the serialized wire body: messages[1] (age-1 tool-call echo) carries `""` (1894-1899 — fails under the old `assistant_age == 0`-only gate), messages[3] (age-0 tail-owner) keeps its key (1900-1903), and a text-only historical raw stays unkeyed (1921-1924). Two companion pins updated to the corrected contract (`deepseek_tail_owner_keyed_when_system_follows_tool_results` assert `""` at 1819-1823; `prefix_cache_rebuilds_when_the_readd_ages_out` with hit_count 0) and the round-2 LOW-2 pin `retention_deepseek_synthetic_tool_call_turn_keeps_bare_key_under_pressure` (2576-2604) intact.
- **Root cause documented:** plan context (".coding/plans/c6cb69f7.md" ROOT CAUSE ESTABLISHED: freshness ruled out by binary mtimes, tail-owner provably keyed, cross-vendor Rule-5 strip discriminator, census 72/78 + 79/85, strict-superset conclusion, honest inferential residual) and BUG memory (.coding/knowledge/bug/2027-01-11-deepseek-reasoning-content-400-3rd-recurrence-cr.md: symptom → root cause → fix → regression-test name, single self-contained record).
- **Plan `regression_test` field now recorded** ("deepseek_keys_every_tool_call_turn_after_cross_vendor_reentry", plan file line 56) — closes the round-2 note that it was still pending; step 4 checked.
- **SPEC amendment matches the corrected contract** (2026-12-23-deepseek-tail-turn-reasoning-content-guarantee-i.md, both dated paragraphs) — verified sentence-by-sentence against request.rs code and the test names.
- **Matrix:** comment-only changes since the reported run; static review of the diff found no test that cannot pass (all three comment hunks are well-formed `//`/`///` lines; no code motion).

## (e) Docs sync / platform / file-tools / warnings

- README.md / PLAN.md: correctly judged as needing no update — internal wire-contract correction with zero user-facing surface; the front-door convention (SPEC 2027-01-11) routes such detail to code docs + knowledge records, where it lives.
- Multi-platform: the diff is pure Rust plus Markdown data; no platform-specific APIs, paths, or syntax anywhere (incl. the .coding/ files).
- File-tools-first: no shell-based file mutation in the changeset; the SPEC amendments carry the sanctioned memory_amend signature (dated "Amended…" paragraphs).
- deny(warnings): no `#[allow(...)]` in the diff; `raw_has_tool_calls`/`needs_reasoning_readd` are used by both production paths (echo + fingerprint) — no dead code; comment-only deltas cannot introduce warnings.

## Findings

None. The three round-2 LOW-1 sentences are fixed accurately, the staleness class is exhausted (verified by phrase census, not spot-sampling), and the functional code is exactly the round-2-reviewed state. Residuals carried from prior rounds (not defects): the live-turn acceptance and the API-side mechanism remain inferential-as-stated and are the user's relaunch step; matrix re-execution is the main agent's step-4 duty. Ready to commit.
