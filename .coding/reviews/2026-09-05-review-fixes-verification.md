## Verdict: FINDINGS (0 high, 1 low)

Follow-up verification of the three fixes from
`.coding/reviews/2026-09-05-tao-backport-resize-seam-review.md` (1 high, 2 low),
all now committed as `75e4c82` on `wt/fix-tao-deadlock`. Scope: the fix diff
(`src-tauri/src/ipc/events.rs`, root `Cargo.toml`, `src-tauri/Cargo.toml`,
`Cargo.lock`) re-verified against the prior review; everything the prior review
passed (tao backport fidelity, batcher pure semantics, resize motif, docs) was
not re-litigated. Working tree confirmed clean (`git diff HEAD` and
`git status --short` both empty) — the commit contains the full change set
including the fixes.

**All three prior findings are verified FIXED.** One new LOW finding on the
strength of the HIGH-1 regression test (below) — the production fix itself is
correct.

---

## HIGH-1 (timer starvation) — FIXED, verified

**(a) Arming/disarm logic pins the deadline.** `events.rs:469-473`:
`if delta_batcher.is_empty() { flush_deadline = None } else if flush_deadline.is_none() { flush_deadline = Some(now() + DELTA_FLUSH_INTERVAL) }`
— the re-arm branch is reachable only when the deadline is already `None`, i.e.
after a full drain. Traced every path that leaves deltas pending:

- `Event` arm absorbed into the batcher → next loop top keeps the SAME
  `Some(deadline)` (`is_none()` is false) — never recomputed.
- `Deadline` arm (`events.rs:475-483`) → **(b) confirmed**: `flush_all()`
  emitted per `(agent, event)` via `emit_payload`, `flush_deadline = None`,
  `continue`; `flush_all` removes every bucket so the loop top re-disarm is a
  no-op.
- Structural event flushes only its own agent (`push` → `flush_agent`,
  `events.rs:285-291`); if other agents' deltas remain pending, the original
  deadline is kept — which is ≤ every pending delta's max-age (all were armed
  at or before it), so the max-age guarantee holds for every pending bucket.
  Never late, at worst early. Correct.

The helper (`events.rs:394-412`) honors the passed `Instant`: `None` → plain
`recv()` mapping `None`→`Closed`; `Some(d)` → `sleep_until(d)` pinned in the
select. Recreating `sleep_until` per call with an *unchanged* deadline fires at
the original instant (documented at `events.rs:391-393`) — equivalent to pinning
one sleep across calls. A deadline that expires *during* event processing
(manager locks, Run-All resolution) fires immediately on the next
`recv_until_deadline` (`sleep_until(past)` is instantly ready) — no missed
flush. The expired-deadline + queued-event select race is benign either way
(both orderings flush within one iteration). Idle forwarder keeps the deadline
disarmed (zero armed timers).

**(c) The tests do NOT genuinely pin the fixed-deadline property — see finding
N-1 below.**

## LOW-1 (trailing batch dropped on teardown) — FIXED, verified

`events.rs:487-496`: `Closed → None` → the `let … else` branch runs
`delta_batcher.flush_all()` + `emit_payload` for each event **before** `break`,
with a comment citing the finding. The helper contract is pinned by
`no_deadline_is_a_plain_receive_and_closed_surfaces` (`events.rs:1340-1353`):
plain receive without a deadline, and `drop(tx)` surfaces `Closed` (not a hang).
Correct.

## LOW-2 (vendor not excluded) — FIXED, verified

Root `Cargo.toml:90-97`: `[workspace] members = ["src-tauri"]`,
`exclude = ["vendor"]` with an explanatory comment, `resolver = "2"` retained —
correct placement (all keys under the `[workspace]` table; `exclude` paths are
relative to the manifest). Patch override intact at `Cargo.toml:104-105`
(`tao = { path = "vendor/tao" }`). `Cargo.lock:5401-5437`: `tao` **0.35.4**
with **no `source` and no `checksum`** (path resolution) — contrast
`tao-macros` at `:5440-5443`, which retains its registry source + checksum. The
override resolves exactly as required.

## Supporting change (tokio test-util dev-dep) — clean

`src-tauri/Cargo.toml:32-35`: `tokio = { version = "1", features = ["test-util"] }`
in `[dev-dependencies]` with a doc comment. Correct claim — `full` does not
include `test-util`. Dev-dependencies apply only to test/bench/example targets,
so the shipped app binary's tokio feature set is unchanged; for the test target
the features merge (`full` + `test-util`). Mirrors the pre-existing root-crate
pattern (root `Cargo.toml:84-88`). No side effects found.

## New finding introduced by the fixes

### N-1 (low): `deadline_is_fixed_at_arm_time` does not pin WHEN the deadline fires — a re-arming regression passes it (`src-tauri/src/ipc/events.rs:1305-1334`)

The test sends events at t+5/t+10 ms (both → `Event`), then asserts the third,
silent call returns `Deadline`. But it never asserts the fire time: under a
regressed helper that ignores the passed deadline and internally does
`sleep(DELTA_FLUSH_INTERVAL)` (or a caller that re-arms per event), the paused
clock simply auto-advances to the *later* internal deadline (t+26 instead of
t+16) and `matches!(…, RecvOutcome::Deadline)` still passes. The test's doc
comment claims "the ORIGINAL deadline (t+16 ms) fires — proving the timer was
not re-armed" — overclaimed; the only regressions it actually catches are
plain-recv/never-fires/Closed-mapping shapes. Per the constitution's regression
bar (must fail without the fix), the re-arm variant — the actual HIGH-1 defect —
would not fail this test.

One-line fix: after the third call, assert the fire instant —

```rust
assert_eq!(
    tokio::time::Instant::now(),
    armed,
    "deadline must fire at the ORIGINAL arm instant, not now + interval"
);
```

(auto-advance lands exactly on the sleep's deadline; a re-armed impl would land
at t+26 ≠ t+16). Honest residual: the caller-side arming block
(`events.rs:469-473`) — where the original bug physically lived — is not
unit-testable without an `AppHandle`; the helper-level pin is the achievable
guard, which is why it should actually pin.

## Constitution compliance (fix diff)

- **Doc comments**: `RecvOutcome` + all three variants (`events.rs:371-380`),
  `recv_until_deadline` (`:382-393`), both new test fns (`:1298-1304`,
  `:1336-1339`), loop comments at `:455-461`/`:466-468`/`:476-477`/`:488-491`.
  All present.
- **No `#[allow]`**: swept `src/` and `src-tauri/` — the only hits are string
  literals/comments plus two pre-existing `clippy::too_many_arguments` in
  `src/agent/loop_impl.rs:376,418` (untouched by this commit, which does not
  modify `src/` at all). Nothing added.
- **Doc sync**: `DELTA_FLUSH_INTERVAL` doc updated to the max-age semantics
  (`events.rs:209-211`); `src-tauri/Cargo.toml` and root `Cargo.toml` carry
  comments explaining `test-util` and `exclude`. In sync.
- **Multi-platform neutrality**: pure `tokio::time` APIs + cargo manifest edits;
  nothing platform-specific. Compliant.
- **Security**: no new native/exec/network surface; manifest-only changes.

## Test-run results

**Could not be executed by this reviewer** — the reviewer environment is
read-only by construction (no shell/exec tool), so `cargo test` cannot be run
here. Static consistency check of the claimed numbers instead:

- Prior review (pre-fix) reported `cargo test -p mnemo-app` **165+4**; +2 new
  deadline tests = **167+4** — matches the task's and commit message's claim.
- Root `cargo test` **1483** unchanged is consistent: the root manifest is a
  package (`mnemo`) that also declares the workspace, so plain `cargo test`
  from the root tests only the `mnemo` crate — src-tauri's +2 tests don't
  affect it.
- Zero-warning status cannot be independently confirmed without a build; it
  rests on the commit's claimed green runs (the two new tests + doc comments
  introduce no obviously warning-prone code; the generic `T` and `Debug`-only
  derive are used, `tokio::pin!` is correct).

**Action for the main agent:** run `cargo test -p mnemo-app -q` and `cargo test -q`
unpiped and read the `test result:` lines to confirm 167+4 / 1483 passed before
`finish`, then fix N-1 (one assertion line) and re-run the two deadline tests.

---

**Summary:** all three prior findings are genuinely fixed in 75e4c82 — the
deadline is correctly pinned (armed once, never reset while a batch is open),
the trailing batch is flushed on channel close, and the vendor exclusion +
path-patch resolution are correct. One new LOW: the HIGH-1 regression test
asserts the outcome but not the fire instant, so it cannot fail a re-arm
regression — add the `assert_eq!(Instant::now(), armed)` line.
