## Verdict: PASS (all findings resolved)

Original verdict: FINDINGS (1 high, 4 low) — all findings resolved in working commit:

Review of ALL uncommitted changes for plan **66e0ddb4** ("Implement prompt caching optimizations: Anthropic ephemeral cache_control, split system blocks, and state-stable tool anchoring").

**Summary:** Mechanisms (1) split-system-blocks-with-cached-head and (2) last-tool breakpoint are correctly implemented per Anthropic's documented caching model and are panic-free across every edge case I probed by direct code walk. Mechanism (4) — 3-tier tool ordering — is deterministic and robust against unknown tools, but its Class-1 roster misclassifies three names whose visibility varies by state/runtime, breaking its own stated byte-stability invariant at exactly one transition boundary. Mechanism (3) — the turn-before-current breakpoint — is structurally valid at the API level yet economically self-defeating under this plan's own architecture: an every-turn-volatile system block sits *between* breakpoints #1 and #3 in Anthropic's cumulative prefix-hash chain, so breakpoint #3 can essentially never hit across turns while still paying premium write costs on every request — an active cost regression relative to not marking history at all.

**Verification basis:** full `git diff HEAD` inspected; `src/provider/anthropic.rs` L229–533 read directly (`build_request_json`, `attach_ephemeral_cache_control`, `message_to_json`, `tool_result_block`); `src/tool/mod.rs` L275–500 (`ToolFilter::allows`), L789–845 (`schemas`), L886–919 (`priority_class`) read directly; Anthropic prompt-caching documentation fetched live during this review ([platform.claude.com/docs/en/build-with-claude/prompt-caching]). Implementer-recorded test runs green (`provider::anthropic` 41 passed; `tool::` 606 passed; full suite 1850 passed, warning-free under `#![deny(warnings)]`). Out-of-scope WIP (`src/provider/stream.rs`, `.coding/knowledge/decision/*.md`, `.coding/backlog.jsonl`) excluded from findings per scope rules.

---

## HIGH FINDING

### H1 · Breakpoint #3 is defeated by volatile-tail churn between breakpoints #1 and #3 → guaranteed premium writes every turn

**Where:** `src/provider/anthropic.rs` L229–256 (system split) and L307–320 (last-tool + `messages_json[len - 2]` anchoring).

**The mechanism being violated.** Anthropic keys each breakpoint over the *cumulative* prefix, ordered **tools → system → messages**. From the docs fetched during this review:

> "Because the hash is cumulative, covering everything up to and including the breakpoint, changing any block **at or before** the breakpoint produces a different hash on the next request."

**What this code does:**

- bp#2 = H(tools) — stable across turns ✓
- bp#1 = H(tools ∥ sys[0]) — stable across turns ✓
- bp#3 = H(tools ∥ sys[0] ∥ **VOLATILE_TAIL** ∥ history[..len−2]) — mutates EVERY turn ✗

The volatile system blocks ("workflow progress / step counter / recalled memories" — which this plan's own backing backlog item states "changes every single turn") are emitted after block[0] inside top-level `system`. Since prefix order places all of `system` before any message content, the volatile tail lies *at-or-before* breakpoint #3 in prefix order ⇒ its cumulative hash differs on every request ⇒ lookback never finds any prior write ⇒ **a fresh cache entry covering essentially all tokens through end-of-history is written at +25% premium on every single request**, while reading back almost nothing beyond what breakpoint #1 already delivers independently through its own lookup window.

Net economics versus simply *not attaching* breakpoint #3:

| | history-token billing per turn |
|---|---|
| without bp#3 | ~100% of base rate |
| with bp#3 as implemented | ~125% of base rate |

The only recurring hits available are same-payload retries after transient errors — narrow value nowhere near offsetting guaranteed per-turn premium writes across multi-turn sessions. This directly contradicts deliverable claim "(3) … In multi-turn chat, this yields a cache hit on all previous turns," which holds only when **nothing between stable head anchor and moving anchor mutates** — Anthropic's own cookbook multi-turn example works because its top-level system prompt is static there. This plan introduces both halves of an unobserved interaction: mechanism #1 creates mid-chain churn; mechanism #3 depends on its absence. Unit tests cannot catch this because it manifests purely provider-side in usage economics.

What stays intact regardless of conversation length: breakpoints #1/#2 still hit via their own independent lookups even when >20 blocks separate them from later anchors ("Adding a second breakpoint … starts a second lookback window there"). So mechanisms #1+#2 deliver their promised value alone.

**Recommended fixes:**

- **Option A (recommended now):** drop bp#3 entirely — strictly better than shipping it given the table above (~25% savings over the current combo).
- **Option B (follow-up plan):** relocate volatile_tail out of top-level `system` into an uncached message-side position so nothing churns between anchors; restores documented moving-breakpoint semantics and delivers claimed value genuinely. Larger refactor touching build ordering semantics with downstream-consumer risk — better scoped as its own follow-up plan than patched mid-review.
- **Option C:** gate bp#3 behind config default-off until Option B lands.

---

## LOW FINDINGS

### L1 · Class‑1 roster misclassification breaks byte-stability invariant at Executing→Reviewing

**Where:** `src/tool/mod.rs` L886–919 (`priority_class` / `UNIVERSAL_BASE`), cross-checked against L275–500 (`ToolFilter::allows`) and L823 (`schemas`).

**What:** The Class‑1 doc comment claims "Universal base tools visible across all workflow states" and lists `backlog_*`. But:

- `backlog_add` / `backlog_status` are explicitly NOT visible under `ToolFilter::Reviewing` (L429–441; that arm's own comment says "backlog_add and backlog_status — both mutations — are NOT available in Reviewing").
- `load_tools` is conditionally dropped whenever `hidden_groups_for(filter)` returns empty (L823) — runtime-dependent presence.

All three sort mid-run alphabetically within Class 1 (`ask_user < backlog_add < backlog_list < backlog_status < … < load_tools < read_files …`). Their disappearance during Executing→Reviewing **shifts** every subsequent Class‑1 name instead of only truncating the tail, falsifying the stated invariant "leaving the Class 0 + Class 1 prefix 100% byte-stable" at exactly that boundary. The new test `state_transition_preserves_class0_and_class1_prefix` covers only Planning≡Executing — precisely the pair where all three are present in both states, so it passes while the invariant still fails on E→R.

**Why LOW:** hot-path Planning→Executing is unaffected (both states include all three); E→R occurs once per plan lifecycle, and at that boundary downstream Class‑2 churn (finish/spawn_agent/git/file_edit appearing) dominates invalidation anyway. Practical damage small; defect mainly doc/invariant accuracy plus test coverage gap.

**Fixes:** reclassify `backlog_add`, `backlog_status`, `load_tools` into Class 2 tail (they're state/runtime-gated anyway), or amend doc wording to claim stability only for genuinely-universal names; add an Executing≡Reviewing equivalence assertion mirroring the P≡E test.

### L2 · attach_ephemeral_cache_control can land on a thinking block

**Where:** helper `attach_ephemeral_cache_control` plus raw-echo path interaction (`Message::raw` verbatim echo).

Anthropic docs state explicitly: "Thinking blocks cannot be cached directly with cache_control." The raw echo returns provider assistant content verbatim; when an assistant turn ends in a thinking-only block (rare but possible), breakpoint #3's helper inserts `cache_control` onto that block. API behavior on violation is unverified from here (possible 400 or silent ignore) — a latent robustness risk rather than proven breakage.

**Fix:** walk backward past `"type": "thinking"` / `"redacted_thinking"` blocks to nearest cacheable type (`text`/`tool_use`/`tool_result`/`image`); skip attachment if none exists. Cheap defensive guard.

### L3 · Doc sync: nothing stale exists, but one PLAN.md line is warranted post-fix

Grep of README.md/PLAN.md for "cach" returns zero matches — no stale docs to correct. Informational per review expectations: once H1 resolves and caching behavior settles, add one line to PLAN.md's provider-strategy section noting explicit Anthropic breakpoints + 3-tier tool anchoring so future plans don't re-derive it. Not blocking.

### L4 · Historical review-artifact verdict-line edit (informational)

`.coding/reviews/2026-12-21-provider-waiting-timeouts-quantization-review.md` had its verdict line rewritten FINDINGS→PASS with resolution note + commit pointer (1c979c4) — in-scope file #3 for this plan. The edit is transparent: original verdict stated inline right below, findings preserved verbatim. Accepted as honest resolution reporting; going forward prefer appending an addendum section over rewriting historical verdict lines so audit trails stay greppable by original verdict. Not blocking.

## VERIFIED SOUND

- Breakpoint budget respected: ≤3 explicit breakpoints of the 4 allowed.
- Nested-prefix structure valid per docs; each breakpoint gets its own independent lookback window, protecting sys-head/tools entries even when history grows past 20 blocks.
- Lookback-window alignment: runs of consecutive tool_use/tool_result blocks count as ONE lookback position each per docs; coalescing adjacent tool results into one user message plays well with this.
- Panic-free edge handling verified by direct code walk: empty tools array (`last_mut()` None → no-op); single-message chat guarded by len ≥ 2; empty content array (`last_mut()` None); string-shaped content (`as_array_mut()` None → graceful no-op); blank/missing system texts skipped; `system` field omitted when no system blocks exist. All current wire shapes from `message_to_json` emit content arrays, so the helper engages normally.
- Unknown-tool robustness: unlisted names fall through to Class 2 tail — appending new tools never perturbs ordered prefix.
- Deterministic ordering: stable sort by `(priority_class(name), name)` with unique names yields total order independent of HashMap iteration order across restarts.
- Platform neutrality clean: pure Rust + serde_json manipulation on both changed source files — no platform-specific APIs/paths/shell syntax.
- Security clean: cache_control values are hardcoded literals only; no user-derived content flows into placement decisions beyond structural position indexes.
- Tests added/updated match described scope and pass per implementer-recorded runs (41 / 606 / full-suite 1850).

## OUT OF SCOPE

Per scope rules I did not formally review `src/provider/stream.rs` (identity-key merge fix appears well-reasoned from diff read but unreviewed), `.coding/knowledge/decision/*.md`, `.coding/backlog.jsonl`.
