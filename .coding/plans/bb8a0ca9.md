# Plan: Fix: agent can't drive the visible Browser tab in Complete/Planning

## Goal
Make the mutating browser tools (browser_navigate/click/type/eval) available in Planning, Reviewing, and Complete — matching the Executing arms — so the agent can drive the user-visible Browser tab on explicit request in any state, with the existing per-call approval gate (NeedsApproval) as the guard.

## Kind
bug_fixing

## Context
Reproduced live: browser_snapshot (read-only, AutoRun) works — CDP attach to the child WebView2 is healthy. browser_navigate fails at dispatch (src/agent/dispatch.rs:102) because ToolFilter.allows denies it. Root cause in src/tool/mod.rs ToolFilter::allows: Planning arm (~325) and Complete arm (~437) use `ToolCategory::Browser => safety == SafetyLevel::AutoRun` (hides all NeedsApproval browser tools: navigate/click/type/eval), and Reviewing arm (~404) uses `Browser => false`. The rationale copied the "no project mutation without a plan" rule onto browser tools, but browser tools drive the user-visible Browser tab (child WebView2 over CDP, src/browser/mod.rs webview_* methods) — they cannot mutate project files, and their NeedsApproval safety level already prompts the user on every call. The approval gate is the correct guard; state hiding just breaks the user's primary ask. The Executing and ExecutingResearch arms already use `Browser => true`. Existing test `reviewing_hides_browser_tools` (src/tool/mod.rs:1273) pins the old Reviewing behavior and must be inverted. No user-facing docs (README.md/PLAN.md) mention browser-tool state gating.

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
browser_navigate (and browser_click/browser_type/browser_eval) fail with "tool 'browser_navigate' is not allowed in the current workflow state (Complete)" — reproduced live this session. In Complete (and Planning/Reviewing) the agent can never navigate the user-visible Browser tab, even when the user explicitly asks ("navigate so I can follow what you do").

## Regression test
browser_tools_available_in_every_base_state
