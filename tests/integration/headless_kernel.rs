//! The kernel-level engine fixtures (job 3a.4): the workspace contract —
//! an engine starts on a plain NON-git directory (G3) and honors an
//! out-of-tree state dir (the row-27 engine form).

use std::path::PathBuf;

use mnemo::agent::policy::PermissionPolicy;
use mnemo::headless::{Engine, EngineOptions, MemoryBackend};
use mnemo::workspace::Workspace;

/// Real-start options: a fresh workspace, a temp config dir, indexing off
/// (test speed), the free default.
fn opts(workspace: Workspace, config_dir: Option<PathBuf>) -> EngineOptions {
    EngineOptions {
        workspace,
        config_dir,
        permissions: PermissionPolicy::OverridePermissions,
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: false,
        require_git: false,
        auto_install_embedder: false,
    }
}

/// G3 (§5.9.3): an engine works on ANY directory — a fresh temp dir with
/// no git repo starts cleanly (require_git false; the Git tool group
/// simply does not register).
#[tokio::test]
async fn workspace_without_git_starts() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let engine = Engine::start(opts(
        Workspace::new(root.path()),
        Some(cfg.path().to_path_buf()),
    ))
    .await
    .expect("a plain non-git directory starts");
    engine
        .shutdown()
        .await
        .expect("shutdown is clean with no sessions");
}

/// G3's row-27 engine form: the state dir may sit anywhere — the memory
/// database lands at the OUT-of-tree state dir, not under the root.
#[tokio::test]
async fn state_dir_outside_tree() {
    let root = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let workspace = Workspace::with_state_dir(root.path(), state.path());
    let engine = Engine::start(opts(workspace, Some(cfg.path().to_path_buf())))
        .await
        .expect("an out-of-tree state dir starts");
    engine.shutdown().await.expect("shutdown ok");
    assert!(
        state.path().join("memory.db").exists(),
        "the memory db lands at the out-of-tree state dir ({})",
        state.path().display()
    );
    assert!(
        !root.path().join(".coding").exists(),
        "the root stays untouched — no in-tree state dir appears"
    );
}
