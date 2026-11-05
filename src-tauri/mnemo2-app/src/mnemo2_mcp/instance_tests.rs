// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tests for run-file selection, project resolution and starting Mnemo2.

use super::*;
use crate::testutil::{closed_port, health, json_reply, write_run_file, FakeServer, Reply, Sandbox};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// A fake Mnemo2 that answers `/health` for `project`.
fn live(pid: u32, project: Option<&str>) -> FakeServer {
    let project = project.map(str::to_string);
    FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => health(pid, project.as_deref()),
        _ => json_reply(serde_json::json!({})),
    })
}

#[test]
fn picks_the_instance_whose_project_contains_the_cwd() {
    let sandbox = Sandbox::new();
    let server = live(101, Some(&sandbox.project_str()));
    sandbox.write_run(101, server.port, "tok", Some(&sandbox.project_str()));
    // cwd is two levels below the project: the project is an ancestor.
    let found = find_instance(&sandbox.env(), &sandbox.deep).expect("instance");
    assert_eq!(found.pid, 101);
    assert_eq!(found.root, sandbox.project_str());
    assert_eq!(found.endpoint.port, server.port);
    // cwd equal to the project root matches too.
    assert!(find_instance(&sandbox.env(), &sandbox.project).is_some());
}

#[test]
fn no_matching_run_file_means_no_network_call() {
    let sandbox = Sandbox::new();
    let elsewhere = sandbox.elsewhere.to_string_lossy().into_owned();
    let server = live(102, Some(&elsewhere));
    sandbox.write_run(102, server.port, "tok", Some(&elsewhere));
    let idle = live(103, None);
    sandbox.write_run(103, idle.port, "tok", None);
    assert!(find_instance(&sandbox.env(), &sandbox.deep).is_none());
    assert!(server.requests().is_empty(), "a non-matching instance was pinged");
    assert!(idle.requests().is_empty(), "an instance without a project was pinged");
}

#[test]
fn missing_run_dir_and_garbage_files_are_ignored() {
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.run_dir.join("mnemo2-7.json"), "{ not json").unwrap();
    std::fs::write(sandbox.run_dir.join("notes.txt"), "hi").unwrap();
    assert!(find_instance(&sandbox.env(), &sandbox.deep).is_none());
    let mut env = sandbox.env();
    env.run_dir = sandbox.dir.path().join("does-not-exist");
    assert!(find_instance(&env, &sandbox.deep).is_none());
}

#[test]
fn a_dead_entry_is_skipped_and_its_run_file_removed() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    // The dead one is newer, so it is tried first.
    let dead = write_run_file(&sandbox.run_dir, 201, closed_port(), "tok", Some(&project), 5000);
    let server = live(202, Some(&project));
    write_run_file(&sandbox.run_dir, 202, server.port, "tok", Some(&project), 1000);
    let found = find_instance(&sandbox.env(), &sandbox.deep).expect("live instance");
    assert_eq!(found.pid, 202);
    assert!(!dead.exists(), "the stale run file should be removed");
}

#[test]
fn the_most_specific_project_wins() {
    let sandbox = Sandbox::new();
    let outer = sandbox.project_str();
    let inner = sandbox.project.join("sub").to_string_lossy().into_owned();
    let outer_server = live(301, Some(&outer));
    let inner_server = live(302, Some(&inner));
    write_run_file(&sandbox.run_dir, 301, outer_server.port, "tok", Some(&outer), 9000);
    write_run_file(&sandbox.run_dir, 302, inner_server.port, "tok", Some(&inner), 1000);
    let found = find_instance(&sandbox.env(), &sandbox.deep).expect("instance");
    assert_eq!(found.pid, 302);
    assert_eq!(found.root, inner);
}

#[test]
fn a_sibling_directory_with_a_shared_prefix_does_not_match() {
    let sandbox = Sandbox::new();
    let prefix = sandbox.dir.path().join("proj-two");
    std::fs::create_dir_all(&prefix).unwrap();
    let prefix = dunce::canonicalize(prefix).unwrap();
    let server = live(401, Some(&sandbox.project_str()));
    sandbox.write_run(401, server.port, "tok", Some(&sandbox.project_str()));
    assert!(find_instance(&sandbox.env(), &prefix).is_none());
}

#[test]
fn health_is_the_live_truth_about_the_project() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    // The run file still names the project, but the instance has moved on.
    let moved = live(501, Some(&sandbox.elsewhere.to_string_lossy()));
    let path = sandbox.write_run(501, moved.port, "tok", Some(&project));
    assert!(find_instance(&sandbox.env(), &sandbox.deep).is_none());
    assert!(path.exists(), "a live instance's run file must stay");
    // ...and one that closed its project is skipped too.
    let closed = live(502, None);
    sandbox.write_run(502, closed.port, "tok", Some(&project));
    assert!(find_instance(&sandbox.env(), &sandbox.deep).is_none());
}

#[test]
fn a_pid_mismatch_means_the_port_was_reused() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let impostor = live(999, Some(&project));
    let path = sandbox.write_run(601, impostor.port, "tok", Some(&project));
    assert!(find_instance(&sandbox.env(), &sandbox.deep).is_none());
    assert!(!path.exists());
}

#[test]
fn a_rejected_token_skips_without_deleting() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = live(701, Some(&project));
    let path = sandbox.write_run(701, server.port, "stale-token", Some(&project));
    assert!(find_instance(&sandbox.env(), &sandbox.deep).is_none());
    assert!(path.exists());
}

#[test]
fn app_resolution_follows_the_documented_order() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let helper_dir = base.join("plugin-bin");
    let path_dir = base.join("on-path");
    let elsewhere = base.join("elsewhere");
    for dir in [&helper_dir, &path_dir, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let touch = |path: &Path| std::fs::write(path, b"x").unwrap();
    let env_exe = elsewhere.join("from-env.exe");
    let hinted = elsewhere.join("from-hint.exe");
    let sibling = helper_dir.join(APP_EXE);
    let on_path = path_dir.join(APP_EXE);
    let path_var = std::env::join_paths([&path_dir]).unwrap();
    let search = |env_exe: Option<&Path>| AppSearch {
        env_exe: env_exe.map(Path::to_path_buf),
        helper_dir: helper_dir.clone(),
        path_var: Some(path_var.clone()),
    };

    // Nothing exists: an error that says how to build the app.
    let err = resolve_app(&search(None)).expect_err("nothing installed");
    assert!(err.contains("npm run build:mnemo2:native"), "{err}");
    assert!(err.contains("isn't running"), "{err}");

    // 4. PATH.
    touch(&on_path);
    assert_eq!(resolve_app(&search(None)).unwrap(), on_path);
    // 3. next to the helper beats PATH.
    touch(&sibling);
    assert_eq!(resolve_app(&search(None)).unwrap(), sibling);
    // 2. mnemo2-path.txt beats the sibling: first non-empty line, trimmed.
    touch(&hinted);
    std::fs::write(
        helper_dir.join("mnemo2-path.txt"),
        format!("\u{feff}\n  \n  {}  \nignored second line\n", hinted.display()),
    )
    .unwrap();
    assert_eq!(resolve_app(&search(None)).unwrap(), hinted);
    // 1. MNEMO2_EXE beats everything...
    touch(&env_exe);
    assert_eq!(resolve_app(&search(Some(&env_exe))).unwrap(), env_exe);
    // ...but only when the file exists.
    assert_eq!(resolve_app(&search(Some(&elsewhere.join("gone.exe")))).unwrap(), hinted);
    // A hint that points nowhere falls through to the sibling.
    std::fs::write(helper_dir.join("mnemo2-path.txt"), "C:\\no\\such\\mnemo2.exe\n").unwrap();
    assert_eq!(resolve_app(&search(None)).unwrap(), sibling);
    // An empty hint file falls through too.
    std::fs::write(helper_dir.join("mnemo2-path.txt"), "\n\n").unwrap();
    assert_eq!(resolve_app(&search(None)).unwrap(), sibling);
}

#[test]
fn registered_root_is_the_deepest_registered_ancestor() {
    let sandbox = Sandbox::new();
    let nested = sandbox.project.join("sub");
    sandbox.register(&[&sandbox.project, &nested, &sandbox.elsewhere]);
    let root = registered_root(&sandbox.env(), &sandbox.deep).unwrap();
    assert_eq!(root, Some(nested.to_string_lossy().into_owned()));
    let root = registered_root(&sandbox.env(), &sandbox.project).unwrap();
    assert_eq!(root, Some(sandbox.project_str()));
}

#[test]
fn registered_root_is_none_outside_every_project_or_without_a_registry() {
    let sandbox = Sandbox::new();
    assert_eq!(registered_root(&sandbox.env(), &sandbox.deep).unwrap(), None);
    sandbox.register(&[&sandbox.elsewhere]);
    assert_eq!(registered_root(&sandbox.env(), &sandbox.deep).unwrap(), None);
}

#[test]
fn resolve_without_a_registered_project_is_the_stub() {
    let sandbox = Sandbox::new();
    assert!(matches!(resolve(&sandbox.env(), &sandbox.deep), Resolution::Unregistered));
}

#[test]
fn resolve_starts_mnemo2_and_waits_for_its_run_file() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = Arc::new(live(801, Some(&project)));
    let calls = Arc::new(AtomicUsize::new(0));
    let started_for = Arc::new(Mutex::new(None::<PathBuf>));
    let mut env = sandbox.env();
    env.spawner = {
        let (calls, started_for, server) = (calls.clone(), started_for.clone(), server.clone());
        let (run_dir, project) = (sandbox.run_dir.clone(), project.clone());
        Arc::new(move |root: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            *started_for.lock().unwrap() = Some(root.to_path_buf());
            let (run_dir, project, port) = (run_dir.clone(), project.clone(), server.port);
            // The app takes a moment to open its window and the project.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                write_run_file(&run_dir, 801, port, "tok", Some(&project), 1);
            });
            Ok(None)
        })
    };
    let Resolution::Live(instance) = resolve(&env, &sandbox.deep) else {
        panic!("expected a live instance");
    };
    assert_eq!(instance.pid, 801);
    assert_eq!(instance.root, project);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(started_for.lock().unwrap().as_deref(), Some(sandbox.project.as_path()));
    assert!(lock_files(&sandbox).is_empty(), "the spawn lock must be released");
}

/// Write the marker `scripts/rebuild-mnemo2.mjs` writes, dated `age` ago.
fn write_rebuild_marker(sandbox: &Sandbox, age: Duration) -> PathBuf {
    let started = SystemTime::now().checked_sub(age).expect("after the epoch");
    let path = sandbox.run_dir.join("rebuilding");
    let body = serde_json::json!({
        "pid": std::process::id(),
        "started_ms": unix_ms(started).expect("epoch"),
    });
    std::fs::write(&path, body.to_string()).expect("write the rebuild marker");
    path
}

#[test]
fn a_fresh_rebuild_marker_stops_the_spawner_with_the_rebuild_message() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    write_rebuild_marker(&sandbox, Duration::from_secs(5));
    let (env, calls) = counting_env(&sandbox);
    let Resolution::Failed(message) = resolve(&env, &sandbox.deep) else {
        panic!("expected the rebuild message");
    };
    assert_eq!(
        message,
        "Mnemo2 is being rebuilt (npm run rebuild:mnemo2); try again when the build finishes"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no Mnemo2 may start mid-build");
}

#[test]
fn a_rebuild_marker_older_than_fifteen_minutes_does_not_stop_the_start() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let marker = write_rebuild_marker(&sandbox, Duration::from_secs(16 * 60));
    let (mut env, calls) = counting_env(&sandbox);
    env.spawn_timeout = Duration::from_millis(150);
    // The start itself is expected to time out (the spawner starts nothing).
    assert!(matches!(resolve(&env, &sandbox.deep), Resolution::Failed(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the stale marker is ignored");
    assert!(!marker.exists(), "a stale marker is removed");
}

#[test]
fn without_a_rebuild_marker_the_helper_starts_mnemo2_as_before() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let (mut env, calls) = counting_env(&sandbox);
    env.spawn_timeout = Duration::from_millis(150);
    assert!(matches!(resolve(&env, &sandbox.deep), Resolution::Failed(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_running_instance_is_still_found_during_a_rebuild() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = live(1101, Some(&project));
    sandbox.write_run(1101, server.port, "tok", Some(&project));
    write_rebuild_marker(&sandbox, Duration::from_secs(1));
    let (env, calls) = counting_env(&sandbox);
    let Resolution::Live(instance) = resolve(&env, &sandbox.deep) else {
        panic!("the running instance must be used");
    };
    assert_eq!(instance.pid, 1101);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a running Mnemo2 is reused, not started");
}

#[test]
fn the_rebuild_marker_is_honoured_for_fifteen_minutes() {
    let sandbox = Sandbox::new();
    let now = SystemTime::now();
    assert!(!rebuild_in_progress(&sandbox.run_dir, now), "no marker is no rebuild");
    let path = sandbox.run_dir.join("rebuilding");
    // Unparsable: the file's own age decides, and it was just written.
    std::fs::write(&path, "{ not json").unwrap();
    assert!(rebuild_in_progress(&sandbox.run_dir, now));
    // Dated exactly at the limit, the rebuild is over.
    let started_ms = unix_ms(now).unwrap() - 15 * 60 * 1000;
    std::fs::write(&path, format!("{{\"pid\":1,\"started_ms\":{started_ms}}}")).unwrap();
    assert!(!rebuild_in_progress(&sandbox.run_dir, now));
    assert!(!path.exists(), "an expired marker is removed");
    // A marker from slightly in the future (two clocks) is simply young.
    let started_ms = unix_ms(now).unwrap() + 1000;
    std::fs::write(&path, format!("{{\"pid\":1,\"started_ms\":{started_ms}}}")).unwrap();
    assert!(rebuild_in_progress(&sandbox.run_dir, now));
}

/// What a hidden instance that failed to open the project does when asked
/// to try again.
enum Retry {
    /// The retry works: the project is open afterwards.
    Opens,
    /// The retry fails again, at once, with this reason.
    FailsAgain(&'static str),
    /// The retry is taken, but nothing new is ever reported.
    Silent,
    /// The retry is declined.
    Declined,
}

/// A fake Mnemo2 whose UI failed to open `project`: no open project, and
/// `reason` (reported `age_ms` ago) on `/health`, until `/lead/open` retries.
fn failed_instance(pid: u32, project: &str, reason: &str, age_ms: u64, retry: Retry) -> FakeServer {
    let (project, reason) = (project.to_string(), reason.to_string());
    let retried = AtomicBool::new(false);
    FakeServer::start("tok", move |req| match req.path.as_str() {
        "/lead/open" => match retry {
            Retry::Declined => json_reply(serde_json::json!({ "requested": false, "reason": "no" })),
            _ => {
                retried.store(true, Ordering::SeqCst);
                json_reply(serde_json::json!({ "requested": true }))
            }
        },
        "/health" => match (&retry, retried.load(Ordering::SeqCst)) {
            (Retry::Opens, true) => health(pid, Some(&project)),
            (Retry::FailsAgain(again), true) => json_reply(serde_json::json!({
                "pid": pid, "version": "test", "project": null,
                "open_error": again, "open_error_age_ms": 0,
            })),
            _ => json_reply(serde_json::json!({
                "pid": pid, "version": "test", "project": null,
                "open_error": reason, "open_error_age_ms": age_ms,
            })),
        },
        _ => json_reply(serde_json::json!({})),
    })
}

/// An `Env` that counts how often it is asked to start Mnemo2.
fn counting_env(sandbox: &Sandbox) -> (Env, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.spawner = {
        let calls = calls.clone();
        Arc::new(move |_: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    (env, calls)
}

#[test]
fn a_fresh_open_failure_is_returned_without_starting_another_mnemo2() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let reason = "could not start the headless engine: not_configured";
    let server = failed_instance(802, &project, reason, 5_000, Retry::Opens);
    // The run file names the project, but `/health` says it is not open: only
    // the failure counts.
    sandbox.write_run(802, server.port, "tok", Some(&project));
    let (env, calls) = counting_env(&sandbox);
    let Resolution::Failed(message) = resolve(&env, &sandbox.deep) else {
        panic!("expected the reported failure");
    };
    assert!(message.contains("not_configured"), "{message}");
    assert!(
        message.ends_with(" Open Mnemo2 from the tray to fix it, then retry."),
        "{message}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no second Mnemo2 may start");
    assert!(server.requests_to("/lead/open").is_empty(), "a fresh failure is not retried");
}

#[test]
fn an_instance_that_does_not_date_its_failure_is_never_retried() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", |req| match req.path.as_str() {
        "/health" => json_reply(serde_json::json!({
            "pid": 803, "version": "test", "project": null, "open_error": "no engine",
        })),
        _ => json_reply(serde_json::json!({ "requested": true })),
    });
    sandbox.write_run(803, server.port, "tok", Some(&project));
    let (env, calls) = counting_env(&sandbox);
    assert!(matches!(resolve(&env, &sandbox.deep), Resolution::Failed(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(server.requests_to("/lead/open").is_empty());
}

#[test]
fn an_old_open_failure_is_retried_on_the_same_instance() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = failed_instance(804, &project, "no engine", 60_000, Retry::Opens);
    sandbox.write_run(804, server.port, "tok", Some(&project));
    let (env, calls) = counting_env(&sandbox);
    let Resolution::Live(instance) = resolve(&env, &sandbox.deep) else {
        panic!("expected the retry to open the project");
    };
    assert_eq!(instance.pid, 804);
    assert_eq!(instance.root, project);
    let retries = server.requests_to("/lead/open");
    assert_eq!(retries.len(), 1, "one retry request");
    assert_eq!(retries[0].json()["root"], project.as_str());
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the instance is reused, not replaced");
}

#[test]
fn a_retry_that_fails_again_reports_the_new_failure() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = failed_instance(805, &project, "old news", 60_000, Retry::FailsAgain("still no engine"));
    sandbox.write_run(805, server.port, "tok", Some(&project));
    let (env, calls) = counting_env(&sandbox);
    let Resolution::Failed(message) = resolve(&env, &sandbox.deep) else {
        panic!("expected the fresh failure");
    };
    assert!(message.contains("still no engine"), "{message}");
    assert!(message.contains("Open Mnemo2 from the tray"), "{message}");
    assert_eq!(server.requests_to("/lead/open").len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_retry_waits_through_the_old_failure_until_the_timeout() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = failed_instance(806, &project, "old news", 60_000, Retry::Silent);
    sandbox.write_run(806, server.port, "tok", Some(&project));
    let (mut env, calls) = counting_env(&sandbox);
    env.spawn_timeout = Duration::from_millis(400);
    let Resolution::Failed(message) = resolve(&env, &sandbox.deep) else {
        panic!("expected the timeout");
    };
    // The failure that predates the retry is not the answer to it.
    assert!(!message.contains("old news"), "{message}");
    assert!(message.contains("asked to try again"), "{message}");
    assert_eq!(server.requests_to("/lead/open").len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no second Mnemo2 may start");
}

#[test]
fn a_declined_retry_leaves_the_old_failure_as_the_answer() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = failed_instance(807, &project, "old news", 60_000, Retry::Declined);
    sandbox.write_run(807, server.port, "tok", Some(&project));
    let (env, calls) = counting_env(&sandbox);
    let Resolution::Failed(message) = resolve(&env, &sandbox.deep) else {
        panic!("expected the old failure");
    };
    assert!(message.contains("old news"), "{message}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_second_helper_waits_instead_of_starting_a_second_mnemo2() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    // Another helper holds the spawn lock for this project.
    let _held = SpawnLock::acquire(&sandbox.run_dir, &project);
    let server = live(811, Some(&project));
    let port = server.port;
    let mut env = sandbox.env();
    let calls = Arc::new(AtomicUsize::new(0));
    env.spawner = {
        let calls = calls.clone();
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    let (run_dir, project_for_file) = (sandbox.run_dir.clone(), project.clone());
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        write_run_file(&run_dir, 811, port, "tok", Some(&project_for_file), 1);
    });
    assert!(matches!(resolve(&env, &sandbox.deep), Resolution::Live(_)));
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the loser must not spawn");
}

/// The pid of a process that has already exited.
fn dead_pid() -> u32 {
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd").args(["/c", "exit"]).spawn().unwrap();
    #[cfg(not(windows))]
    let mut child = std::process::Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    child.id()
}

/// An `Env` whose spawner counts its calls and, like the real app, publishes a
/// run file for `project` a moment later.
fn publishing_env(sandbox: &Sandbox, pid: u32, port: u16) -> (Env, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.spawner = {
        let calls = calls.clone();
        let (run_dir, project) = (sandbox.run_dir.clone(), sandbox.project_str());
        Arc::new(move |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            let (run_dir, project) = (run_dir.clone(), project.clone());
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                write_run_file(&run_dir, pid, port, "tok", Some(&project), 1);
            });
            Ok(None)
        })
    };
    (env, calls)
}

/// A session that ended while its helper was starting Mnemo2 leaves a young
/// lock behind and takes that Mnemo2 along. The next helper must start one
/// itself, not wait out the lock.
#[test]
fn a_young_lock_whose_helper_and_mnemo2_are_gone_does_not_block_the_next_start() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let path = lock_path(&sandbox.run_dir, &project);
    std::fs::create_dir_all(&sandbox.run_dir).unwrap();
    std::fs::write(&path, format!("{}-1-0\n{}", dead_pid(), dead_pid())).unwrap();
    let server = live(812, Some(&project));
    let (mut env, calls) = publishing_env(&sandbox, 812, server.port);
    env.spawn_timeout = Duration::from_secs(5);
    let started = Instant::now();
    let Resolution::Live(instance) = resolve(&env, &sandbox.deep) else {
        panic!("expected a live instance");
    };
    assert_eq!(instance.pid, 812);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(3), "waited out a dead lock");
    assert!(lock_files(&sandbox).is_empty(), "the taken-over lock is released");
}

/// A helper that found the lock taken starts Mnemo2 itself once the holder
/// lets go without an instance having come up.
#[test]
fn a_waiting_helper_starts_mnemo2_when_the_holder_gives_up() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let held = SpawnLock::acquire(&sandbox.run_dir, &project);
    assert!(matches!(held, SpawnLock::Owned { .. }));
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(held);
    });
    let server = live(813, Some(&project));
    let (mut env, calls) = publishing_env(&sandbox, 813, server.port);
    env.spawn_timeout = Duration::from_secs(5);
    let Resolution::Live(instance) = resolve(&env, &sandbox.deep) else {
        panic!("expected a live instance");
    };
    assert_eq!(instance.pid, 813);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_lock_names_its_helper_and_later_its_mnemo2() {
    assert_eq!(lock_pids("4242-1700000000-3"), vec![4242]);
    assert_eq!(lock_pids("4242-1700000000-3\n977"), vec![4242, 977]);
    assert!(lock_pids("crashed").is_empty());
    assert!(lock_pids("").is_empty());
    let alive = |pid: u32| pid == 977;
    // Mnemo2 still starting: the lock stands though its helper is gone.
    assert!(!owners_gone("4242-1-0\n977", alive));
    assert!(owners_gone("4242-1-0\n978", alive));
    assert!(owners_gone("4242-1-0", alive));
    // A lock that names nobody is judged by its age alone.
    assert!(!owners_gone("crashed", alive));
}

#[test]
fn the_owner_records_the_mnemo2_it_started_and_still_releases_its_lock() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let lock = SpawnLock::acquire(&sandbox.run_dir, &project);
    let path = owned_path(&lock).to_path_buf();
    lock.record_app(std::process::id());
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(lock_pids(&text), vec![std::process::id(), std::process::id()]);
    assert!(!lock_is_stale(&path, LOCK_STALE));
    drop(lock);
    assert!(!path.exists());
}

/// Every spawn lock or takeover guard left in the run dir.
fn lock_files(sandbox: &Sandbox) -> Vec<PathBuf> {
    std::fs::read_dir(&sandbox.run_dir)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            name.ends_with(".lock") || name.ends_with(".takeover")
        })
        .collect()
}

fn age(path: &Path, by: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

fn owned_path(lock: &SpawnLock) -> &Path {
    match lock {
        SpawnLock::Owned { path, .. } => path,
        _ => panic!("expected to own the lock"),
    }
}

#[test]
fn a_stale_spawn_lock_is_taken_over() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let first = SpawnLock::acquire(&sandbox.run_dir, &project);
    // Age the lock past the stale threshold (a crashed starter).
    let path = owned_path(&first).to_path_buf();
    age(&path, LOCK_STALE + Duration::from_secs(30));
    let second = SpawnLock::acquire(&sandbox.run_dir, &project);
    assert_eq!(owned_path(&second), path);
    // The takeover guard is gone again, and a third helper now finds a fresh lock.
    assert_eq!(lock_files(&sandbox), vec![path.clone()]);
    assert!(matches!(SpawnLock::acquire(&sandbox.run_dir, &project), SpawnLock::Busy));
    // The crashed owner's late Drop must not remove the new owner's lock.
    drop(first);
    assert!(path.exists(), "an old owner removed its successor's lock");
    drop(second);
    assert!(!path.exists());
}

#[test]
fn staleness_is_a_pure_function_of_the_lock_age() {
    let now = SystemTime::now();
    let limit = Duration::from_secs(60);
    assert!(!is_stale_at(Some(now - Duration::from_secs(5)), now, limit));
    assert!(!is_stale_at(Some(now - limit), now, limit), "exactly at the limit is still live");
    assert!(is_stale_at(Some(now - limit - Duration::from_secs(1)), now, limit));
    // Gone, unreadable or from the future: nothing live to protect.
    assert!(is_stale_at(None, now, limit));
    assert!(is_stale_at(Some(now + Duration::from_secs(30)), now, limit));
}

#[test]
fn a_takeover_that_finds_the_lock_already_replaced_leaves_it_alone() {
    // Two helpers saw the same stale lock; the first replaced it. The second
    // reaches the takeover step with a lock that is now fresh.
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let winner = SpawnLock::acquire(&sandbox.run_dir, &project);
    let path = owned_path(&winner).to_path_buf();
    let loser = SpawnLock::take_over(&path, LOCK_STALE, "loser-token".to_string());
    assert!(matches!(loser, SpawnLock::Busy));
    assert!(path.exists(), "the winner's fresh lock must survive");
    let SpawnLock::Owned { token, .. } = &winner else {
        unreachable!()
    };
    assert_eq!(&std::fs::read_to_string(&path).unwrap(), token);
    assert_eq!(lock_files(&sandbox), vec![path], "the guard is released");
}

#[test]
fn only_one_helper_at_a_time_may_clear_a_stale_lock() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let path = lock_path(&sandbox.run_dir, &project);
    std::fs::write(&path, "crashed").unwrap();
    age(&path, LOCK_STALE + Duration::from_secs(30));
    // Another helper is mid-takeover: its guard is fresh.
    let guard = guard_path(&path);
    std::fs::write(&guard, "other").unwrap();
    assert!(matches!(SpawnLock::acquire(&sandbox.run_dir, &project), SpawnLock::Busy));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "crashed");
    // A guard left by a helper that died mid-takeover is cleared, and the next
    // helper then gets the lock.
    age(&guard, GUARD_STALE + Duration::from_secs(5));
    assert!(matches!(SpawnLock::acquire(&sandbox.run_dir, &project), SpawnLock::Busy));
    assert!(!guard.exists());
    assert!(matches!(SpawnLock::acquire(&sandbox.run_dir, &project), SpawnLock::Owned { .. }));
}

#[test]
fn racing_helpers_on_a_stale_lock_produce_exactly_one_owner() {
    const HELPERS: usize = 8;
    for round in 0..30 {
        let sandbox = Sandbox::new();
        let path = lock_path(&sandbox.run_dir, &sandbox.project_str());
        std::fs::write(&path, "crashed").unwrap();
        age(&path, LOCK_STALE + Duration::from_secs(30));
        let barrier = Arc::new(std::sync::Barrier::new(HELPERS));
        let handles: Vec<_> = (0..HELPERS)
            .map(|_| {
                let (barrier, path) = (barrier.clone(), path.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    SpawnLock::acquire_at(&path, LOCK_STALE)
                })
            })
            .collect();
        // Held until every helper has finished, as a real starter holds its lock.
        let locks: Vec<SpawnLock> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let owners = locks.iter().filter(|lock| matches!(lock, SpawnLock::Owned { .. })).count();
        assert_eq!(owners, 1, "round {round}: {owners} helpers think they may start Mnemo2");
        drop(locks);
        assert!(lock_files(&sandbox).is_empty(), "round {round}: leftovers");
    }
}

#[test]
fn spawn_failure_and_startup_timeout_are_reported() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let mut env = sandbox.env();
    env.spawner = Arc::new(|_| Err("the Mnemo2 app was not found".to_string()));
    match resolve(&env, &sandbox.deep) {
        Resolution::Failed(message) => assert!(message.contains("not found"), "{message}"),
        _ => panic!("expected a failure"),
    }
    env.spawner = Arc::new(|_| Ok(None));
    env.spawn_timeout = Duration::from_millis(200);
    match resolve(&env, &sandbox.deep) {
        Resolution::Failed(message) => assert!(message.contains("did not open"), "{message}"),
        _ => panic!("expected a timeout"),
    }
    // Neither failure leaves a lock behind.
    assert!(lock_files(&sandbox).is_empty());
}

// ---- dead, busy and unknown instances -------------------------------------

#[test]
fn only_a_refusal_or_a_pid_mismatch_is_dead() {
    let connect = |kind| classify_error(&crate::http::HttpError::Connect(std::io::Error::from(kind)));
    let io_err = |kind| classify_error(&crate::http::HttpError::Io(std::io::Error::from(kind)));
    assert!(matches!(connect(std::io::ErrorKind::ConnectionRefused), Probe::Dead));
    // Timeouts mean busy, not gone.
    assert!(matches!(connect(std::io::ErrorKind::TimedOut), Probe::Busy));
    assert!(matches!(io_err(std::io::ErrorKind::TimedOut), Probe::Busy));
    assert!(matches!(io_err(std::io::ErrorKind::WouldBlock), Probe::Busy));
    // Anything else is just unusable for now.
    assert!(matches!(io_err(std::io::ErrorKind::ConnectionReset), Probe::Unknown));
    assert!(matches!(connect(std::io::ErrorKind::AddrNotAvailable), Probe::Unknown));
    assert!(matches!(
        classify_error(&crate::http::HttpError::Protocol("x".into())),
        Probe::Unknown
    ));
}

/// A fake Mnemo2 whose first `slow` health answers take `delay`.
fn slow_health(pid: u32, project: &str, slow: usize, delay: Duration) -> FakeServer {
    let project = project.to_string();
    let seen = AtomicUsize::new(0);
    FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => {
            if seen.fetch_add(1, Ordering::SeqCst) < slow {
                std::thread::sleep(delay);
            }
            health(pid, Some(&project))
        }
        _ => json_reply(serde_json::json!({})),
    })
}

#[test]
fn a_probe_that_times_out_is_busy_and_keeps_its_run_file() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = slow_health(901, &project, usize::MAX, Duration::from_millis(700));
    let path = sandbox.write_run(901, server.port, "tok", Some(&project));
    let mut env = sandbox.env();
    env.health_timeout = Duration::from_millis(100);
    let found = lookup(&env, &sandbox.deep, Limits::normal(&env));
    assert!(found.instance.is_none());
    assert!(found.busy);
    assert!(path.exists(), "a busy instance's run file must stay");
}

#[test]
fn a_busy_instance_is_waited_for_never_duplicated() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    // The first health probe is slower than the timeout, later ones are fast.
    let server = slow_health(902, &project, 1, Duration::from_millis(500));
    sandbox.write_run(902, server.port, "tok", Some(&project));
    let spawned = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.health_timeout = Duration::from_millis(100);
    env.spawner = {
        let spawned = spawned.clone();
        Arc::new(move |_| {
            spawned.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    let Resolution::Live(instance) = resolve(&env, &sandbox.deep) else {
        panic!("the busy instance should be picked up once it answers");
    };
    assert_eq!(instance.pid, 902);
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "a second Mnemo2 was started");
}

#[test]
fn an_instance_that_stays_busy_is_reported_not_replaced() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = slow_health(903, &project, usize::MAX, Duration::from_millis(600));
    let path = sandbox.write_run(903, server.port, "tok", Some(&project));
    let spawned = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.health_timeout = Duration::from_millis(80);
    env.spawn_timeout = Duration::from_millis(400);
    env.spawner = {
        let spawned = spawned.clone();
        Arc::new(move |_| {
            spawned.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    };
    match resolve(&env, &sandbox.deep) {
        Resolution::Failed(message) => assert!(message.contains("did not answer"), "{message}"),
        _ => panic!("expected a failure"),
    }
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    assert!(path.exists());
}

#[test]
fn hook_lookups_fit_the_ten_second_hook_budget() {
    let sandbox = Sandbox::new();
    let env = sandbox.env();
    let limits = Limits::hook(&env);
    assert!(limits.health_timeout <= Duration::from_secs(1));
    assert_eq!(limits.budget, Some(HOOK_LOOKUP_BUDGET));
    // The last probe may start just inside the budget; then the call follows.
    let worst = HOOK_LOOKUP_BUDGET + HOOK_PROBE_TIMEOUT + CALL_CONNECT_TIMEOUT + HOOK_TIMEOUT;
    assert!(worst <= Duration::from_secs(9), "{worst:?}");
    // The normal lookup has no budget but the same probe patience as the Env.
    assert_eq!(Limits::normal(&env).budget, None);
}

#[test]
fn a_hook_lookup_stops_probing_once_its_budget_is_spent() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let env = sandbox.env();
    // Three busy candidates at ~100 ms each; a 150 ms budget probes at most two.
    let servers: Vec<FakeServer> = (0..3)
        .map(|index| {
            let server = slow_health(950 + index, &project, usize::MAX, Duration::from_millis(400));
            write_run_file(&sandbox.run_dir, 950 + index, server.port, "tok", Some(&project), u64::from(index));
            server
        })
        .collect();
    let limits = Limits {
        health_timeout: Duration::from_millis(100),
        budget: Some(Duration::from_millis(150)),
    };
    let started = Instant::now();
    let found = lookup(&env, &sandbox.deep, limits);
    assert!(found.instance.is_none() && found.busy);
    assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
    let probed = servers.iter().filter(|server| !server.requests().is_empty()).count();
    assert_eq!(probed, 2, "the budget should have cut the third probe");
}

#[test]
fn call_returns_the_json_of_a_200_and_nothing_otherwise() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", |req| match req.path.as_str() {
        "/health" => health(1, None),
        "/ok" => json_reply(serde_json::json!({ "x": 1 })),
        "/broken" => Reply::Status(500, "boom".to_string()),
        _ => Reply::Status(200, "not json".to_string()),
    });
    let instance = Instance {
        pid: 1,
        endpoint: crate::http::Endpoint {
            port: server.port,
            token: "tok".to_string(),
        },
        root: project,
    };
    let body = serde_json::json!({});
    let timeout = Duration::from_secs(5);
    assert_eq!(call(&instance, "/ok", &body, timeout), Some(serde_json::json!({ "x": 1 })));
    assert_eq!(call(&instance, "/broken", &body, timeout), None);
    assert_eq!(call(&instance, "/garbled", &body, timeout), None);
}

#[cfg(windows)]
#[test]
fn stop_inheriting_stdio_clears_the_inherit_flag_of_the_std_handles() {
    use std::os::windows::io::AsRawHandle;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetHandleInformation(handle: *mut std::ffi::c_void, flags: *mut u32) -> i32;
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    // SAFETY: process-owned std handles; a call that fails (a harness with no
    // stdio) just leaves that handle out.
    let usable: Vec<_> = handles
        .into_iter()
        .filter(|handle| !handle.is_null())
        // Start from what Claude Code hands over: an inheritable handle.
        .filter(|handle| unsafe {
            SetHandleInformation(*handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) != 0
        })
        .collect();
    stop_inheriting_stdio();
    for handle in usable {
        let mut flags = 0;
        // SAFETY: as above.
        assert_ne!(unsafe { GetHandleInformation(handle, &mut flags) }, 0);
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0, "{handle:?} is still inheritable");
    }
}

/// Claude Desktop terminates each Claude Code session's job object when it
/// quits or restarts, so Mnemo2 breaks away from it; inside a job that forbids
/// breakaway it still starts, inside that job.
#[cfg(windows)]
#[test]
fn the_app_breaks_away_from_a_session_job_and_starts_inside_one_that_forbids_it() {
    use std::os::windows::io::AsRawHandle;
    type Handle = *mut std::ffi::c_void;
    // BREAKAWAY_OK is an extended limit: the basic class rejects it.
    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_BREAKAWAY_OK: u32 = 0x0000_0800;
    /// JOBOBJECT_EXTENDED_LIMIT_INFORMATION, with its nested
    /// JOBOBJECT_BASIC_LIMIT_INFORMATION and IO_COUNTERS spelled out.
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
        io_counters: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(job: Handle, class: i32, info: *const std::ffi::c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn IsProcessInJob(process: Handle, job: Handle, result: *mut i32) -> i32;
        fn GetCurrentProcess() -> Handle;
    }
    // The test process joins a new job, nested in any job it is already in,
    // for good: neither job here limits or kills anything.
    let join_new_job = |limit_flags: u32| -> Handle {
        let limits = ExtendedLimits {
            limit_flags,
            ..ExtendedLimits::default()
        };
        let os_error = std::io::Error::last_os_error;
        // SAFETY: plain Win32 calls on a job this test owns; the limits struct
        // outlives the call that reads it.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            assert!(!job.is_null(), "CreateJobObjectW: {}", os_error());
            let set = SetInformationJobObject(
                job,
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&limits as *const ExtendedLimits).cast(),
                std::mem::size_of::<ExtendedLimits>() as u32,
            );
            assert_ne!(set, 0, "SetInformationJobObject: {}", os_error());
            let joined = AssignProcessToJobObject(job, GetCurrentProcess());
            assert_ne!(joined, 0, "AssignProcessToJobObject: {}", os_error());
            job
        }
    };
    let sleeper = || {
        let mut command = Command::new("ping");
        command
            .args(["-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    };
    // Whether `child` is in `job`; the child is killed either way.
    let in_job_then_kill = |mut child: Child, job: Handle| -> bool {
        let mut result = 0;
        // SAFETY: a live child handle and a job handle this test keeps open.
        let queried = unsafe { IsProcessInJob(child.as_raw_handle(), job, &mut result) };
        let error = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        assert_ne!(queried, 0, "IsProcessInJob: {error}");
        result != 0
    };

    let session = join_new_job(JOB_OBJECT_LIMIT_BREAKAWAY_OK);
    let child = spawn_detached(sleeper).expect("a job that allows breakaway starts the app");
    assert!(
        !in_job_then_kill(child, session),
        "the app stayed in the session job, so a Desktop restart would kill it"
    );

    let strict = join_new_job(0);
    let child = spawn_detached(sleeper).expect("a job that forbids breakaway still starts the app");
    assert!(
        in_job_then_kill(child, strict),
        "the app started outside a job it may not leave"
    );
}
