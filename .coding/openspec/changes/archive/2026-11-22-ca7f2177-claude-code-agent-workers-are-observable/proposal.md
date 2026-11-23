An agent-tier worker can be seen and steered through the usual Mnemo tools. It is registered with its transcript, its liveness, activity and stalls show in team_status and Mission Control, and it can post progress notes. team_message and team_route_fix return ready SendMessage instructions and record the round, and a new team_integrate lets TeamManager merge another branch into any worker's branch with conflict reporting.

WHY (FactoryGame, 2026-11-22, High tier set to Opus). An agent worker (opusSockets01, model claude-code/opus) sat at "working" with no activity, cost or progress. The lead runs the agent with its own Agent tool, so if it crashes, stalls or is stopped, Mnemo never learns. Its log is hard to find: the Agent tool's task output file can stay empty, and the real transcript is ~/.claude/projects/<project slug>/<session id>/subagents/agent-<agent id>.jsonl, with an agent-<id>.meta.json beside it whose `finished` flag turns true when the agent ends. The agent only reports at the end. Every shell command needs `cd <worktree> &&`, because Claude Code's Agent tool has no working-directory parameter and doesn't keep a subagent's directory between calls. team_message, team_resume, team_route_fix and team_compact refuse agent workers, so records drift. A step that needed another branch's work (an approved art branch) had to copy files by hand, because workers may not merge.

BUILDS ON plans e21c4820 (agent tiers, team_agent_done), e5d93df3 (base_commit, team_run_checks, usage limits) and e52e0be1 (team_tiers, list_changed, escalation onto agent tiers). Steps 1-3 here dispatch only after e52e0be1's steps 1 and 3 have merged, because they share record.rs, lead_tools.rs, control.rs and external_lead.rs.

DECISIONS (settled):
1. Registration: a new lead tool `team_agent_started {worker_id, agent_id, transcript_path?}`.
   - It accepts only claude_agent workers in Working or Fixing.
   - It stores `claude_agent_id` and `claude_agent_transcript` on the WorkerRecord (both Option<String>, serde default, persisted).
   - When transcript_path is missing or the file doesn't exist, Mnemo looks for the newest `~/.claude/projects/*/*/subagents/agent-<agent_id>.jsonl`. If none is found yet, it stores the agent id, retries the lookup on later reads, and says so in the result.
   - The dispatch claude_agent block's prompt is unchanged, but the mnemo-lead guidance (written later) tells the lead to call team_agent_started right after launching its Agent.
2. Liveness and activity. For a registered agent worker in Working or Fixing, Mnemo reads the transcript file:
   - last_activity_ms is the file's mtime;
   - tool_calls counts assistant content blocks of type tool_use;
   - tokens sums message.usage input_tokens, output_tokens, cache_read_input_tokens and cache_creation_input_tokens;
   - finished comes from the meta.json `finished` flag.
   Reading is incremental: remember the byte offset and only parse new lines. team_status (compact and detail) carries `agent: {id, transcript, last_activity_ms, tool_calls, tokens, finished, stalled}`. stalled is true when the worker is Working or Fixing, finished is false, and the transcript hasn't changed for 10 minutes; this is fixed, with no setting.
   A watcher task (every 60 s, only while such workers exist) sends one notice per stall episode: `<id>'s Claude Code agent has written nothing for <N> minutes; it may have stalled or stopped. Check it, then continue it with SendMessage or call team_agent_done.` When finished turns true while the worker is still Working or Fixing, it sends once: `<id>'s Claude Code agent finished; call team_agent_done with its report.`
3. Progress notes: a new tool `team_progress {worker_id, note}`, meant for the agent itself (it shares the lead's MCP connection).
   - It accepts only claude_agent workers in Working or Fixing; note is non-empty, at most 500 characters.
   - It appends an inbox entry (sender ClaudeAgent, a new InboxKind::Progress) and pushes the notice `[TeamManager notice — not the human] worker_id=<id>\n<id> progress: <note>`.
   - Its description says it is for a worker's Claude Code agent to post a one-line note after each milestone.
4. Agent prompt rules (record.rs agent_prompt):
   - Replace the first rule with: `- Work only inside <worktree>. Claude Code doesn't keep your working directory between shell calls, so start every shell command with cd "<worktree>" && (in PowerShell: Set-Location "<worktree>"; ), and read and edit files only by absolute paths under it. Never touch the project's main checkout or any other checkout.`
   - Add: `- Run shell commands in the foreground. Start one in the background only when it will run longer than about two minutes (a full build or test suite).`
   - Add: `- After each milestone (you have read the code, the change is in, the tests pass, you committed), post a one-line note with the Mnemo tool team_progress (worker_id <id>).`
   - Change the Mnemo-tools rule to: `- Leave Mnemo's team_* and plan_* tools to the lead; plan_show and team_progress are the only ones you may use.`
5. Lifecycle tools speak agent.
   - team_message on a claude_agent worker: record the inbox entry as for a Mnemo worker, set status Working when the worker was idle (Completed, InReview or Conflict), and return `continue_agent: {agent_id, worktree, message}` (in result and data) instead of refusing. The text says: `Continue <id>'s Claude Code agent with SendMessage to <agent_id>, sending the message below; call team_agent_done when it finishes.` Without a registered agent_id it says: `No agent is registered for <id>: start a new Agent in <worktree> with this message, then call team_agent_started.`
   - team_route_fix on a claude_agent worker: record the fix round exactly as for a Mnemo worker (fix_rounds, fixer_finding_id, status Fixing, checks and review cleared), and return continue_agent with route_fix_brief's text as the message.
   - team_agent_done accepts Working, Interrupted and Fixing. On Fixing it clears fixer_finding_id and closes the round like the pump's Finished handling does after a fix turn. This supersedes e21c4820's "team_agent_done is called only once per worker": it is called once per agent run.
   - The gate-agent hook allows Agent calls while any claude_agent worker is Working or Fixing.
   - team_resume and team_compact still refuse claude_agent workers, with: `<id> runs as a Claude Code agent: Mnemo can't resume or compact it. Continue it with team_message (which gives you the SendMessage instruction) or start a new Agent.`
6. team_integrate: a new lead tool `team_integrate {worker_id, branch}`.
   - Refused unless the worker is an implementation worker that is not Merged, Merging or Cancelled; has no busy live session, no proof and no check run; is not a claude_agent worker in Working or Fixing; and has a clean worktree. `branch` must be an existing local branch other than the worker's own.
   - In the worker's worktree it runs `git merge --no-ff --no-edit -m "integrate <branch> into <worker branch> (team_integrate)" <branch>`.
   - On conflict it collects `git diff --name-only --diff-filter=U`, runs `git merge --abort`, and returns the error `merging <branch> into <id> conflicts in: <paths>; nothing changed.`
   - On success it appends `{branch, commit, at_ms}` to a new persisted `integrations` list. Checks and review stay tip-bound, so they are invalidated naturally.
   - A Mnemo worker gets the note `[From Mnemo2] Mnemo merged <branch> into your branch at <short>; read git log -1 --stat to see what came in.` prefixed to its next prompt. A claude_agent worker's note goes into the result text for the lead to pass on.
   - Integrating the lead branch is the deliberate catch-up for a stale base.
7. Mission Control log view. The worker transcript API Mission Control already uses for Mnemo workers serves a registered claude_agent worker too: the backend converts the Claude JSONL into the same message shape (user and assistant text, tool calls with their input, and tool results truncated as the panel already does for Mnemo results), read-only. The worker panel shows the transcript path, the agent's last activity, tool calls and tokens, and a "stalled" badge. The Tiers page Run as hint becomes: `When Claude Code leads, this tier runs on your Claude Code agent, and Mnemo follows it through its transcript; the models above are used otherwise.`

VERIFY: the four .coding/checks.toml commands.
