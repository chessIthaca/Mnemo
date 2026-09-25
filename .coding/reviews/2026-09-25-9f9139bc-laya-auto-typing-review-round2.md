## Verdict: PASS

Round-2 verification for plan 9f9139bc "Memory auto-typing via Laya" (backlog a147b63c), `wt/mnemo`, uncommitted working tree. All three round-1 findings are **verified fixed** — no new issues introduced, no drift spotted in the rest of the change. The round-1 verdict (FINDINGS 0 high / 3 low) can be closed out.


## Scope reviewed

All uncommitted changes (19 tracked modified files + 3 untracked: `src/memory/auto_typing.rs`, `.coding/plans/9f9139bc.md`, the round-1 report), with focus on the three fix sites: `src/memory/auto_typing.rs` (UnknownLabel construction, `strip_typed_prefix`/`strip_ci`, `seed_dataset` docs, both new tests), `src/tool/memory/mod.rs` (`keep_writer_note`), and the five LOW 2 doc surfaces (`src/config/general.rs`, `docs/FEATURES.md`, `README.md`, `PLAN.md`, the module doc).

## Fix 1 (LOW 1 — unbounded endpoint-controlled label echo): VERIFIED FIXED

- **Construction-site scrub is correct and well-placed** (`auto_typing.rs:217-231`): `other.chars().filter(|c| !c.is_control()).take(40).collect()`. The order is the strong one — filter **before** take, so control characters cannot consume the 40-char budget (hostile padding can't push real content past the cap), and the cap counts chars, not bytes (multi-byte safe). `is_control()` covers the injection class round 1 named (U+000A/U+0009/U+000D and friends).
- **Single-surface claim holds**: a project-wide search finds exactly ONE `UnknownLabel` construction site (the scrubbed one at `:229`) and one consumer (`src/tool/memory/mod.rs:391`, verbatim `{label}` interpolation in `keep_writer_note`). The payload therefore cannot reach the agent-visible message or `auto_typed` data unscrubbed anywhere. `keep_writer_note` staying unchanged is correct — the fix moved to the construction site, as stated.
- **Regression test is genuine** (`an_unknown_label_is_bounded_and_scrubbed`, `:497-517`): stub label `"BAD\n" + 10_000 × 'x'` at 0.99 confidence; asserts no `'\n'`, ≤ 40 chars, starts with `"BAD"`. Without the fix the payload is 10,004 chars containing a newline — both assertions fail; with it, `"BAD" + 37 × 'x'`. Fail-without / pass-with confirmed by inspection.
- Cleared (no action): Unicode *format* characters (Cf — bidi overrides, zero-width) are not Cc and survive the filter; but they cannot forge multi-line output or bypass the 40-char size cap, and the shape matches round 1's prescribed remedy (`filter(..).take(..)`). Not worth a finding.

## Fix 2 (LOW 2 — seed set trains 4 of 6): VERIFIED FIXED (docs-first, as round 1 prescribed)

- All five surfaces state the same scope, consistently and accurately:
  - `seed_dataset` doc (`auto_typing.rs:286-290`): trains only the four dir-backed kinds; a confident PLAN/REVIEW retype "rides an UNTRAINED class until those corpora can be seeded too (the documented follow-up)".
  - Config field doc (`src/config/general.rs`): "covers SPEC/DECISION/BUG/HOW; PLAN/REVIEW ship without seed examples".
  - `docs/FEATURES.md`: covers SPEC/DECISION/BUG/HOW; "PLAN/REVIEW records have no knowledge-home corpus, so those two classes ride untrained until seeded, a documented follow-up".
  - `README.md`: "seed examples cover SPEC/DECISION/BUG/HOW".
  - `PLAN.md`: "covers SPEC/DECISION/BUG/HOW; PLAN/REVIEW ride untrained until those corpora can seed them — follow-up".
- **Ground truth verified**: `dir_for_record_type` (`knowledge.rs:221-229`) maps exactly Spec/Decision/Bug/How → `Some`, Plan/Review/None → `None` — the docs' claim is factually right, and none of the five statements overstates the contract (round 1's actual complaint).
- The docs-first choice over behavior hardening is round 1's primary recommended fix (its "Optional hardening" was optional), and the supersets rationale (hard-coding PLAN/REVIEW keeps would block legitimately fine-tuned checkpoints) is sound.

## Fix 3 (LOW 3 — case-variant prefix survives inside retitle): VERIFIED FIXED

- **`strip_ci` byte-safety** (`auto_typing.rs:162-174`): `text.len() >= len && text.is_char_boundary(len) && text[..len].eq_ignore_ascii_case(prefix)` — the boundary check rejects any slice ending mid-char, and since all six prefixes are pure ASCII (a byte ≥ 0x80 never ASCII-case-equals an ASCII letter or colon), there is no false-positive path from multi-byte lead bytes. Correct.
- **Loop termination** (`:150-160`): every iteration strips at least the 4+ bytes of a matched prefix (shortest is `HOW:`) plus inter-token whitespace — `rest.len()` strictly decreases each iteration and the loop exits on the first no-match. Empty rest is safe (len 0 < 4). No non-terminating input exists.
- **Case variants and repeats**: `"spec: tabs lost"` → `strip_ci` → `"SPEC: tabs lost"`; `"BUG: bug: mixed"` → exact strip + case-variant strip → `"HOW: mixed"` — both pinned by the new test and both reproduce round 1's defect without the fix.
- **Non-prefix guard**: `"speculative: keep"` is untouched (test-pinned → `"SPEC: speculative: keep"`) — the trailing colon in every prefix makes a leading-word false positive impossible (`specu` ≠ `SPEC:` at byte 5).
- **Parse-side parity kept**: `MemoryRecordType::from_title` (`types.rs:153-171`) is unchanged — case-sensitive, position-0, same six prefixes — and its doc still says so; the strip-side widening is documented in `strip_typed_prefix`'s own doc. The round-1 invariant `from_title(retitle(x, to)) == to` still holds (retitle always emits the exact uppercase prefix at position 0).
- **Blast radius of the widening** — only two callers: `state_text` (classifier input; stripping case-variant writer prefixes there is the same bias-removal rationale, and seed/train text flows through the same function, so train/inference shape parity is preserved) and `retitle` (only runs on the already-behavior-changing retype path). Downstream consumers verified in `src/tool/memory/mod.rs:264-331`: `knowledge.write` receives the corrected title; `record_type` comes from `from_title` on it; the knowledge file/dir, reindexed row, and `auto_typed` note all agree.
- **Regression test** `strip_typed_prefix_handles_case_variants_and_repeats` (`:539-551`) covers the case variant, the mixed-case repeat, `state_text` parity, and the non-prefix guard — fail-without/pass-with by inspection (the old single-pass case-sensitive strip yields `"SPEC: spec: tabs lost"`).

## Drift spot-check

The tracked diff matches round 1's described scope plus exactly the LOW 2 doc statements; `mod.rs`, factory wiring, IPC/DTO/fixtures, and the frontend diff are the round-1-reviewed hunks (with the round-1-passed acceptance tests intact — all four `memory_write`-level auto-typing tests plus the module's 16 unit tests are present and assert what round 1 verified). No new shell-mutation artifacts, no platform-specific code, no new doc/tool inconsistencies. The reported test runs (targeted 23/23; root 2,605 passed / 0 failed under `deny(warnings)`; `mnemo-app` 320 + integration green; tsc exit 0; vitest 1,274) cover the full-coverage discipline (root + app crate + tsc + vitest).

## Verdict detail

0 findings. LOW 1, LOW 2, and LOW 3 are all correctly and completely fixed, each with a genuine regression test where one was prescribed; the fixes introduce no new issues (byte-safety and termination of the new strip helpers check out, the 40-char cap placement is the strong ordering, and the five scope statements are mutually consistent and factually accurate). The three round-1 findings can be marked resolved — this change is ready to commit.
