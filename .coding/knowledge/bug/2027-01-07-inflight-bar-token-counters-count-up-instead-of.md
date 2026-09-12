+++
title = "inflight bar token counters count up instead of updating per event"
created = "2027-01-07"
+++

Symptom: inflight bar ↑/↓/🧠/ctx token counters animated toward targets via ~400ms rAF count-up — distracting during streaming (user report 2027-01-07, backlog d9b560a0); expected per-event updates like the trace graph. Root cause: useCountUp interpolated cosmetically on top of already-event-driven values (live chars/4 estimates per stream chunk; tokenUsage on Usage events). Fix: InflightBar.tsx renders the four raw values directly; useCountUp.ts deleted (sole caller gone). Regression test: InflightBar.test.ts "updates the ↓/🧠 token counters per event via the live estimates — no count-up interpolation".
