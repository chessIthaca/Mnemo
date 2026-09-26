+++
title = "Dashboard view live-updates while its tab is open (plan 864ad68e)"
created = "2027-01-11"
+++

The token Dashboard self-refreshes — no close/reopen needed (user report 2027-01-25).

WHERE: frontend/src/components/views/DashboardView.tsx — module const `POLL_MS = 2000`; the mount-once `useEffect(() => { void refresh(); }, [refresh])` is now a mount-scoped polling effect (`[]` deps + the house eslint-disable): a `tick` returns early when `cancelled`, when `inFlight.current` (useRef guard against stacked IPC), or when `document.hidden`; immediate `void tick()` on mount; `setInterval(() => void tick(), POLL_MS)`; `window` "focus" + `document` "visibilitychange" listeners for an at-once refresh on return to the app; teardown sets `cancelled`, `clearInterval`s, and removes both listeners. `refresh()` (getSavingsStats + getProjectStats; pricing comes from the agent store, NOT polled) is unchanged, as are the loading/empty/body gates and all markup.

WHY IT IS SAFELY BOUNDED TO "THE TAB IS OPEN" (the load-bearing premise, verified): closing the right panel unmounts it — frontend/src/App.tsx:842-851 renders `{rightPanelVisible && <RightPanel />}` — and RightPanel.tsx:130-136 renders only the active tab's component, so switching tabs also unmounts. No CSS-hide path exists, hence no gating on a panel-open store field was needed.

TEST PIN: frontend/src/components/views/DashboardView.test.tsx — a `?raw` source-assertion block (house idiom, frontend/src/components/chat/InflightBar.test.ts:115) pinning POLL_MS, the setInterval call, document.hidden, inFlight.current, both listener registrations and removals, clearInterval. There is no jsdom/fake-timer path in this repo, so source assertions are the sanctioned pin for such wiring.

DOCS: docs/FEATURES.md + README.md Dashboard sentences carry the live-update clause; docs/CONFIGURATION.md's terse pointer stayed correct (it makes no cadence claim).

Commit 4a2bd99 on wt/mnemo; review .coding/reviews/2026-09-26-dashboard-live-update-864ad68e-review.md (PASS, 0 findings). Verified: frontend tsc 0 + 1297 vitest tests; root cargo test 2718/0/5 + 19 + 1/1.
