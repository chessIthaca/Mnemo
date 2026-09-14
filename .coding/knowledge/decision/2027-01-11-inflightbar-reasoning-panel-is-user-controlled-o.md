+++
title = "InflightBar reasoning panel is user-controlled only (persisted, default closed)"
created = "2027-01-11"
+++

DECISION (user report 2027-01-11): the InflightBar reasoning/activity panel is opened/closed ONLY by the user — no code path may expand it, and a fresh install starts collapsed.

Why it changed: the auto-expand effect fired on the inactive→active reasoning edge (`reasoningActive` / `reasoningBlockStarted` from frontend/src/lib/reasoningPanel.ts), and every turn's `started` event clears the activity log — which re-armed the edge — so each new turn re-opened a panel the user had deliberately collapsed. Introduced 2026-08-22 (plan 0d079599 / backlog #84, review 2026-08-22-per-agent-model-picker-review) as "thinking must stream visibly"; reversed now that the user reports it opening "quite often".

Mechanics: frontend/src/components/chat/InflightBar.tsx holds `const [expanded, setExpanded] = useState(readInflightPanelOpen)`; the ONLY writer is the local `setPanel(open)` (= setExpanded + writeInflightPanelOpen), called by the chevron click (`onClick={() => setPanel(!expanded)}`) and startDrag's collapsed branch. Persisted under localStorage key `mh.inflightPanelOpen` via frontend/src/lib/inflightPanelPref.ts — `parsePanelOpen` treats only the literal "1" as open (absent/""/"0"/garbage → CLOSED), and read/write are guarded like readLs/writeLs in frontend/src/hooks/appearance.ts (no window / throwing localStorage → default).

Invariant to preserve: `reasoningActive` / `reasoningBlockStarted` / `prevReasoningActive` must NOT reappear in InflightBar.tsx — frontend/src/components/chat/InflightBar.test.ts (describe "InflightBar reasoning panel is user-controlled") pins their absence. frontend/src/lib/reasoningPanel.ts + its test were deleted (vitest.config.ts include entry removed; the new inflightPanelPref.test.ts is registered there — an unregistered test file never runs in this repo). docs/FEATURES.md:49 now documents the user-controlled behavior.
