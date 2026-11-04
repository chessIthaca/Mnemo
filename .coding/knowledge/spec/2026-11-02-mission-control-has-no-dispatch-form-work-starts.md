+++
title = "Mission control has no dispatch form; work starts through the lead"
created = "2026-11-02"
+++

Mission control (`mnemo2/src/components/ControlRoom.svelte`) has no dispatch form: workers are started only through the lead (Claude Code's MCP `team_dispatch`, or the built-in lead's team tools). The header (title, live/slots chip, Stop all, Delete old) and the draggable worker list/output split remain.

Consequences worth knowing before touching this area:
- `actions.dispatchWorker` no longer exists in `mnemo2/src/lib/store.ts`; `Test` code that spied on it was deleted.
- `TeamTransport.dispatch` in `mnemo2/src/lib/teamEngine.ts` still exists even though it has no production caller — it is exercised directly by `teamEngine.test.ts` and implemented by several typed `TeamTransport` fakes in store/panel tests.
- The Rust/JS `team_dispatch` IPC command is unchanged; the built-in lead dispatches through it server-side.

`ControlRoom.test.ts` guards the removal: with `board: []` it asserts the title, slot pill, worker list, resize divider and output render while no Plan / Plan step / New worker brief / Dispatch / "No open plan" node exists.

Shipped in commit 059134dd.
