// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! A tiny blocking HTTP/1.1 client for Mnemo2's loopback API.
//!
//! Every route is a `POST` with a JSON body to `127.0.0.1:<port>` and answers
//! with `Connection: close`. A hand-rolled client over `TcpStream` keeps the
//! hook path (run on every tool call) free of an async runtime and a TLS stack.

use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

/// How long a loopback connect may take. Windows reports a refused loopback
/// connection only after about 2 s, so this must stay above that: otherwise a
/// dead instance would look merely slow, and its run file would never be
/// recognised as stale.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a write of one request may block.
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// Where an instance listens and the bearer token it demands.
#[derive(Clone)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

/// A parsed HTTP response.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// Why a request produced no response.
#[derive(Debug)]
pub enum HttpError {
    /// The TCP connection could not be opened: nothing listens there.
    Connect(io::Error),
    /// The connection broke or timed out after it was open.
    Io(io::Error),
    /// The peer answered something that is not HTTP.
    Protocol(String),
}

impl HttpError {
    /// True when the request provably never reached a server.
    pub fn is_connect(&self) -> bool {
        matches!(self, HttpError::Connect(_))
    }

    /// True when the OS refused the connection: nothing listens on the port.
    /// A connect that merely timed out does not count: that peer may be busy.
    pub fn is_refused(&self) -> bool {
        matches!(self, HttpError::Connect(err) if err.kind() == io::ErrorKind::ConnectionRefused)
    }

    /// True when a connect or a read ran into its timeout.
    pub fn is_timeout(&self) -> bool {
        match self {
            HttpError::Connect(err) | HttpError::Io(err) => {
                matches!(err.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
            }
            HttpError::Protocol(_) => false,
        }
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Connect(err) => write!(f, "could not connect ({err})"),
            HttpError::Io(err) => write!(f, "connection failed ({err})"),
            HttpError::Protocol(msg) => write!(f, "bad response ({msg})"),
        }
    }
}

/// POST `body` to `path` on `endpoint` with the bearer token plus `headers`.
///
/// `read_timeout` bounds each blocking read (None waits forever, which the
/// long-lived `/mcp` and `/lead/wait` calls need).
pub fn post(
    endpoint: &Endpoint,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
    read_timeout: Option<Duration>,
) -> Result<Response, HttpError> {
    post_with(endpoint, path, headers, body, CONNECT_TIMEOUT, read_timeout)
}

/// [`post`] with an explicit connect timeout.
pub fn post_with(
    endpoint: &Endpoint,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
    connect_timeout: Duration,
    read_timeout: Option<Duration>,
) -> Result<Response, HttpError> {
    for (name, value) in headers {
        if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
            return Err(HttpError::Protocol(format!("header {name} contains a line break")));
        }
    }
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, endpoint.port));
    let mut stream =
        TcpStream::connect_timeout(&addr, connect_timeout).map_err(HttpError::Connect)?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(read_timeout);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));

    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nAccept: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n",
        endpoint.port,
        endpoint.token,
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).map_err(HttpError::Io)?;
    stream.write_all(body.as_bytes()).map_err(HttpError::Io)?;
    stream.flush().map_err(HttpError::Io)?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return parse(&buf, true).unwrap_or_else(unreachable_eof),
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(done) = parse(&buf, false) {
                    return done;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(HttpError::Io(err)),
        }
    }
}

fn unreachable_eof() -> Result<Response, HttpError> {
    Err(HttpError::Protocol("connection closed before a response".to_string()))
}

/// Parse `buf` as a response. Returns None while more bytes are needed; at
/// EOF it always returns Some.
fn parse(buf: &[u8], eof: bool) -> Option<Result<Response, HttpError>> {
    let Some(head_end) = find(buf, b"\r\n\r\n") else {
        return eof.then(unreachable_eof);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = match status_line
        .strip_prefix("HTTP/1.")
        .and_then(|rest| rest.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
    {
        Some(status) => status,
        None => {
            return Some(Err(HttpError::Protocol(format!(
                "unexpected status line {status_line:?}"
            ))))
        }
    };
    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse().ok();
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    let raw = &buf[head_end + 4..];
    let body = if chunked {
        match decode_chunked(raw) {
            Some(body) => body,
            None if eof => return Some(Err(HttpError::Protocol("truncated chunked body".to_string()))),
            None => return None,
        }
    } else if let Some(len) = content_length {
        if raw.len() >= len {
            raw[..len].to_vec()
        } else if eof {
            return Some(Err(HttpError::Protocol("truncated body".to_string())));
        } else {
            return None;
        }
    } else if eof || status == 204 || status == 304 {
        raw.to_vec()
    } else {
        return None;
    };
    Some(Ok(Response {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    }))
}

/// Decode a complete chunked body; None when the terminal chunk has not
/// arrived yet.
fn decode_chunked(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = find(raw, b"\r\n")?;
        let size_text = String::from_utf8_lossy(&raw[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if raw.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{closed_port, json_reply, FakeServer, Reply};

    fn endpoint(server: &FakeServer) -> Endpoint {
        Endpoint {
            port: server.port,
            token: server.token.clone(),
        }
    }

    #[test]
    fn posts_json_with_token_and_custom_headers() {
        let server = FakeServer::start("tok123", |req| {
            json_reply(serde_json::json!({ "echo": req.body }))
        });
        let response = post(
            &endpoint(&server),
            "/mcp",
            &[("X-Mnemo-Root", r"C:\proj"), ("X-Mnemo-Helper", "/bin/mnemo2-mcp")],
            "{\"a\":1}",
            Some(Duration::from_secs(5)),
        )
        .expect("response");
        assert_eq!(response.status, 200);
        assert!(response.body.contains("{\\\"a\\\":1}"), "{}", response.body);
        let seen = server.requests();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].path, "/mcp");
        assert_eq!(seen[0].headers["authorization"], "Bearer tok123");
        assert_eq!(seen[0].headers["x-mnemo-root"], r"C:\proj");
        assert_eq!(seen[0].headers["x-mnemo-helper"], "/bin/mnemo2-mcp");
        assert_eq!(seen[0].headers["content-type"], "application/json");
        assert_eq!(seen[0].headers["connection"], "close");
        assert_eq!(seen[0].body, "{\"a\":1}");
    }

    #[test]
    fn a_wrong_token_is_a_401_response_not_an_error() {
        let server = FakeServer::start("right", |_| json_reply(serde_json::json!({})));
        let wrong = Endpoint {
            port: server.port,
            token: "wrong".to_string(),
        };
        let response = post(&wrong, "/health", &[], "{}", Some(Duration::from_secs(5))).expect("response");
        assert_eq!(response.status, 401);
    }

    #[test]
    fn empty_202_body_is_returned_as_is() {
        let server = FakeServer::start("t", |_| Reply::Status(202, String::new()));
        let response = post(&endpoint(&server), "/mcp", &[], "{}", Some(Duration::from_secs(5)))
            .expect("response");
        assert_eq!(response.status, 202);
        assert!(response.body.is_empty());
    }

    #[test]
    fn decodes_chunked_bodies() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        let server = FakeServer::start("t", move |_| Reply::Raw(raw.to_vec()));
        let response = post(&endpoint(&server), "/x", &[], "{}", Some(Duration::from_secs(5)))
            .expect("response");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "Wikipedia");
    }

    #[test]
    fn reads_until_close_without_content_length() {
        let raw = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{\"ok\":true}";
        let server = FakeServer::start("t", move |_| Reply::Raw(raw.to_vec()));
        let response = post(&endpoint(&server), "/x", &[], "{}", Some(Duration::from_secs(5)))
            .expect("response");
        assert_eq!(response.body, "{\"ok\":true}");
    }

    #[test]
    fn a_refused_connection_is_a_connect_error() {
        let dead = Endpoint {
            port: closed_port(),
            token: "t".to_string(),
        };
        let err = post(&dead, "/health", &[], "{}", Some(Duration::from_secs(5))).expect_err("refused");
        assert!(err.is_connect(), "{err}");
        // Windows needs ~2 s to say so; the connect timeout must outlast that.
        assert!(err.is_refused(), "{err}");
        assert!(!err.is_timeout(), "{err}");
    }

    #[test]
    fn error_kinds_are_told_apart() {
        let connect = |kind| HttpError::Connect(io::Error::from(kind));
        let io_err = |kind| HttpError::Io(io::Error::from(kind));
        assert!(connect(io::ErrorKind::ConnectionRefused).is_refused());
        assert!(!connect(io::ErrorKind::TimedOut).is_refused());
        assert!(connect(io::ErrorKind::TimedOut).is_timeout());
        assert!(io_err(io::ErrorKind::TimedOut).is_timeout());
        assert!(io_err(io::ErrorKind::WouldBlock).is_timeout());
        assert!(!io_err(io::ErrorKind::ConnectionReset).is_timeout());
        assert!(!io_err(io::ErrorKind::ConnectionRefused).is_refused(), "only a failed connect counts");
        assert!(!HttpError::Protocol("x".into()).is_timeout());
    }

    #[test]
    fn a_slow_server_is_a_read_error_after_the_timeout() {
        let server = FakeServer::start("t", |_| {
            std::thread::sleep(Duration::from_millis(600));
            json_reply(serde_json::json!({}))
        });
        let err = post(&endpoint(&server), "/x", &[], "{}", Some(Duration::from_millis(100)))
            .expect_err("timeout");
        assert!(matches!(err, HttpError::Io(_)), "{err}");
        assert!(!err.is_connect());
    }

    #[test]
    fn garbage_is_a_protocol_error() {
        let server = FakeServer::start("t", |_| Reply::Raw(b"not http at all\r\n\r\n".to_vec()));
        let err = post(&endpoint(&server), "/x", &[], "{}", Some(Duration::from_secs(5)))
            .expect_err("protocol");
        assert!(matches!(err, HttpError::Protocol(_)), "{err}");
    }

    #[test]
    fn header_values_cannot_inject_lines() {
        let dead = Endpoint {
            port: closed_port(),
            token: "t".to_string(),
        };
        let err = post(&dead, "/x", &[("X-Mnemo-Root", "a\r\nEvil: 1")], "{}", None).expect_err("rejected");
        assert!(matches!(err, HttpError::Protocol(_)), "{err}");
    }
}
