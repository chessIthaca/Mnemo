+++
title = "close several tiny review findings in one round, or Mnemo escalates a healthy worker"
created = "2026-11-09"
evidence = "Team notices and team_status on 2026-11-09: costHonest01 routine to medium to high while idle, sidePanelAll01 routine to medium."
+++

review_fix needs a fresh delta team_review before the next fix, so closing N tiny findings one at a time means N review rounds. Any finding still open after two rounds fires the "stayed open after 2 review rounds" nudge, and in Auto mode each one moves the worker up a tier even though the worker did nothing wrong and never ran. Seen 2026-11-09: three comment and CSS findings moved a worker from routine to high with no worker turn, and the same happened with one word in a comment left open across two rounds. No money is spent unless the worker runs again, but the routing log then records escalations that mean nothing. So when several tiny findings exist, send them together with team_route_fix on the current tier, or fold them into one review_fix, or apply one before ever recording the others as open findings.
