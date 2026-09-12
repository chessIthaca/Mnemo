+++
title = "429-fallback viability = live count + alternate's own threshold, OR'd with the window comparison"
created = "2027-01-07"
+++

429-fallback alternate-endpoint viability (backlog a117e827, commit 739c68a on wt/agenticcoding) is decided by the LIVE conversation size, not context windows: viable ⟺ alt_window >= live_tokens + alt_cm.effective_summarize_at() OR alt_window >= current_window (window-only when live == 0, i.e. no request built yet). Design rationale:
1. Headroom = the alternate's OWN effective summarize threshold — the immediate serve plus one growth step fits before the alternate's compaction fires; the alternate's preflight hard-ceiling + compaction remain the last line of defense for further growth.
2. The OR is load-bearing (round-1 review HIGH-1): with fill_rate > 0.5 the headroom bar live ≤ alt×(1−fill) sits BELOW the incumbent's own operating band cur×fill, so the headroom rule alone would reject equal- and larger-window alternates the old window comparison always served. The OR makes the rule strictly dominant over the pre-fix acceptance set for ANY fill rate (configurable 0.05–0.95) — no UI-reachable value can regress failover below pre-fix behavior.
3. The live count basis: AgentLoop.live_token_count (AtomicUsize, Relaxed — same-task await-chain sequencing), stored at the top of every request iteration in run_turn right after token_accounting.update (messages + tools-schema overhead, the harness's canonical count, EXCLUDING per-request head/tail scaffolding — absorbed by the headroom). The request built from that count is the one that can 429.
4. Sticky-entry semantics unchanged: viable → sticky entry recorded; not viable → none (turn fails once with the actionable message, later turns keep the original endpoint).
NOTE: the loop's fill_rate comes ONLY from the with_fill_rate builder (from_config hardcodes 0.5; the app factory build_inner src/agent/factory.rs:683 passes the settings value) — AgentLoopConfig.context_manager's fill parameter does NOT set it. Tests are hermetic (explicit .with_fill_rate / caps on provider doubles; never read the config UI/settings/endpoints.toml).
Four regression tests pin the rule (two verified red on the code they guard against). Reviews: .coding/reviews/2026-09-08-429-fallback-live-token-count-review{,-round2,-round3}.md (round-3 PASS).
