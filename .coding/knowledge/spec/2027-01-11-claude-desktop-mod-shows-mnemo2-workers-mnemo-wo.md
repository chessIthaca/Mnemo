+++
title = "Claude Desktop mod shows Mnemo2 workers (mnemo-workers)"
created = "2027-01-11"
+++

The Claude Code mod at mnemo2/claude-desktop-mod/mnemo-workers/ draws a status entry "Mnemo ▶running ◆waiting" and an AbovePrompt band (Claude Desktop Code tab, also terminal) listing active or needs-you workers: name as a Markdown link, short model (kimi, glm, deepseek-f; -f = flash), status, elapsed. Data: `mnemo2-mcp team-summary --json --cwd <dir>` every 4 s and after each mcp__plugin_mnemo_mnemo__* tool call; a click runs `mnemo2-mcp show-worker <id> --cwd <dir>`, which opens that worker in mission control. Both helper commands are read-only (never attach the external lead, never drain notices). team-summary prints {} unless a running Mnemo2 already has the project open: right after a Mnemo2 restart the band stays empty until the first Mnemo tool call opens the project. Mnemo2's own ModelBadge keeps full model names by decision. Check: `claude plugin validate` / `claude plugin test` on the folder (needs a Claude Code CLI >= 2.1.275 for `types`/`modules`).

Amended 2027-01-11: Correction on the empty band: the helper's hook lookup (src-tauri/src/bin/mnemo2_mcp/instance.rs lookup) only matches a running instance whose run file names a project containing the cwd. A Mnemo2 started with no project open (for example launched by hand without the project) has no team to show, so team-summary prints {} until that project is opened in the app or by the helper's serve/start path. This is intended, not a restart bug.
