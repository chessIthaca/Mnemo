# Review: Async/blocking hygiene — spawn_blocking for memory store + browser sweep, reqwest redirect policy

**Date:** 2026-11-21
**Scope:** All uncommitted changes (`git diff HEAD`): `src/browser/mod.rs`, `src/memory/mod.rs`, `src/provider/openai.rs` (+ `.coding/plans/` bookkeeping).
**Goal:** Move blocking I/O off the async runtime — (R2) wrap 13 memory-store rusqlite methods in `spawn_blocking` after switching `conn`/`read_conn` from `tokio::sync::Mutex` to `std::sync::Mutex`; (M1-perf) fire-and-forget the browser temp-dir sweep off the locked async path; (L1) set explicit `reqwest::redirect::Policy::none()` on both client builders.

## Verdict: NO FINDINGS

The diff is clean. All focus points (a)–(h) verified correct. Details below.

### (a) No `std::sync::MutexGuard` held across `.await` — PASS

Every `spawn_blocking` closure is self-contained: it clones the `Arc<Mutex<Connection>>` out of `&self`, moves owned values in, locks **inside** the closure, does synchronous rusqlite work, and returns. The `MutexGuard` is a closure-local that drops at the closure's end; the `.await` resolves the `JoinHandle` only **after** the closure returns. Verified for all 13 methods:

- `load_all` (mod.rs:404-412), `write` (598-634), `recall` (660-699), `access` (773-787), `batch_access` (793-821), `strongest` (847-869), `delete_working_for_session` (887-908), `start_session` (910-926), `end_session` (928-942), `record_request_stats` (983-1010), `session_stats` (1012-1069), `project_stats` (1071-1141), `session_list` (1143-1177).

The test at mod.rs:1950 (`load_recent_locked_caps_and_orders_by_recency`) locks `store.read_conn().lock().expect(...)` then calls the sync `load_recent_locked` helpers with **no `.await`** between the lock and the calls. No `await_holding_lock` risk.

### (b) `spawn_blocking` closures are `Send + 'static` — PASS

Each closure moves only owned values; none captures `&self` or a field reference. `self.conn.clone()` / `self.read_conn().clone()` are taken before the closure. Moved values: `Arc<Mutex<Connection>>` (owned), `String` (owned), `Vec<String>`/`Vec<MemoryTier>` (owned), `i64`/`bool`/`Option<MemoryTier>` (Copy), `Memory` (owned, `Clone` per types.rs:54), `RequestStats` (owned, `Clone` per types.rs:215), `Session` fields (owned). `Connection: Send` ⇒ `Mutex<Connection>: Send+Sync` ⇒ `Arc<Mutex<Connection>>: Send+Sync+'static`. All closure return types (`Vec<Memory>`, `()`, `usize`, `SessionStats`, `ProjectStats`, `Vec<SessionSummary>`) are `Send`.

### (c) Error handling — `?`/`??` counts correct — PASS

- **Single `?` (tail expression, closure returns `Result<T>`, method returns `Result<T>`):** the inner `Result<T>` becomes the method's return value; a closure `Err` propagates as the method's `Err`, a `JoinError` becomes `Error::Memory(...)`. Applies to: `load_all`, `access`, `batch_access`, `delete_working_for_session`, `end_session`, `record_request_stats`, `session_stats`, `project_stats`, `session_list`.
- **`??` (closure returns `Result<()>`, method returns a different type):** first `?` unwraps `JoinError`→`Result<()>`, second `?` unwraps `Result<()>`→`()`, then the method constructs its return (`Ok(id)` / `Ok(session)`). Applies to: `write` (→`Result<String>`), `start_session` (→`Result<Session>`).
- **`??` (closure returns `Result<Vec<Memory>>`, value assigned to a variable):** first `?` unwraps `JoinError`, second `?` unwraps to `Vec<Memory>`. Applies to: `recall` (→`memories`), `strongest` (→`memories`).

No swallowed errors, no double-unwraps that drop a value. `JoinError` consistently mapped to `Error::Memory(format!("... task failed: {e}"))`.

### (d) No behavior change — PASS

Same SQL, params, error semantics, and return values throughout. Spot-checks:
- **`recall`** (636-771): embedding async **before** spawn_blocking (638-639); FTS+load in closure; scoring pure Rust **after** (700-748); `batch_access` called after scoring (768). `exclude_working` computed before, moved in as `bool` (Copy). `query` still used after for `q_lower` (712); `query_owned` is the closure's copy.
- **`strongest`** (833-885): query+load in closure; `let mut memories: Vec<Memory> = ...??` is correctly typed + `mut` for the sort/truncate after (876-883).
- **`session_stats`** (1012-1069): `session_id` moved as owned `String`; used for `query_row` param (1020, borrow) then struct field (1024, move) — ordered correctly, no use-after-move. The `params![stats.session_id]` borrow during `for row in rows` is a disjoint-field borrow from `stats.per_model.push` — compiles, identical value to the original `&str` param.
- **`write`/`start_session`/`record_request_stats`**: owned clones/tuples moved into closures; same SQL + params.

### (e) Browser sweep — PASS

`sweep_stale_profiles` is `fn() -> ()` (mod.rs:205) — a function pointer, `Send+'static`. Fire-and-forget via `let _ = tokio::task::spawn_blocking(Self::sweep_stale_profiles)` (172). The sweep only removes dirs with `modified < now - 3600s` (209, 215-219), never the fresh profile created at 174. Correctly removes the blocking `read_dir`+`remove_dir_all` from the state-lock-held `ensure_browser` path.

### (f) reqwest `Policy::none()` — PASS

Both `reqwest::Client::builder()` sites in source got `.redirect(reqwest::redirect::Policy::none())` after `.deflate(true)`, before `.build()`:
- Main client: openai.rs:122 (`new_with_trace`).
- Models client: openai.rs:202 (`fetch_models_with_vision`).

Only 2 `reqwest::Client::builder()` sites exist in `src/`. `Policy::none()` returns 3xx responses as-is (no redirect following), so a cross-host redirect can never carry the `Authorization: Bearer` header. LLM/embeddings/`/models` endpoints don't redirect, so no provider breaks.

### (g) No `#[allow(...)]` suppressions — PASS

None added in the diff.

### (h) Build warning-free under `#![deny(warnings)]` — PASS

No dead code, unused imports, or unused `mut` introduced. The `use tokio::sync::Mutex;` import was removed (it was the only tokio Mutex use — `conn`/`read_conn` are now `std::sync::Mutex`); `Error` was added to `use crate::error::{Error, Result}` (used in the 13 `map_err` closures). `record_tool_event` (944-981) correctly left unchanged — it delegates to `self.write(memory).await` with no direct DB work. Per the task, `cargo test` is green (783 lib + integration tests); under `#![deny(warnings)]` a green build proves zero warnings.

---

## Informational note (pre-existing, NOT introduced by this diff — no fix required)

`new_with_trace` has a fallback `.unwrap_or_else(|_| reqwest::Client::new())` (openai.rs:124) that does **not** inherit the `Policy::none()` redirect policy (`reqwest::Client::new()` follows up to 10 redirects by default). This is pre-existing code not touched by this diff, and it only triggers on a `ClientBuilder::build()` failure (TLS-backend init error) — a near-impossible condition where no HTTP request could succeed anyway. The L1 security goal is fully met on the normal code path (both builders). Noting for completeness only; not a finding against this diff.
