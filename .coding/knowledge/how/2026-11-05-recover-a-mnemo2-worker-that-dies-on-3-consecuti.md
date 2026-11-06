+++
title = "recover a Mnemo2 worker that dies on 3 consecutive tool errors"
created = "2026-11-05"
status = "superseded"
+++

Signature (4 occurrences on 2026-11-05: traceUiStatus01, boundaryFix01, specGate01, traceLeadScope01): a Mnemo2 worker's turn aborts with "Aborting turn: 3 consecutive tool errors (the model may be stuck)", the worktree is clean at base (all requests were reading), and Laya nudges one tier up. Recovery that worked every time: (1) team_status detail plus a worktree git check to confirm the failure and that nothing was written; (2) team_resume with the nudge's suggested tier (one rung up); (3) a team_message telling the worker its worktree is clean, to go straight to writing rather than more reading, with the tool discipline: never resend an identical failing call — read the error and fix the named parameter; ops-array anchor items carry ONLY the fields they use (no null placeholder keys); never pass lines and old_string in the same file_edit call; after two failures of the same tool, STOP and message the lead the error text; (4) a plan progress note. Related: "finished its turn" notices also fire at question-breaks and turn boundaries — always verify commits exist on the worker branch (git log <base>..HEAD) before reviewing or merging; a clean worktree means the notice is premature.
