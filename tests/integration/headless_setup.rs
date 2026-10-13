// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Job 4 fixtures — the standalone Setup surface (§5.8 :486-589):
//! open/isolation + the readiness report (D1), the endpoints/keys group
//! (D2), the settings split (D3), the live-apply (D4), the classifier
//! surface (D5) — plus the `Engine::start` precondition
//! (`not_configured`; a missing embedder model is NOT fatal).

use std::path::{Path, PathBuf};

use mnemo::agent::policy::PermissionPolicy;
use mnemo::config::settings_dto::VisionModelDto;
use mnemo::headless::{
    EndpointDto, Engine, EngineOptions, KeySource, MemoryBackend, RecvError, Secret, Setup,
    SetupEvent, SetupStep,
};
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
// --- D2: Secret + KeySource + the endpoints group ---

/// §5.8 :535: `set_api_key` is WRITE-ONLY — there is no getter; a fresh
/// handle reads the persisted key; `keys.toml` lands with the entry.
#[test]
fn set_and_has_api_key_round_trip() {
    let cfg = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    assert!(!setup.has_api_key("test"));
    setup.set_api_key("test", Secret::new("sk-witness")).unwrap();
    assert!(setup.has_api_key("test"));
    let keys = std::fs::read_to_string(cfg.path().join("keys.toml")).unwrap();
    assert!(keys.contains("sk-witness"), "the file stores the key: {keys}");
    // A FRESH handle over the same dir sees the persisted key.
    let fresh = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    assert!(fresh.has_api_key("test"));
}

/// §5.8 :578-580: `Env`/`InMemory` sources serve READS without any
/// plaintext (or file) on disk — and refuse the write paths.
#[test]
fn non_file_key_sources_refuse_writes_and_serve_reads() {
    let cfg = tempfile::tempdir().unwrap();
    let env_source = Setup::open(Some(cfg.path().to_path_buf()))
        .unwrap()
        .with_key_source(KeySource::Env("MNEMO_TEST_KEY_DEFINITELY_UNSET_9431".into()));
    assert!(!env_source.has_api_key("any"));
    env_source
        .set_api_key("any", Secret::new("sk-x"))
        .expect_err("Env sources refuse writes");
    let mut map = std::collections::HashMap::new();
    map.insert("injected".to_string(), "sk-mem".to_string());
    let mem_source = Setup::open(Some(cfg.path().to_path_buf()))
        .unwrap()
        .with_key_source(KeySource::InMemory(map));
    assert!(mem_source.has_api_key("injected"));
    assert!(!mem_source.has_api_key("missing"));
    mem_source
        .set_api_key("any", Secret::new("sk-x"))
        .expect_err("InMemory sources refuse writes");
    // Neither read created a keys.toml — the sources never touch disk.
    assert!(!cfg.path().join("keys.toml").exists());
}

/// §5.8 :534: upsert validates (the same lib rules the GUI's save
/// uses), persists, and PRESERVES an untouched key; a refused upsert
/// leaves the config unchanged.
#[test]
fn upsert_endpoint_validates_persists_and_preserves_keys() {
    let cfg = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    let dto = EndpointDto {
        name: "test".into(),
        kind: "openai".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        models: vec!["test-model".into()],
        max_context: Some(8192),
        max_output_tokens: None,
        multimodal: false,
        supports_reasoning_effort: false,
        reasoning_effort: None,
        workspace_id: None,
        has_api_key: false,
    };
    setup
        .upsert_endpoint(dto.clone(), Some(Secret::new("sk-first")))
        .unwrap();
    let listed = setup.endpoints().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "test");
    assert_eq!(listed[0].kind, "openai");
    assert!(
        listed[0].base_url.ends_with('/'),
        "the trailing slash is appended"
    );
    assert_eq!(listed[0].models, vec!["test-model".to_string()]);
    assert!(listed[0].has_api_key);

    // Upsert again with key: None — the EXISTING key is preserved.
    let mut dto2 = dto.clone();
    dto2.max_output_tokens = Some(4096);
    setup.upsert_endpoint(dto2, None).unwrap();
    assert!(setup.has_api_key("test"));
    assert_eq!(
        setup.endpoints().unwrap()[0].max_output_tokens,
        Some(4096)
    );

    // A refused upsert (empty base_url) changes nothing.
    let mut bad = dto;
    bad.name = "bad".into();
    bad.base_url = " ".into();
    setup
        .upsert_endpoint(bad, Some(Secret::new("sk-leak")))
        .expect_err("an empty base_url is refused");
    assert_eq!(setup.endpoints().unwrap().len(), 1);
    assert!(!setup.has_api_key("bad"), "the refused key never landed");
}

/// §5.8 :534: remove drops the endpoint AND its key; the set
/// re-validation refuses to remove the default provider's endpoint.
#[test]
fn remove_endpoint_drops_key_and_guards_the_default() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    // write_ready_config's endpoint "test" IS the default provider.
    setup
        .remove_endpoint("test")
        .expect_err("the default provider's endpoint is guarded");
    // A second endpoint with its own key, then remove it.
    let dto = EndpointDto {
        name: "extra".into(),
        kind: "local".into(),
        base_url: "http://127.0.0.1:9/v1/".into(),
        models: vec![],
        max_context: None,
        max_output_tokens: None,
        multimodal: false,
        supports_reasoning_effort: false,
        reasoning_effort: None,
        workspace_id: None,
        has_api_key: false,
    };
    setup
        .upsert_endpoint(dto, Some(Secret::new("sk-extra")))
        .unwrap();
    assert!(setup.has_api_key("extra"));
    setup.remove_endpoint("extra").unwrap();
    assert!(!setup.has_api_key("extra"), "the key dropped with it");
    let listed = setup.endpoints().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "test");
    assert!(setup.has_api_key("test"), "the survivor's key is preserved");
}

/// The shared credential chain (moved from ipc/models.rs — one
/// implementation): override args win; the saved endpoint fills
/// base_url/kind + the stored key; an unknown endpoint without a
/// base_url errors.
#[test]
fn resolve_endpoint_credentials_shared_chain() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let config = mnemo::config::Config::load(cfg.path()).unwrap();
    // The saved endpoint fills base_url + kind + the stored key.
    let (base_url, api_key, kind) =
        mnemo::provider::models::resolve_endpoint_credentials(&config, "test", None, None, None)
            .unwrap();
    assert!(base_url.starts_with("http://127.0.0.1:9"));
    assert_eq!(api_key, "sk-test");
    assert_eq!(kind, "openai");
    // Override args win.
    let (base_url, _api_key, kind) = mnemo::provider::models::resolve_endpoint_credentials(
        &config,
        "test",
        Some("http://localhost:4/v1/".into()),
        Some("sk-override".into()),
        Some("anthropic".into()),
    )
    .unwrap();
    assert_eq!(base_url, "http://localhost:4/v1/");
    assert_eq!(kind, "anthropic");
    // An unknown endpoint with no base_url errors.
    assert!(mnemo::provider::models::resolve_endpoint_credentials(
        &config,
        "nope",
        None,
        None,
        None
    )
    .is_err());
}

/// §5.8 :540: `patch_settings` validates + persists (the same lib rules
/// the GUI's save path enforces); WITHOUT a live engine only the
/// persist happens — a fresh re-read sees the applied values.
#[tokio::test]
async fn patch_settings_persists_without_an_engine() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    let patch = mnemo::config::settings_dto::SettingsSaveDto {
        summarize_at_fill_rate: Some(0.42),
        ..Default::default()
    };
    setup.patch_settings(patch).await.expect("patch applies");
    // The persist: a fresh re-read sees the applied value.
    let config = mnemo::config::Config::load(cfg.path()).unwrap();
    assert_eq!(config.general.context.summarize_at_fill_rate, 0.42);
    // The ready state survives the patch.
    let fresh = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    assert!(fresh.status(None).unwrap().ready_to_run);
}
/// G9 (§5.9.9, row 33 leg 2): `Setup::settings()` exposes ONLY
/// EngineConfig — the app-owned `[ui]` table is absent from the type
/// (compile-time), and it SURVIVES a standalone engine-side save
/// ("on disk nothing moves": the GUI's theme rides through an
/// engine-half upsert untouched).
#[test]
fn settings_exposes_engine_config_and_preserves_ui() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    // A GUI user's [ui] half in the same file.
    std::fs::write(
        cfg.path().join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"test-model\"\nbundled_embedding_model = \"hash\"\n\n[ui]\ntheme = \"dark\"\nchat_thread_line = false\n",
    )
    .unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).unwrap();
    let settings = setup.settings().expect("settings");
    // The engine sections are exposed…
    assert_eq!(settings.general.default_model.as_deref(), Some("test-model"));
    assert_eq!(settings.endpoints.len(), 1);
    assert!(settings.endpoints[0].has_api_key);
    // …and the [ui] table survives a standalone engine-side save (the
    // type carries no field to touch it — there is nothing to name).
    setup
        .upsert_endpoint(
            EndpointDto {
                name: "second".into(),
                kind: "local".into(),
                base_url: "http://127.0.0.1:9/v1/".into(),
                models: vec![],
                max_context: None,
                max_output_tokens: None,
                multimodal: false,
                supports_reasoning_effort: false,
                reasoning_effort: None,
                workspace_id: None,
                has_api_key: false,
            },
            None,
        )
        .unwrap();
    let reloaded = mnemo::config::Config::load(cfg.path()).unwrap();
    assert_eq!(reloaded.general.ui.theme, "dark", "the [ui] table rides through");
    assert!(!reloaded.general.ui.chat_thread_line);
}// -- D5: the classifier surface (§5.8 :554) --

/// `classifier_setup(false)` persists the disable, stops nothing (no
/// sidecar runs in a fixture), parks the shared status on `Disabled`,
/// and the receiver yields the single terminal `Done` then closes.
#[tokio::test]
async fn classifier_setup_disable_persists_and_reports_done() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path()).unwrap();
    // Enabled in the config — the disable leg flips it back off.
    std::fs::write(
        cfg.path().join("config.toml"),
        "[general.laya]\nenabled = true\n",
    )
    .unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    let mut rx = setup.classifier_setup(false).expect("the disable leg");
    // The single terminal event, then the channel closes (the sender
    // rode the call).
    let event = rx.recv().await.expect("the Done event");
    assert!(matches!(
        event,
        SetupEvent::Done {
            step: SetupStep::LayaRuntime,
            ..
        }
    ));
    assert!(matches!(rx.recv().await, Err(RecvError::Closed)));
    // The disable PERSISTED. The serializer elides the all-default
    // `[general.laya]` section (the app's opt-in convention: absent ==
    // disabled), so the round-trip goes through the settings surface,
    // not the file text.
    let settings = setup.settings().expect("settings");
    assert!(!settings.general.laya.enabled);
    // The status leg reports the live (Disabled) state.
    let status = setup.status(None).expect("status");
    assert_eq!(status.classifier, ClassifierStatus::Disabled);
}

/// The enable guard (§5.8 :554): a claimed setup guard rejects a second
/// enable with `setup_in_flight` — the config flip still persisted (the
/// idempotent write happens before the guard check). The enable pipeline
/// itself is network-bound — never exercised in tests; the guard is what
/// keeps a second run out.
#[test]
fn classifier_setup_rejects_a_second_enable_with_setup_in_flight() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path()).unwrap();
    std::fs::write(cfg.path().join("config.toml"), "").unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    // Claim the guard the way a live pipeline would.
    assert!(setup.laya_manager().begin_setup());
    let err = setup.classifier_setup(true).expect_err("the guard rejects");
    assert_eq!(err.code(), "setup_in_flight");
    assert!(err.message().contains("already running"));
    // The flip persisted BEFORE the guard rejected (idempotent) —
    // through the settings surface (the file may elide or carry the
    // section; both read back enabled).
    let settings = setup.settings().expect("settings");
    assert!(settings.general.laya.enabled);
}

/// Install isolation (G4): the Setup's manager roots at
/// `<config_dir>/laya`, never the process-global dir.
#[test]
fn laya_manager_roots_at_the_config_dir() {
    let cfg = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    assert_eq!(setup.laya_manager().base_dir(), cfg.path().join("laya"));
}// -- D6: the embedding/vision surface (§5.8 :542-549) --

/// The catalog's installed flags track THIS Setup's config-dir cache
/// (D6: the global coupling died). A fresh dir reports nothing
/// installed; a doctored HF tree flips the matching entry. Under the
/// no-embeddings build the catalog is empty — the loops are vacuous.
#[test]
fn embedding_catalog_flags_track_the_config_dir_cache() {
    let cfg = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    for m in setup.embedding_models() {
        assert!(!m.installed, "'{}' flagged installed on a fresh dir", m.id);
    }
    // Doctor the HF tree for the catalog's light entry
    // (all-MiniLM-L6-v2): snapshots/<rev>/model_quantized.onnx — the
    // exact file `is_model_installed` probes for.
    let onnx = cfg
        .path()
        .join("models")
        .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
        .join("snapshots")
        .join("deadbeef");
    std::fs::create_dir_all(&onnx).unwrap();
    std::fs::write(onnx.join("model_quantized.onnx"), b"onnx").unwrap();
    for m in setup.embedding_models() {
        if m.id == "all-MiniLM-L6-v2" {
            assert!(m.installed, "'{}' NOT flagged installed after the doctor", m.id);
        }
    }
}

/// The air-gapped install copies a model tree into the config-dir
/// cache (no network; works in every build).
#[test]
fn install_embedding_model_from_copies_into_the_config_dir_cache() {
    let cfg = tempfile::tempdir().unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    let source = tempfile::tempdir().unwrap();
    let onnx = source
        .path()
        .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
        .join("snapshots")
        .join("deadbeef");
    std::fs::create_dir_all(&onnx).unwrap();
    std::fs::write(onnx.join("model_quantized.onnx"), b"onnx-bytes").unwrap();
    setup
        .install_embedding_model_from("all-MiniLM-L6-v2", source.path())
        .expect("install_from");
    let copied = cfg
        .path()
        .join("models")
        .join("models--Qdrant--all-MiniLM-L6-v2-onnx")
        .join("snapshots")
        .join("deadbeef")
        .join("model_quantized.onnx");
    assert_eq!(
        std::fs::read(&copied).unwrap(),
        b"onnx-bytes".to_vec(),
        "the air-gapped copy lands in the config-dir cache"
    );
    // Unknown ids reject; nothing lands.
    let err = setup
        .install_embedding_model_from("no-such-model", source.path())
        .expect_err("unknown id");
    assert!(err.message().contains("unknown embedding model"));
}

/// `select_embedding_model` persists the choice through the settings
/// pipeline and validates the id first ("hash" is the explicit
/// opt-out; an unknown id rejects without touching the config).
#[tokio::test]
async fn select_embedding_model_persists_and_validates() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    // Unknown ids reject.
    let err = setup
        .select_embedding_model("no-such-model")
        .await
        .expect_err("unknown id");
    assert!(err.message().contains("unknown embedding model"));
    // "hash" persists — the explicit keyword-only opt-out.
    setup
        .select_embedding_model("hash")
        .await
        .expect("select hash");
    let settings = setup.settings().expect("settings");
    assert_eq!(
        settings.general.bundled_embedding_model.as_deref(),
        Some("hash")
    );
}

/// `vision_model` sets and clears through the settings pipeline — the
/// set validates the endpoint against the configured ones.
#[tokio::test]
async fn vision_model_set_and_clear_ride_the_settings_patch() {
    let cfg = tempfile::tempdir().unwrap();
    write_ready_config(cfg.path());
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    let dto = VisionModelDto {
        endpoint: "test".to_string(),
        model: "vision-x".to_string(),
    };
    setup.vision_model(Some(dto)).await.expect("set");
    let settings = setup.settings().expect("settings");
    let vm = settings.general.vision_model.expect("vision model set");
    assert_eq!(vm.endpoint, "test");
    assert_eq!(vm.model, "vision-x");
    // None clears.
    setup.vision_model(None).await.expect("clear");
    let settings = setup.settings().expect("settings");
    assert!(settings.general.vision_model.is_none());
}

/// No surprise downloads at the Setup surface (D6; D1's
/// `auto_install_embedder` flag's contract — types.rs pins the default
/// as false, and `EngineOptions` has no Default impl: every
/// construction site opts in explicitly). A fresh `open` + a status
/// read never spawn an install — the status stays at the interim
/// `Checking` until an EXPLICIT `install_embedding_model` /
/// `select_embedding_model` (or an engine started with the flag on)
/// moves it.
#[test]
fn no_surprise_downloads_at_the_setup_surface() {
    let cfg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(cfg.path()).unwrap();
    let setup = Setup::open(Some(cfg.path().to_path_buf())).expect("open");
    let _ = setup.status(None).expect("status");
    assert_eq!(setup.embedder_status(), EmbedderStatus::Checking);
}