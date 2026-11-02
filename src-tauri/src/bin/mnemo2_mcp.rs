// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `mnemo2-mcp`: the console helper of the "mnemo" Claude Code plugin.
//!
//! It lets Claude Code lead a Mnemo2 project from outside the app:
//!
//! - `serve` bridges the plugin's stdio MCP server to a running Mnemo2 and
//!   starts Mnemo2 hidden when none has the project open;
//! - `notices --drain | --session-start | --stop-check` and `gate-agent` are
//!   Claude Code hooks (JSON on stdin, JSON on stdout, always exit 0);
//! - `wait [--timeout <s>]` is the background command that wakes an idle lead
//!   when a worker reports.
//!
//! It is deliberately small and synchronous (no Tauri, no async runtime): the
//! hooks run on every tool call.

// The helpers live in `mnemo2_mcp/`; `#[path]` keeps cargo from auto-discovering
// them as binaries of their own.
#[path = "mnemo2_mcp/hooks.rs"]
mod hooks;
#[path = "mnemo2_mcp/http.rs"]
mod http;
#[path = "mnemo2_mcp/instance.rs"]
mod instance;
#[path = "mnemo2_mcp/permissions.rs"]
mod permissions;
#[path = "mnemo2_mcp/serve.rs"]
mod serve;
#[path = "mnemo2_mcp/summary.rs"]
mod summary;
#[cfg(test)]
#[path = "mnemo2_mcp/testutil.rs"]
mod testutil;

use std::io::{IsTerminal, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::SystemTime;

use hooks::NoticesMode;
use instance::Env;

const USAGE: &str = "usage: mnemo2-mcp serve\n       \
    mnemo2-mcp notices --drain | --session-start | --stop-check\n       \
    mnemo2-mcp wait [--timeout <seconds>] [--session <id>]\n       \
    mnemo2-mcp hint --pre-search\n       \
    mnemo2-mcp gate-agent\n       \
    mnemo2-mcp team-summary [--json] [--cwd <dir>]\n       \
    mnemo2-mcp show-worker <worker_id> [--cwd <dir>]";

/// Default `wait` timeout, in seconds.
const DEFAULT_WAIT_S: u64 = 1800;

/// Set (to anything) in the delegated child so it does not delegate again.
const NO_DELEGATE_ENV: &str = "MNEMO2_MCP_NO_DELEGATE";

#[cfg(windows)]
const HELPER_EXE: &str = "mnemo2-mcp.exe";
#[cfg(not(windows))]
const HELPER_EXE: &str = "mnemo2-mcp";

/// True when the candidate copy is at least as new as this executable. An
/// older copy in `<config>/bin` (say, from a previous build) must never take
/// over from a newer one; a copy whose age cannot be read is not trusted.
fn candidate_is_fresh(candidate: Option<SystemTime>, current: Option<SystemTime>) -> bool {
    matches!((candidate, current), (Some(candidate), Some(current)) if candidate >= current)
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// The fresher copy of this helper to hand over to, if there is one.
///
/// `claude plugin install` copies the plugin (and so `bin/mnemo2-mcp`) into
/// Claude Code's plugin cache, where it goes stale after every rebuild. The
/// build installs the current helper to `<config>/bin/`, and any other copy
/// delegates to it, provided that copy is not older than this one. The
/// decision reads only file metadata, so it can be tested without spawning.
fn delegate_target(config_dir: &Path, current_exe: &Path, no_delegate: bool) -> Option<PathBuf> {
    if no_delegate {
        return None;
    }
    let candidate = config_dir.join("bin").join(HELPER_EXE);
    if !candidate.is_file() || instance::same_path(&candidate, current_exe) {
        return None;
    }
    candidate_is_fresh(modified(&candidate), modified(current_exe)).then_some(candidate)
}

/// The copy to delegate to for this process, if any.
fn delegate_exe() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let config_dir = mnemo::config::resolve_config_dir(None);
    delegate_target(
        &config_dir,
        &current,
        std::env::var_os(NO_DELEGATE_ENV).is_some(),
    )
}

/// Run the `<config>/bin` copy with our arguments and stdio and return its
/// exit code (`serve`); None when there is nothing to delegate to or it cannot
/// start.
fn delegate_passthrough() -> Option<ExitCode> {
    let status = Command::new(delegate_exe()?)
        .args(std::env::args_os().skip(1))
        .env(NO_DELEGATE_ENV, "1")
        .status()
        .ok()?;
    Some(ExitCode::from(
        status.code().and_then(|code| u8::try_from(code).ok()).unwrap_or(1),
    ))
}

/// Run `target` as a hook with `args`, feeding it `input` on stdin, and return
/// what it printed. None when it cannot start, is killed, or exits non-zero:
/// the caller then runs the hook itself, so a broken delegate never costs the
/// hook its answer.
fn run_delegated_hook(target: &Path, args: &[String], input: &str) -> Option<String> {
    let mut child = Command::new(target)
        .args(args)
        .env(NO_DELEGATE_ENV, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let input = input.to_string();
        // A thread, so a child that never reads cannot block us on a full pipe.
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// A hook subcommand and what it runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hook {
    Notices(NoticesMode),
    GateAgent,
    /// PreToolUse on Grep/Glob: point a code or knowledge lookup at the
    /// Mnemo tool that answers it better. Never blocks.
    Hint,
}

/// Which hook `command rest...` names; the error is a note for stderr.
fn parse_hook(command: &str, rest: &[String]) -> Result<Hook, String> {
    match command {
        "notices" => parse_notices_mode(rest).map(Hook::Notices).ok_or_else(|| {
            "notices needs exactly one of --drain, --session-start, --stop-check".to_string()
        }),
        "hint" => match rest {
            [flag] if flag == "--pre-search" => Ok(Hook::Hint),
            [] => Ok(Hook::Hint),
            _ => Err("hint takes no arguments other than --pre-search".to_string()),
        },
        "gate-agent" if rest.is_empty() => Ok(Hook::GateAgent),
        "gate-agent" => Err("gate-agent takes no arguments".to_string()),
        other => Err(format!("{other} is not a hook")),
    }
}

/// The hook a Claude Code hook event runs, for a call without arguments.
///
/// Claude Code 2.1.138 ignores the `args` of an exec-form hook and runs the
/// bare command, so the hook names itself through the `hook_event_name` its
/// stdin JSON always carries. The plugin keeps `args` for versions that pass
/// them; either way the same hook runs, with no shell in between.
fn hook_for_event(input: &str) -> Option<Hook> {
    let event: serde_json::Value = serde_json::from_str(input).ok()?;
    match event.get("hook_event_name")?.as_str()? {
        "SessionStart" => Some(Hook::Notices(NoticesMode::SessionStart)),
        "PostToolUse" => Some(Hook::Notices(NoticesMode::Drain)),
        "Stop" => Some(Hook::Notices(NoticesMode::StopCheck)),
        // One event, two hooks: the search hint rides on Grep and Glob, the
        // agent gate on everything else (its plugin matcher lists the agents).
        "PreToolUse" => match event.get("tool_name").and_then(serde_json::Value::as_str) {
            Some("Grep" | "Glob") => Some(Hook::Hint),
            _ => Some(Hook::GateAgent),
        },
        _ => None,
    }
}

/// The explicit arguments of `hook`, for a delegate that may predate the
/// event-name dispatch.
fn hook_args(hook: Hook) -> Vec<String> {
    let args: &[&str] = match hook {
        Hook::Notices(NoticesMode::Drain) => &["notices", "--drain"],
        Hook::Notices(NoticesMode::SessionStart) => &["notices", "--session-start"],
        Hook::Notices(NoticesMode::StopCheck) => &["notices", "--stop-check"],
        Hook::GateAgent => &["gate-agent"],
        Hook::Hint => &["hint", "--pre-search"],
    };
    args.iter().map(|arg| arg.to_string()).collect()
}

fn main() -> ExitCode {
    // First, for every command: a copy that delegates hands its own stdio
    // handles to the delegate, and anything either spawns later (the hidden
    // Mnemo2) must not hold Claude Code's pipes.
    instance::stop_inheriting_stdio();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let rest = args.get(1..).unwrap_or_default();
    match command {
        "notices" | "gate-agent" | "hint" => run_hook_command(command, rest, &args),
        // A bare call with a hook's JSON on stdin (see `hook_for_event`).
        "" if !std::io::stdin().is_terminal() => run_event_hook(),
        "serve" => {
            if let Some(code) = delegate_passthrough() {
                return code;
            }
            let env = Env::from_process();
            let stdin = std::io::stdin();
            ExitCode::from(serve::run(env, stdin.lock(), Box::new(std::io::stdout())))
        }
        // `wait` always runs in place: a delegated waiter is a second process
        // that outlives a kill of the first and would drain notices nobody reads.
        "wait" => match parse_wait_args(rest) {
            Some((timeout_s, session)) => {
                let env = Env::from_process();
                print_line(&hooks::wait(&env, timeout_s, session.as_deref()));
                ExitCode::SUCCESS
            }
            None => usage(),
        },
        "team-summary" => {
            let env = Env::from_process();
            print_line(&summary::team_summary(&env, rest));
            ExitCode::SUCCESS
        }
        "show-worker" => {
            let env = Env::from_process();
            match summary::show_worker(&env, rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(reason) => {
                    eprintln!("{reason}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn parse_notices_mode(args: &[String]) -> Option<NoticesMode> {
    match args {
        [flag] => match flag.as_str() {
            "--drain" => Some(NoticesMode::Drain),
            "--session-start" => Some(NoticesMode::SessionStart),
            "--stop-check" => Some(NoticesMode::StopCheck),
            _ => None,
        },
        _ => None,
    }
}

/// The `wait` command line: `--timeout <s>` or `--timeout=<s>` (the default
/// otherwise), plus `--session <id>` or `--session=<id>` (the Claude Code
/// session the Stop hook named), in any order.
fn parse_wait_args(args: &[String]) -> Option<(u64, Option<String>)> {
    let mut timeout_s = DEFAULT_WAIT_S;
    let mut session = None;
    let mut index = 0;
    while index < args.len() {
        let (flag, inline) = match args[index].split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (args[index].clone(), None),
        };
        let has_inline = inline.is_some();
        let value = match inline {
            Some(value) => value,
            None => args.get(index + 1)?.clone(),
        };
        match flag.as_str() {
            "--timeout" => timeout_s = value.parse().ok()?,
            "--session" => session = Some(value),
            _ => return None,
        }
        index += if has_inline { 1 } else { 2 };
    }
    Some((timeout_s, session))
}

/// Run a hook: read its JSON from stdin, print what it answers. A hook never
/// fails loudly: bad arguments, a broken delegate, an unreachable Mnemo2 or a
/// panic all end in at most a note on stderr, no stdout and exit code 0 (any
/// other code would show up as an error in the user's session).
fn run_hook_command(command: &str, rest: &[String], args: &[String]) -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    let hook = match parse_hook(command, rest) {
        Ok(hook) => hook,
        Err(note) => {
            eprintln!("mnemo2-mcp: ignoring this hook call: {note}");
            return ExitCode::SUCCESS;
        }
    };
    run_hook(hook, args, read_stdin())
}

/// A hook named by its stdin JSON (no arguments). Anything else is ignored,
/// like every other hook failure: no stdout, exit code 0.
fn run_event_hook() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    let input = read_stdin();
    match hook_for_event(&input) {
        Some(hook) => run_hook(hook, &hook_args(hook), input),
        None => {
            eprintln!("mnemo2-mcp: ignoring a call without arguments that is not a known hook event");
            ExitCode::SUCCESS
        }
    }
}

fn run_hook(hook: Hook, args: &[String], input: String) -> ExitCode {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // A fresher copy answers when it can; otherwise this copy does.
        let delegated = delegate_exe().and_then(|target| run_delegated_hook(&target, args, &input));
        if let Some(output) = delegated {
            print_raw(&output);
            return;
        }
        let env = Env::from_process();
        let output = match hook {
            Hook::Notices(mode) => hooks::notices(&env, mode, &input),
            Hook::GateAgent => hooks::gate_agent(&env, &input),
            Hook::Hint => hooks::hint(&env, &input),
        };
        if let Some(output) = output {
            print_line(&output);
        }
    }));
    ExitCode::SUCCESS
}

/// All of stdin (hooks pipe a small JSON document), empty on a terminal.
fn read_stdin() -> String {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = stdin.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn print_line(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{text}");
    let _ = stdout.flush();
}

fn print_raw(text: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn a_bare_call_picks_the_hook_from_the_event_name() {
        let event = |name: &str| format!(r#"{{"hook_event_name":"{name}","cwd":"C:/x"}}"#);
        assert_eq!(
            hook_for_event(&event("SessionStart")),
            Some(Hook::Notices(NoticesMode::SessionStart))
        );
        assert_eq!(hook_for_event(&event("PostToolUse")), Some(Hook::Notices(NoticesMode::Drain)));
        assert_eq!(hook_for_event(&event("Stop")), Some(Hook::Notices(NoticesMode::StopCheck)));
        assert_eq!(hook_for_event(&event("PreToolUse")), Some(Hook::GateAgent));
        // One PreToolUse hook per tool: the search hint on Grep and Glob, the
        // agent gate everywhere else (its plugin matcher lists the agents).
        let tool_event = |name: &str, tool: &str| {
            format!(r#"{{"hook_event_name":"{name}","tool_name":"{tool}","cwd":"C:/x"}}"#)
        };
        assert_eq!(hook_for_event(&tool_event("PreToolUse", "Agent")), Some(Hook::GateAgent));
        assert_eq!(hook_for_event(&tool_event("PreToolUse", "Task")), Some(Hook::GateAgent));
        assert_eq!(hook_for_event(&tool_event("PreToolUse", "Grep")), Some(Hook::Hint));
        assert_eq!(hook_for_event(&tool_event("PreToolUse", "Glob")), Some(Hook::Hint));
        // A Grep after a tool call is the notice drain, not a hint.
        assert_eq!(
            hook_for_event(&tool_event("PostToolUse", "Grep")),
            Some(Hook::Notices(NoticesMode::Drain))
        );
        assert_eq!(hook_for_event(&event("SubagentStop")), None);
        assert_eq!(hook_for_event("not json"), None);
        assert_eq!(hook_for_event("{}"), None);
        // The explicit form a delegate gets round-trips to the same hook.
        for hook in [
            Hook::Notices(NoticesMode::Drain),
            Hook::Notices(NoticesMode::SessionStart),
            Hook::Notices(NoticesMode::StopCheck),
            Hook::GateAgent,
            Hook::Hint,
        ] {
            let args = hook_args(hook);
            assert_eq!(parse_hook(&args[0], &args[1..]), Ok(hook));
        }
    }

    fn touch(path: &Path, age: Duration) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - age)
            .unwrap();
    }

    #[test]
    fn notices_flags_map_to_modes() {
        assert_eq!(parse_notices_mode(&strings(&["--drain"])), Some(NoticesMode::Drain));
        assert_eq!(
            parse_notices_mode(&strings(&["--session-start"])),
            Some(NoticesMode::SessionStart)
        );
        assert_eq!(
            parse_notices_mode(&strings(&["--stop-check"])),
            Some(NoticesMode::StopCheck)
        );
        assert_eq!(parse_notices_mode(&strings(&[])), None);
        assert_eq!(parse_notices_mode(&strings(&["--bogus"])), None);
        assert_eq!(parse_notices_mode(&strings(&["--drain", "--stop-check"])), None);
    }

    #[test]
    fn hook_arguments_that_do_not_parse_are_an_error_note_never_a_failure() {
        assert_eq!(
            parse_hook("notices", &strings(&["--drain"])),
            Ok(Hook::Notices(NoticesMode::Drain))
        );
        assert_eq!(parse_hook("gate-agent", &strings(&[])), Ok(Hook::GateAgent));
        // The hint's flag is optional (the bare event-name dispatch passes none).
        assert_eq!(parse_hook("hint", &strings(&["--pre-search"])), Ok(Hook::Hint));
        assert_eq!(parse_hook("hint", &strings(&[])), Ok(Hook::Hint));
        // Missing, unknown and surplus arguments are all notes (main prints
        // them to stderr and still exits 0).
        for (command, args) in [
            ("notices", strings(&[])),
            ("notices", strings(&["--bogus"])),
            ("notices", strings(&["--drain", "--stop-check"])),
            ("gate-agent", strings(&["extra"])),
            ("hint", strings(&["--drain"])),
            ("hint", strings(&["--pre-search", "--drain"])),
        ] {
            let note = parse_hook(command, &args).expect_err("bad arguments");
            assert!(!note.is_empty());
        }
    }

    #[test]
    fn delegates_to_a_different_copy_in_the_config_bin_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let fresh = config.join("bin").join(HELPER_EXE);
        touch(&fresh, Duration::from_secs(10));
        let stale = dir.path().join("plugin-cache").join("bin").join(HELPER_EXE);
        // The plugin-cache copy is the older build.
        touch(&stale, Duration::from_secs(600));

        assert_eq!(delegate_target(&config, &stale, false), Some(fresh.clone()));
        // The delegated child (or an explicit opt-out) runs locally.
        assert_eq!(delegate_target(&config, &stale, true), None);
        // The config copy itself never delegates, however its path is spelled.
        assert_eq!(delegate_target(&config, &fresh, false), None);
        let roundabout = config.join("bin").join("..").join("bin").join(HELPER_EXE);
        assert_eq!(delegate_target(&config, &roundabout, false), None);
    }

    #[test]
    fn an_older_config_copy_never_overrides_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let old_config_copy = config.join("bin").join(HELPER_EXE);
        touch(&old_config_copy, Duration::from_secs(3600));
        let newer = dir.path().join("plugin-cache").join("bin").join(HELPER_EXE);
        touch(&newer, Duration::from_secs(60));
        assert_eq!(delegate_target(&config, &newer, false), None);

        // Equal timestamps still delegate (a copy preserves the build time).
        let when = SystemTime::now() - Duration::from_secs(300);
        for path in [&old_config_copy, &newer] {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        assert_eq!(delegate_target(&config, &newer, false), Some(old_config_copy));
    }

    #[test]
    fn the_freshness_rule_is_pure() {
        let now = SystemTime::now();
        let earlier = now - Duration::from_secs(5);
        assert!(candidate_is_fresh(Some(now), Some(earlier)));
        assert!(candidate_is_fresh(Some(now), Some(now)));
        assert!(!candidate_is_fresh(Some(earlier), Some(now)));
        // Unknown ages are not trusted, on either side.
        assert!(!candidate_is_fresh(None, Some(now)));
        assert!(!candidate_is_fresh(Some(now), None));
        assert!(!candidate_is_fresh(None, None));
    }

    #[test]
    fn nothing_to_delegate_to_means_running_locally() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let current = dir.path().join("somewhere").join(HELPER_EXE);
        // No <config>/bin at all.
        assert_eq!(delegate_target(&config, &current, false), None);
        // A directory where the exe should be is not a helper.
        std::fs::create_dir_all(config.join("bin").join(HELPER_EXE)).unwrap();
        assert_eq!(delegate_target(&config, &current, false), None);
    }

    #[test]
    fn a_delegated_hook_that_fails_hands_the_job_back() {
        let me = std::env::current_exe().unwrap();
        // The test harness doubles as a program that prints and exits 0 ...
        let ok = run_delegated_hook(&me, &strings(&["--list"]), "{}");
        assert!(ok.is_some_and(|text| text.contains("tests")), "the harness lists its tests");
        // ... one that exits non-zero ...
        assert_eq!(run_delegated_hook(&me, &strings(&["--no-such-flag"]), "{}"), None);
        // ... and a target that cannot start at all.
        let nowhere = me.with_file_name("no-such-helper.exe");
        assert_eq!(run_delegated_hook(&nowhere, &strings(&["notices", "--drain"]), "{}"), None);
    }

    #[test]
    fn wait_args_parse_both_spellings_and_the_session() {
        assert_eq!(parse_wait_args(&strings(&[])), Some((DEFAULT_WAIT_S, None)));
        assert_eq!(parse_wait_args(&strings(&["--timeout", "90"])), Some((90, None)));
        assert_eq!(parse_wait_args(&strings(&["--timeout=15"])), Some((15, None)));
        assert_eq!(
            parse_wait_args(&strings(&["--session", "sess-9"])),
            Some((DEFAULT_WAIT_S, Some("sess-9".to_string())))
        );
        assert_eq!(
            parse_wait_args(&strings(&["--session=sess-8"])),
            Some((DEFAULT_WAIT_S, Some("sess-8".to_string())))
        );
        assert_eq!(
            parse_wait_args(&strings(&["--timeout", "60", "--session", "sess-7"])),
            Some((60, Some("sess-7".to_string())))
        );
        assert_eq!(
            parse_wait_args(&strings(&["--session", "sess-7", "--timeout=60"])),
            Some((60, Some("sess-7".to_string())))
        );
        assert_eq!(parse_wait_args(&strings(&["--timeout", "soon"])), None);
        assert_eq!(parse_wait_args(&strings(&["--timeout"])), None);
        assert_eq!(parse_wait_args(&strings(&["--session"])), None);
        assert_eq!(parse_wait_args(&strings(&["90"])), None);
    }
}
