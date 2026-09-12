+++
title = "close Run-All-dispatched items reliably — check the item's status at closure and stamp done directly if the resolution missed it"
created = "2027-01-07"
+++

When a session completes work for a Run-All-dispatched backlog item, check the item's status at closure (backlog_list or .coding/backlog.jsonl). If it is still pending/in_flight despite the work being complete, stamp done directly via backlog_status — the agent-stamped terminal status wins over the automatic stamp (escape hatch, documented in the run_all.rs module doc "Finished — the one notion" section).

HISTORY (live 2027-01-07, item 207dc316): the closing sequence spans turns — spawn reviewer → END turn → reviewer-finished notification resumes → fix → finish. The turn-resolution gate used to run at the FIRST turn's end, see workflow=Reviewing, and HALT the run — the finish in the later turn never re-triggered the stamp. FIXED the same day (the amplifier fix, plan 96e2862a): the gate now keeps the run armed on non-closures and turn failures — the next turn resolution re-checks, so the finish in the later turn stamps Done automatically. The check-at-closure habit remains the safety net for residual misses (e.g. a run already ended by an intervention, or an item dispatched outside any run); the user's manual "c" continue was the typical surfacing of the old state.
