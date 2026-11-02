+++
title = "The lead reviews every worker deliverable itself, running it only when judgement says so"
created = "2027-01-11"
+++

The user's standing rule: every worker deliverable gets a real review by the lead (Claude Code or the built-in lead) before team_review and team_merge: read the diff unanchored, check it against each point of the user's request (professional, functional, genuinely useful), and route fixes back to the worker (team_route_fix / review_fix) before merge. A passing test count alone is not a review.

Running the change (rendering UI in the worktree's dev server, exercising a command or app path) is a judgement call, applied conservatively: do it when reading cannot settle whether the change works or looks right (new or redesigned UI, behaviour that tests cannot show, risky integration). Skip it when the diff plus targeted tests settle the question. The user warned that testing is often too aggressive; do not run everything by default. Record what was actually run in team_check and what was judged in team_review evidence.
