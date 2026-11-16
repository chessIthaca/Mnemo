+++
title = "one Claude Code session per Mnemo project; outside a project the bridge offers mnemo_projects and mnemo_init_project"
created = "2026-11-16"
evidence = "serve_tests.rs stub_lists_the_project_tools, init_registers_and_goes_live_without_reload, init_of_another_folder_registers_without_switching, stub_picks_up_a_project_registered_meanwhile"
+++

Decision (user, 2026-11-16): one Mnemo2 per project folder and one Claude Code session per project (Claude Desktop runs several sessions side by side); a session never leads several projects. src-tauri/mnemo2-app/src/mnemo2_mcp/serve.rs: outside a registered project the bridge's stub offers `mnemo_projects` (registered projects, running?, opened before?) and `mnemo_init_project({path?})`. Init refuses a missing folder, an overlap with a registered project (either direction), and a folder without .git; registers via project::add; only for the session's own folder it starts that project's Mnemo2 and goes Live (go_live + tools/list_changed), so the team tools appear without reloading; another folder is only registered. While on the stub, every tools/list and tools/call re-reads the registry (adopt_registered), so a project registered elsewhere is picked up. Claude Code runs the plugin from a frozen cache copy, so a new helper needs a plugin update and a new session.
