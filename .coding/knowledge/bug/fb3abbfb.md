+++
title = "A closing helper only releases the lead when no other leading helper is left"
created = "2026-11-05"
+++

Symptom: When a second, short-lived mnemo2-mcp helper process that made a tool call exits (for example a one-off `mnemo2-mcp serve` probe), Mnemo2 detaches the external lead even though the main Claude Code session's helper is still connected and leading; its waiter returns 'The Mnemo lead was detached (another session closed or Mnemo2 restarted)'. Observed three times on 2026-11-05. · regression test: a_closing_helper_keeps_the_lead_while_another_helper_led

Full record for plan fb3abbfb (see .coding/plans/fb3abbfb.md for the plan file).

regression test: a_closing_helper_keeps_the_lead_while_another_helper_led · path .coding/plans/fb3abbfb.md · branch wt/mnemo @ 240a1e9c (unmerged — exists only on this branch)
