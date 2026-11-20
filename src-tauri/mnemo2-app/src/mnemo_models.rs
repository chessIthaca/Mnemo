// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `mnemo-models`: the machine-wide shared embedding-model host (plan
//! d3336a8a step 2).
//!
//! One process per machine holds the embedding models; every Mnemo
//! instance embeds through it over loopback HTTP instead of loading the
//! models into its own address space. The first instance to need it spawns
//! it detached (never inside an instance's kill-on-close job), passes a
//! fresh bearer token through the environment, and publishes the
//! descriptor under `~/.mnemo/run/models/`; every other instance attaches
//! after a health check. The idle exit is built into this binary itself:
//! the serve loop counts live instance leases and returns once none has
//! existed for `--idle-ms` (default 60 s), so a crash of every instance
//! never leaves the host running forever.
//!
//! Serves the models named by `--models` (the spawning instance's
//! configured memory model plus the code-search model), loading each
//! lazily on its first request. Deliberately a small synchronous console
//! binary: no Tauri, no async runtime — `mnemo::models_host::serve`
//! runs the loop on std threads.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// The hand-parse failure text (the house style of mnemo2-mcp's hidden
/// subcommands).
const USAGE: &str = "usage: mnemo-models --config-dir <dir> --port <port> --models m1,m2 [--idle-ms <ms>]\n       \
     the bearer token comes from the MNEMO_MODELS_TOKEN environment variable";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("mnemo-models: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The parsed command line: where the host serves, which model ids, and
/// how long to idle before exiting when no instance leases it.
#[derive(Debug, PartialEq, Eq)]
struct HostOptions {
    /// The config dir whose `run/models/` dir holds the descriptor and the
    /// instance leases.
    config_dir: PathBuf,
    /// The loopback port to bind.
    port: u16,
    /// The model ids to serve (each loaded lazily on its first request).
    models: Vec<String>,
    /// How long with no live lease before the host exits on its own.
    idle: Duration,
}

/// Parse `mnemo-models`'s command line: `--config-dir`, `--port`,
/// `--models` (comma-separated ids) and the optional `--idle-ms` — every
/// flag required except `--idle-ms`, which defaults to
/// [`mnemo::models_host::HOST_IDLE`]. The bearer token deliberately
/// does not come from the args (it arrives through the environment — see
/// [`run`]).
fn parse_args(args: &[String]) -> Result<HostOptions, String> {
    let mut config_dir: Option<PathBuf> = None;
    let mut port: Option<u16> = None;
    let mut models: Vec<String> = Vec::new();
    let mut idle_ms: Option<u64> = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--config-dir" | "--port" | "--models" | "--idle-ms" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err(format!("{USAGE}\nmissing value for {flag}"));
                };
                match flag {
                    "--config-dir" => config_dir = Some(PathBuf::from(value)),
                    "--port" => {
                        port = Some(value.parse().map_err(|_| format!("invalid --port: {value}"))?)
                    }
                    "--models" => {
                        models = value
                            .split(',')
                            .filter(|model| !model.is_empty())
                            .map(|model| model.to_string())
                            .collect();
                    }
                    "--idle-ms" => {
                        idle_ms =
                            Some(value.parse().map_err(|_| format!("invalid --idle-ms: {value}"))?)
                    }
                    _ => unreachable!("the match above names only these flags"),
                }
            }
            other => return Err(format!("{USAGE}\nunknown argument: {other}")),
        }
        index += 1;
    }
    let Some(config_dir) = config_dir else {
        return Err(USAGE.to_string());
    };
    let Some(port) = port else {
        return Err(USAGE.to_string());
    };
    if models.is_empty() {
        return Err(format!(
            "{USAGE}\n--models is required (the served model ids)"
        ));
    }
    let idle = Duration::from_millis(
        idle_ms.unwrap_or_else(|| mnemo::models_host::HOST_IDLE.as_millis() as u64),
    );
    Ok(HostOptions {
        config_dir,
        port,
        models,
        idle,
    })
}

/// Read the command line and the bearer token from the environment, then
/// hand both to the serve loop (which never returns while serving).
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = parse_args(&args)?;
    let token = std::env::var(mnemo::models_host::TOKEN_ENV).map_err(|_| {
        format!(
            "the bearer token env {} is not set (the spawning instance provides it)",
            mnemo::models_host::TOKEN_ENV
        )
    })?;
    mnemo::models_host::serve(
        &options.config_dir,
        options.port,
        &token,
        options.models,
        options.idle,
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn parse_args_reads_every_flag() {
        let options = parse_args(&strings(&[
            "--config-dir",
            "C:/run",
            "--port",
            "8123",
            "--models",
            "bge-base-en-v1.5,jina-embeddings-v2-base-code",
            "--idle-ms",
            "60000",
        ]))
        .expect("a full command line parses");
        assert_eq!(options.config_dir, PathBuf::from("C:/run"));
        assert_eq!(options.port, 8123);
        assert_eq!(
            options.models,
            strings(&["bge-base-en-v1.5", "jina-embeddings-v2-base-code"])
        );
        assert_eq!(options.idle, Duration::from_millis(60000));
    }

    #[test]
    fn parse_args_defaults_the_idle_budget_to_the_host_default() {
        let options = parse_args(&strings(&[
            "--config-dir",
            "C:/run",
            "--port",
            "8123",
            "--models",
            "bge-base-en-v1.5",
        ]))
        .expect("no --idle-ms is fine");
        assert_eq!(options.idle, mnemo::models_host::HOST_IDLE);
    }

    #[test]
    fn parse_args_splits_models_and_drops_empty_ids() {
        let options = parse_args(&strings(&[
            "--config-dir", "C:/run", "--port", "1", "--models", "a,,b,",
        ]))
        .expect("empty model ids are filtered out");
        assert_eq!(options.models, strings(&["a", "b"]));
    }

    #[test]
    fn parse_args_requires_each_flag_and_rejects_junk() {
        // A flag without a value.
        let error = parse_args(&strings(&["--config-dir"])).unwrap_err();
        assert!(error.contains("missing value for --config-dir"));
        // An unknown argument.
        let error = parse_args(&strings(&["--who"])).unwrap_err();
        assert!(error.contains("unknown argument: --who"));
        // A missing required flag (and a --models of only empty ids, which
        // still counts as missing).
        assert!(parse_args(&strings(&["--port", "1", "--models", "a"]))
            .unwrap_err()
            .contains(USAGE));
        assert!(parse_args(&strings(&["--config-dir", "C:/x", "--models", "a"]))
            .unwrap_err()
            .contains(USAGE));
        let error = parse_args(&strings(&[
            "--config-dir", "C:/x", "--port", "1", "--models", ",",
        ]))
        .unwrap_err();
        assert!(error.contains("--models is required"));
        // Junk numbers.
        assert!(
            parse_args(&strings(&[
                "--config-dir", "C:/x", "--port", "not-a-port", "--models", "a",
            ]))
            .unwrap_err()
            .contains("invalid --port")
        );
        assert!(
            parse_args(&strings(&[
                "--config-dir",
                "C:/x",
                "--port",
                "1",
                "--models",
                "a",
                "--idle-ms",
                "soon",
            ]))
            .unwrap_err()
            .contains("invalid --idle-ms")
        );
    }
}
