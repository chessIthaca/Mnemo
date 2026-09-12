# Memory System Review — Self-Improvement Adequacy

Date: 2026-05-21 (resumed session)
Reviewer: main agent (implementation plan step, review-style report)
Scope: `src/memory/{mod,consolidation,strength,embedder,types}.rs`,
`src/agent/turn.rs` (auto-recall + is_durable_tool),
`src/agent/prompt.rs` (TOOL STRATEGY), `src/runtime/agent.rs`
(spawn_consolidation + correction hook), `src/tool/memory/mod.rs`.

## Verdict

The memory system IS structurally adequate for a self-improving agent — the
four-tier pipeline (working → episodic → semantic → procedural) is complete,
consolidation runs at every session end (Cancel AND natural completion) with
full LLM extraction, and the prompt steers the model to feed it. Prior
remediation (selective durable-tool capture, recall decay, working-tier
exclusion by default) fixed the original "stale raw snapshots flood recall"
defect. No blocking correctness bugs found. Two genuine gaps remain, both
quality-of-retrieval issues rather than structural ones.

## What's solid (verified in code)

- **Four-tier model + consolidation pipeline** (`consolidation.rs:20-81`):
  working events → LLM-assisted episodic summary → `extract_semantic_facts` +
  `extract_procedural_workflows` → `delete_working_for_session` prunes the raw
  events. Synthetic fallback when no LLM. Idempotent cleanup.
- **Auto-consolidation at session end** (`agent.rs:444-456 spawn_consolidation`):
  fire-and-forget, `Some(provider)` so full extraction runs, invoked on both
  Cancel and natural completion (not just Cancel — the earlier gap is closed).
- **Selective capture** (`turn.rs:1368 is_durable_tool`): only durable actions
  (file writes, git commit/merge/push, plan/skill lifecycle, memory_write,
  spawn_agent) + `user_correction` (`agent.rs:208-231`) are recorded to working
  memory. Read-only/noise tools are skipped — consolidation gets signal, not
  flood.
- **Recall ranking is sound** (`mod.rs:673-810`): FTS5 pre-filter (top-50 pool,
  O(K) not O(N)), then score = `sim*0.2 + decayed_strength*0.3 +
  keyword_boost(0.3) + tier_weight(0–0.3)`. Working tier is **excluded by
  default** (`types.rs:166 exclude_working: true`) and weighted 0 anyway, so
  raw snapshots can't crowd out distilled facts. Strength decays with
  time-since-last-access. Deterministic title tiebreaker.
- **Auto-recall per turn** (`turn.rs:264-304`): top-5 memories matched against
  the latest user message, injected into the volatile tail; per-turn query cache
  avoids re-embedding on unchanged queries.
- **Session primer** (`turn.rs:330-378`): top-8 strongest semantic+procedural
  memories fetched once per session, appended to the byte-stable head (cache
  friendly), three-state cache prevents per-turn refetch.
- **Prompt steering** (`prompt.rs:150-160`): TOOL STRATEGY names memory_write /
  recall / consolidate and tells the model to proactively persist distilled
  facts — the key "learning agent" lever.

## Findings

### M1 (quality, not correctness) — Recall quality is gated on the optional bundled embedder

`recall` relies on **keyword substring** (`contains_ascii_ci`, weight 0.3) plus a
hash-projection `sim` (weight 0.2) because the default [`HashEmbedder`] is a
random-hash token overlap, not semantic vectors. The genuine semantic embedder
(`BundledEmbedder`, fastembed/ONNX) is **opt-in** via
`[general].bundled_embedding_model` (`client_factory.rs:170 build_embedder`
returns `HashEmbedder` when unset, default `None` per `general.rs:173`).
Consequence: paraphrase-level recall (the model words a query differently than
the stored fact) silently misses until the user configures a model.
**Recommendation (non-blocking):** consider defaulting to a small bundled model
(e.g. all-MiniLM-L6-v2) on first run, or surfacing a one-line hint in the
Memory debug view when the active embedder is `hash`. No code change made here —
it's a product decision with a download-size trade-off.

### M2 (resolved on verification) — LLM-assisted consolidation IS covered

I initially flagged the LLM-extraction path as untested. Verification shows it
is NOT: `consolidates_with_llm_creates_all_tiers` (`consolidation.rs:397`) uses a
sequenced `MockLlm` to drive `synthesize_with_llm` + `extract_semantic_facts` +
`extract_procedural_workflows`, asserting the episodic narrative, the semantic
fact (+ confidence), the procedural workflow (+ steps/frequency), and the
working-tier cleanup. `consolidates_with_llm_falls_back_on_bad_json` (`:489`)
covers the malformed-JSON → synthetic-fallback degradation. The earlier blanket
claim in `reviews/02-quality.md:127` predates these tests. **No gap — withdrawn.**

(One narrow residual, not worth a finding: the consolidation LLM path is tested
with a mock, not against a real provider's JSON-mode quirks — same standing
caveat as every mock-LLM test in the codebase.)

### M3 (informational) — Consolidation failures are silent

`spawn_consolidation` discards results (`let _ = …`, `agent.rs:450-453`). A
failed consolidation (LLM error, DB error) leaves working memory unpruned with
no log. Acceptable for a best-effort background op, but a single
`eprintln!`/trace line on `Err` would make "why didn't this session distill"
diagnosable. (Noted in `reviews/01-architecture.md:125`.)

## Conclusion

The machinery for a self-improving agent is in place and wired end-to-end:
capture (selective) → recall (ranked, decay-aware, primer + per-turn) → distill
(consolidate at session end, full LLM extraction) → prune (working cleanup).
The loop is closed. The two improvements worth a follow-up are (a) making the
semantic embedder the default so recall is paraphrase-robust, and (b) test
coverage for the LLM-extraction path. Neither blocks the current work.
