## Verdict: PASS

Both round-2 findings are resolved exactly as prescribed, and nothing regressed: the changeset is documentation-only (doc-comment rewords in anthropic.rs and prompt.rs + the round-2 report side-car), the core fix re-verifies intact at HEAD, and the full-suite attestation rides the commit message. No new findings.

### Scope reviewed

- Commit 15fd691 in full (`git show 15fd691`): src/provider/anthropic.rs (1 hunk), src/agent/prompt.rs (3 hunks), and the new round-2 review report side-car. Parent confirmed e54fb7b via `git log`, so the range diff e54fb7b..15fd691 equals the single-commit diff.
- Working tree clean at HEAD = 15fd691 (`git diff HEAD` and `git status --short` both empty).
- Final-state reads of every edited site (prompt.rs:440-549, anthropic.rs:1360-1384) plus the openai/stream.rs:112-114 twin for wording parity.
- Core-fix spot-checks at HEAD, verified directly rather than by git arithmetic alone: detect_repetition (stream.rs:460-471), the policy registry (policy.rs:329-508), install_system_messages and the pops (turn.rs:2124-2327, 455-458), and both regression tests (openai/tests.rs:76-91, agent/tests.rs:2788-2864).

### R2-1 — RESOLVED (Anthropic R10 accumulator comment)

src/provider/anthropic.rs:1372-1375 now reads "R10 repetition-guard accumulator — tracks the response text for stuck-loop detection (ANY repeating unit of ≤ 200 bytes spans the last 600 tail bytes). Bounded by `bound_repetition_buffer` to ~2 KB." The old exact-window phrasing ("same ~200-char window repeated 3× at the tail") is gone, and the semantics phrase is verbatim the openai/stream.rs:112-114 twin's ("when ANY repeating unit of ≤ 200 bytes spans the last 600 tail bytes, the stream is aborted") — exactly the one-line reword the finding prescribed. The comment is now byte-wise/any-period accurate.

### R2-2 — RESOLVED (all three prompt.rs placement docs)

- **(a) CONTEXT_FOOTER doc (prompt.rs:447-450):** now scoped "on the trailing placement only: fold-tail providers (Local/Ollama, DeepSeek-vendor) omit it entirely (see `ProviderPolicy::fold_volatile_tail`)" — trailing-only stated, both fold classes named, policy flag referenced. The following cache-law paragraph describes the trailing path's rationale and remains accurate in that scope.
- **(b) build_stable_head doc (prompt.rs:476-487):** "On providers that keep the trailing placement it is the only content placed in `messages[0]`…" replaces the false OpenAI-kind claim; BOTH fold-tail classes are enumerated with their distinct reasons — "Local-kind (Ollama requires the system message to be first) and DeepSeek-vendor (`ProviderPolicy::fold_volatile_tail`; its models echo trailing system blocks)" — and the accepted tradeoffs are stated ("accepted: Local has no prefix cache to preserve; the DeepSeek cache sacrifice is documented in the policy", which policy.rs:214-222 indeed documents).
- **(c) build_volatile_tail doc (prompt.rs:521-534):** describes the two placements — the trailing default (cache-optimized, footer appended after the tail, last message byte-stable) vs fold-tail providers ("instead fold it into the leading system message and omit the footer") — and the compaction rationale is now scoped "on the trailing path".

All three match the code: turn.rs `install_system_messages` computes `fold_tail = is_local || ProviderPolicy::for_kind_and_model(kind, model()).fold_volatile_tail` (turn.rs:2263-2268), folds the tail into the head (2279-2285), and pushes tail + CONTEXT_FOOTER only `if !fold_tail` (2321-2324). The prompt.rs docs and turn.rs's own (pre-existing, consistent) comments now tell the same story.

### No regression — verified

1. **Diff is documentation-only.** All three prompt.rs hunks and the anthropic.rs hunk touch only `///`/`//` comment lines; the only non-comment line in any hunk is unchanged context (`let mut response_text = String::new();`). No statement, signature, import, or test changed. The third file is the round-2 report side-car (designed to ride the commit).
2. **Core fix intact at HEAD** (spot-checked directly): the any-period scan `(1..=window).any(|p| tail[p..].iter().zip(tail).all(|(a, b)| a == b))` with the `needed == 0 || text.len() < needed` early-false (stream.rs:460-471); `fold_volatile_tail: true` only in deepseek() (policy.rs:429) and `false` in the other seven registry literals (claude 340, gemini 375, openai_responses 398, qwen 448, kimi 467, glm 487, vllm_default 506), with policy tests asserting both poles (policy.rs:524, 548); the pops guarded `if !tail_folded` immediately after request_stream returns and before the early exits (turn.rs:455-458); the cache tradeoff documented at policy.rs:214-222 (PLAN.md item (8) is untouched by this commit — round-2's verification stands).
3. **Both regression tests present and intact:** `detect_repetition_fires_on_exit_note_loop_with_74_byte_period` (openai/tests.rs:76-91 — asserts the 69-char / 74-byte unit, fires at 10× = 740 bytes, no-fire at 8× = 592) and `deepseek_vendor_folds_volatile_tail_no_trailing_system_messages` (agent/tests.rs:2788-2864 — asserts tails[0] == "hi", the folded head carries # WORKFLOW STATE, no CONTEXT_FOOTER anywhere, messages.len() == 3, nothing transient persisted).
4. **Suite attested in the commit message** ("Full suite: 2256 + 16 passed, 0 failed, zero warnings (deny(warnings))"). This reviewer is read-only and cannot run cargo (same caveat as rounds 1-2); code inspection agrees — a comment-only diff has no warning surface (no new items; the new `ProviderPolicy::fold_volatile_tail` mentions are plain backticks, not intra-doc links, and the pre-existing [`CONTEXT_FOOTER`] link is untouched).

### Constitution checks

- Multi-platform neutrality: no code, no paths, no shell syntax in the diff ✓.
- File-tools-first: no shell mutation; the side-car is the reviewer-authored report ✓.
- Docs sync: this commit IS the docs sync — the placement docs now match the shipped behavior; README.md / PLAN.md need nothing (no behavior or config surface changed) ✓.
- Public-item docs: build_stable_head / build_volatile_tail / CONTEXT_FOOTER remain documented (improved) ✓.

### Checked and acceptable (no action)

- The CONTEXT_FOOTER doc's "(see `ProviderPolicy::fold_volatile_tail`)" pointer sits in a sentence that names both fold classes; the Local class folds via `is_local`, not the flag — the mechanism distinction is made precisely one doc down (build_stable_head) and in turn.rs's comments. A "see also" pointer, not a false claim.
- build_stable_head's "the only content placed in `messages[0]`" claim (stable head vs volatile tail) predates this change and survives it reworded; the session primer / hidden-group index that turn.rs also appends to messages[0] are byte-stable per session, so the cache-survival claim it supports holds.
- Round-2's "checked and acceptable" items (ASCII fixture comments, headroom comments, Anthropic cache-breakpoint comments, request.rs sanitizer doc, openai/stream.rs borrow comment) are untouched by this diff — those verifications stand.
