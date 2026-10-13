//! The §5.3 public-API contract fixtures (job 3a.3): the wire shapes and
//! the Send/Sync/'static guarantees are pinned BEFORE the Engine exists —
//! every later 3a step keeps these green (the types-first contract).

use serde_json::json;
use std::path::PathBuf;

use mnemo::agent::policy::PermissionPolicy;
use mnemo::config::SafetyMode;
use mnemo::headless::{
    Engine, EngineError, EngineOptions, EventReceiver, MemoryBackend, RecvError, SessionCommand,
    SessionEvent, SessionId, SessionLimits, SpawnOptions, Transcript,
};
use mnemo::runtime::channels::{Approval, SerializableAgentEvent, UserAnswer};
use mnemo::workspace::Workspace;

/// Serialize → deserialize → re-serialize; the two JSON forms must be
/// equal (exercises both serde directions without needing PartialEq on
/// every type).
fn roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> serde_json::Value {
    let first = serde_json::to_value(value).expect("serialize");
    let back: T = serde_json::from_value(first.clone()).expect("deserialize");
    let second = serde_json::to_value(&back).expect("re-serialize");
    assert_eq!(first, second, "the wire shape must survive a round-trip");
    first
}

fn sample_options() -> EngineOptions {
    EngineOptions {
        workspace: Workspace {
            root: "/tmp/w".into(),
            state_dir: "/tmp/w/.coding".into(),
            require_git: false,
        },
        config_dir: Some("/tmp/cfg".into()),
        permissions: PermissionPolicy::ApproveViaApi {
            safety_mode: SafetyMode::Autonomous,
        },
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: true,
        require_git: false,
        auto_install_embedder: false,
    }
}

/// §7 row 13: every public type is plain serde data — the round-trip
/// holds for representative values of each, and the `type` tags are the
/// snake_case spellings a JS binding matches on.
#[test]
fn public_types_serde_roundtrip() {
    // EngineOptions — full value + the memory-Disabled variant.
    roundtrip(&sample_options());
    let mut opts = sample_options();
    opts.memory = MemoryBackend::Disabled;
    roundtrip(&opts);

    // SpawnOptions — default and full (the 3a subset).
    roundtrip(&SpawnOptions::default());
    roundtrip(&SpawnOptions {
        name: Some("worker".into()),
        model: Some("local/m2".into()),
        worktree: Some("/tmp/wt".into()),
        permissions: Some(PermissionPolicy::OverridePermissions),
        policy: Some("mnemo.workflow".into()),
        resume: Some(Transcript {
            messages: vec![json!({"role": "user", "content": "hi"})],
        }),
    });

    // SessionCommand — every 3a variant.
    let commands = vec![
        SessionCommand::Prompt {
            text: "hi".into(),
            images: vec!["data:image/png;base64,AAA".into()],
        },
        SessionCommand::Steer { text: "go".into() },
        SessionCommand::CancelSteer { text: "go".into() },
        SessionCommand::Interrupt,
        SessionCommand::Compact,
        SessionCommand::Clear,
        SessionCommand::Approve {
            request_id: "r1".into(),
            decision: Approval::DenyAll,
        },
        SessionCommand::Answer {
            question_id: "q1".into(),
            answer: UserAnswer::Choice { index: 1 },
        },
    ];
    for command in &commands {
        roundtrip(command);
    }

    // SessionEvent — the newtype over SerializableAgentEvent + the tag.
    roundtrip(&SessionEvent {
        agent: 7,
        event: SerializableAgentEvent::Started,
    });

    // The small types.
    roundtrip(&SessionLimits {
        max_turns: Some(3),
        wall_clock_ms: Some(120_000),
        ..SessionLimits::default()
    });
    roundtrip(&Transcript::default());
    // EngineError — constructed via serde (its constructors are
    // engine-internal; the wire shape is the public surface).
    let err: EngineError =
        serde_json::from_value(json!({"code": "session_closed", "message": "closed"}))
            .expect("the error wire shape");
    roundtrip(&err);

    // Pin the tag spellings a binding matches on (§5.3 :328).
    assert_eq!(
        serde_json::to_value(SessionCommand::Interrupt).unwrap(),
        json!({"type": "interrupt"})
    );
    assert_eq!(
        serde_json::to_value(SessionCommand::Prompt {
            text: "hi".into(),
            images: vec![]
        })
        .unwrap(),
        json!({"type": "prompt", "text": "hi", "images": []})
    );
    // The permission policy rides along in kebab-case (the 2c.5 lesson:
    // SafetyMode is kebab-case on the wire; the tag is "mode").
    let opts_json = serde_json::to_value(&sample_options()).unwrap();
    assert_eq!(
        opts_json["permissions"],
        json!({"mode": "ApproveViaApi", "safety_mode": "autonomous"})
    );
}

/// §7 row 13: malformed `SessionCommand` JSON errors at the boundary —
/// unknown tags, missing fields, wrong tag keys, and non-objects all
/// reject instead of silently defaulting.
#[test]
fn session_command_malformed_json_errors() {
    // Unknown tag.
    assert!(
        serde_json::from_value::<SessionCommand>(json!({"type": "explode"})).is_err(),
        "an unknown tag must reject"
    );
    // Missing required field.
    assert!(
        serde_json::from_value::<SessionCommand>(json!({"type": "prompt"})).is_err(),
        "a missing field must reject"
    );
    // Wrong tag key ("type" is the public tag; anything else rejects).
    assert!(
        serde_json::from_value::<SessionCommand>(json!({"kind": "interrupt"})).is_err(),
        "a wrong tag key must reject"
    );
    // Non-object payload.
    assert!(
        serde_json::from_value::<SessionCommand>(json!("interrupt")).is_err(),
        "a non-object must reject"
    );
}

/// §7 row 13: every public type is Send + Sync + 'static — a headless
/// caller can hold sessions and receivers across threads and await
/// points freely.
#[test]
fn public_types_are_send_sync_static() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_sync_static::<EngineOptions>();
    assert_send_sync_static::<SpawnOptions>();
    assert_send_sync_static::<SessionCommand>();
    assert_send_sync_static::<SessionEvent>();
    assert_send_sync_static::<EventReceiver<SessionEvent>>();
    assert_send_sync_static::<SessionId>();
    assert_send_sync_static::<SessionLimits>();
    assert_send_sync_static::<MemoryBackend>();
    assert_send_sync_static::<Transcript>();
    assert_send_sync_static::<EngineError>();
    assert_send_sync_static::<RecvError>();
}

/// Real-start options for the engine fixtures: a fresh non-git workspace,
/// a temp config dir, indexing off (test speed), the free default.
fn engine_opts(workspace: mnemo::workspace::Workspace, config_dir: Option<PathBuf>) -> EngineOptions {
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

/// §7 row 9: `Engine::start` never blocks and never shows a picker — an
/// unusable workspace (a missing root, or `require_git` on a non-repo
/// root) is `EngineError` code `no_project`, returned promptly.
#[tokio::test]
async fn no_project_errors() {
    // A missing root.
    let opts = engine_opts(
        Workspace::new("/definitely/not/a/real/dir"),
        None,
    );
    let err = match Engine::start(opts).await {
        Ok(_) => panic!("a missing workspace root must error"),
        Err(e) => e,
    };
    assert_eq!(err.code(), "no_project", "never a picker: {}", err);

    // require_git + a non-repo root (the §5.9.3 :664 open-time gate —
    // either the option or the workspace marker demands a repo).
    let dir = tempfile::tempdir().unwrap();
    let opts = engine_opts(
        Workspace::new(dir.path()).requiring_git(),
        Some(tempfile::tempdir().unwrap().path().to_path_buf()),
    );
    let err = match Engine::start(opts).await {
        Ok(_) => panic!("require_git on a non-repo root must error"),
        Err(e) => e,
    };
    assert_eq!(err.code(), "no_project", "never a picker: {}", err);
}

/// §7 row 15: `start_blocking` works from a plain (non-async) context —
/// the engine owns a static tokio runtime, so a plain `fn main` can start
/// one with no ambient reactor.
#[test]
fn start_blocking_without_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let opts = engine_opts(
        Workspace::new(dir.path()),
        Some(tempfile::tempdir().unwrap().path().to_path_buf()),
    );
    let engine =
        Engine::start_blocking(opts).expect("start_blocking builds its own runtime");
    // The host-events bus exists before any subscriber.
    let _ = engine.host_events();
    drop(engine); // the owned runtime drops with the engine
}
