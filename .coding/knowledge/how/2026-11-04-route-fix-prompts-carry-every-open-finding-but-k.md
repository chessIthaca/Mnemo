+++
title = "Route-fix prompts carry every open finding, but keep each fix round to one message"
supersedes = "2026-11-03-a-multi-finding-fix-round-usually-stops-after-th"
created = "2026-11-04"
+++

Since 2026-11-04 the team_route_fix prompt includes the full detail of every open finding on the worker, so a multi-finding round no longer needs the other findings sent by message. Two habits still hold. (1) Do not send a team_message right after team_route_fix: it interrupts the fix turn and produces a "finished its turn" notice with no commit while the worker is still busy. Check that the worker is busy (team_compact refuses with "busy") before nudging. (2) A fix round on a strong model can cost several dollars, so put everything the worker needs into the finding detail.
