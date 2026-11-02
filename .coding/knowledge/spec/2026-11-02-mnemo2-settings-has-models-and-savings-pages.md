+++
title = "Mnemo2 settings has Models and Savings pages"
created = "2026-11-02"
+++

Mnemo2's settings dialog (mnemo2/src/components/SettingsDialog.svelte) has four pages: Endpoints, Tiers, Models and Savings. The Laya page joins later. Models (ModelsPage.svelte): per-task model overrides (planning, executing, bug fixing, reviewing, summarize; "Default" clears), vision model, code indexing (embedding) model with sizes, Installed badges, "Keyword only", status and an Install button with progress, and budget caps. Savings (SavingsPage.svelte): optimizer lever switches with explanations, knobs, and a per-model pricing table. The pages use model_settings_get / model_settings_save(section, value) / embedding_install from src-tauri/src/mnemo2/model_settings.rs. Install progress arrives on mnemo2://embedder-event. Every save goes through Setup::patch_settings, which rewrites config.toml from the in-memory config: schema keys survive, comments do not. Most changes apply after reopening the project. Mnemo2 never auto-downloads the embedding model (auto_install_embedder false), so a missing model means keyword-only recall until it is installed here.
