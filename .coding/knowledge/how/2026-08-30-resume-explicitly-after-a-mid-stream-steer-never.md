+++
title = "resume explicitly after a mid-stream steer — never go silent mid-flow"
created = "2026-08-30"
+++

HOW (user feedback 2026-09-19): when a mid-stream steer interrupts an in-flight task, the agent sometimes goes SILENT instead of resuming — the user sees this as "stopping in the middle of a flow" and finds it odd/disorienting. Root cause: the steer cuts the turn (the announced tool call gets no terminal result — the known ToolCard-stuck-running bug a0572e85), and on resume the agent doesn't explicitly restate where it was. FIX (behavior): after an interruption, ALWAYS acknowledge it ("picking up where I left off — I was on step N of plan X") and resume the in-flight work or restate the next action. Never go silent after a steer. This is a habit, not a code change — the underlying turn-cut bug is already fixed (merged 7f4a1c1); the residual is the agent's resume behavior.
