// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Test-only helpers: a fake Mnemo2 HTTP endpoint and a sandboxed run/config
//! directory, so no test ever touches the real `~/.mnemo` or a real instance.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{json, Value};

use crate::instance::{Env, Spawner};

/// One request the fake endpoint received.
#[derive(Clone, Debug)]
pub struct Req {
    pub path: String,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Req {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// What the fake endpoint answers.
pub enum Reply {
    Status(u16, String),
    /// Bytes written verbatim, then the connection closes.
    Raw(Vec<u8>),
}

pub fn json_reply(value: Value) -> Reply {
    Reply::Status(200, value.to_string())
}

pub fn health(pid: u32, project: Option<&str>) -> Reply {
    json_reply(json!({ "pid": pid, "version": "test", "project": project }))
}

type Handler = Arc<dyn Fn(&Req) -> Reply + Send + Sync>;

/// A loopback HTTP server that checks the bearer token like Mnemo2 does.
/// Dropping it stops the server, so bind it for the length of the test.
#[must_use = "dropping a FakeServer stops it"]
pub struct FakeServer {
    pub port: u16,
    pub token: String,
    requests: Arc<Mutex<Vec<Req>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start<F>(token: &str, handler: F) -> FakeServer
    where
        F: Fn(&Req) -> Reply + Send + Sync + 'static,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind fake server");
        let port = listener.local_addr().expect("local addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Handler = Arc::new(handler);
        let thread = {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            let token = token.to_string();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    let requests = Arc::clone(&requests);
                    let handler = Arc::clone(&handler);
                    let token = token.clone();
                    std::thread::spawn(move || serve_connection(stream, &token, &requests, &handler));
                }
            })
        };
        FakeServer {
            port,
            token: token.to_string(),
            requests,
            stop,
            thread: Some(thread),
        }
    }

    /// Every request so far, in arrival order.
    pub fn requests(&self) -> Vec<Req> {
        self.requests.lock().expect("requests lock").clone()
    }

    /// The requests for one path.
    pub fn requests_to(&self, path: &str) -> Vec<Req> {
        self.requests().into_iter().filter(|req| req.path == path).collect()
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn read_request(stream: &mut TcpStream) -> Option<Req> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let mut lines = head.split("\r\n");
            let request_line = lines.next()?;
            let path = request_line.split_whitespace().nth(1)?.to_string();
            let headers: HashMap<String, String> = lines
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_lowercase(), value.trim().to_string()))
                .collect();
            let length: usize = headers
                .get("content-length")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + length {
                let body = String::from_utf8_lossy(&buf[end + 4..end + 4 + length]).into_owned();
                return Some(Req { path, headers, body });
            }
        }
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn serve_connection(
    mut stream: TcpStream,
    token: &str,
    requests: &Mutex<Vec<Req>>,
    handler: &Handler,
) {
    let Some(request) = read_request(&mut stream) else {
        return;
    };
    requests.lock().expect("requests lock").push(request.clone());
    let authorised = request
        .headers
        .get("authorization")
        .is_some_and(|value| value == &format!("Bearer {token}"));
    let reply = if authorised {
        handler(&request)
    } else {
        Reply::Status(401, r#"{"error":"unauthorized"}"#.to_string())
    };
    let bytes = match reply {
        Reply::Raw(bytes) => bytes,
        Reply::Status(status, body) => {
            let reason = match status {
                200 => "OK",
                202 => "Accepted",
                401 => "Unauthorized",
                500 => "Internal Server Error",
                _ => "Status",
            };
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes()
        }
    };
    let _ = stream.write_all(&bytes);
    let _ = stream.flush();
}

/// A throwaway config dir, run dir and project tree.
pub struct Sandbox {
    pub dir: tempfile::TempDir,
    pub config_dir: PathBuf,
    pub run_dir: PathBuf,
    /// The canonical project root `<tmp>/proj`.
    pub project: PathBuf,
    /// `<tmp>/proj/sub/deep`.
    pub deep: PathBuf,
    /// `<tmp>/elsewhere`, outside the project.
    pub elsewhere: PathBuf,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dunce::canonicalize(dir.path()).expect("canonical tempdir");
        let config_dir = base.join("config");
        let run_dir = config_dir.join("run");
        let project = base.join("proj");
        let deep = project.join("sub").join("deep");
        let elsewhere = base.join("elsewhere");
        for path in [&run_dir, &deep, &elsewhere] {
            std::fs::create_dir_all(path).expect("create dirs");
        }
        Sandbox {
            dir,
            config_dir,
            run_dir,
            project,
            deep,
            elsewhere,
        }
    }

    pub fn project_str(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }

    /// An `Env` whose cwd is the project's `sub/deep`, with no way to start Mnemo2.
    pub fn env(&self) -> Env {
        let spawner: Spawner = Arc::new(|_| Err("spawning is not expected here".to_string()));
        Env {
            run_dir: self.run_dir.clone(),
            config_dir: self.config_dir.clone(),
            helper: PathBuf::from("/opt/mnemo/mnemo2-mcp"),
            process_cwd: self.deep.clone(),
            project_dir: None,
            spawner,
            spawn_timeout: Duration::from_secs(3),
            list_wait: Duration::from_secs(3),
            poll_interval: Duration::from_millis(20),
            health_timeout: Duration::from_secs(3),
            wait_reach_timeout: Duration::from_millis(300),
            wait_poll: Duration::from_millis(20),
        }
    }

    /// Write `mnemo2-<pid>.json` the way a running Mnemo2 would.
    pub fn write_run(&self, pid: u32, port: u16, token: &str, project: Option<&str>) -> PathBuf {
        write_run_file(&self.run_dir, pid, port, token, project, u64::from(pid))
    }

    /// Register `projects` in `<config>/projects.toml`.
    pub fn register(&self, projects: &[&Path]) {
        let mut text = String::new();
        for (index, path) in projects.iter().enumerate() {
            text.push_str(&format!(
                "[[project]]\nname = \"p{index}\"\npath = '{}'\n\n",
                path.display()
            ));
        }
        std::fs::write(self.config_dir.join("projects.toml"), text).expect("write projects.toml");
    }
}

pub fn write_run_file(
    run_dir: &Path,
    pid: u32,
    port: u16,
    token: &str,
    project: Option<&str>,
    started_ms: u64,
) -> PathBuf {
    std::fs::create_dir_all(run_dir).expect("run dir");
    let path = run_dir.join(format!("mnemo2-{pid}.json"));
    let body = json!({
        "pid": pid,
        "port": port,
        "token": token,
        "version": "test",
        "project": project,
        "started_ms": started_ms,
    });
    std::fs::write(&path, body.to_string()).expect("write run file");
    path
}

/// A port nothing listens on.
pub fn closed_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    port
}
