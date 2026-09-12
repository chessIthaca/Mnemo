# craby.rs evaluation — path (b) native shell

**Date:** 2026-12
**Status:** Research finding (no source changes). Resolves the in-flight architecture decision.
**Verdict:** **NO-GO for craby.rs / path (b). Ship path (a).**

---

## 1. What craby.rs actually is

craby.rs is **not** a native window shell or webview host. It is a **React Native ↔ Rust binding generator** — it auto-generates Rust↔C++↔TypeScript glue via React Native's TurboModule/JSI, bypassing the platform-specific ObjC/Java TurboModule layers for zero-overhead native-module calls.

- Repo: `github.com/leegeunhyeok/craby` (240★, 12 forks, 177 commits, single owner)
- Version: **0.1.0-rc.8** (pre-1.0 release candidate, published 15 Aug 2026)
- README: *"This project is under development"*
- 57% documented

This reframes path (b): "React Native/craby.rs" really means *rewrite the app in React Native*, where craby.rs is only the Rust-brain binding layer. The webview/CDP hosting would have to come from React Native's *own* webview component — a separate unknown craby.rs does nothing to de-risk.

## 2. Scorecard against the four make-or-breaks

| Requirement | craby.rs | Verdict |
|---|---|---|
| **(1) WebView2 + CDP on Windows for the game browser** | No webview, no CDP, no Windows story. craby.rs is a binding generator, not a host. Its own docs: ❌ "native UI components", ❌ "platform context access". Config (`craby.toml`) has only `[project]` + `[android]` sections — **no Windows/desktop config at all**. | ❌ FAIL (category mismatch) |
| **(2) Multi-webview hosting + creation order** | Out of scope — no window, no webview, no creation-order concept. Would fall to RN's own webview component (separate unknown). | ❌ FAIL (out of scope) |
| **(3) IPC story (the stated payoff)** | **Half-realized.** craby.rs DOES replace the JS→Rust `invoke` half (83 commands → typed generated module methods, zero-overhead). But it CANNOT do Rust→JS push: docs explicitly exclude ❌ "event emitters". The ~20 streaming event types (token streaming, reasoning deltas, plan progress, inflight status) — the part that makes the UI feel alive — would need a separate hand-written mechanism. | ⚠️ PARTIAL |
| **(4) Maturity + Windows readiness** | v0.1.0-rc.8, "under development", single owner, Android-only config, zero Windows mention anywhere. | ❌ FAIL |

**Result: 0/4 hard requirements fully met.** craby.rs fails on the game-browser requirement (the entire point of the architecture) and on Windows readiness, independent of everything else.

## 3. Comparison vs path (a)

| | Path (a) — Tauri bare-window ordered children | Path (b) — React Native + craby.rs |
|---|---|---|
| **De-risked?** | ✅ Fully (spike fa099902: draw order, tab switching, crash isolation) | ❌ No — 0/4 make-or-breaks met |
| **Effort** | **S** (reconfigure main.rs window model + 1 spike, already done) | **M/L** (full RN rewrite + separate webview/CDP solution + separate event-push mechanism) |
| **Keeps React frontend?** | ✅ All of it (it IS the agent-chat webview) | ❌ Rewrites it as RN components |
| **Keeps IPC?** | ✅ Tauri invoke/listen works in child webviews | ⚠️ invoke→generated bindings (win); listen→unsolved (needs separate bridge) |
| **Keeps myharness brain?** | ✅ Untouched | ✅ Untouched (craby.rs binds to it) |
| **CDP scoped to game browser only?** | ✅ Yes (security win) | ❓ Unknown — depends on RN webview, not craby.rs |
| **Windows maturity** | ✅ Tauri + WebView2 = production on Windows | ❌ craby.rs Android-only, pre-1.0 |

## 4. Recommendation

**Ship path (a).** craby.rs is the wrong category of tool for this architecture (a binding generator, not a webview host), fails the Windows-readiness bar, and only half-solves the IPC payoff it was being considered for. Path (a) is fully de-risked, is the smallest change, keeps all of React + IPC + the brain, and delivers the CDP-scoping security win.

Path (b) via craby.rs is **NO-GO**. The broader "leave Tauri" question has no compelling trigger in the record — the IPC layer works, the brain is decoupled, and the only real problem (CDP reading the app's own DOM) is solved by path (a)'s creation-order reconfigure, not by a rewrite.

## 5. Sources

- docs.rs: https://docs.rs/craby/latest/craby/ (v0.1.0-rc.8, MIT, 15 Aug 2026)
- Introduction: https://craby.rs/docs/get-started/introduction ("When to Use Craby" — ❌ native UI components, ❌ event emitters)
- Configuration: https://craby.rs/docs/get-started/configuration (`craby.toml` — `[project]` + `[android]` only, no Windows)
- Sync vs Async: https://craby.rs/docs/guides/sync-vs-async (sync = JS thread, async = Promise on separate thread)
- Repo: https://github.com/leegeunhyeok/craby (240★, "under development")
