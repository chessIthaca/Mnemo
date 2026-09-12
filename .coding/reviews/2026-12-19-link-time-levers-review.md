## Verdict: FINDINGS (0 high, 1 low)

Review of ALL uncommitted changes on `wt/agenticcoder` (working tree vs HEAD) for plan 1c712c30 "Implement all link-time levers". The change is solid: gating is complete and total-bodied per feature world, feature-on behavior is provably unchanged (exact registry-count asserts + green full-feature run), the merged integration binary has no shared-resource collisions, and both stale-test repairs strengthen rather than weaken their assertions. One LOW pre-existing doc imprecision adjacent to the new browser-feature paragraph is flagged below.

**Summary of what was verified:** every cfg-gate seam (browser/embeddings) traced to its references; light-branch logic of `build_embedder` / `embedder_startup_plan` read and cross-checked against their two new light-mode tests; cargo `[profile.*.package."*"]` semantics verified against the Cargo Book; the 429 drain task proven race-free by ordering analysis; the FTS probe confirmed to bypass the capped-semantic-fallback that invalidated the old `recall("alpha")` probe; tests/integration scanned for Windows-only assumptions and for cross-suite resource collisions (env vars, ports, fixed temp paths).

---

### Findings

**F1 (LOW, pre-existing — recommend one-word fix): README.md:113 — "the headless `browser_*` tools work everywhere" is imprecise about tool naming and Windows-only scope.**
The headless (chromiumoxide) tools are actually named `offscreen_browser_*`; the on-screen live-WebView2 inspection tools are the ones named `browser_*` (`browser_navigate`, `browser_screenshot`, `browser_eval`, …), and those six are registered only under `#[cfg(windows)]` inside `register_browser_tools` (src/agent/factory.rs, expected-set test lines 1866-1874 confirm). As written, the parenthetical implies every `browser_*` tool works everywhere, which is wrong for the six on-screen ones and misnames the cross-platform set. This line is unchanged by this diff (README diff is +2 insertions only), but since this plan gates exactly that seam and adds a feature-flag paragraph directly above it, a reader consulting the README to reason about the new `browser` feature will be misled. Suggested wording: "…they are disabled on macOS (the headless `offscreen_browser_*` tools work everywhere; the on-screen `browser_*` inspection tools are also Windows-only)." Low severity: doc-only, no behavioral impact.

---

### Verified — check 1: cfg-gate completeness (light build)

- **No dangling references.** Grep across all `.rs` for `crate::browser`, `BundledEmbedder`, `BundledModelInfo`, `hf_cache_subdir`, `is_model_installed`: every lib hit is inside a gated module (`src/browser/`, `src/tool/browser`, gated sections of `embedder.rs`, `factory.rs`, `client_factory.rs`); the remaining hits are in `src-tauri`, which permanently selects both features (`features = ["browser", "embeddings"]`). `fastembed`/`chromiumoxide` strings outside gated code are doc comments only.
- **String literals are safe.** `src/tool/mod.rs` tests referencing `offscreen_browser_eval` / `browser_snapshot` etc. use them as plain strings against `ToolFilter` filter logic — no gated types, no registry dependency; correct in light builds.
- **Light-branch logic is correct, not just compiling.** `build_embedder`'s light branch (client_factory.rs:325-338) returns a working `HashEmbedder` AND sets status `Failed` with a clear eprintln explaining the missing feature — memory recall degrades gracefully; it does not silently pretend all is well. `embedder_startup_plan`'s light branch returns `HashDefault` for a configured model, and the two are documented as complementary (the plan function serves the app's startup UI, which always has the feature; `build_embedder` is what surfaces the mismatch in light builds). Both branches are pinned by the two new `#[cfg(not(feature = "embeddings"))]` tests (`build_embedder_without_embeddings_feature_reports_failed_and_runs_hash`, `embedder_startup_plan_without_feature_degrades_to_hash_default`) — the first asserts both the Failed status and a working `EMBEDDING_DIM`-sized embedding.
- **Dual-cfg total-body helpers** (`load_bundled_or_fallback`, `installed_model_plan`) are the right pattern: one body per feature world, no `cfg` mid-function, each independently readable and testable.
- **`expected_tool_names` shadowing** (factory.rs:1856-1876) avoids the unused-`mut` trap under light builds while keeping the exact-count assertion (`registry.iter().count() == expected.len()`) strict in every feature/platform combination — including the `#[cfg(windows)]` extend for the six on-screen tools. The full-feature Windows run green under `deny(warnings)` proves both worlds stay warning-free.

### Verified — check 2: feature-on behavior unchanged

- Browser tool registration moved verbatim into the gated `register_browser_tools` fn called at the original call site; the registry test asserts the identical expected set (10 `offscreen_browser_*` + 6 Windows `browser_*` + everything else) and an exact total count — no silent tool loss or addition.
- `with_browser` / `browser_slot` / the `browser` field are pure moves behind gates; the IPC seam (src-tauri) uses them unchanged.
- Embeddings path with the feature on is byte-identical logic (`load_bundled_or_fallback` feature branch is the old `build_embedder` body, including the do-not-overwrite-to-Ready Failed semantics).
- Profile overrides change build inputs, not behavior. Verified against the Cargo Book (doc.rust-lang.org/cargo/reference/profiles.html): `[profile.dev.package."*"]` applies "for any non-workspace member" — i.e. workspace code (mnemo, src-tauri) keeps the base `debug = "line-tables-only"`; only dependency rlibs lose debuginfo. The README and Cargo.toml comment claims match the documented semantics exactly. (Build deps use `build-override`, unaffected.)

### Verified — check 3: documentation sync

- README Building paragraph: feature names, app-always-selects-both claim (matches `src-tauri/Cargo.toml`), light-build degrades-to-hash-with-status-Failed behavior (matches code), and the dep-debugnote claim (verified above) are all accurate.
- PLAN.md decision row accurately reflects the measured outcome including the CGU=1 no-win-not-adopted decision.
- F1 above is the only doc gap found, and it is pre-existing.

### Verified — check 4: multi-platform neutrality

- No new `cfg(windows)` in library code outside the sanctioned WebView2/browser seam: the only platform gates added are the `#[cfg(all(windows, feature = "browser"))]` test gate and the `#[cfg(windows)]` expected-names extend, both inside the browser feature block — the sanctioned exception.
- The light/full code paths (`build_embedder`, `embedder_startup_plan`, factory browser seam) are platform-neutral.
- `tests/integration/`: the four moved files are byte-identical renames (similarity 100%), and a scan of all five files found no `cfg(windows)`, no `C:\`/`LOCALAPPDATA`/`WebView2` paths, no Windows-only assumptions. The new `main.rs` is a 14-line doc comment + four `mod` declarations — nothing platform-specific. On macOS the browser feature still compiles (chromiumoxide is cross-platform) and the on-screen tools stay unregistered exactly as before.

### Verified — check 5: the 429 test changes (drain fix + `>=1`)

- **No event-eating race.** The drain task is spawned strictly after the final `wait_for_turn_outcome(&mut fanin_rx)` for turn 2, and every subsequent assertion in both tests reads atomic counters (`primary_exec_calls`, `fallback_plan_calls`), not the channel. Nothing the tests still need flows through `fanin_rx` after the drain starts, so the drainer cannot eat anything required.
- **Termination is structural, not lucky.** The fanin sender is owned by the agent task's `run(cmd_rx, fanin_tx)` future; the tests await `handle` BEFORE `drain`, so when the handle completes the sender is dropped, the drain empties the buffer, `recv()` returns `None`, and both awaits resolve. On the default current-thread `#[tokio::test]` runtime the test's own `handle.await` drives the agent to completion — no scheduling dependence. The pre-fix deadlock (agent blocked on a full bounded channel with nobody draining) is genuinely gone, and the bounded channel itself is retained, so the fix tests the real production channel shape.
- **The regression stays pinned.** The relaxed assertion is only `primary_exec_calls >= 1` (exec-model served turn 2 at least once) — legitimate, since auto-continuation may legitimately drive additional primary prompts. The fallback counter assertion is exact (`assert_eq!(fallback_plan_calls, fallback_after_turn1)`): if the 429-fallback-pinning bug ever returned, the fallback client would serve the Executing turn and the counter would advance past its turn-1 value, failing the assert. The frozen-counter check plus `>= 1` together pin exactly the intended invariant: fallback absorbs the 429 blip, primary serves the rest.

### Verified — check 6: the memory test changes

- `update_memory_roundtrips_and_keeps_fts_in_sync` now probes `MemoryStore::fts_candidates_locked(&conn, "alpha", …)` directly and asserts `FtsResult::NoMatches` — this is the *right* probe. The in-code comment correctly explains why `recall("alpha")` stopped working as a probe: since cb1914b9 a zero-FTS-match recall falls through to the capped semantic scan, so the row legitimately resurfaces even though FTS dropped it. The old assertion was testing an accident of implementation; the new one tests the actual FTS re-index contract. The positive direction (recall("beta") == 1) is retained, so the test still verifies both directions of the re-index.
- The other updated recall tests assert the intended cb1914b9 semantics with meaningful values, not gutted assertions: zero-keyword recall → capped semantic scan returns both rows (len 2, not empty); no-FTS-match ("zebra") → the same capped scan returns rows (len asserted, ordering pinned most-recent-first); the fallback cap test asserts cap-at-`limit` + exact ordering + tier filtering. These pin the capped-semantic-fallback contract the tests now claim to test.

### Verified — collateral risks of the test-binary merge (lever 5)

The classic hazard of merging four test binaries into one process is shared mutable global state that used to be process-isolated. Scanned all four moved suites plus `main.rs`: no `std::env::set_var`/`remove_var`, no fixed `127.0.0.1:<port>`/`localhost:<port>` bindings, no fixed `mh-`/`.mnemo` paths — the suites are self-contained (as the `main.rs` doc comment states) and use per-test tempdirs. The declared `mod` set exactly matches the four moved files, and the 1986-test count parity confirms no suite was silently dropped. Test names keep their `module::test` paths, so `cargo test --test integration workflow_integration::…` targeting still works as before (by name filter rather than by binary — an acceptable, documented trade).

### Verified — knowledge files

The three 1-line knowledge-file edits add `status = "superseded"` markers (correct hygiene per the memory conventions — supersede, never delete stale state), and the new link-time research/spec knowledge files back the measured claims cited in the plan. No repo behavior.

---

**Bottom line:** no correctness, security, or platform findings in the changed code. The single LOW finding (F1) is a pre-existing README wording imprecision adjacent to the new docs; fix it with a one-word correction (`offscreen_browser_*`) or skip with justification that it predates this change.
