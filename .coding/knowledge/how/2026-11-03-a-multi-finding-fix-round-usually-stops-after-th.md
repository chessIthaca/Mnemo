+++
title = "a multi-finding fix round usually stops after the routed finding (durable)"
created = "2026-11-03"
status = "superseded"
+++

team_route_fix hands the worker ONE finding (the finding_id). Even when the instructions list several findings, workers often fix only the routed one, commit, report and end the turn. Expect to follow up with team_message listing the remaining findings. Wait for the "finished its turn" notice of the LAST round before dispatching the closing audit; a notice arriving right after a team_message usually belongs to the previous turn.
