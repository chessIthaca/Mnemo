+++
title = "app shell stops re-rendering per streaming frame — InflightBar self-subscribes (outer gate + inner panel), shell memoized"
created = "2027-01-06"
+++

DECISION (mem-perf review HIGH 1, plan 00d88e13, wt/agenticcoding): the app shell no longer re-renders on every streaming frame. App.tsx's whole-agent subscription (`s.agents[s.activeAgent]` — identity changes on every rAF streaming flush because appendStreamingText spreads the agent per flush) was deleted; InflightBar now self-subscribes by its stable agentId.

Design: InflightBar.tsx is split into a thin self-subscribing outer gate (`export function InflightBar({ agentId })` → `useAgentStore((s) => (agentId != null ? s.agents[agentId] : undefined))` → `if (!state) return null;` → `<InflightBarPanel state={state} agentId={agentId} />`) + the unchanged inner `InflightBarPanel`. The split exists because the panel destructures `state` at the top and feeds hooks (useCountUp etc.), so it cannot handle undefined state in one component — the outer gate guarantees a defined slice. App keeps only stable-identity subscriptions (activeAgent number, rightPanelVisible, rightPanelWidth, actions) so it no longer re-renders per frame.

Complementary hardening: Sidebar/InputBar/StatusBar (prop-less, self-subscribing) are wrapped in `memo(function X() {...})` so future App-level re-renders can't drag them along. MainPanel/RightPanel deliberately NOT memoized (MainPanel must re-render during streaming; the review named exactly those three).

Reusable pattern: "self-subscribing outer gate + inner panel" for store components that destructure a possibly-absent slice into hooks. Pinned by frontend/src/App.shellRender.test.ts (`?raw` source-contract: App must not contain `s.agents[s.activeAgent]`; InflightBar must contain `s.agents[agentId]` + the null gate; the three shells must contain `= memo(function`). Review: PASS, 0 findings (.coding/reviews/2027-01-06-app-shell-streaming-rerender-review.md).
