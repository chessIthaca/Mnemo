+++
title = "approving a Mnemo2 rebuild also approves force-quitting what blocks it"
created = "2026-11-09"
evidence = "The user said so on 2026-11-09 after a rebuild stopped to ask before force-ending a stuck pid."
+++

When the user approves `npm run rebuild:mnemo2`, that includes force-quitting Mnemo2 if a normal quit fails or times out, because the rebuild already means the app closes. So on a failed quit, force-end the blocking Mnemo2 and rerun without asking again; this covers `mnemo2-mcp quit --force`, which interrupts running worker turns but keeps their branches and transcripts. Confirm the pid really is mnemo2.exe, end nothing else, and report what was ended and whether any turn was interrupted. The rebuild itself still needs approval, since it closes the app the user is working in. The script deliberately never kills a process, so the force step is the lead's, not the script's.
