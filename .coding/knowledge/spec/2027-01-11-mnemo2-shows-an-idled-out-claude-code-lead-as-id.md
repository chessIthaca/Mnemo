+++
title = "Mnemo2 shows an idled-out Claude Code lead as idle, not gone"
created = "2027-01-11"
+++

When the external lead (Claude Code) is released for silence (10 minutes without a Mnemo call or waiter), Mnemo2 remembers it as idle instead of forgetting it. Backend: TeamRuntime keeps ExternalLead.idle_since_ms, set by the idle sweep (mark_external_lead_idle) and cleared on the next attach or on a real detach (/lead/detach, project close or lead reopen). The sweep emits mnemo2://external-lead {attached:false, idle:true, root}; the external_lead_status command reports idle_since_ms and keeps root while idle. Frontend: ExternalLeadState.idleSinceMs (optional) is set from that event or status, and the top bar badge reads "Lead: Claude Code (idle)" in muted colours, with a tooltip saying worker notices go to the built-in lead until Claude Code's next call. Notice routing is unchanged: while idle the built-in lead receives notices; the next Mnemo tool call or waiter re-attaches and the badge returns to "Lead: Claude Code (external)". Why: the badge vanishing after an idle pause looked like Claude had stopped leading (user report 2026-11-01).
