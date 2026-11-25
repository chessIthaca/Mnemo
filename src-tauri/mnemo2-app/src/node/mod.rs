// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `mnemo2-node` command line: pair a headless worker node with a hub once,
//! then run it (plan a040713f, phase 2 step 6).
//!
//! ```text
//! mnemo2-node pair   --hub <host:port> --code <CODE> [--name <name>] [--config-dir <dir>]
//! mnemo2-node run    --workspace <dir> [--config-dir <dir>]
//! mnemo2-node status [--config-dir <dir>]
//! mnemo2-node unpair [--config-dir <dir>]
//! ```
//!
//! The node's home is `<config dir>/node`, where the config dir resolves as
//! `--config-dir`, else `$MNEMO_HOME`, else `~/.mnemo`. The home holds
//! `node.json` (the hub address, the node id and the node token, the only
//! secret a node keeps), the mirrored model catalog, the node-local state and
//! `node.log`. The runtime itself lives in the library
//! (`mnemo::remote::node`); this module is the process around it: arguments,
//! paths, logging, the Tokio runtime and the exit codes.

mod logfile;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use mnemo::config::resolve_config_dir;
use mnemo::remote::node::{self, NodeConfig, NodeError};
use mnemo::remote::pairing::NodeIdentity;
use mnemo::remote::HUB_OUTAGE_BUDGET;

/// The usage text printed on a usage error and on `--help`.
const USAGE: &str = "usage: mnemo2-node pair --hub <host:port> --code <CODE> [--name <name>] [--config-dir <dir>]\n       \
    mnemo2-node run --workspace <dir> [--config-dir <dir>]\n       \
    mnemo2-node status [--config-dir <dir>]\n       \
    mnemo2-node unpair [--config-dir <dir>]\n\
    \n\
    exit codes: 0 ok, 2 usage, 3 not paired, 4 refused by the hub\n\
    (unknown_node, bad_proof, bad_code, expired), 5 protocol mismatch, 1 other\n\
    unpair exits 0 when the node is not paired (nothing to do)";

/// Exit code: a usage error.
const EXIT_USAGE: u8 = 2;
/// Exit code: this node is not paired.
const EXIT_NOT_PAIRED: u8 = 3;
/// Exit code: the hub refused the node or the pairing code.
const EXIT_REFUSED: u8 = 4;
/// Exit code: the hub speaks another protocol version.
const EXIT_PROTOCOL: u8 = 5;
/// Exit code: any other failure.
const EXIT_FAILURE: u8 = 1;

/// How long the runtime may take to wind down after the node stopped.
const RUNTIME_WIND_DOWN: Duration = Duration::from_secs(5);

/// One parsed command line.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Pair {
        hub: String,
        code: String,
        name: Option<String>,
        config_dir: Option<PathBuf>,
    },
    Run {
        workspace: PathBuf,
        config_dir: Option<PathBuf>,
    },
    Status {
        config_dir: Option<PathBuf>,
    },
    Unpair {
        config_dir: Option<PathBuf>,
    },
    Help,
}

/// The process entry: parse, run the command, answer the exit code.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match parse(&args) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(command) => command,
        Err(message) => {
            eprintln!("mnemo2-node: {message}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    ExitCode::from(execute(command))
}

/// Run one parsed command and answer its exit code.
fn execute(command: Command) -> u8 {
    match command {
        Command::Help => 0,
        Command::Status { config_dir } => status(&node_home(config_dir)),
        Command::Unpair { config_dir } => unpair(&node_home(config_dir)),
        Command::Pair {
            hub,
            code,
            name,
            config_dir,
        } => {
            let Some(runtime) = build_runtime() else {
                return EXIT_FAILURE;
            };
            let code = runtime.block_on(pair(
                &node_home(config_dir),
                &hub,
                &code,
                &name.unwrap_or_else(default_name),
            ));
            runtime.shutdown_timeout(RUNTIME_WIND_DOWN);
            code
        }
        Command::Run {
            workspace,
            config_dir,
        } => {
            let home = node_home(config_dir);
            let Some(runtime) = build_runtime() else {
                return EXIT_FAILURE;
            };
            let log = logfile::start(&home);
            let code = runtime.block_on(run(&home, &workspace));
            runtime.shutdown_timeout(RUNTIME_WIND_DOWN);
            if let Some(log) = log {
                log.finish();
            }
            code
        }
    }
}

/// The node's home: `<config dir>/node`.
fn node_home(config_dir: Option<PathBuf>) -> PathBuf {
    resolve_config_dir(config_dir).join("node")
}

/// The multi-thread runtime every networked command runs on.
fn build_runtime() -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("mnemo2-node")
        .build()
    {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            eprintln!("mnemo2-node: could not start the async runtime: {error}");
            None
        }
    }
}

/// A name for this machine when `--name` is not given.
fn default_name() -> String {
    ["COMPUTERNAME", "HOSTNAME", "HOST"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "mnemo-node".to_string())
}

/// `pair`: redeem the code and store the identity.
async fn pair(home: &Path, hub: &str, code: &str, name: &str) -> u8 {
    match NodeIdentity::load(home) {
        Ok(Some(previous)) => eprintln!(
            "mnemo2-node: replacing the earlier pairing with {} (node {})",
            previous.hub, previous.node_id
        ),
        Ok(None) => {}
        Err(error) => {
            eprintln!("mnemo2-node: {error}");
            return EXIT_FAILURE;
        }
    }
    match node::pair(home, hub, code, name).await {
        Ok(identity) => {
            println!(
                "paired with hub {} as node {} (hub id {})",
                identity.hub, identity.node_id, identity.hub_id
            );
            0
        }
        Err(error) => {
            eprintln!("mnemo2-node: pairing failed: {error}");
            exit_code(&error)
        }
    }
}

/// `run`: serve the hub until Ctrl-C (or SIGTERM on Unix).
async fn run(home: &Path, workspace: &Path) -> u8 {
    let identity = match NodeIdentity::load(home) {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            eprintln!(
                "mnemo2-node: this node is not paired yet; run `mnemo2-node pair --hub <host:port> --code <CODE>`"
            );
            return EXIT_NOT_PAIRED;
        }
        Err(error) => {
            eprintln!("mnemo2-node: {error}");
            return EXIT_FAILURE;
        }
    };
    let workspace = match dunce::canonicalize(workspace) {
        Ok(path) if path.is_dir() => path,
        _ => {
            eprintln!(
                "mnemo2-node: the workspace {} is not a directory",
                workspace.display()
            );
            return EXIT_USAGE;
        }
    };
    eprintln!(
        "mnemo2-node: node {} serving hub {} from {}",
        identity.node_id,
        identity.hub,
        workspace.display()
    );
    let cfg = NodeConfig {
        home: home.to_path_buf(),
        workspace,
        outage_budget: HUB_OUTAGE_BUDGET,
        // The node's engine keeps no code-graph index: a session works in a
        // checkout and reads it with the file tools.
        index_codegraph: false,
    };
    match node::run(cfg, shutdown_signal()).await {
        Ok(()) => {
            eprintln!("mnemo2-node: stopped");
            0
        }
        Err(error) => {
            eprintln!("mnemo2-node: {error}");
            exit_code(&error)
        }
    }
}

/// `status`: the pairing, never the token.
fn status(home: &Path) -> u8 {
    match NodeIdentity::load(home) {
        Ok(Some(identity)) => {
            println!("hub: {}", identity.hub);
            println!("node_id: {}", identity.node_id);
            println!("hub_id: {}", identity.hub_id);
            println!("home: {}", home.display());
            0
        }
        Ok(None) => {
            eprintln!("mnemo2-node: not paired");
            EXIT_NOT_PAIRED
        }
        Err(error) => {
            eprintln!("mnemo2-node: {error}");
            EXIT_FAILURE
        }
    }
}

/// `unpair`: forget the hub by deleting the identity file (the node token).
/// The hub keeps its own record until its operator revokes the node.
fn unpair(home: &Path) -> u8 {
    let path = NodeIdentity::path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            println!("unpaired; the hub still lists this node until it is revoked there");
            0
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("this node was not paired");
            0
        }
        Err(error) => {
            eprintln!("mnemo2-node: could not remove {}: {error}", path.display());
            EXIT_FAILURE
        }
    }
}

/// The exit code of a node-runtime failure.
fn exit_code(error: &NodeError) -> u8 {
    match error {
        NodeError::NotPaired => EXIT_NOT_PAIRED,
        NodeError::Refused(reason) => match reason.as_str() {
            "unknown_node" | "bad_proof" | "bad_code" | "expired" => EXIT_REFUSED,
            "proto" => EXIT_PROTOCOL,
            _ => EXIT_FAILURE,
        },
        NodeError::Io(_) | NodeError::Engine(_) => EXIT_FAILURE,
    }
}

/// Resolves on Ctrl-C, and on SIGTERM where the platform has it.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

// ── arguments ──────────────────────────────────────────────────────────────

/// Parse the arguments after the program name.
fn parse(args: &[String]) -> Result<Command, String> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err("a subcommand is required".to_string());
    };
    match subcommand.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "pair" => {
            let mut flags = Flags::parse(rest, &["hub", "code", "name", "config-dir"])?;
            let hub = flags.require("hub")?;
            if !hub.contains(':') {
                return Err(format!("--hub must be <host:port>, got {hub}"));
            }
            Ok(Command::Pair {
                hub,
                code: flags.require("code")?,
                name: flags.take("name"),
                config_dir: flags.take("config-dir").map(PathBuf::from),
            })
        }
        "run" => {
            let mut flags = Flags::parse(rest, &["workspace", "config-dir"])?;
            Ok(Command::Run {
                workspace: PathBuf::from(flags.require("workspace")?),
                config_dir: flags.take("config-dir").map(PathBuf::from),
            })
        }
        "status" => {
            let mut flags = Flags::parse(rest, &["config-dir"])?;
            Ok(Command::Status {
                config_dir: flags.take("config-dir").map(PathBuf::from),
            })
        }
        "unpair" => {
            let mut flags = Flags::parse(rest, &["config-dir"])?;
            Ok(Command::Unpair {
                config_dir: flags.take("config-dir").map(PathBuf::from),
            })
        }
        other => Err(format!("unknown subcommand {other}")),
    }
}

/// The `--flag value` / `--flag=value` pairs of one subcommand.
struct Flags {
    values: Vec<(String, String)>,
}

impl Flags {
    /// Collect the flags in `args`, accepting only `allowed` names, each once.
    fn parse(args: &[String], allowed: &[&str]) -> Result<Self, String> {
        let mut values: Vec<(String, String)> = Vec::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let Some(flag) = arg.strip_prefix("--") else {
                return Err(format!("unexpected argument {arg}"));
            };
            let (name, inline) = match flag.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (flag, None),
            };
            if !allowed.contains(&name) {
                return Err(format!("unknown flag --{name}"));
            }
            if values.iter().any(|(seen, _)| seen == name) {
                return Err(format!("--{name} given twice"));
            }
            let value = match inline {
                Some(value) => value,
                None => iter
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("--{name} needs a value"))?,
            };
            if value.is_empty() {
                return Err(format!("--{name} must not be empty"));
            }
            values.push((name.to_string(), value));
        }
        Ok(Self { values })
    }

    /// Remove and answer one flag's value.
    fn take(&mut self, name: &str) -> Option<String> {
        let at = self.values.iter().position(|(seen, _)| seen == name)?;
        Some(self.values.remove(at).1)
    }

    /// Remove and answer one flag's value, or fail when it is missing.
    fn require(&mut self, name: &str) -> Result<String, String> {
        self.take(name)
            .ok_or_else(|| format!("--{name} is required"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn parses_pair_with_every_flag() {
        let parsed = parse(&args(&[
            "pair",
            "--hub",
            "10.0.0.5:7000",
            "--code=ABCD-1234",
            "--name",
            "build box",
            "--config-dir",
            "cfg",
        ]))
        .expect("parses");
        assert_eq!(
            parsed,
            Command::Pair {
                hub: "10.0.0.5:7000".to_string(),
                code: "ABCD-1234".to_string(),
                name: Some("build box".to_string()),
                config_dir: Some(PathBuf::from("cfg")),
            }
        );
    }

    #[test]
    fn parses_run_status_and_unpair() {
        assert_eq!(
            parse(&args(&["run", "--workspace", "repo"])).expect("run"),
            Command::Run {
                workspace: PathBuf::from("repo"),
                config_dir: None,
            }
        );
        assert_eq!(
            parse(&args(&["status"])).expect("status"),
            Command::Status { config_dir: None }
        );
        assert_eq!(
            parse(&args(&["unpair", "--config-dir", "x"])).expect("unpair"),
            Command::Unpair {
                config_dir: Some(PathBuf::from("x")),
            }
        );
        assert_eq!(parse(&args(&["--help"])).expect("help"), Command::Help);
    }

    #[test]
    fn rejects_bad_command_lines() {
        for bad in [
            args(&[]),
            args(&["serve"]),
            args(&["pair", "--hub", "10.0.0.5:7000"]),
            args(&["pair", "--hub", "nohostport", "--code", "X"]),
            args(&["pair", "--hub", "h:1", "--code", "X", "--code", "Y"]),
            args(&["pair", "--hub", "h:1", "--code"]),
            args(&["run"]),
            args(&["run", "--workspace", "a", "--bogus", "b"]),
            args(&["run", "stray"]),
            args(&["status", "--workspace", "a"]),
        ] {
            assert!(parse(&bad).is_err(), "{bad:?} should be a usage error");
        }
    }

    #[test]
    fn exit_codes_follow_the_refusal() {
        assert_eq!(exit_code(&NodeError::NotPaired), EXIT_NOT_PAIRED);
        for reason in ["unknown_node", "bad_proof", "bad_code", "expired"] {
            assert_eq!(
                exit_code(&NodeError::Refused(reason.to_string())),
                EXIT_REFUSED,
                "{reason}"
            );
        }
        assert_eq!(
            exit_code(&NodeError::Refused("proto".to_string())),
            EXIT_PROTOCOL
        );
        assert_eq!(
            exit_code(&NodeError::Refused("internal".to_string())),
            EXIT_FAILURE
        );
        assert_eq!(exit_code(&NodeError::Io("x".to_string())), EXIT_FAILURE);
    }

    #[test]
    fn home_is_the_node_dir_of_the_config_dir() {
        assert_eq!(
            node_home(Some(PathBuf::from("cfg"))),
            PathBuf::from("cfg").join("node")
        );
    }
}
