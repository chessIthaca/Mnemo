## Verdict: FINDINGS (0 high, 1 low)

One documentation-accuracy finding on the new agent.md rule; the code change itself is correct on every axis checked (lock safety, failure contract, feature confinement, cache-dir semantics).

## What I read

- Commit 7d60f41 (message + stat): 5 files — src/memory/embedder.rs (19 lines), Cargo.toml, Cargo.lock (out of scope), agent.md (10 lines), .coding/plans/4c7cacc1.md (bookkeeping). Diff hunks of embedder.rs via `git show`; working tree clean except the plan file's review stamp.
- src/memory/embedder.rs lines 1–260 (struct, `new`, catalog, `embed`) and the test tail.
- src/provider/client_factory.rs `build_embedder`/`load_bundled_or_fallback` (both feature branches) — untouched by the commit.
- src-tauri/Cargo.toml, src-tauri/src/ipc/embeddings.rs (cache-dir + download paths), root Cargo.toml (features, workspace, vendored patches).
- .github/workflows/build.yml and codeql.yml in full; docs.rs fastembed 7.1.0 crate docs; repo-wide `fastembed`/`ureq` searches.

## Tasked verification

1. **Lock safety — PASS.** The guard (`model.lock().expect(...)`, embedder.rs:227) is created and dropped entirely inside the sync `spawn_blocking` closure; the only `.await` is on the `JoinHandle` afterward. `std::sync::Mutex` is the right choice — this is the canonical tokio pattern for a sync closure (a `tokio::sync::Mutex` guard would be useless inside `spawn_blocking` and add async overhead). Concurrency on callers: `build_embedder` returns `Arc<dyn Embedder>` shared by the live `MemoryStore`, so concurrent recalls serialize on the mutex — inherent to fastembed 7's `&mut self` receiver and acceptable (single ONNX model instance = one CPU-bound inference at a time; parallelism would just contend on CPU). No deadlock: the model mutex and the status `RwLock` are never held simultaneously (guard dropped before the status write). Poisoning degrades per contract: a panic inside the closure (including the lock-poison `expect`) surfaces as a `JoinError`, caught by the `_` arm → status `Fallback` + zero vector.

2. **Failure contract — PASS, byte-for-byte.** The diff shows the two match arms as unchanged context lines; only the future construction and `let embed = ...; match embed.await` split changed. `Ok(Ok(batches)) if !batches.is_empty()` → dim check → `Ready` / dim-mismatch `Fallback` + zero; `_` (covers `Ok(Err)`, `Err(JoinError)`, and empty batches) → `Fallback` + zero. No arm lost or reordered, no early returns, `status` written on every path.

3. **Feature confinement — PASS.** All touched code sits inside `#[cfg(feature = "embeddings")]` regions (struct 144, impls 155/213). client_factory.rs is not in the commit — the hash fallback path is byte-identical. Default-feature suite (2971) green.

4. **Cache-dir semantics — PASS.** docs.rs fastembed 7.1.0: "Override the location with the FASTEMBED_CACHE_DIR env var or TextInitOptions::with_cache_dir" — `with_cache_dir` still means exactly what it did in 4.x (explicit cache location; the env var is only the unset-default). Download (Settings path) and load (startup path) both go through `BundledEmbedder::new` with `embedder_cache_dir()`, so first-run downloads land where the app polls and re-loads. Minor observation, not a finding: the 7.x docs state `HF_HOME` takes precedence over `with_cache_dir` — a user-set `HF_HOME` would redirect downloads away from the polled dir; environment-dependent and the same class of risk existed via hf-hub before.

5. **Standing checks — PASS.** No `cfg(windows)` additions; change is platform-neutral (ort/hf-hub cross-platform, macOS covered by build.yml). No shell-based file mutation. No `#[allow(...)]`; `#![deny(warnings)]` + green builds prove warning-free. README/PLAN.md need no update (PLAN.md:398 mentions fastembed generically, still accurate; the module docs were updated with the Arc<Mutex> rationale). No source references ureq (transitive only). Plan file matches what shipped.

## Findings

### L1 (low) — agent.md CI-coverage claim is over-stated: a CI job DOES compile the feature tree

The new rule says "Plain `cargo test` never compiles those paths and **no CI job does either** (`build.yml` triggers on manual dispatch + `v*` tags only; `codeql.yml` builds default features)". Two inaccuracies:

- **build.yml, when it runs, compiles the embeddings tree.** Step "Rust tests (workspace...)" (build.yml:64) runs `cargo test --workspace`; the workspace includes `src-tauri`, whose Cargo.toml:12 declares `mnemo = { path = "..", features = ["browser", "embeddings"] }`. Under workspace feature unification that compiles the `embeddings`-gated code — so a broken feature tree fails every tag build / manual dispatch loudly. The real gap is narrower: **no pull-request-triggered workflow compiles it** (PRs trigger only codeql.yml).
- **codeql.yml does not "build default features"** for Rust — its own comment (codeql.yml:48–51) says Rust is extracted *without a build* (no-build-only analysis).

Fix: reword to the accurate claim, e.g. "Plain `cargo test` never compiles those paths, and no pull-request-triggered CI job does either (codeql.yml's Rust analysis is no-build-only; build.yml — which does compile them via `cargo test --workspace` with src-tauri's feature selection — triggers only on manual dispatch and `v*` tags), so a PR can merge a broken feature tree unnoticed until the next tag build." The rule itself (feature-enabled run required on a feature-gated bump) is sound and needed either way.

## Regression-test judgment

**Sufficient as-is.** The defect is compile-shaped (E0596 + deprecated-alias-as-error); no runtime test can assert a compile failure, and the documented red-check (`cargo check --features embeddings` exit 101 at PR #67's head → 0 after) exercises the real artifact directly. The durable guards are (a) the new agent.md rule and (b) the fact — once L1 is reworded, still true — that a broken feature tree cannot survive a v* tag build. A strictly stronger guard would be a PR-triggered `cargo check --features embeddings --workspace` CI step, making the check mechanical rather than procedural; that is an infrastructure change outside this plan's scope — file it as a backlog candidate, not a blocker on this fix.

Reviewed-state: 7d60f419e34c573cc3f823c6b7ad590ca8ae83ae
