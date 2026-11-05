# Plan: A closing helper only releases the lead when no other leading helper is left

## Goal
The external lead stays attached while any helper that led (forwarded a tool call) is still connected; a helper closing only releases the lead when it was the last such helper. The 10-minute idle detach remains the fallback for helpers that crash without closing.

## Kind
bug_fixing

## Context
Root cause: src-tauri/src/bin/mnemo2_mcp/serve.rs `Bridge::finish` (stdin closed) posts `/lead/detach` with body `{"root": <root>}` whenever this process forwarded a `tools/call` (`attached` flag). On the server, src-tauri/src/mnemo2/mcp_server.rs `lead_route` sends `/lead/detach` to `detach(target)`, which calls `target.lead.release_external_lead()` unconditionally. Tool calls attach anonymously (`self.attach(&target, None)` in `mcp`, around line 1042) because Claude Code does not tell MCP servers its session id, so the server cannot tell one helper from another: any helper that led and then closes drops the lead held by every other helper.

Fix design (decided): every helper process gets a random id at start (`uuid::Uuid::new_v4().simple()`; the uuid crate is already a dependency of src-tauri). It sends it as header `X-Mnemo-Bridge: <id>` on every forwarded `/mcp` request, and as `"bridge": <id>` in the `/lead/detach` body from `finish`. The server keeps, per team, the set of bridge ids that made a `tools/call` while attached (`ExternalLead` in src-tauri/src/mnemo2/team.rs, a new `bridges: BTreeSet<String>` field). `/lead/detach` with a `bridge` removes that id; it releases the lead only when the set is then empty. `/lead/detach` without `bridge` (older helpers, the existing tests) behaves exactly as today and releases. Any release (detach, idle sweep, `detach_external_lead`) clears the set. A `tools/call` without the header still attaches as today and adds nothing to the set. The idle sweep (`sweep_idle`, `idle_detach_ms`, 10 minutes with no call and no waiter) is unchanged and remains the fallback for a helper that crashes without posting detach.

This plan creates the OpenSpec capability `external-lead` (.coding/openspec/specs/external-lead/spec.md, new) through its ADDED spec changes.

Regression test: `a_closing_helper_keeps_the_lead_while_another_helper_led` in src-tauri/src/mnemo2/mcp_server.rs tests (step 1): two `/mcp` tools/call requests with `X-Mnemo-Bridge: a` and `X-Mnemo-Bridge: b`, then `/lead/detach` with `{root, bridge: "b"}`; it asserts the team is still attached (`team.external_lead_attached()`), then detach with bridge `a` and assert detached. Before the fix the first detach releases the lead and the first assertion fails.

Verification (the mnemo2 module is feature-gated): `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::mcp_server`, `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::team`, `cargo test -p mnemo-app --features mnemo2 --bin mnemo2-mcp` (helper tests incl. serve_tests), `cargo clippy -p mnemo-app --all-targets --features mnemo2 -- -D warnings` and the same clippy without `--features mnemo2`. Report test counts.

## Acceptance scenarios
- With two helpers that both made tool calls, closing one leaves the external lead attached and its waiter keeps waiting; closing the second releases it
- A detach without a bridge id (older helper) releases the lead as before
- A helper that crashes without detaching is still released by the 10-minute idle sweep
- Every forwarded /mcp call from one helper carries the same X-Mnemo-Bridge id, and its detach names it

## Spec changes

### external-lead

#### ADDED Requirements

##### Requirement: Tool calls attach the external lead
The system SHALL attach the external lead to a project's team on the first MCP tools/call for that project, and SHALL NOT attach it for initialize, tools/list, ping or a notices read.

###### Scenario: Opening Claude Code
- **WHEN** Claude Code starts in a registered project and lists the Mnemo tools
- **THEN** the in-app lead keeps receiving worker notices

###### Scenario: First tool call
- **WHEN** a helper forwards a tools/call for the project
- **THEN** the external lead is attached and worker notices queue for it

##### Requirement: A closing helper keeps the lead while another helper led
The system SHALL remember every helper id that made a tools/call while the external lead is attached, and a helper's detach on close SHALL release the lead only when no other remembered helper remains. A detach that names no helper SHALL release the lead.

###### Scenario: Second helper closes
- **WHEN** helpers a and b both made tool calls
- **WHEN** helper b closes
- **THEN** the external lead stays attached and its waiter keeps waiting

###### Scenario: Last helper closes
- **WHEN** helper a is the only remembered helper
- **WHEN** helper a closes
- **THEN** the external lead is released
- **AND** unread notices go to the in-app lead

###### Scenario: Anonymous detach
- **WHEN** a detach arrives without a helper id
- **THEN** the external lead is released

##### Requirement: Idle external leads are released
The system SHALL release an external lead that made no Mnemo call for 10 minutes while no waiter runs, regardless of remembered helpers, and SHALL forget every remembered helper on any release.

###### Scenario: Crashed helper
- **WHEN** the only leading helper crashed without detaching
- **WHEN** 10 minutes pass with no call and no waiter
- **THEN** the external lead is released

## Steps
- [x] 1. **Server: release the lead only when the last leading helper closes** — files: src-tauri/src/mnemo2/team.rs, src-tauri/src/mnemo2/mcp_server.rs. 1. Write the regression test `a_closing_helper_keeps_the_lead_while_another_helper_led` (see the plan context) first, next to `detach_hands_the_project_back_and_wakes_waiters` in mcp_server.rs tests, using the existing Fixture helpers for posting `/mcp` tools/call with headers (look at how existing tests post `/mcp` with `X-Mnemo-Root`, around lines 3098 and 3560) and `fixture.lead_post`. Run it and report that it fails. 2. team.rs: add `bridges: std::collections::BTreeSet<String>` to `struct ExternalLead` (around line 884) with a doc comment. Add `pub fn note_external_bridge(&self, bridge: &str)` (inserts when the lead is attached, i.e. `since_ms.is_some()`), and `pub fn forget_external_bridge(&self, bridge: &str) -> usize` (removes it, returns how many remain). In `detach_external_lead` clear `bridges`. 3. mcp_server.rs `mcp`: in the `tools/call` branch, after `self.attach(&target, None)`, read `request.header("x-mnemo-bridge")`, trim it, and when non-empty and at most 64 chars of [0-9a-f], call `target.team.note_external_bridge(id)`. Ignore other values. 4. mcp_server.rs: change `detach` to take the parsed body: when `body["bridge"]` is a valid id as above, call `forget_external_bridge`; if it returns > 0, return `json!({"released": false})` without releasing; otherwise release exactly as today and return `json!({"released": true})` (also when nothing was attached return `{"released": false}` as appropriate — keep `lead_changed` calls exactly as today). A body without `bridge` releases as today. Update `lead_route` to pass the body. 5. Update the module doc comment at the top of mcp_server.rs (the `/mcp` and detach paragraphs) to describe `X-Mnemo-Bridge` and the rule. 6. More tests in mcp_server.rs: `detach_without_a_bridge_still_releases` (anonymous detach after a bridged tool call releases); `idle_sweep_releases_even_with_bridges_left` (attach with a bridge header, run the existing idle-sweep path as `a_silent_lead_is_detached_after_the_idle_window` does, assert detached and that a later detach with that bridge reports released false); in team.rs a unit test that `detach_external_lead` clears the bridge set (a bridge noted, detach, re-attach, `forget_external_bridge` returns 0). 7. Run the verification commands for the server (mcp_server and team tests with --features mnemo2, both clippies) and report counts. Commit (`mcp_server: a closing helper only releases the lead when no other leading helper is left`). Do not touch src-tauri/src/bin/mnemo2_mcp/*.
- [x] 2. **Helper: identify each helper process to the server** — files: src-tauri/src/bin/mnemo2_mcp/serve.rs, src-tauri/src/bin/mnemo2_mcp/serve_tests.rs. 1. serve.rs `struct Bridge`: add `id: String`, set once where the Bridge is built to `uuid::Uuid::new_v4().simple().to_string()` (32 lowercase hex). If the bin cannot see the uuid crate, check src-tauri/Cargo.toml (it is a package dependency) and report rather than adding dependencies. 2. `Bridge::forward`: add `("X-Mnemo-Bridge", self.id.as_str())` to the headers sent with `/mcp`. 3. `Bridge::finish`: send `{"root": ..., "bridge": <id>}` as the `/lead/detach` body (extend what `root_body(&instance)` returns into a new JSON object; keep `root_body` itself unchanged if other callers use it). Keep the rest of finish as is (only a helper that forwarded a tools/call posts detach). Update finish's doc comment: the server releases the lead only when no other leading helper remains. 4. serve_tests.rs: the stub server there records requests (look at how it answers `/lead/detach` around line 77). Add tests: `forwarded_calls_carry_a_stable_bridge_id` (two tools/call lines through one bridge: both requests have the same non-empty 32-hex `X-Mnemo-Bridge` header) and `detach_on_close_names_the_bridge` (after a tools/call and EOF, the recorded `/lead/detach` body has `bridge` equal to that header value). If the stub cannot record headers or bodies yet, extend it minimally inside serve_tests.rs. 5. Run `cargo test -p mnemo-app --features mnemo2 --bin mnemo2-mcp` and both clippies (with and without --features mnemo2); report counts. Commit (`mnemo2-mcp: send a per-process bridge id with calls and on detach`). Do not touch src-tauri/src/mnemo2/*.
- [x] 3. **Integration check** — run the combined result (build, tests, or launching the entry point) and verify the parallel steps fit together; fix any seam breakage.

## Bug
When a second, short-lived mnemo2-mcp helper process that made a tool call exits (for example a one-off `mnemo2-mcp serve` probe), Mnemo2 detaches the external lead even though the main Claude Code session's helper is still connected and leading; its waiter returns 'The Mnemo lead was detached (another session closed or Mnemo2 restarted)'. Observed three times on 2026-11-05.

## Regression test
a_closing_helper_keeps_the_lead_while_another_helper_led

## Decisions
- 2026-11-05: added step 3 (Integration check) because steps 1 and 2 ran in parallel

## Interfaces
- 2026-11-05: Wire contract between the helper (step 2) and the server (step 1): forwarded `/mcp` requests carry header `X-Mnemo-Bridge: <id>` (32 lowercase hex chars, `Uuid::new_v4().simple()`, one per helper process, fixed for its lifetime); `/lead/detach` body is `{"root": <root>, "bridge": <id>}`. The server treats a missing header/field as an anonymous helper (today's behaviour). The `/lead/detach` response stays a JSON object; step 1 may add `"released": bool` but the helper ignores the response.
- 2026-11-05: File ownership: step 1 owns src-tauri/src/mnemo2/mcp_server.rs and src-tauri/src/mnemo2/team.rs; step 2 owns src-tauri/src/bin/mnemo2_mcp/serve.rs and src-tauri/src/bin/mnemo2_mcp/serve_tests.rs. Neither touches the other's files.

## Progress
- 2026-11-05 step 2 done: step 2 merged: helperBridge01 (helperBridge01) on deepseek-v4.1-flash:1017, merge commit ba4e2bd8; review: Each helper process gets one uuid simple id, sends it as X-Mnemo-Bridge on every forwarded /mcp request and as bridge in its /lead/detach body; detach still fires only after a forwarded tools/call.
- 2026-11-05 step 1 done: step 1 merged: leadBridgeSrv01 (leadBridgeSrv01) on glm-5.3, merge commit 240a1e9c; review: The server remembers helper ids that made tool calls while attached; a named detach forgets one and releases only when none remain; anonymous detach, idle sweep and every release behave as before and clear the set.
- 2026-11-05 step 3 done: combined lead branch 240a1e9c: mnemo2-mcp bin 168 passed, mcp_server 53 passed; live check after the next rebuild
