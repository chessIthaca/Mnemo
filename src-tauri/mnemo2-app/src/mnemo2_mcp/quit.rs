// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `mnemo2-mcp quit [--force] [--timeout <s>]`: ask every running Mnemo2 to
//! leave, and wait until it has.
//!
//! `npm run rebuild:mnemo2` needs this: a running `mnemo2.exe` locks the
//! executable the release build writes. The command only asks through each
//! instance's run file (`POST /app/quit`); it never kills a process. An
//! instance that refuses, cannot be reached, or does not exit in time turns
//! into one plain line on stderr and exit code 1.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::http::{self, Endpoint, HttpError, Response};
use crate::instance::{self, Env};

/// Default `quit` timeout, in seconds.
pub const DEFAULT_TIMEOUT_S: u64 = 60;

/// How long one quit request may take to answer. The app answers first and
/// exits ~300 ms later, so this is generous.
const QUIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Pause between two liveness checks while waiting for an instance to exit.
const EXIT_POLL: Duration = Duration::from_millis(250);

/// The `quit` command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// Interrupt running worker turns instead of refusing.
    pub force: bool,
    /// How long to wait for every instance to exit.
    pub timeout: Duration,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            force: false,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_S),
        }
    }
}

/// Parse `quit`'s arguments: `--force` plus `--timeout <s>` or
/// `--timeout=<s>`, in any order. The error is one line for stderr.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;
    while index < args.len() {
        let (flag, inline) = match args[index].split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (args[index].as_str(), None),
        };
        match flag {
            "--force" if inline.is_none() => options.force = true,
            "--timeout" => {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        index += 1;
                        args.get(index).map(String::as_str).ok_or_else(|| {
                            "quit: --timeout needs a number of seconds".to_string()
                        })?
                    }
                };
                let seconds: u64 = value.parse().map_err(|_| {
                    format!("quit: --timeout needs a number of seconds, not {value}")
                })?;
                options.timeout = Duration::from_secs(seconds);
            }
            _ => return Err(format!("quit: unexpected argument {}\n{}", args[index], crate::USAGE)),
        }
        index += 1;
    }
    Ok(options)
}

/// One quit request: `POST /app/quit` with `{"force": …}` to one instance.
pub struct Ask<'a> {
    pub endpoint: &'a Endpoint,
    /// Interrupt running worker turns instead of being refused.
    pub force: bool,
}

/// What an instance answers a quit request with: its status and its body.
pub type Answer = Result<Response, HttpError>;

/// What `quit` reaches the outside world with; injectable so tests need neither
/// a real Mnemo2 nor a real wait.
pub struct Deps {
    /// Ask one instance to quit and read its answer.
    pub call: Box<dyn Fn(&Ask) -> Answer>,
    /// True while the process with this pid is still running.
    pub alive: Box<dyn Fn(u32) -> bool>,
    /// The monotonic clock the timeout is measured on.
    pub now: Box<dyn Fn() -> Instant>,
    /// Rest between two liveness checks.
    pub wait: Box<dyn Fn(Duration)>,
}

impl Deps {
    /// The real HTTP client, liveness check and clock.
    pub fn real() -> Deps {
        Deps {
            call: Box::new(|ask: &Ask| {
                let body = json!({ "force": ask.force }).to_string();
                http::post(ask.endpoint, "/app/quit", &[], &body, Some(QUIT_TIMEOUT))
            }),
            alive: Box::new(mnemo_app::instance_marker::pid_alive),
            now: Box::new(Instant::now),
            wait: Box::new(std::thread::sleep),
        }
    }
}

/// `main`'s entry: parse the arguments, then ask and wait with the real deps.
pub fn run(env: &Env, args: &[String]) -> Result<Vec<String>, String> {
    let options = parse_args(args)?;
    quit(env, &options, &Deps::real())
}

/// Ask every Mnemo2 with a run file to quit and wait until each has exited.
///
/// `Ok` carries the lines to print (one per instance, none when none ran);
/// `Err` is the one line that explains the failure. Nothing here kills a
/// process: an instance that does not leave is reported, not removed.
pub fn quit(env: &Env, options: &Options, deps: &Deps) -> Result<Vec<String>, String> {
    let mut targets: Vec<(u32, Option<String>, Endpoint)> = Vec::new();
    for run in instance::run_files(&env.run_dir) {
        // A stale run file (its pid is gone) names no instance to ask.
        if !(deps.alive)(run.pid) {
            continue;
        }
        targets.push((
            run.pid,
            run.project,
            Endpoint {
                port: run.port,
                token: run.token,
            },
        ));
    }
    if targets.is_empty() {
        return Ok(vec!["no Mnemo2 running".to_string()]);
    }
    for (pid, _, endpoint) in &targets {
        let ask = Ask {
            endpoint,
            force: options.force,
        };
        let answer = (deps.call)(&ask)
            .map_err(|_| format!("pid {pid} did not answer the quit request"))?;
        match answer.status {
            200 => {}
            409 => return Err(workers_running(*pid, &answer.body)),
            404 => {
                return Err(format!(
                    "pid {pid} is an older build without /app/quit; quit it from the tray once"
                ))
            }
            401 => {
                return Err(format!(
                    "pid {pid} rejected the quit request (its run file has another token); \
                     quit it from the tray once"
                ))
            }
            status => {
                return Err(format!(
                    "pid {pid} answered HTTP {status} to the quit request"
                ))
            }
        }
    }
    let deadline = (deps.now)() + options.timeout;
    loop {
        let remaining: Vec<u32> = targets
            .iter()
            .map(|(pid, ..)| *pid)
            .filter(|pid| (deps.alive)(*pid))
            .collect();
        if remaining.is_empty() {
            break;
        }
        if (deps.now)() >= deadline {
            return Err(did_not_exit(&remaining, options.timeout));
        }
        (deps.wait)(EXIT_POLL);
    }
    Ok(targets
        .iter()
        .map(|(pid, project, _)| match project {
            Some(project) => format!("quit pid {pid} ({project})"),
            None => format!("quit pid {pid}"),
        })
        .collect())
}

/// The refusal of an instance with running worker turns: how many, and that
/// `--force` is the way to interrupt them.
fn workers_running(pid: u32, body: &str) -> String {
    let reply: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let detail = match reply.get("running").and_then(Value::as_u64) {
        Some(running) => format!("{running} worker(s) are running"),
        None => reply
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("workers are running")
            .to_string(),
    };
    format!("pid {pid}: {detail}; rerun with --force to interrupt them")
}

/// The timeout message: which pids are still there, and after how long.
fn did_not_exit(pids: &[u32], timeout: Duration) -> String {
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
    let who = if pids.len() == 1 {
        format!("pid {list}")
    } else {
        format!("pids {list}")
    };
    format!("{who} did not exit within {} s", timeout.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::testutil::{json_reply, write_run_file, FakeServer, Req, Sandbox};

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// The answer a test instance gives to `/app/quit`.
    fn answer(status: u16, body: Value) -> Answer {
        Ok(Response {
            status,
            body: body.to_string(),
        })
    }

    /// Deps that never wait, record every quit request, and decide "alive" by
    /// `alive`.
    fn deps(
        call: impl Fn(&Ask) -> Answer + 'static,
        alive: impl Fn(u32) -> bool + 'static,
        waits: Arc<AtomicUsize>,
    ) -> Deps {
        Deps {
            call: Box::new(call),
            alive: Box::new(alive),
            now: Box::new(Instant::now),
            wait: Box::new(move |_| {
                waits.fetch_add(1, Ordering::SeqCst);
            }),
        }
    }

    fn always_alive() -> impl Fn(u32) -> bool {
        |_| true
    }

    #[test]
    fn quit_asks_every_instance_and_waits_until_it_has_exited() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4242, 1, "tok", Some(&project), 1);
        // Still there for the first checks, gone afterwards: quitting takes a
        // moment, and the command must wait for it.
        let alive = Arc::new(AtomicUsize::new(3));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let waits = Arc::new(AtomicUsize::new(0));
        let deps = deps(
            {
                let asked = Arc::clone(&asked);
                move |ask: &Ask| {
                    asked.lock().unwrap().push((
                        ask.endpoint.port,
                        ask.endpoint.token.clone(),
                        ask.force,
                    ));
                    answer(200, json!({ "quitting": true, "pid": 4242 }))
                }
            },
            {
                let alive = Arc::clone(&alive);
                move |_: u32| {
                    alive
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
                        .is_ok()
                }
            },
            Arc::clone(&waits),
        );
        let lines = quit(&sandbox.env(), &Options::default(), &deps).expect("quit");
        assert_eq!(lines, vec![format!("quit pid 4242 ({project})")]);
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, 1);
        assert_eq!(asked[0].1, "tok");
        assert!(!asked[0].2, "no force without --force");
        assert!(waits.load(Ordering::SeqCst) > 0, "the exit was waited for");
    }

    #[test]
    fn quit_without_a_running_instance_says_so_and_exits_zero() {
        let sandbox = Sandbox::new();
        let deps = deps(
            |_: &Ask| panic!("nothing may be asked"),
            |_: u32| false,
            Arc::new(AtomicUsize::new(0)),
        );
        let lines = quit(&sandbox.env(), &Options::default(), &deps).expect("quit");
        assert_eq!(lines, vec!["no Mnemo2 running".to_string()]);
    }

    #[test]
    fn a_stale_run_file_is_not_asked_and_does_not_count_as_running() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4243, 2, "tok", Some(&project), 1);
        let deps = deps(
            |_: &Ask| panic!("a dead pid must not be asked"),
            |_: u32| false,
            Arc::new(AtomicUsize::new(0)),
        );
        let lines = quit(&sandbox.env(), &Options::default(), &deps).expect("quit");
        assert_eq!(lines, vec!["no Mnemo2 running".to_string()]);
    }

    #[test]
    fn force_is_passed_through() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4244, 3, "tok", Some(&project), 1);
        // The fake instance leaves as soon as it is asked, so the wait ends.
        let asked = Arc::new(AtomicUsize::new(0));
        let forces = Arc::new(Mutex::new(Vec::new()));
        let deps = deps(
            {
                let asked = Arc::clone(&asked);
                let forces = Arc::clone(&forces);
                move |ask: &Ask| {
                    forces.lock().unwrap().push(ask.force);
                    asked.fetch_add(1, Ordering::SeqCst);
                    answer(200, json!({ "quitting": true, "pid": 4244 }))
                }
            },
            move |_: u32| asked.load(Ordering::SeqCst) == 0,
            Arc::new(AtomicUsize::new(0)),
        );
        let options = Options {
            force: true,
            ..Options::default()
        };
        quit(&sandbox.env(), &options, &deps).expect("quit");
        assert_eq!(*forces.lock().unwrap(), vec![true]);
    }

    #[test]
    fn a_refused_quit_names_the_workers_and_force() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4245, 4, "tok", Some(&project), 1);
        let deps = deps(
            |_: &Ask| {
                answer(
                    409,
                    json!({
                        "error": "2 worker(s) are running; quit with force to interrupt them",
                        "running": 2,
                    }),
                )
            },
            always_alive(),
            Arc::new(AtomicUsize::new(0)),
        );
        let error = quit(&sandbox.env(), &Options::default(), &deps).expect_err("409");
        assert_eq!(
            error,
            "pid 4245: 2 worker(s) are running; rerun with --force to interrupt them"
        );
    }

    #[test]
    fn an_instance_without_the_route_says_to_quit_it_from_the_tray() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4246, 5, "tok", Some(&project), 1);
        let deps = deps(
            |_: &Ask| answer(404, json!({ "error": "not found" })),
            always_alive(),
            Arc::new(AtomicUsize::new(0)),
        );
        let error = quit(&sandbox.env(), &Options::default(), &deps).expect_err("404");
        assert_eq!(
            error,
            "pid 4246 is an older build without /app/quit; quit it from the tray once"
        );
    }

    #[test]
    fn a_rejected_token_is_reported_as_such() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4247, 6, "tok", Some(&project), 1);
        let deps = deps(
            |_: &Ask| answer(401, json!({ "error": "unauthorized" })),
            always_alive(),
            Arc::new(AtomicUsize::new(0)),
        );
        let error = quit(&sandbox.env(), &Options::default(), &deps).expect_err("401");
        assert!(error.starts_with("pid 4247 rejected the quit request"), "{error}");
        assert!(error.ends_with("quit it from the tray once"), "{error}");
    }

    #[test]
    fn an_unreachable_instance_is_reported_as_unanswered() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4248, 7, "tok", Some(&project), 1);
        let deps = deps(
            |_: &Ask| {
                Err(HttpError::Connect(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "refused",
                )))
            },
            always_alive(),
            Arc::new(AtomicUsize::new(0)),
        );
        let error = quit(&sandbox.env(), &Options::default(), &deps).expect_err("refused");
        assert_eq!(error, "pid 4248 did not answer the quit request");
    }

    #[test]
    fn an_instance_that_never_exits_is_reported_after_the_timeout() {
        let sandbox = Sandbox::new();
        let project = sandbox.project_str();
        write_run_file(&sandbox.run_dir, 4249, 8, "tok", Some(&project), 1);
        // A clock that jumps 15 s on every reading, so the wait ends quickly.
        let clock = Arc::new(Mutex::new(Instant::now()));
        let deps = Deps {
            call: Box::new(|_: &Ask| {
                answer(200, json!({ "quitting": true, "pid": 4249 }))
            }),
            alive: Box::new(always_alive()),
            now: Box::new({
                let clock = Arc::clone(&clock);
                move || {
                    let mut now = clock.lock().unwrap();
                    *now += Duration::from_secs(15);
                    *now
                }
            }),
            wait: Box::new(|_: Duration| {}),
        };
        let error = quit(&sandbox.env(), &Options::default(), &deps).expect_err("timeout");
        assert_eq!(error, "pid 4249 did not exit within 60 s");
    }

    #[test]
    fn the_command_line_is_force_plus_an_optional_timeout() {
        assert_eq!(parse_args(&[]).unwrap(), Options::default());
        assert_eq!(
            parse_args(&strings(&["--force"])).unwrap(),
            Options {
                force: true,
                ..Options::default()
            }
        );
        let timed = Options {
            timeout: Duration::from_secs(5),
            ..Options::default()
        };
        assert_eq!(parse_args(&strings(&["--timeout", "5"])).unwrap(), timed);
        assert_eq!(parse_args(&strings(&["--timeout=5"])).unwrap(), timed);
        assert_eq!(
            parse_args(&strings(&["--force", "--timeout=5"])).unwrap(),
            Options {
                force: true,
                ..timed
            }
        );
        assert!(parse_args(&strings(&["--force", "extra"])).is_err());
        assert!(parse_args(&strings(&["--timeout"])).is_err());
        assert!(parse_args(&strings(&["--timeout", "soon"])).is_err());
    }

    /// The real client posts the documented request: `/app/quit` with the run
    /// file's bearer token and `{"force": …}`.
    #[test]
    fn the_real_client_posts_the_documented_request() {
        let server = FakeServer::start("tok", |_: &Req| {
            json_reply(json!({ "quitting": true, "pid": 4242 }))
        });
        let endpoint = Endpoint {
            port: server.port,
            token: "tok".to_string(),
        };
        let deps = Deps::real();
        let response = (deps.call)(&Ask {
            endpoint: &endpoint,
            force: true,
        })
        .expect("the fake endpoint answers");
        assert_eq!(response.status, 200);
        let requests = server.requests_to("/app/quit");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].headers.get("authorization").map(String::as_str),
            Some("Bearer tok")
        );
        assert_eq!(requests[0].json(), json!({ "force": true }));
    }

    /// The module reports itself in the helper's usage text, so a bare run
    /// shows the command (see `main`).
    #[test]
    fn the_usage_names_the_command() {
        assert!(crate::USAGE.contains("mnemo2-mcp quit [--force] [--timeout <seconds>]"));
    }
}
