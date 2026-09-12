# Cache-Hit Analysis Round 2 — after the R1–R3 prompt-management fixes

**Date:** 2026-08-13 (session log) / analyzed 2026-08-13
**Source:** `.coding/logs/traces.jsonl` — 119 lines, 86.6 MB, span 04:19–17:23 on 2026-08-13
**Models:** deepseek-v4-pro, deepseek-v4-flash, kimi-k3 (two providers: api.deepseek.com, api.kimi.com)
**Previous baseline:** `.coding/analysis/cache-hit-analysis.md` (pre-R1, 82 requests, 80.7%)

---

## 1. Headline: R1–R3 WORKED — post-fix segment is measurably better

| Metric | Baseline (pre-R1) | This log — pre-R1 segment | **This log — post-R1 segment** |
|---|---|---|---|
| Requests (with usage) | 82 | 17 (ids 1–17, 114–119) | **69 (ids 18–88, 100–112)** |
| Overall hit rate | 80.7% | 80.3% | **87.2%** |
| Requests ≥95% hit | 67% | 59% | **82.6%** |
| Full resets (hit < 50%) | 15 | 4 | **9** (excl. degenerate rows) |
| Tokens wasted on resets | 2,001,688 (17.8%) | 336,921 (17.6%) | **1,209,559 (12.0%)** |
| Projected hit if resets eliminated | 98.5% | 97.9% | **99.3%** |

(The full mixed log — including the kimi interlude, model switches, and pre-R1 rows — shows
78.4% overall; that number is misleading because it mixes eras, models, and cold provider caches.
The segmented comparison above is the apples-to-apples one.)

**What R1–R3 fixed (verified at byte level):**
- The system head (`messages[0]`) is **byte-identical across the entire post-R1 session: 1 distinct
  variant across ids 18–112** (was: rewritten every turn pre-R1). `sysEq=True` on 108/115 adjacent
  pairs (94%), vs. every reset being a head change in the baseline.
- Tool schemas are deterministically ordered (R2) — no cross-restart reshuffle observed.
- The volatile content (`# WORKFLOW STATE` + `# RECALLED MEMORIES`) moved to a trailing `system`
  message (R1). Stable-head residual caching works: resets bottom out at ~2,176–3,584 cached
  tokens (the head), and in-loop pairs hit 99.5–100% with context up to 240K tokens.

## 2. What still resets the cache — and why (root cause, byte-level)

Post-R1, every remaining reset was classified by diffing adjacent requests at the raw-byte level:

| Cause | Pairs | Waste share |
|---|---|---|
| **TAIL-CHANGE — `complete_step` / workflow progress** | 24→25, 29→30, 40→41, 55→56, 61→62, 74→75, 85→86, 88→100, 106→107, 107→108 (10 pairs) | ~1.0M tokens |
| **TOOLS-CHANGE — state transition swaps the tool set** | 20→21, 104→105, 105→106, 110→111 (4 pairs) | ~0.5M tokens |
| **MODEL-SWITCH — pro↔flash↔kimi** | 22→23, 88→89, 90→91, 91→92, 99→100 | small (cold caches) |
| Degenerate rows (usage=0, aborted) | 21→22, 23→24 | n/a |

**The mechanism (empirically established, 83 post-R1 pairs):** DeepSeek's cache reuses the longest
byte-identical prefix **only when the request's LAST message is byte-identical to the previous
request's last message**. Proof pairs:

- `28→29` (hit 87%): tail content identical, 2 messages inserted before it → the common prefix
  (0..65) is reused.
- `29→30` (hit 2.4%): **the tail changed** (`PROGRESS: 0/7 → 1/7`, `CURRENT STEP` text swapped) —
  even though 93% of the request bytes are identical, only the ~2,944-token head is reused.
- `30→31 … 84→85` (99.9% each): tail stable, tool-loop appends → full reuse.

So the volatile tail sitting at the END still breaks the whole prefix whenever it changes — and it
changes on every `complete_step` (progress text) and every new turn (recall results). The R1
placement fixed the head, but the tail is still a full-cache killer.

## 3. Recommendations (ranked by impact/effort)

### R5 — Add a byte-stable sentinel message AFTER the volatile tail (high impact, low effort)
Append one constant `system` message with fixed content (e.g. `"<context footer>"`) as the LAST
message, after the volatile workflow/memories tail. Per the observed cache law, the last message
becomes byte-stable, so every tail change (complete_step, new turn) now costs only the tail +
sentinel (~300–600 tokens) to re-process instead of the entire 150–240K-token context.
- Evidence this works: pair 28→29 already demonstrates prefix reuse with an identical tail but
  earlier differences; a stable sentinel generalizes that to tail changes.
- The model can be told once in the stable head to ignore the footer. ~10 lines in
  `src/agent/turn.rs` (append sentinel after the volatile tail) + a test.
- Expected: eliminates ~10 of the 17 post-R1 resets → hit rate ≈ 97–99%.

### R6 — Keep the tool set stable across workflow states (design note, likely reject)
The 4 TOOLS-CHANGE resets come from the workflow gate swapping the tool array per state
(19↔29↔32 tools — Planning must not expose write tools). Sending a padded/stable superset would
violate the security gate. **Recommendation: accept these 4 resets/session** (each ~150K tokens)
rather than weaken the gate. If ever desired: keep the *schema array* stable and enforce the
state filter at dispatch time — but that changes the security posture; not recommended.

### R7 — Avoid mid-session model/provider switches
DeepSeek and Kimi caches are per-model; switching costs a full reset. The kimi interlude
(ids 89–99) also showed a slow warmup ramp (0 → 29 → 50 → 79 → 87 → 96%), suggesting kimi's
cache needs ~10K+ token prefixes before hitting. Not a bug — a usage note.

### R8 — (already in place, keep) recall cache + deterministic ordering
The turn-loop recall cache keeps the memories block stable within a tool loop (R3) — keep it.
With R5's sentinel, even cross-turn recall changes become cheap.

## 4. Caveats
- Snapshot of a live log; ids 22/24 have no usage (excluded); the log mixes eras — segment
  comparisons are the reliable ones.
- The "last message must match" cache law is empirical (83 pairs, 100% consistent, including the
  model/tool-switch exceptions) — it matches DeepSeek's documented prefix-cache semantics as
  observed here, but is worth re-validating after R5 lands (the sentinel makes the last message
  constant by construction).
- ttft_ms is ~0 on all rows (provider doesn't report it); hit rate is the reliable signal, and it
  agrees perfectly with the byte-diffs.

## 5. Code references (for R5)
| Location | Role |
|---|---|
| `src/agent/turn.rs` ~288–317 | system prompt / volatile-tail assembly (append sentinel after the tail) |
| `src/agent/prompt.rs` | `workflow_section` + `format_recall_context` (unchanged; still the tail body) |
| `src/provider/trace.rs` | trace capture (no change) |
