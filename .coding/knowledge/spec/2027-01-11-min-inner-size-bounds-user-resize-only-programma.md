+++
title = "min_inner_size bounds user resize only — programmatic setSize is unclamped (tao WM_GETMINMAXINFO)"
created = "2027-01-11"
+++

Verified against the vendored tao tree (2027-01-14, plan 9ff59133): Rust-side `min_inner_size(...)` (src-tauri/src/main.rs, currently 800×560) constrains ONLY user-driven resizing — it does NOT clamp programmatic window resizes. Mechanism, three links:
1. `set_min_inner_size` (vendor/tao/src/platform_impl/windows/window.rs:317-329) merely stores the value into `window_state.size_constraints` (then re-applies the current size so Windows re-checks).
2. The constraints are consumed only in the `WM_GETMINMAXINFO` handler (vendor/tao/src/platform_impl/windows/event_loop.rs:1888-1938) → `ptMinTrackSize` / `ptMaxTrackSize` — Windows' TRACKING size, which applies to user sizing (border drag / maximize), not to `SetWindowPos`.
3. `set_inner_size` (window.rs:273-314) → `util::set_inner_size_physical` (vendor/tao/src/platform_impl/windows/util.rs:91-118) → explicit `SetWindowPos(..., outer_x, outer_y, SWP_NOMOVE|…)` with NO consultation of `size_constraints`.

Consequence: any frontend `getCurrentWindow().setSize(...)` (e.g. the startup geometry restore in frontend/src/App.tsx) applies the requested size verbatim, however small. Frontend code that restores sizes MUST clamp itself; the Rust min is not a safety net. macOS/Linux have their own paths (`contentMinSize`), but the same rule holds: treat the Rust min as resize-time-only. Root cause of the 2027-01-14 "tiny window top left" report (fix plan 9ff59133, helper frontend/src/lib/windowRestore.ts).
