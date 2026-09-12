## Verdict: FINDINGS (0 high, 5 low)

The core fix is correct and well-tested: the any-period repetition scan is algorithmically sound with negligible per-delta cost, the DeepSeek fold is exactly scoped (only OpenAI-kind + deepseek model strings change behavior), the pops are safe on every exit path, and the cache tradeoff is deliberate and documented in both places it should be. All five findings are documentation/hygiene/residual-risk items — none is a correctness bug, and none blocks the commit once addressed or consciously waived.

### Scope reviewed

- Full `git diff HEAD` (8 tracked files: PLAN.md, src/agent/{turn,tests}.rs, src/provider/{policy,stream,anthropic}.rs, src/provider/openai/tests.rs, .coding/analysis deletions ×2, backlog.jsonl +4) plus the untracked side-car files riding the commit (.coding/knowledge/bug/2027-01-07-*.md ×2, the fresh-eyes decision record, plans 06648a64/77bf6692/a0eab9ca/b2172228, reviews 2027-01-11-*).
- Cross-checked against the plan file, the knowledge bug record, and every production touchpoint: both R10 call sites (openai/stream.rs:451, anthropic.rs:1491), the OpenAI request builder's trailing-system-run handling (openai/request.rs:315, 1108), the retry-loop comment (loop_impl.rs:112), and the suggestion re-injection paths (turn.rs:1011, 1383; runtime/agent.rs:1114).

### Verified correct

**1. Any-period scan (src/provider/stream.rs:447-469)**

- Algorithm: `(1..=window).any(|p| tail[p..].iter().zip(tail).all(|(a, b)| a == b))` checks `tail[i] == tail[i-p]` for all i ≥ p — exactly p-periodicity over the whole `needed`-byte tail. Correct.
- Suffix property: a suffix of a p-periodic string is itself p-periodic, so the mid-unit 600-byte window (the regression test's 740-byte case starts 140 bytes in = unit offset 66) fires correctly.
- p = window is included in `1..=window`, preserving the old exact-window case as a subset; periods dividing 200 behave identically to before.
- Edge cases: window=0 or threshold=0 → `needed == 0` → early `false` (no panic, no empty-slice indexing). p == needed is only reachable when threshold == 1 (vacuous zip → fires on any text ≥ window) — that is pre-existing behavior of the old `chunks_exact` code, unchanged, and production uses threshold=3 (needed=600 > window=200, so every zip is non-vacuous).
- Multi-byte UTF-8: byte-wise zip over `as_bytes()` — no str slicing, no char-boundary panics; the mojibake unit (74 bytes / 69 chars) is handled natively, and the multibyte test still passes for the right reason (the trailing 'a' breaks every period).
- Performance per TextDelta: typical prose fails each candidate period at the first byte (~window comparisons total); worst case (near-periodic tail) is ~window × needed = 120K byte comparisons ≈ tens of µs — negligible even at the incident's ~5,244 deltas. The incident loop would now trip at ~600 bytes (~8 units, well under a second).
- False-positive surface (deliberately expanded): the guard now fires on any repeating unit ≤ 200 bytes spanning 600 contiguous tail bytes. Genuine risk is limited to ~20+ identical consecutive short lines (e.g. a 200-element `[0, 0, …]` list = 3-byte unit × 200) or uniform runs — and uniform runs were already caught by the old 200-periodic check. The 600-byte contiguous-periodicity bar keeps this tight; the tradeoff is correct for the incident it fixes. (The doc comments understate the new surface — finding L1.)

**2. Fold wiring (src/agent/turn.rs:2122-2300, 429-458; src/provider/policy.rs)**

- `fold_tail = is_local || ProviderPolicy::for_kind_and_model(...).fold_volatile_tail` — the only behavioral delta is OpenAI-kind + model containing "deepseek" (case-insensitive). claude/glm/gemini/kimi/qwen/openai_responses all resolve `fold_volatile_tail = false` → request shapes byte-identical to before; Local is unchanged (is_local already folded). Verified against `for_kind_and_model`'s kind-gated resolution (Anthropic always → claude()).
- Pops: `install_system_messages` returns `tail_folded`; the pops (turn.rs:455-458) run unconditionally after `request_stream` returns (the request body is serialized before the stream is returned; the stream borrows `&self`, not `messages`) and before every early exit (hard-stop return, `?` propagation). The fold path pushed nothing trailing → no pops needed. No exit path leaks the tail/footer into the persistent vec.
- Policy resolution: pure, one lowercase allocation + contains checks, once per iteration — cheap. Single caller (run_turn); the volatile-tail/CONTEXT_FOOTER push exists only in `install_system_messages` (verified via CONTEXT_FOOTER search — no other push site in src/).
- OpenAI request builder handles the now-empty trailing-system run correctly: `trailing_systems = 0` → `cacheable_len` covers all messages; the per-iteration head mutation busts the prefix-cache fingerprint → always a miss, never a stale hit (the fingerprint check is by-construction safe). Correctness preserved; only the cache benefit is lost — the documented tradeoff.
- The regression test's assertions are semantically right: CapturingProvider captures the LAST message as tails, so `tails[0] == "hi"` proves the request ends with the user message; `prompts[0]` contains "# WORKFLOW STATE" (marker confirmed at prompt.rs:674); `messages.len() == 3` (head + user + assistant, nothing transient persisted); no CONTEXT_FOOTER anywhere (the footer is never folded — only the tail is).

**3. Cache tradeoff** — deliberate and documented in both places it should be: the `fold_volatile_tail` doc comment (policy.rs) and PLAN.md item (8). ✓

### Findings

**L1 (low) — Stale R10 doc comments describing the replaced exact-window semantics.** Plan step 3 explicitly required updating src/provider/openai/stream.rs:106-115; that was not done, and two more call-site comments plus two test comments still describe the old machinery:
- src/provider/openai/stream.rs:112-114 — "when the same ~200-char window repeats 3× consecutively at the tail" (also stale: chars, not bytes).
- src/provider/openai/stream.rs:447-450 — same phrasing at the R10 call site.
- src/provider/anthropic.rs:1485-1488 — same phrasing at the Anthropic R10 call site.
- src/provider/openai/tests.rs:60-73 — "The boundary guard must return false (not panic)" describes the removed char-boundary bail (the test now passes because the trailing 'a' breaks every period); "when the window aligns to a char boundary" no longer matters (byte-wise).
- src/provider/stream.rs REPETITION_THRESHOLD known-limitation — "identical 200-byte segment" understates the new surface (any repeating unit ≤ 200 bytes).
Fix: reword the five sites to "any repeating unit of ≤ 200 bytes spanning 600 tail bytes".

**L2 (low) — Residual trailing-system producer on the DeepSeek fold path.** Text-only suggestion re-injection pushes `Message::system("User suggestion: …")` (src/agent/turn.rs:1024-1025, mirrored in the summarization path and runtime/agent.rs:1114). After a tool call, the next DeepSeek request ends with that system message — the same echo-trigger class the fix eliminates, violating the stated invariant "requests end with the user/tool message like every other client". Pre-existing and much narrower than the fixed tail+footer (a short suggestion vs. the large structured tail), so not a regression — but either demote suggestions to user messages for fold vendors, or document the residual as accepted. (The subagent spawn path already assembles its tail into the head — the folded shape — so no gap there.)

**L3 (low) — Unexplained deletion of tracked files.** .coding/analysis/traces-diff.txt and traces-summary.txt (added in 9d0f3e4) are deleted in this changeset with no plan step or rationale; historical plans (5226ca8e, ac36bee3) and the 2026-08-13 cache-hit review reference them. If this is deliberate cleanup of stale regenerable extracts, say so in the commit message; otherwise restore.

**L4 (low) — Knowledge-file byte-count discrepancy.** The committed knowledge record (.coding/knowledge/bug/2027-01-07-deepseek-exit-note-loop-evades-r10-repetition-gu.md) says the loop unit is "71 bytes (69 chars + em-dash)"; the actual trace unit is 74 bytes (69 chars, the em-dash arriving as mojibake U+00E2 U+20AC U+201D) — the regression test correctly asserts 69 chars / 74 bytes. The root-cause record should carry 74, since the entire ∤-200 argument rests on that number. (The plan file's context section repeats the 71 figure.)

**L5 (low) — Plan/implementation period-range discrepancy.** Plan a0eab9ca's detailed step specifies "p in 16..=window"; the implementation scans 1..=window. The implementation is the safer superset — sub-16 periods that don't divide 200 (3, 6, 7, … 15) are equally degenerate loops, and p=1 uniform runs were already caught by the old 200-periodic check — so no code change is warranted; note the divergence so the committed plan doesn't mislead a future reader into "fixing" the range down.

### Bug-plan checks

- Both regression tests exercise the changed paths: `detect_repetition_fires_on_exit_note_loop_with_74_byte_period` → stream.rs (the generalized scan); `deepseek_vendor_folds_volatile_tail_no_trailing_system_messages` → turn.rs fold + policy.rs resolution. ✓
- Pre-fix failure verified logically (this reviewer cannot re-run the pre-fix tree): 74 ∤ 200 means the old exact-window check returns false on the 740-byte input (a 200-byte segment compare cannot match a 74-periodic tail), and the old install path pushed tail+footer for a deepseek model, so tails[0] would have been the CONTEXT_FOOTER, not "hi". Both tests fail pre-fix. ✓
- Root cause documented: BUG memory (semantic, id 47553472) + the knowledge file committed with this change. ✓ (modulo L4's byte count)
- No `#[allow]` suppressions anywhere in the diff. ✓
- Warning-free build: reported green (2256 + 16 passed, zero warnings under deny(warnings)). This reviewer cannot run cargo (read-only); code inspection found no warning triggers (no unused imports/vars — `is_local` is consumed by `fold_tail`; the renamed `tail_folded` is used on all paths).
- Doc comments present on the new public field and updated on REPETITION_WINDOW/THRESHOLD. ✓ (modulo L1's stale call-site comments)

### Constitution checks

- Multi-platform neutrality: no platform-specific code, paths, or shell syntax in the diff. ✓
- File-tools-first: no shell-mutation evidence in the changeset; the .coding/analysis deletions are file deletions (see L3 for the scope question). ✓
- Docs sync: PLAN.md item (8) documents the fold exception + any-period guard ✓; README.md contains no packaging internals (verified: no volatile/footer/trailing/fold mentions) → no update needed ✓; endpoints.toml does not exist in the repo (verified via glob + root listing) and no config key changed (`fold_volatile_tail` is registry-fixed, not user-configurable) → the parent's judgment is correct ✓.
- backlog.jsonl +4 pending items are incident follow-ups from the fresh-eyes analysis — in-scope side-car changes, fine.
