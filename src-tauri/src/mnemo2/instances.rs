// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The instances overview: every Mnemo2 running on this machine.
//!
//! Each instance writes its [run file](crate::mnemo2::run_file) (port, bearer
//! token, open project) and serves an authenticated `POST /instance/summary`
//! with its own counters. This module joins the two: it prunes the run files of
//! dead processes, asks every remaining instance (this one included) for its
//! summary in parallel within [`PROBE_TIMEOUT`], and keeps the instances that
//! do not answer in the list marked `answered: false` — an unreachable window
//! is information, never a reason to hide its row.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mnemo2::run_file::{self, RunFile};

/// How long one instance has to answer before it counts as not answering.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// The route an instance answers its own summary on.
pub const SUMMARY_PATH: &str = "/instance/summary";

/// The route that brings an instance's window to the front.
pub const SHOW_PATH: &str = "/instance/show";

/// What an instance reports about itself on [`SUMMARY_PATH`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceSummary {
    /// The open project root; `None` on the start screen.
    #[serde(default)]
    pub project: Option<String>,
    /// Workers with active work in progress.
    #[serde(default)]
    pub workers_running: usize,
    /// Workers blocked on the person.
    #[serde(default)]
    pub workers_waiting: usize,
    /// Today's spend in micro-USD (the Cost tab's per-day estimate).
    #[serde(default)]
    pub cost_today_micros: u64,
    /// The instance's version.
    #[serde(default)]
    pub version: String,
}

/// One row of the Mission Control instances list.
///
/// Every field of an instance that did not answer is still present: the run
/// file supplies `project`, `version` and `started_ms`, and `answered` is what
/// tells the UI to show "not answering" in place of the counters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstanceRow {
    /// The instance's process id (also its run file's name).
    pub pid: u32,
    /// The open project root, or `None` on the start screen.
    pub project: Option<String>,
    /// The instance's version: its answer's, else the run file's.
    pub version: String,
    /// When the instance started, in Unix epoch milliseconds.
    pub started_ms: u64,
    /// Whether this is the window asking.
    pub is_self: bool,
    /// Whether the instance answered its summary in time.
    pub answered: bool,
    /// Workers with active work in progress (`0` when it did not answer).
    pub workers_running: usize,
    /// Workers blocked on the person (`0` when it did not answer).
    pub workers_waiting: usize,
    /// Today's spend in micro-USD (`0` when it did not answer).
    pub cost_today_micros: u64,
}

impl InstanceRow {
    /// One row from a run file and its summary, if the instance answered.
    fn new(file: &RunFile, self_pid: u32, summary: Option<InstanceSummary>) -> Self {
        let is_self = file.pid == self_pid;
        let Some(summary) = summary else {
            return Self {
                pid: file.pid,
                project: file.project.clone(),
                version: file.version.clone(),
                started_ms: file.started_ms,
                is_self,
                answered: false,
                workers_running: 0,
                workers_waiting: 0,
                cost_today_micros: 0,
            };
        };
        Self {
            pid: file.pid,
            project: summary.project.or_else(|| file.project.clone()),
            version: if summary.version.is_empty() {
                file.version.clone()
            } else {
                summary.version
            },
            started_ms: file.started_ms,
            is_self,
            answered: true,
            workers_running: summary.workers_running,
            workers_waiting: summary.workers_waiting,
            cost_today_micros: summary.cost_today_micros,
        }
    }

    /// The test-visible wire shape is the struct itself; this keeps the
    /// serializer honest for the two boolean fields a client may rely on.
    #[cfg(test)]
    fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("instance row json")
    }
}

/// Every live instance on this machine, this window included, oldest start
/// first.
///
/// Run files of dead processes are pruned first. An instance that does not
/// answer within [`PROBE_TIMEOUT`] stays in the list as `answered: false`.
pub async fn list(config_dir: &Path, self_pid: u32) -> Vec<InstanceRow> {
    list_with(config_dir, self_pid, crate::instance_marker::pid_alive).await
}

/// [`list`] with the process-liveness check injected, so a test can put fake
/// pids in a temp config dir (a fake pid is never alive).
pub async fn list_with(
    config_dir: &Path,
    self_pid: u32,
    alive: impl Fn(u32) -> bool,
) -> Vec<InstanceRow> {
    run_file::prune_stale(config_dir, &alive);
    let files = run_file::list(config_dir);
    let client = probe_client();
    let mut probes = tokio::task::JoinSet::new();
    for (index, file) in files.iter().enumerate() {
        let client = client.clone();
        let port = file.port;
        let token = file.token.clone();
        probes.spawn(async move {
            let summary = summary(&client, port, &token).await;
            (index, summary)
        });
    }
    let mut answers: Vec<Option<InstanceSummary>> = files.iter().map(|_| None).collect();
    while let Some(joined) = probes.join_next().await {
        if let Ok((index, summary)) = joined {
            answers[index] = summary;
        }
    }
    files
        .into_iter()
        .zip(answers)
        .map(|(file, answer)| InstanceRow::new(&file, self_pid, answer))
        .collect()
}

/// Ask one running instance to bring its window to the front.
///
/// Returns the reason when the instance is not running or does not answer, so
/// the caller can show it instead of a silent no-op.
pub async fn show(config_dir: &Path, pid: u32) -> Result<(), String> {
    let file = run_file::list(config_dir)
        .into_iter()
        .find(|file| file.pid == pid)
        .filter(|_| crate::instance_marker::pid_alive(pid))
        .ok_or_else(|| format!("instance {pid} is not running"))?;
    let response = probe_client()
        .post(url(file.port, SHOW_PATH))
        .bearer_auth(&file.token)
        .send()
        .await
        .map_err(|error| format!("could not reach instance {pid}: {error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "instance {pid} answered {}",
            response.status().as_u16()
        ))
    }
}

/// The loopback URL of one instance's route.
fn url(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

/// The client every probe uses: a system HTTP proxy must never intercept a
/// loopback call, and the probe timeout bounds each step.
fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(PROBE_TIMEOUT)
        .build()
        .unwrap_or_default()
}

/// Ask one instance for its summary; `None` when it does not answer, answers
/// with a non-success status, or sends a body that is not a summary.
async fn summary(client: &reqwest::Client, port: u16, token: &str) -> Option<InstanceSummary> {
    let response = client
        .post(url(port, SUMMARY_PATH))
        .bearer_auth(token)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.bytes().await.ok()?;
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    /// Write one instance's run file into `config_dir`.
    fn run_file_for(config_dir: &Path, pid: u32, port: u16, token: &str, project: Option<&str>) {
        run_file::write(
            config_dir,
            &RunFile {
                pid,
                port,
                token: token.to_string(),
                version: "1.2.3".to_string(),
                project: project.map(str::to_string),
                started_ms: 1_000 + u64::from(pid % 1_000),
            },
        )
        .expect("run file");
    }

    /// A loopback HTTP server that answers every `POST` carrying `token` with
    /// `200` and `body`, and refuses every other request.
    async fn serve(token: &'static str, body: &'static str) -> (u16, JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    loop {
                        match stream.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => {
                                head.extend_from_slice(&chunk[..read]);
                                if head.windows(4).any(|window| window == &b"\r\n\r\n"[..]) {
                                    break;
                                }
                            }
                        }
                    }
                    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
                    let (status, body) = if head.contains(&format!("authorization: bearer {token}")) {
                        ("200 OK", body.to_string())
                    } else {
                        ("401 Unauthorized", r#"{"error":"missing or wrong bearer token"}"#.to_string())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        (port, task)
    }

    /// A port nothing listens on, so a connection is refused at once.
    async fn dead_port() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        listener.local_addr().expect("addr").port()
    }

    #[tokio::test]
    async fn instances_list_includes_others_and_marks_self() {
        let config = tempfile::tempdir().expect("config dir");
        let (port, server) = serve(
            "token-b",
            r#"{"project":"C:\\work\\other","workers_running":2,"workers_waiting":1,"cost_today_micros":1234567,"version":"9.9.9"}"#,
        )
        .await;
        let me = std::process::id();
        let other = me.wrapping_add(1);
        run_file_for(config.path(), me, port, "token-b", Some("C:/work/self"));
        run_file_for(config.path(), other, port, "token-b", Some("C:/work/other"));

        let rows = list_with(config.path(), me, |pid| pid == me || pid == other).await;

        assert_eq!(rows.len(), 2, "{rows:?}");
        let self_row = rows.iter().find(|row| row.pid == me).expect("self row");
        assert!(self_row.is_self);
        assert!(self_row.answered);
        assert_eq!(self_row.project.as_deref(), Some("C:\\work\\other"), "the answer wins");
        assert_eq!(self_row.workers_running, 2);
        assert_eq!(self_row.workers_waiting, 1);
        assert_eq!(self_row.cost_today_micros, 1_234_567);
        assert_eq!(self_row.version, "9.9.9");
        let other_row = rows.iter().find(|row| row.pid == other).expect("other row");
        assert!(!other_row.is_self);
        assert!(other_row.answered);
        assert!(other_row.to_value()["answered"].as_bool().expect("bool"));
        server.abort();
    }

    #[tokio::test]
    async fn unreachable_instance_is_reported_not_dropped() {
        let config = tempfile::tempdir().expect("config dir");
        let me = std::process::id();
        let gone = dead_port().await;
        run_file_for(config.path(), me, gone, "token-c", Some("C:/work/gone"));

        let rows = list_with(config.path(), me, |pid| pid == me).await;

        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert!(!row.answered, "an unreachable instance is not dropped");
        assert_eq!(row.project.as_deref(), Some("C:/work/gone"), "the path stays");
        assert_eq!(row.version, "1.2.3", "the run file's version stays");
        assert_eq!(row.workers_running, 0);
        assert_eq!(row.workers_waiting, 0);
        assert_eq!(row.cost_today_micros, 0);
    }

    #[tokio::test]
    async fn a_wrong_token_counts_as_not_answering() {
        let config = tempfile::tempdir().expect("config dir");
        let (port, server) = serve(
            "the-real-token",
            r#"{"project":"C:/work/x","workers_running":1,"workers_waiting":0,"cost_today_micros":5,"version":"9.9.9"}"#,
        )
        .await;
        let me = std::process::id();
        run_file_for(config.path(), me, port, "a-stale-token", Some("C:/work/x"));

        let rows = list_with(config.path(), me, |pid| pid == me).await;

        assert_eq!(rows.len(), 1);
        assert!(!rows[0].answered);
        assert_eq!(rows[0].project.as_deref(), Some("C:/work/x"));
        server.abort();
    }

    #[tokio::test]
    async fn stale_run_files_are_pruned() {
        let config = tempfile::tempdir().expect("config dir");
        let me = std::process::id();
        let dead = me.wrapping_add(7);
        run_file_for(config.path(), me, dead_port().await, "token-d", None);
        run_file_for(config.path(), dead, dead_port().await, "token-e", Some("C:/work/dead"));

        let rows = list_with(config.path(), me, |pid| pid == me).await;

        assert_eq!(rows.len(), 1, "the dead run file is gone: {rows:?}");
        assert!(rows[0].is_self);
        assert!(!run_file::run_path(config.path(), dead).exists());
    }

    #[tokio::test]
    async fn show_reports_an_instance_that_is_not_running() {
        let config = tempfile::tempdir().expect("config dir");
        let error = show(config.path(), 999_999).await.expect_err("no such instance");
        assert!(error.contains("not running"), "{error}");
    }

    #[tokio::test]
    async fn show_asks_the_instance_to_show_its_window() {
        let config = tempfile::tempdir().expect("config dir");
        let (port, server) = serve("token-f", r#"{"shown":true}"#).await;
        let me = std::process::id();
        run_file_for(config.path(), me, port, "token-f", Some("C:/work/self"));

        show(config.path(), me).await.expect("shown");
        server.abort();
    }
}
