# Trace Log Cache-Hit Analysis — prompt management optimization

**Date:** 2026-08-13
**Source:** `.coding/logs/traces.jsonl` (live trace log, snapshot at analysis time)
**Models:** deepseek-v4-pro, deepseek-v4-flash
**Scope:** quantify current cache-hit rates, identify the exact prompt-management
factors that break the provider's cache prefix, and recommend fixes.

---

## 1. Headline numbers (82 requests with reported usage)

| Metric | Value |
|---|---|
| Total prompt tokens | 11,223,551 |
| Total cached tokens | 9,054,592 |
| **Overall cache hit rate** | **80.7%** |
| Requests with ≥95% hit | 55 / 82 (67%) |
| Full cache-reset events (hit < 50%) | **15** |
| Tokens wasted by resets (prompt−cached on reset requests) | **2,001,688 (17.8% of all prompt tokens)** |
| Projected hit rate if resets were eliminated | **98.5%** |

**Conclusion: the system is already near-optimal *within* a tool loop**
(consecutive requests in the same workflow step hit 99–100%), and the entire
cache inefficiency is concentrated in ~15 full-prefix resets, each discarding a
~150K-token context.

## 2. How the cache behaves (evidence)

The provider (DeepSeek) caches the **byte-prefix** of the request. Two facts
fall out of the trace:

- **Stable prefix survives across sessions.** Requests 1 and 43 (fresh
  contexts, different sessions) both report exactly `cached=2176` — the
  byte-stable head of the system prompt (preamble + both constitution blocks,
  ~2,176 tokens) is still in the provider's cache.
- **Any byte change in the head zeroes everything after it.** Every reset
  request reports `cached=2176/2688/2816` — only that same stable head is
  cached; the other ~150K tokens of prompt are re-processed.

Adjacent-request diff (mirroring the Trace view's `compareMessages`), key rows:

```
pair=20->21 hit=2%   k=0/48 firstDiff=0 role=system sysEq=False toolsEq=True
pair=26->27 hit=2%   k=0/60 firstDiff=0 role=system sysEq=False toolsEq=True
pair=38->39 hit=1.7% k=0/85 firstDiff=0 role=system sysEq=False toolsEq=True
pair=10->11 hit=2%   k=0/27 firstDiff=0 role=system sysEq=False toolsEq=True
pair=8->9   hit=1.6% k=0/23 firstDiff=0 role=system sysEq=False toolsEq=False
pair=51->52 hit=1.5% k=0/93 firstDiff=0 role=system sysEq=False (model switch)
pair=70->71 hit=1.4% k=0/131 firstDiff=0 role=system sysEq=False (state transition)
```

**Every single reset is `firstDiff=0` — the system message (message[0]) changed.**
When `sysEq=True` (the ~99% pairs), the cache prefix holds even at 140+ messages.

### 2.1 Pinned example — the step-progress reset (pair 20→21)

Both requests have **byte-identical memories blocks** and **byte-identical tools
arrays** (32 tools, same order). The system message differs in exactly one
region — the workflow section:

```
id=20: PROGRESS: 1/5 steps complete
       CURRENT STEP: "**Rework InflightBar counters** — ..."
id=21: PROGRESS: 2/5 steps complete
       CURRENT STEP: "**Unify StatsView formatters** — ..."
```

That is the *entire* difference. Because it sits inside message[0], the
provider re-processes ~138K tokens of conversation that were cached one second
earlier.

## 3. Root cause — volatile state is embedded in the cache-prefix head

`src/agent/turn.rs:288-317` rebuilds the **entire system prompt every turn** and
replaces `messages[0].content` in place. `build_system_prompt`
(`src/agent/prompt.rs:43-79`) concatenates, in order:

1. `CODING_SYSTEM_PREAMBLE` (stable) — `prompt.rs:13`
2. Global + project constitution (stable; re-read from disk, mtime-guarded) — `prompt.rs:55-64`
3. **`# RECALLED MEMORIES` block** (volatile: per-query recall results, scores,
   contents; `format_recall_context` `prompt.rs:82-94`)
4. **`# WORKFLOW STATE` section** (highly volatile: plan title, GOAL, `PROGRESS:
   N/M steps`, `CURRENT STEP` text, parent breadcrumbs, state text —
   `workflow_section` `prompt.rs:97-231`)

Items 3–4 change constantly *between* steps/turns, and they sit in the middle
of message[0] — **before** the conversation history. Result: any of

- a `complete_step` (PROGRESS + CURRENT STEP text change) — the most frequent,
- a workflow-state transition (Planning→Executing→Reviewing→Complete: also
  swaps the tools array),
- a new turn with a different user query (memory recall results change),
- a constitution edit,

invalidates the entire cache prefix, not just the changed bytes.

Secondary contributors (unavoidable or lower-frequency):

- **Tools array changes on state transitions** (19↔32 tools, e.g. pair 8→9,
  42→43, 70→71, 77→78). `ToolRegistry::schemas` (`src/tool/mod.rs:355-375`)
  rebuilds the array each turn and iterates a `HashMap` (`mod.rs:357`) — stable
  within a process run, but **not guaranteed across restarts** (a restart
  mid-session can reshuffle tool order and reset the cache for no semantic
  reason).
- **Model switch** (pair 51→52, pro→flash): provider cache is per-model —
  inherent, nothing to fix.
- **Summarization** (`src/agent/context.rs:115-128`): rewrites the middle of the
  conversation (inserts a summary system message at index 1). It correctly
  clones the head (`context.rs:126`), so the stable prefix survives, but the
  summary itself is a new prefix break — inherent to context management.
- Requests 75–76 reported no usage (excluded from aggregates); the log is
  live-appending.

## 4. Recommendations (ranked by impact / effort)

### R1 — Move the volatile sections out of the system message (high impact, low effort)

Split the system message into two parts:

- **message[0] — stable head only:** preamble + constitution. This block is
  *already proven byte-stable* (the 2,176-token residual); it should never be
  rebuilt except on a real constitution edit.
- **A trailing message appended after the conversation** (e.g. a fresh
  `system`-or-`user` message carrying `# WORKFLOW STATE` + `# RECALLED
  MEMORIES`). It changes per step/turn, but sits at the **tail**, exactly where
  the provider's prefix cache is immune.

This mirrors the pattern the tool loop already uses (append-only messages →
99-100% hits). Expected result: `complete_step` progress bumps and new turns
become free, eliminating the ~10 step/state resets — the single biggest
win. Implementation touches `src/agent/turn.rs` (build two messages instead of
one, append the volatile one after the history) and
`src/agent/prompt.rs` (split `build_system_prompt` into `stable_head` +
`volatile_tail`; keep `workflow_section` and `format_recall_context` as-is).

### R2 — Deterministic tool-schema ordering (low effort)

Sort `ToolRegistry::schemas` output by `name` (`src/tool/mod.rs:357-374`).
Eliminates cross-restart cache resets from `HashMap` iteration order.

### R3 — Stabilize the memories block (low effort)

- Order recall results deterministically (score desc, then title) and drop the
  redundant `score: 0.00`/`0.7x` floats from the rendered block if unused
  (`prompt.rs:82-94`) — or accept it once R1 moves memories to the tail.
- The turn-loop recall cache (`turn.rs:253-275`) already keeps the block stable
  within a tool loop — keep that.

### R4 — Batch workflow-state transitions where possible (design note)

Each state transition rewrites the workflow section *and* swaps the tools
array, so it resets the cache by construction. There is no way around the
transition itself; just avoid *extra* churn around it (e.g. don't re-emit the
full workflow section on unrelated events, don't toggle tool sets per micro-
state).

### What NOT to do

- Do **not** freeze the workflow state or memories to force cache hits — the
  model needs current plan/progress. The fix is *placement* (tail), not
  staleness.
- Do **not** add timestamps or other per-request values anywhere in the head.

## 5. Code references

| Location | Role |
|---|---|
| `src/agent/turn.rs:288-317` | System prompt rebuilt + `messages[0].content` replaced every turn |
| `src/agent/prompt.rs:43-79` | `build_system_prompt` — head order: preamble → constitution → memories → workflow |
| `src/agent/prompt.rs:82-94` | `format_recall_context` — volatile memories block |
| `src/agent/prompt.rs:97-231` | `workflow_section` — PROGRESS/CURRENT STEP/GOAL/breadcrumbs (changes per step) |
| `src/tool/mod.rs:355-375` | `ToolRegistry::schemas` — per-turn rebuild, `HashMap` order |
| `src/agent/context.rs:115-128` | Summarization inserts summary at index 1 (inherent prefix break; head preserved) |
| `frontend/src/components/views/LlmTraceView.tsx:494-511` | Reference implementation of the message-diff used here (`compareMessages`) |

## 6. Caveats

- Snapshot analysis of a live log; ids 75–76 have no usage record and are
  excluded.
- `ttft_ms=0` on nearly all requests suggests the provider doesn't report TTFT
  here — hit-rate is the reliable signal, and it agrees perfectly with the
  prefix-stability diff (no false positives: every reset pair has `sysEq=False`,
  every stable pair has `sysEq=True`).
