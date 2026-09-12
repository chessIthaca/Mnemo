## Verdict: PASS

Round-2 verification of plan cb1914b9 "Recall: semantic fallback on zero keyword hits + embedder-aware sim weight" (branch `wt/agenticcoder`, uncommitted working tree vs HEAD). Both round-1 findings (0 high, 2 low, both doc-only) are **verified fixed**; no new findings. The code itself is unchanged from the round-1-reviewed state — every delta is inside comments.

### LOW-1 — crossover value in the weight comments: FIXED

Both cited occurrences now state the exact crossover:

- Weight-rationale comment, `src/memory/mod.rs:1355-1356`: "…0.35 under real embedding models (bundled MiniLM default, remote), where a strong semantic match (**sim > 6/7 ≈ 0.857, the exact crossover 0.3/0.35**) legitimately outweighs a bare keyword hit (0.3)."
- `sim_weight` computation comment, `src/memory/mod.rs:1371-1372`: "…so they get 0.35 — a strong match (**sim > 6/7 ≈ 0.857, the exact crossover 0.3/0.35**) then outweighs a bare keyword hit (0.3)."

Math verified: 0.3/0.35 = 6/7 ≈ 0.857142…; `sim × 0.35 > 0.3 ⟺ sim > 6/7`, so the strict `>` is correct (at exactly 6/7 the terms tie, so "outweighs" needs strictly greater — the wording is right). The old hedged "≥ ~0.85" is gone: a literal search for `~0.85` across all 252 `.rs` files returns zero matches. The claim is also correctly scoped: under the hash weight 0.2 no sim ≤ 1 can reach 0.3, and the comment restricts the crossover statement to the 0.35 tier with "where"/"then".

### LOW-2 — must-override contract on `model_id()`: FIXED

`src/memory/embedder.rs:43-51`, trait-method doc now reads: "…and to tier recall's cosine weight (0.2 for the `"hash"` fallback, 0.35 otherwise — see `recall_peek`). **Real embedding models MUST override this: an implementation that forgets silently inherits the noise weight and its semantic matches are systematically understated.** The default is `"hash"` (the offline fallback); [`BundledEmbedder`] overrides with its model id…"

Accuracy checked against the code: the trait default returns exactly `"hash"` (embedder.rs:52-54), and `recall_peek` branches `if embedder.model_id() == "hash" { 0.2 } else { 0.35 }` (mod.rs:1375-1379) — so an implementation that forgets the override does land in the 0.2 branch, i.e. the warning describes real behavior. The `recall_peek` pointer resolves: the method exists at `src/memory/mod.rs:1281`-1443 (trait decl at mod.rs:155) and contains the tiering.

### Change scope since round 1 — clean

`git diff HEAD` modified files are exactly the expected five: `src/memory/mod.rs`, `src/memory/embedder.rs`, `README.md`, `PLAN.md`, `.coding/backlog.jsonl`.

- **embedder.rs**: the delta is 10 changed lines, 100% inside the `model_id` `///` doc comment (3 removed / 7 added, all doc lines). Zero code changes.
- **mod.rs**: every hunk matches what round 1 reviewed (FtsResult docs, `fts_candidates_locked` doc, FULL_SCAN_FALLBACK_CAP comment, NoMatches arm, `sim_weight` block, keyword-boost comment, `sim * sim_weight`, AnchorEmbedder + the two new regression tests, the three updated tests); the only textual deltas vs the round-1-quoted state are the two corrected comment passages above. Line citations from round 1 (1355, 1370; embedder.rs:48-50) drift by ≤1 line, consistent with doc-only growth.
- **backlog.jsonl**: bookkeeping only — item 82b831dc (this plan) pending→failed with note "plan loop did not close (workflow: Planning)", one done-item line removed, one new pending suggestion (323036fd, about that very failure-marking). No source impact.
- **Untracked**: `.coding/plans/cb1914b9.md` (this plan) and the round-1 report itself — expected. One pre-existing unrelated knowledge file (`.coding/knowledge/spec/2026-09-01-anthropic-workspace-id-header-settings-errordial.md`, dated ~3 months before this plan) is untracked; it is not part of this plan's diff and predates it — observation only, no action required for this review (commit-or-not is a separate housekeeping call).

Nothing unexpected in the file set; no source changes beyond the three doc/comment edits.

### Test status

Stated: `cargo test --lib memory::` → 181 passed / 0 failed / 4 ignored, warning-free. Treated as stated per instructions; nothing in code reading contradicts it — the deltas since round 1's verified-green full suite (1716/0/16) are comment/doc text only, which cannot alter behavior or emit rustc warnings under `#![deny(warnings)]` (no `#[allow]`, no dead code introduced).

### Recommendation

Land it — both findings closed, docs now precise where a future weight-tuner and a future embedder author will read them.
