// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Job 4 D1 fixtures — the standalone Setup surface (§5.8 :486-589):
//! open/isolation, the readiness report (the ordered missing list), and
//! the `Engine::start` precondition (the `not_configured` fail-fast; a
//! missing embedder model is NOT fatal). The mutating surface lands
//! with D2+.

use std::path::{Path, PathBuf};

use mnemo::agent::policy::PermissionPolicy;
use mnemo::headless::{Engine, EngineOptions, MemoryBackend, Setup, SetupStep};
use mnemo::memory::classifier::ClassifierStatus;
use mnemo::memory::embedder::EmbedderStatus;
use mnemo::workspace::Workspace;

/// The engine options every fixture here uses (the sibling suites'
/// `opts` twin — this suite is self-contained, main.rs's contract).
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

/// A READY config (§5.8 :563: an endpoint + key + default model served
/// by it; the dead base_url is fine — `status` never calls it). The
/// embedding model is explicitly opted OUT (`hash`) so the fixtures
/// below isolate the leg under test (a fresh config otherwise
/// defaults to the lightest bundled model — the optional EmbedderModel
/// step).
fn write_ready_config(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"test-model\"\nbundled_embedding_model = \"hash\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"test-model\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("keys.toml"), "[test]\napi_key = \"sk-test\"\n").unwrap();
}

/// `Setup::open` resolves + creates the config dir, and a FRESH install
/// reports not-ready with the ordered missing list: the fatal leg first,
/// then the optional embedder leg (a fresh config defaults to the
/// lightest bundled model, which is not installed yet — the engine
/// would run on the hash fallback, `semantic: false`).
#[test]
fn open_creates_dir_and_reports_unready() {
    let cfg = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    assert!(cfg.path().is_dir());
    assert_eq!(setup.config_dir(), cfg.path());
    let status = setup.status(None).expect("status");
    assert!(!status.ready_to_run);
    assert_eq!(
        status.missing,
        vec![SetupStep::DefaultModel, SetupStep::EmbedderModel]
    );
    assert!(status.endpoints.is_empty());
    assert_eq!(status.default_model, None);
    assert_eq!(status.classifier, ClassifierStatus::Disabled);
    assert!(status.project.is_none());
    assert!(
        status.embedder.configured.is_some(),
        "a fresh config defaults to the lightest bundled model"
    );
    assert!(!status.embedder.installed);
    assert!(!status.embedder.semantic);
    assert_eq!(status.embedder.status, EmbedderStatus::Failed);
}

/// `ready_to_run`'s rule (§5.8 :563): a default model served by an
/// endpoint that has it, with a key when its kind needs one. Removing
/// the key flips a key-needing endpoint back to not-ready; a Local
/// endpoint runs keyless.
#[test]
fn ready_needs_endpoint_key_and_default_model() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    let status = setup.status(None).unwrap();
    assert!(status.ready_to_run);
    assert_eq!(status.default_model.as_deref(), Some("test-model"));
    assert!(status.missing.is_empty());
    assert_eq!(status.endpoints.len(), 1);
    assert!(status.endpoints[0].has_key);
    assert_eq!(status.endpoints[0].reachable, None);

    // Removing the key: the openai-kind endpoint needs one → not ready.
    std::fs::remove_file(cfg.path().join("keys.toml")).unwrap();
    let status = setup.status(None).unwrap();
    assert!(!status.ready_to_run);
    assert_eq!(status.missing, vec![SetupStep::DefaultModel]);

    // A Local-kind endpoint never needs a key.
    std::fs::write(
        cfg.path().join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"test-model\"]\n",
    )
    .unwrap();
    let status = setup.status(None).unwrap();
    assert!(status.ready_to_run);
    assert!(!status.endpoints[0].has_key);
    assert!(status.missing.is_empty());
}

/// The embedder leg is NEVER fatal (§5.8 :581-587): a configured but
/// uninstalled model reports `semantic: false` + the `EmbedderModel`
/// step while the engine still runs on the hash fallback. The bogus
/// model id keeps `installed` false in EVERY build (plain + embeddings —
/// no real cache can hold it).
#[test]
fn embedder_leg_is_non_fatal() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path()).unwrap();
    std::fs::write(
        cfg.path().join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"test-model\"\nbundled_embedding_model = \"definitely-not-a-real-model\"\n",
    )
    .unwrap();
    std::fs::write(
        cfg.path().join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"test-model\"]\n",
    )
    .unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    let status = setup.status(None).unwrap();
    assert!(status.ready_to_run, "a missing embedder is not fatal");
    assert_eq!(
        status.embedder.configured.as_deref(),
        Some("definitely-not-a-real-model")
    );
    assert!(!status.embedder.installed);
    assert!(!status.embedder.semantic);
    assert_eq!(status.embedder.status, EmbedderStatus::Failed);
    assert_eq!(status.missing, vec![SetupStep::EmbedderModel]);
}

/// The project leg reports plain fs facts and orders the to-do list
/// (§5.8 :569-570): an unscaffolded root carries the (optional) Project
/// step; scaffolding `.coding/plans` + the root `agent.md` clears it.
#[test]
fn project_leg_reports_and_orders() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let root = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();

    let status = setup.status(Some(root.path())).unwrap();
    let project = status.project.expect("the project facts are present");
    assert!(!project.is_git_repo);
    assert!(!project.coding_dir);
    assert!(!project.scaffolded);
    assert!(!project.agent_md);
    assert!(!project.codegraph_indexed);
    // The config is ready — the ONLY missing step is the (optional)
    // project scaffold.
    assert!(status.ready_to_run);
    assert_eq!(status.missing, vec![SetupStep::Project]);

    std::fs::create_dir_all(root.path().join(".coding").join("plans")).unwrap();
    std::fs::write(root.path().join("agent.md"), "# rules\n").unwrap();
    let status = setup.status(Some(root.path())).unwrap();
    let project = status.project.expect("the project facts are present");
    assert!(project.coding_dir);
    assert!(project.scaffolded);
    assert!(project.agent_md);
    assert!(status.missing.is_empty());
}

/// §5.8 :581-587: without an injected provider an unready config fails
/// FAST with error code `not_configured` (the missing list in the
/// message) instead of deep inside the brain. With a ready config the
/// same start succeeds (the bare-start contract the G3 fixtures pin).
#[tokio::test]
async fn engine_start_fails_fast_when_not_configured() {
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    let err =
        match Engine::start(opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf())))
            .await
        {
            Ok(_) => panic!("an empty config without an injected provider is not runnable"),
            Err(e) => e,
        };
    assert_eq!(err.code(), "not_configured");
    assert!(
        err.message().contains("default model"),
        "the message names the missing steps: {}",
        err.message()
    );

    // The same start over a READY config succeeds — the precondition is
    // the only thing between them.
    let root = tempfile::tempdir().unwrap();
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let engine = Engine::start(opts(Workspace::new(root.path()), Some(cfg.path().to_path_buf())))
        .await
        .expect("a ready config starts");
    engine.shutdown().await.expect("shutdown ok");
}