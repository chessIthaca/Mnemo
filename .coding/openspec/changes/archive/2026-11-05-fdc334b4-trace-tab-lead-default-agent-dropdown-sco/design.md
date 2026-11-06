## Decisions

- 2026-11-05: 2026-11-05, worker read-back adjudications: (A) the agent reset path adopts option (c) — TraceView.svelte:226 restores "lead" AND activeTraceChips suppresses the agent chip when the value equals the default, making "lead" the neutral chip-less value (no chip on open, X on any agent chip returns to Lead, "All agents" stays chip-less, the empty-log welcome unchanged). (B) The Insights panel adopts option (i) — the section moves above the list/detail columns and insightsOpen defaults to true (saved prefs still override), so the tab opens with the Lead's charts at the top per the user's ask and Mnemo1's TraceStats placement. Also recorded: the plan's gap description was partly stale — traceAgentOptions already emits a Lead option (second, after "All agents") and the backend agent_matches (trace_view.rs:367-379) already handles "lead"; the real gaps are the default value, the option order, the reset path, and the panel placement.

## Interfaces

- 2026-11-05: The agent filter value space gains the stable string "lead" alongside the existing numeric worker/agent-id values and "all"; matching for "lead" is by attribution.actor_kind === "lead" (null-attribution rows never match it). DEFAULT_TRACE_FILTERS.agent = "lead".
- 2026-11-05: traceAgentOptions emits Lead first, then the observed workers, then All — labels via the existing actorLabel.
- 2026-11-05: File ownership: this plan owns mnemo2/src/lib/trace.ts, mnemo2/src/components/TraceView.svelte and the affected test files only.
