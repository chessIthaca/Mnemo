---
name: mnemo-status
description: Show a concise status of the Mnemo2 team and plan board. Use only in a Mnemo2 project, when the Mnemo tools (team_status, plan_list) are available.
when_to_use: The user asks for the status of the Mnemo team, the workers or the plans, or types /mnemo:mnemo-status.
argument-hint: "[plan_id]"
---

# Mnemo status

Read-only: change no Mnemo state. The only action allowed is starting the background waiter (step 3).

1. Call `team_status`. If the user gave a plan id ($ARGUMENTS), pass it as `plan_id`. Call `plan_list` for the board.
2. Answer in at most 12 lines:
   - Workers that are not merged: id, plan step, tier or model, status, and open findings. Group them as needs you (blocked, failed, conflict), ready for review or merge, working, idle or stopped.
   - Open plans: id, title, steps done out of total. Mention recently closed plans only if the user asked.
   - What to do next, in one line (for example "review worker X", "restart the waiter", "nothing pending").
3. If workers are active and no background waiter is running in this session, say so and start one as the `mnemo-lead` skill describes.
4. If the Mnemo tools are missing or Mnemo2 cannot be reached, say so in one sentence and ask the user how to proceed. Do not guess the state from Git or files.
