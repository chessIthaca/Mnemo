## Verdict: PASS

Focused verification of commit `2e9a058` ("Pin the fixed-deadline fire instant in the HIGH-1
regression test (review N-1)") on `wt/fix-tao-deadlock`, parent `75e4c82`. Both prior reviews
were read first; per the task's scope rule, their verified-fixed findings are not re-litigated.
Working tree confirmed clean (`git diff HEAD`, `git diff HEAD --stat`, `git status --short` all
empty; `git log -3` shows HEAD = `2e9a058`), so every file read reflects exactly the committed
state.

The single fix for N-1 is verified correct, discriminating, and complete. No new findings.

---

## 1. The assertion is present, correctly placed, and genuinely discriminates

**Present and placed** (`src-tauri/src/ipc/events.rs:1333-1341`): the
`assert_eq!(tokio::time::Instant::now(), armed, "deadline must fire at the ORIGINAL arm instant,
not now + interval")` is the final statement of `deadline_is_fixed_at_arm_time`, immediately
following the `matches!(…, RecvOutcome::Deadline)` on the third, silent call — exactly the
placement review 2 prescribed.

`DELTA_FLUSH_INTERVAL = Duration::from_millis(16)` confirmed at `events.rs:212` — the t+16/t+26
arithmetic in the extended comment is real, not assumed.

**Trace (a) — current helper (`sleep_until(armed)` pinned in the select, `events.rs:406-411`),
passes:**

- t=0 (paused start): `armed = Instant::now() + 16 ms` = t+16 (`events.rs:1308`).
- Call 1: `advance(5)` → now = t+5; `tx.send(1)` queued *before* the await → `rx.recv()` is Ready
  at the current instant (the pinned sleep is not ready until t+16) → select returns `Event(1)`
  with **no clock movement**. The helper's `Sleep` is dropped when the future returns, so no stray
  timer remains to clamp later advances.
- Call 2: `advance(5)` → now = t+10; `tx.send(2)` queued → `Event(2)` at t+10, same reasoning.
- Call 3 (silent): `rx.recv()` Pending (no events, `tx` alive), `sleep_until(t+16)` Pending → the
  paused clock auto-advances to the earliest timer deadline, t+16 → `Deadline`. Then
  `Instant::now() == t+16 == armed` exactly → `assert_eq!` **passes**.

**Trace (b) — regressed shape (helper ignores the passed deadline, internally
`sleep(now + 16 ms)` armed per call), now fails:**

- Call 1 at t+5: internal deadline t+21, but recv is Ready → `Event(1)` at t+5 → the earlier
  `matches!` passes.
- Call 2 at t+10: internal deadline t+26, recv Ready → `Event(2)` at t+10 → passes.
- Call 3 at t+10: recv Pending → auto-advance to the internal deadline t+26 → `Deadline` → the
  `matches!` **still passes** (this was exactly the N-1 hole).
- The new assertion: `assert_eq!(t+26, t+16)` → **FAILS**. The pin genuinely discriminates.

**Earlier Event-phase advances do not perturb the arithmetic:** `armed` is bound once at t=0
before any advance — advances move only `now`, never `armed`; calls 1 and 2 complete at the
current instant because their events are pre-queued (the select never waits on the clock), and
each helper call's `Sleep` deregisters on return, so neither `advance(5)` is clamped or overshoots.
Cumulative 5+5 = 10 ms < 16 ms keeps call 2 pre-deadline (had it overshot, call 2 would itself
fail loudly — the test is self-checking on that side too).

Bonus beyond N-1's ask: exact equality also catches a *fires-early* regression (returning
`Deadline` immediately at t+10 would fail `assert_eq!(t+10, t+16)` while previously passing
`matches!`) — the pin is strictly stronger than outcome-only in both directions.

## 2. Compiles and holds semantically

- `armed` is `tokio::time::Instant` (`Instant::now() + Duration`); both `assert_eq!` operands are
  the same type, which implements `Debug` + `PartialEq`/`Eq` — the macro's bounds are satisfied.
- Exact equality is sound on the paused clock: tokio's test-util auto-advance sets `now` exactly
  to the pending timer's deadline (the same mechanism `time::advance` uses, which lands exactly on
  its requested delta); `Instant` comparisons are nanosecond-exact and `t0 + 16 ms` is
  deterministic. No rounding concern — consistent with review 2's own prescribed fix text.
- `start_paused = true` requires tokio's `test-util` dev-feature — confirmed present
  (`src-tauri/Cargo.toml:32-35`, verified in review 2, untouched here).
- Warning-free by inspection: no new bindings, the message is a plain `&str` literal (valid third
  `assert_eq!` argument); nothing warning-prone under `#![deny(warnings)]`.

## 3. Nothing else changed

`git show 2e9a058` contains exactly two paths:

1. `.coding/reviews/2026-09-05-review-fixes-verification.md` — new file, 158 lines, byte-identical
   to the on-disk copy (verdict line, N-1 section with the prescribed snippet, and summary all
   match the file read from disk).
2. `src-tauri/src/ipc/events.rs` — a single hunk inside
   `mod recv_until_deadline_tests::deadline_is_fixed_at_arm_time`: the pre-call comment extended
   from 4 to 8 lines (explains the pin: regressed helper armed at the last event, t+10 → fires
   t+26 ≠ armed; cites review N-1) plus the 5-line `assert_eq!`. No production code, no
   manifests, no other modules, no `#[allow]`.

## 4. Test results — static consistency (reviewer is read-only; no shell)

Commit message: "`cargo test -p mnemo-app` 167+4 passed / 0 failed; `cargo test` 1483 passed /
0 failed." Task-reported runs: 167 passed + 4 passed / 0 failed and 1483 passed / 1 ignored, both
exit 0. **Consistent** — the commit message merely omits the ignored count (ignored ≠ failed).
The arithmetic also holds: this commit adds no test functions (an assertion inside an existing
test), so counts must be unchanged from `75e4c82`'s verified 165+4 → +2 deadline tests = 167+4,
and the root crate (`mnemo`) is unaffected by src-tauri test counts → 1483 stable. Since the tree
is clean, the runs the main agent executed were exactly this code. No discrepancy between the
commit message, the review reports, and the code read.

## Constitution compliance (this diff)

- **Doc comments:** the test fn was already documented; the change *enriches* the in-test comment
  to honestly describe what is now pinned (the prior overclaim N-1 flagged is gone).
- **No `#[allow]` added;** pure test-module change; nothing platform-specific (pure
  `tokio::time`) — multi-platform neutral. No security surface. **Doc sync:** not required for a
  test-strength tweak; prior README/PLAN.md claims remain accurate and the commit message records
  the rationale.

## Residuals (previously acknowledged in review 2 — not re-raised, per scope)

The caller-side arming block (`events.rs:469-473`), where the original HIGH-1 physically lived,
remains untestable without an `AppHandle`; the helper-level pin is the achievable guard, and this
commit makes that guard actually pin.

**Summary:** commit `2e9a058` does exactly what it says — the regression test now fails a
re-arming implementation (t+26 ≠ t+16) and passes the current one exactly (auto-advance lands on
t+16 == armed), with only the assertion + comment + the review report file in the commit. N-1 is
resolved; nothing new found.
