+++
title = "Test temp folders live inside the checkout's target folder"
created = "2026-11-24"
evidence = ".cargo/config.toml [env] and its comment; tests/integration/ci_workflow.rs temp_dir_is_inside_the_checkouts_target_dir; scripts/test-tmp.mjs; the user asked on 2026-11-24 that test output stay in the dev tree and on its drive."
+++

Every temp folder a cargo-launched process creates (tests, build scripts, `cargo run`, and their children) lives in `<checkout>/target`, never in the user's system temp folder. The workspace `.cargo/config.toml` [env] sets TMP, TEMP and TMPDIR to `{ value = "target", relative = true, force = true }`.
- The value is `target`, not `target/tmp`: cargo creates target/tmp only for integration and bench builds, so unit-test binaries would see a missing folder and every tempdir() would fail with NotFound.
- `force` is required because Windows always sets TMP/TEMP; `relative` gives each pool checkout its own target.
- [env] also sets GIT_CEILING_DIRECTORIES=target. A temp folder under target lies inside the checkout's worktree, so without the ceiling, git run there finds the checkout's own .git: "not a repository" tests break, and a test's commit landed on the checkout's real branch.
- Consequence: a bare tempdir now has the checkout's `.coding/` among its ancestors. A test that needs "no project above this path" uses a non-existent child of the drive root instead of a tempdir (see the path_outside_any_project helper in src/app/project.rs).
- The JavaScript tests (vitest and the `node --test` script tests) use `<checkout>/target/tmp` through scripts/test-tmp.mjs, which creates it and sets the same three variables.
- Guards: temp_dir_is_inside_the_checkouts_target_dir (tests/integration/ci_workflow.rs) for Rust, and "keeps os.tmpdir() inside the repo target/tmp" / "script tests keep os.tmpdir() inside the repo target/tmp" for JS.
- Leaked folders therefore show up as `.tmp*` directly in target; `cargo clean` and the pool trim remove them.
