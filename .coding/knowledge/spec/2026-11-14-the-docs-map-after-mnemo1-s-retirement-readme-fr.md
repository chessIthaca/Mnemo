+++
title = "The docs map after Mnemo1's retirement — README front door, FEATURES engine, CONFIGURATION files, REVIEW-POLICY gate, mnemo2/README app"
supersedes = "2027-01-11-readme-is-the-marketing-front-door-deep-detail-l"
created = "2026-11-14"
evidence = "Integration check on the lead branch after the docs plan's six merges: real_agent_md_scopes_for_team passes (17 agent_md tests), no 23755694/merge_to_main/Run-All in the six docs, no workflow.svg reference outside .coding, every relative link resolves."
+++

Since 2026-11-14 the docs describe Mnemo2 only, each file with one job:
- README.md: the front door. Lead + team pitch, Highlights table, "How a task flows" (plan, dispatch, review, merge, land), repository tree, Quickstart (install a release, first run, build from source), the seven ci.yml commands under Development & tests, Settings worth checking, the optimizer levers, docs map. No workflow diagram (assets/workflow.svg was deleted).
- docs/FEATURES.md: the engine reference (memory, graph, tools, providers, safety, levers). Mnemo1 UI features are gone; the app itself is described in mnemo2/README.md.
- docs/CONFIGURATION.md: headed sections per file (config.toml, endpoints.toml with tiers, mnemo2.toml [routing]/[window], MCP, per-project state, index-staleness log). Kernel-only keys (per-turn routing, per-task model slots) and retired Mnemo1 keys are marked as such.
- docs/REVIEW-POLICY.md: the Mnemo2 review gate written for people. agent.md keeps the same rules for the agents (it is in their prompt); mnemo2/README.md only summarizes and links.
- agent.md: no `mnemo:classic` blocks; repository rules (cargo test --workspace, Review expectations) are unmarked so Mnemo2's Team scope keeps them. Pinned by `real_agent_md_scopes_for_team` in src/project/agent_md.rs.
Convention: feature and config detail goes into docs/, not the README; a change to behaviour, settings or commands updates every doc agent.md's Documentation sync rule lists.
