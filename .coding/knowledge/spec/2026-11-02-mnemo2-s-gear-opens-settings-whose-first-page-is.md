+++
title = "Mnemo2's gear opens Settings, whose first page is the open project's settings"
created = "2026-11-02"
+++

The top-bar gear opens SettingsDialog with initialPage "project" (there is no separate project sheet). The dialog's rail has two groups: "This project" (ProjectPage.svelte, labelled with the project's name: worker slots, lead model picker, merge policy note) and "All projects" (Endpoints, Tiers, Models, Savings, Decisions, Safety, MCP, Appearance). With no project open the dialog opens on Endpoints and the Project page explains that a project must be open. SettingsDialog takes an optional initialPage prop, re-resolved on every open.
