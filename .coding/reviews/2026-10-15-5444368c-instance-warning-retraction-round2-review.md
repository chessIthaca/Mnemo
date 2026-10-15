## Verdict: PASS

The delta (f8700c8 + the harness's round-2 base stamp) addresses round-1 L1 exactly: the TOCTOU residual is now documented as an accepted benign residual in BOTH named doc locations, comment-only, matching the round-1 recommended fix including its no-code-change rationale. No behavioral change slipped in (the src diff is two comment hunks), the SPEC amendment's landed-outcome claims check out claim-by-claim against the shipped code — including the `tauri::process::restart` internals claim, verified against the upstream tauri v2 source — and round 1's verified scope is untouched. No findings.

## Scope reviewed (delta since base ae23b305ccf83c6b7d66393acfa6e10bc377ee61)

- **Commits:** exactly one intervening commit — f8700c8 "docs: qualify the pid-guard invariant with its TOCTOU residual (L1, plan 5444368c)" (git log). Read its full diff via git show: 4 files — `src-tauri/src/instance_marker.rs` (two hunks), the round-1 report added verbatim (`.coding/reviews/2026-10-15-5444368c-instance-warning-retraction-review.md`), the SPEC knowledge amendment, and the plan (step-9 flip + the round-1 base stamp).
- **Uncommitted remainder:** one line in `.coding/plans/5444368c.md` — the harness's round-2 stamp (`2 f8700c8…`), the same reviewed-state convention as round 1's `1 ae23b30` stamp. Bookkeeping; consistent.
- **Shipped code read to verify the SPEC amendment's claims:** instance_marker.rs (module doc + `remove_marker_if_self` 109–116 + the module's unit tests incl. the three new preserve-branch tests), main.rs `RunEvent::Exit` handler 1105–1144, ipc/projects.rs `switch_project` 195–239, ipc/startup.rs command 78–81 (registration main.rs:999 confirmed by repo-wide search), frontend/src/lib/incumbentWatch.ts (full), App.tsx effect 145–157 + render gate 795–810, InstanceConflictDialog.tsx (full), tauri.ts `isIncumbentAlive` 294–296.
- **Upstream check:** tauri v2 `crates/tauri/src/process.rs` (tag tauri-v2.0.0, raw source) + docs.rs tauri-2.12.4 — `restart(env: &Env) -> !` spawns the new binary then `std::process::exit(0)` directly.
- **Sweep:** repo-wide search `last-writer-wins|marker must survive|marker survives` — every remaining occurrence of the invariant claim is one of: the two now-qualified doc locations, a test-scenario comment (instance_marker.rs:218–220, describing what its test pins in the non-racy case), bookkeeping texts (backlog/plan/SPEC — pre-fix filings or qualified), or unrelated subsystems' uses of "last-writer-wins".

## Task verification

### (1) L1 addressed exactly — VERIFIED

Round-1 L1: the module doc and `remove_marker_if_self`'s doc comment stated the last-writer-wins invariant categorically while the guard is check-then-act (read → remove). The fix qualifies BOTH locations in f8700c8:

- Module doc (added after line 23): "The read→remove guard is check-then-act, so a launch landing inside that tiny window can lose its fresh marker to the older instance's exit — an accepted benign residual (the marker warns, it never locks), the same class as pid recycling."
- Helper doc (instance_marker.rs:99–104, read in the current file): "That guard is check-then-act (read → remove): a newer instance whose write lands inside that microseconds-wide window can lose its fresh marker to this exit — an accepted benign residual (the marker warns, it never locks), the same class as pid recycling; an atomic conditional delete is not expressible with `std::fs`, and a lock file is not warranted for a single-slot advisory marker."

The residual note matches the round-1 analysis in substance and class ("benign and advisory-class… same class as pid recycling"), and the rationale matches the round-1 recommended fix where it counts (atomic conditional delete not expressible with `std::fs`; lock file not warranted for the single-slot advisory marker). The fix covers both locations (round 1 asked "and/or"). The original categorical sentence ("its marker must survive our exit") remains in both docs but is immediately qualified by the new text — the recommended shape (a qualifying note, not a rewrite).

Reviewed-state: f8700c8aa7142bb9bd1a4181e4ced0a3f4d1b2be
### (2) No behavioral change — VERIFIED

f8700c8's src-tauri diff is exactly two hunks, both pure `//!`/`///` comment lines; no code line is added, removed, or modified, and no other src file appears in the commit. Confirmed in the current file: `remove_marker_if_self` (109–116) is the exact read→compare→remove shape round 1 verified (`Some(marker) if marker.pid == own_pid => std::fs::remove_file(marker_path(dir)).map(|_| true)`, `_ => Ok(false)`). Comments cannot affect compilation or behavior; a comment-only delta has no test or runtime surface.

### (3) SPEC amendment landed-outcome claims — ACCURATE, claim by claim

- **Marker removal on exit:** main.rs `RunEvent::Exit` handler 1117–1137 calls `instance_marker::remove_marker_if_self(root, std::process::id())` under `instance_marker_root`, best-effort (Err → eprintln warning), after the trace flush and before the WebView2 teardown thread — as claimed.
- **Switch-away pid-guarded removal before restart:** projects.rs `switch_project` 215–226 removes via `crate::instance_marker::remove_marker_if_self(&root, std::process::id())` BEFORE `write_pending_project` (227) and `tauri::process::restart(&app.env())` (231) — as claimed.
- **"restart calls std::process::exit(0) directly, so RunEvent::Exit never fires on that path":** verified against the upstream tauri v2 source (tauri-v2.0.0 `crates/tauri/src/process.rs`; docs.rs tauri-2.12.4 confirms the same signature) — `restart(env: &Env) -> !` spawns the replacement process then calls `exit(0)`; it never returns to the event loop, so no exit event can fire. The shipped `switch_project` comment (218–220) says exactly this. ACCURATE.
- **Dialog retraction via incumbentWatch + incumbent_pid_alive:** incumbentWatch.ts:38–80 — `setInterval` re-check (2000 ms default) plus an immediate re-check on window focus (77–78); a rejected probe is not death (`.catch(() => null)`, only a definitive `alive === false` retracts, 57–58); onDead exactly once; stop idempotent. The command: `#[tauri::command] incumbent_pid_alive(pid)` → `Ok(crate::startup::instance_pid_alive(pid))` (startup.rs:78–81), registered main.rs:999, production probe `isIncumbentAlive` → `invoke("incumbent_pid_alive", { pid })` (tauri.ts:294–296). App.tsx effect 150–157 starts the watch when `instanceConflict !== null` with `onDead = () => setInstanceConflict(null)`, deps `[instanceConflict]`; render gate 801 (`instanceConflict !== null && !instanceConflictPicker`) — nulling retracts without restart and does not close the picker variant. All as claimed.
- **Launch-time-only copy:** InstanceConflictDialog.tsx — title "Another mnemo instance launched on this project"; body "Another mnemo instance (PID …) launched on this project at {HH:MM}. If it is still running, …"; `launched` from `toLocaleTimeString` (32–35). No occupancy assertion anywhere ("If it is still running" is the explicit hedge) — as claimed.
- **Residual honesty:** "pid recycling on the re-check stays a benign residual (a recycled pid keeps the dialog up)" matches startup.rs:75–77 and incumbentWatch.ts:19–20; "The mechanism description above is the pre-fix state" correctly frames the SPEC's earlier text.
- **Pointers:** commits aae7cc8 + ae23b30 + the round-1 L1 doc fix, plan 5444368c, branch wt/mnemo — all match git log.

### (4) No regression of round-1's verified scope — VERIFIED

The delta's only src change is comment-only and the only uncommitted line is a plan stamp; there is no behavioral surface to regress. Spot-reads of every round-1-verified site against its round-1 description match exactly: the guard body and its `Ok(false)` preserve branches, the three new preserve-branch unit tests (`remove_marker_if_self_removes_only_our_marker`, `remove_marker_if_self_preserves_newer_incumbent`, `remove_marker_if_self_leaves_malformed_marker_in_place`), the Exit-handler ordering, the `switch_project` ordering + comment, the command + registration, the watch util's probe-rejection semantics, the effect wiring, the render gate, and the dialog copy. The test comment at 218–220 ("last writer wins, so the incumbent's marker must survive our exit") describes the scenario its own test pins in the non-racy case — pre-existing, verified in round 1, and not a doc-guarantee location; no residual note owed there.

## Constitution checks (delta-scoped)

- **Documentation sync** — the L1 fix is itself the doc sync (both doc locations, one commit), and the SPEC knowledge file + round-1 report ship with it; no behavior change, so no README/PLAN update was owed (README's user-facing row makes no invariant claim — verified in round 1, untouched by the delta). PASS.
- **Multi-platform neutrality** — comment-only delta; no code, API, path, or shell syntax introduced. PASS.
- **File-tools-first** — no shell-based mutation anywhere in the delta; bookkeeping rode the sanctioned writers. PASS.
- **Warning-free** — comment-only diff, no `#[allow]`; line comments cannot affect the build. PASS.
- **Bookkeeping accuracy** — SPEC amendment verified claim-by-claim against shipped code (above); plan step-9 flip matches its shipped work; the `## Reviews` stamps follow the reviewed-state convention (round 1's own stamp is the precedent); the round-1 report is committed verbatim. ACCURATE.
- **Empty-diff rule** — not applicable: one commit is in scope and the tree carries one uncommitted bookkeeping line; both enumerated via git log / git show / git diff and reviewed above.

## Process remark

None. Round 1 ended in FINDINGS and its fix is committed inside the delta (verified present, §1); no PASS-verified uncommitted carry-over re-shows in this delta.