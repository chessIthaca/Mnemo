+++
title = "Mnemo2's Models page shows only the settings Mnemo2 uses"
created = "2026-11-02"
+++

Mnemo2 Settings > Models (ModelsPage.svelte, model_settings.rs) shows a Compaction model ([models.summarize], used by any agent's context compaction), Vision, Code indexing and Budget, plus a note that Mnemo1's per-task models (planning, executing, bug_fixing, reviewing, complete, subagent, free, per-skill) stay in the shared ~/.mnemo/config.toml but Mnemo2 does not use them: workers run on their tier's model and reviews are done by the lead and audit workers. Saving the compaction model re-sends every other [models.*] slot unchanged, so Mnemo1's settings are never cleared.
