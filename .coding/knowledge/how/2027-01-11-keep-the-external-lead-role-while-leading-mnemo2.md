+++
title = "keep the external lead role while leading Mnemo2 from Claude Code"
created = "2027-01-11"
+++

The external lead (Claude Code via the mnemo plugin) detaches after 10 minutes with no Mnemo call and no running `mnemo2-mcp wait`; Mnemo2's built-in lead then takes over notices and may review, route fixes and merge on its own. Keep the waiter running while waiting on the user. After team_route_fix the worker's turn often ends at once; a team_message sent while a worker turn runs ends that turn and the next turn picks the message up, so check team_status and the worker branch before re-prompting instead of sending repeated nudges. Mission control's "N workers" header counts all records, including merged and read-only audit workers.
