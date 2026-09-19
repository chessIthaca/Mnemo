// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `web_fetch` — read-only HTTP fetch for research.
//!
//! Fetches an http/https URL, strips HTML to readable text, caps the output,
//! and returns the status + content-type + final URL + body. Redirects are
//! followed manually (up to 5 hops) with the SSRF internal-address gate
//! re-run on every hop, and the gate's validated address is PINNED to the
//! connection (`ClientBuilder::resolve`) — a rebinding DNS server cannot
//! swap the address between the check and the connect (the TOCTOU this
//! closes; SNI/TLS/Host still use the original hostname). Auto-run (a
//! read-only GET — no project mutation), so it is usable in every workflow
//! state including Planning/Complete for research (e.g. reading a library's
//! docs site before deciding whether to adopt it).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::cap_tool_output;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Default max body length (bytes) returned when the caller omits
/// `max_length`. Generous enough for a docs page, small enough to avoid
/// flooding the conversation with a multi-MB dump.
const DEFAULT_MAX_LENGTH: usize = 50_000;

/// Maximum redirect hops followed before erroring — parity with the old
/// `reqwest::redirect::Policy::limited(5)`.
const MAX_REDIRECTS: usize = 5;

/// The `web_fetch` tool — read-only HTTP GET + HTML-to-text.
pub struct WebFetchTool;

impl WebFetchTool {
    /// Construct the tool. Stateless — no shared deps.
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Arguments for `web_fetch`.
#[derive(Debug, Deserialize)]
struct WebFetchArgs {
    /// The http/https URL to fetch.
    url: String,
    /// Max bytes of body text to return (default 50000).
    #[serde(default)]
    max_length: Option<usize>,
}

/// Strip HTML to readable plain text: drop `<script>`/`<style>` blocks,
/// replace remaining tags with a space, collapse whitespace, trim.
///
/// Pure + synchronous — the only logic worth unit-testing (no network).
fn html_to_text(html: &str) -> String {
    // Drop script + style blocks entirely (case-insensitive, dotall).
    let script = regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let style = regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let no_script = script.replace_all(html, " ");
    let no_style = style.replace_all(&no_script, " ");

    // Replace every remaining tag with a space (so words separated only by a
    // tag boundary don't run together).
    let tag = regex::Regex::new(r"(?s)<[^>]+>").unwrap();
    let stripped = tag.replace_all(&no_style, " ");

    // Collapse runs of whitespace to a single space + trim.
    let ws = regex::Regex::new(r"\s+").unwrap();
    ws.replace_all(&stripped, " ").trim().to_string()
}

/// Cap `text` to `max_length` bytes, char-boundary-safe, appending a
/// truncation note when cut. Reuses the shared `truncate_to_boundary` helper
/// (a naive `text[..max_length]` slice panics if the cut lands inside a
/// multi-byte UTF-8 char — see the regression test
/// `cap_body_multibyte_no_panic`).
fn cap_body(text: String, max_length: usize) -> String {
    let mut out = text;
    if out.len() > max_length {
        crate::tool::agent::read_files::truncate_to_boundary(&mut out, max_length);
        out.push_str("\n... (truncated: body exceeded max_length)");
    }
    out
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "web_fetch",
            "Fetch an http/https URL and return the response body as readable text. \
             HTML is stripped to plain text (scripts/styles removed, tags replaced with \
             spaces); non-HTML bodies are returned verbatim. Only http and https URLs are \
             allowed (file://, javascript:, data:, and other schemes are rejected). \
             Read-only GET — usable in any workflow state for research (reading docs, \
             checking a library site). Output is capped to keep the conversation lean.",
            json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The http or https URL to fetch."
                    },
                    "max_length": {
                        "type": "integer",
                        "description": "Max bytes of body text to return (default 50000)."
                    }
                },
                "required": ["url"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: WebFetchArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };

        // Validate the scheme BEFORE any network call. Only http/https — this
        // blocks file:// (local file read), javascript:, data:, and anything
        // else that isn't a normal web fetch. Case-insensitive.
        let lower = args.url.trim().to_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return ToolResult::error(format!(
                "web_fetch only supports http and https URLs (got {:?}). \
                 file://, javascript:, data:, and other schemes are rejected.",
                args.url
            ));
        }

        let max_length = args.max_length.unwrap_or(DEFAULT_MAX_LENGTH);

        // Parse once — the redirect loop needs a `Url` to resolve relative
        // Locations against, and the SSRF gate works off the parsed host
        // (userinfo, bracketed IPv6, ports all handled by the parser).
        let url = match reqwest::Url::parse(args.url.trim()) {
            Ok(u) => u,
            Err(e) => return ToolResult::error(format!("fetch failed: invalid URL: {e}")),
        };

        // SSRF protection: reject internal/private/loopback addresses to
        // prevent prompt-injection-driven exfiltration of cloud metadata
        // (e.g. 169.254.169.254) or local services. The gate resolves the
        // hostname ONCE, checks every resolved address against internal IP
        // ranges, and PINS the connection to the validated address
        // (`ClientBuilder::resolve`) — the gate's answer IS the connection's
        // answer, so a rebinding DNS server cannot return a public IP to the
        // gate and an internal one to the connector (the TOCTOU this closes;
        // security review 2026-09-09 LOW-1). The loop below re-runs the gate
        // on EVERY redirect hop, so a public URL answering 302 → an internal
        // address cannot smuggle the fetch past the gate either.
        let (final_url, resp) =
            match fetch_following_redirects(url, Arc::new(resolve_and_gate_host)).await {
                Ok(r) => r,
                Err(msg) => return ToolResult::error(msg),
            };

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let final_url = final_url.to_string();

        let body = match resp.text().await {
            Ok(t) => t,
            Err(e) => return ToolResult::error(format!("failed to read body: {e}")),
        };

        // Strip HTML to text if the content-type looks like HTML; otherwise
        // return the body verbatim (JSON, plain text, CSV, ...).
        let text = if content_type.to_lowercase().contains("html") {
            html_to_text(&body)
        } else {
            body
        };

        // Cap to max_length (byte-based, char-boundary-safe via the shared
        // truncate_to_boundary helper — a naive `text[..max_length]` slice
        // panics if the cut lands inside a multi-byte char). Append a
        // truncation note when cut.
        let body_out = cap_body(text, max_length);

        let output = format!(
            "GET {url} → {status} ({content_type})\nFinal URL: {final_url}\n\n{body_out}",
            url = args.url,
            status = status,
            content_type = if content_type.is_empty() {
                "unknown"
            } else {
                &content_type
            },
            final_url = final_url,
            body_out = body_out,
        );

        // Second safety net: cap_tool_output (100 KiB) guards against a
        // max_length override that's enormous.
        ToolResult::success(cap_tool_output(output))
    }
}

/// The connection pin produced by the gate: the host to pin
/// (`ClientBuilder::resolve`) and the validated address to connect to.
/// `None` = the host is an IP literal (no DNS involved; the literal itself
/// was validated, and the URL already targets it directly).
type HostPin = Option<(String, std::net::SocketAddr)>;

/// The gate: resolve `host` (connecting on `port`), validate every resolved
/// address against the SSRF blocklist, and return the pin for the
/// connection. The real DNS-backed [`resolve_and_gate_host`] in production,
/// a stub in tests (the production gate blocks loopback, so a local test
/// server needs an exemption to be reachable at all).
type HostGate = std::sync::Arc<dyn Fn(&str, u16) -> Result<HostPin, String> + Send + Sync>;

/// The SSRF refusal for an internal/private/loopback address.
fn internal_addr_error() -> String {
    "web_fetch refuses internal/private/loopback addresses \
     (SSRF protection). The hostname resolved to an internal \
     IP range."
        .to_string()
}

/// Gate a URL for fetching AND produce the connection pin: the scheme must
/// be http/https and the host must not resolve to an internal IP (SSRF
/// protection). Runs the hostname check on the blocking thread pool and
/// FAILS CLOSED — if the check task itself fails (runtime shutting down,
/// cancelled), the URL is treated as internal and refused rather than
/// allowed through unchecked.
///
/// Returns the pin for the connection: the gate's validated address IS the
/// address reqwest connects to (via `ClientBuilder::resolve`), closing the
/// resolve-then-connect TOCTOU a rebinding DNS server could exploit.
async fn gate_and_pin_url(url: &reqwest::Url, gate: HostGate) -> Result<HostPin, String> {
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        return Err(format!(
            "web_fetch only supports http and https URLs (got {scheme:?}). \
             file://, javascript:, data:, and other schemes are rejected."
        ));
    }
    let Some(host) = extract_host(url.as_str()) else {
        return Ok(None);
    };
    let port = url.port_or_known_default().unwrap_or(80);
    tokio::task::spawn_blocking(move || gate(&host, port))
        .await
        // Fail-closed: if the spawn_blocking task fails (runtime
        // shutting down, cancelled), treat as internal and block the
        // request rather than allowing it through unchecked.
        .unwrap_or_else(|_| Err(internal_addr_error()))
}

/// Fetch `start` following redirects MANUALLY — up to [`MAX_REDIRECTS`]
/// hops — running the full SSRF gate ([`gate_and_pin_url`]) on EVERY URL
/// before requesting it, `start` included: the function is self-contained
/// (no caller-side gate precondition — a future caller or refactor cannot
/// silently reintroduce the bypass this fixed). Each hop's client is built
/// with the hop's validated address PINNED (`ClientBuilder::resolve`) — the
/// gate's answer IS the connection's answer, so a rebinding DNS server
/// cannot swap the address between the check and the connect; TLS SNI, cert
/// validation, and the Host header keep using the original hostname.
/// Redirects are NOT auto-followed — this loop is the only thing that
/// follows them, so no hop can bypass the gate. (Pre-fix behaviors:
/// reqwest's `Policy::limited(5)` auto-followed redirect targets unchecked,
/// letting a public URL 302 → an internal address through; and the
/// connection re-resolved the hostname after the gate, letting a rebinding
/// DNS server swap a public answer for an internal one.)
///
/// Parity with reqwest's auto-follow: only 301/302/303/307/308 with a
/// `Location` header are followed (a 3xx without one is returned as-is);
/// relative Locations resolve against the current URL; a Location that
/// cannot be joined (e.g. an invalid port) returns the 3xx response as-is
/// — "the original response is still valid"; exceeding the hop cap errors
/// with a clear message.
///
/// Returns the final URL (after all followed hops) and the response.
async fn fetch_following_redirects(
    start: reqwest::Url,
    gate: HostGate,
) -> Result<(reqwest::Url, reqwest::Response), String> {
    let mut current = start.clone();
    for hop in 0..=MAX_REDIRECTS {
        // Gate EVERY URL before requesting it — `start` included — so the
        // function is self-contained: no caller-side gate precondition that
        // a future refactor could silently drop (defense-in-depth; an
        // already-gated start just re-checks idempotently). The gate also
        // returns the validated address to PIN the connection to.
        let pin = gate_and_pin_url(&current, Arc::clone(&gate)).await?;
        let client = build_fetch_client(pin)?;
        let resp = client
            .get(current.clone())
            .send()
            .await
            .map_err(|e| format!("fetch failed: {e}"))?;
        let status = resp.status();
        // Only the statuses reqwest itself follows; anything else —
        // including a 3xx without a Location header — is the final response.
        if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            return Ok((current, resp));
        }
        let Some(location) = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
        else {
            return Ok((current, resp));
        };
        if hop == MAX_REDIRECTS {
            return Err(format!(
                "too many redirects (more than {MAX_REDIRECTS} hops) fetching {start}"
            ));
        }
        // Resolve the Location (absolute or relative) against the current
        // URL. A Location that cannot be joined (e.g. an invalid port or a
        // malformed IPv6 literal) is reqwest parity: the 3xx response is
        // returned as-is — "the original response is still valid" — rather
        // than erroring.
        let next = match current.join(location) {
            Ok(next) => next,
            Err(_) => return Ok((current, resp)),
        };
        current = next;
    }
    // The hop == MAX_REDIRECTS iteration always returns (final response,
    // no-Location or unjoinable-Location response, or the too-many-redirects
    // error) — the loop cannot fall through.
    unreachable!("redirect loop always returns on its final iteration")
}

/// Extract the hostname from an http/https URL string.
///
/// Uses `reqwest::Url::parse` (a re-export of `url::Url::parse`) so that
/// userinfo (`user:pass@host`), bracketed IPv6 literals (`[::1]`), ports,
/// paths, queries, and fragments are all handled correctly. The hand-rolled
/// parser this replaced stopped at the first `:` and mis-parsed both forms,
/// allowing SSRF bypasses.
///
/// Returns `None` if the URL is malformed or has no host.
fn extract_host(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.to_string())
}

/// Whether an IP address is internal (loopback, link-local, or private).
///
/// This is the SSRF blocklist: any address in these ranges is rejected by
/// `web_fetch` to prevent prompt-injection-driven exfiltration of cloud
/// metadata (e.g. `169.254.169.254`) or local services.
fn is_internal_ip(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_link_local() || v4.is_private() || v4.is_unspecified()
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // Link-local IPv6: fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // Unique-local IPv6: fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // IPv4-mapped IPv6 (::ffff:a.b.c.d): an attacker can embed
                // an internal IPv4 address in an IPv6 literal to bypass the
                // v4 checks above. `to_ipv4()` returns Some for exactly these
                // mapped addresses, so we recurse into the v4 check.
                || v6.to_ipv4().map(|v4| {
                    v4.is_loopback() || v4.is_link_local() || v4.is_private() || v4.is_unspecified()
                }).unwrap_or(false)
        }
    }
}

/// Build the per-hop fetch client: 30s timeout, no auto-follow (the manual
/// loop is the only redirect follower), a UA so sites don't 403 a bare
/// reqwest default, and — when the gate produced one — the hop's validated
/// address PINNED for its host (`ClientBuilder::resolve`): reqwest connects
/// to that exact address while TLS SNI/cert validation and the Host header
/// keep using the hostname. rustls-tls is already enabled on reqwest in
/// Cargo.toml — no native-tls / OpenSSL dependency.
fn build_fetch_client(pin: HostPin) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("mnemo-agent/0.1");
    if let Some((host, addr)) = pin {
        builder = builder.resolve(&host, addr);
    }
    builder
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))
}

/// Resolve `host` and gate every resolved address against the SSRF
/// blocklist, returning the pin for the connection (see [`HostPin`]).
///
/// This is the DNS-rebinding TOCTOU fix (security review 2026-09-09 LOW-1):
/// the address validated here is the address the connection uses — reqwest
/// does not re-resolve. Fail-closed on resolution errors: previously a
/// resolution failure passed the gate (the request then failed naturally at
/// connect), but an attacker could serve SERVFAIL to the gate and a private
/// address to the connector; now the fetch is refused instead.
fn resolve_and_gate_host(host: &str, port: u16) -> Result<HostPin, String> {
    use std::net::ToSocketAddrs;
    // IP literal (bracketed IPv6 included — `extract_host` keeps the
    // brackets): no DNS involved — validate the literal itself; the URL
    // already targets it directly, so there is nothing to pin.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        return if is_internal_ip(&ip) {
            Err(internal_addr_error())
        } else {
            Ok(None)
        };
    }
    // Hostname: resolve ONCE, validate EVERY address, pin the first public
    // one. Any internal address rejects the whole host (fail-closed — the
    // connector never sees the addresses at all).
    let addrs = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| format!("DNS resolution failed for {host}: {e} (fail-closed SSRF gate)"))?;
    let mut first_public: Option<std::net::SocketAddr> = None;
    for addr in addrs {
        if is_internal_ip(&addr.ip()) {
            return Err(internal_addr_error());
        }
        if first_public.is_none() {
            first_public = Some(addr);
        }
    }
    first_public
        .map(|addr| Ok(Some((host.to_string(), addr))))
        .unwrap_or_else(|| {
            Err(format!(
                "DNS resolution returned no addresses for {host} (fail-closed SSRF gate)"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HTML stripping removes tags, scripts, and styles; keeps visible text.
    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        let html = "<html><head><style>body{color:red}</style>\
                    <script>alert(1)</script></head>\
                    <body><h1>Title</h1><p>Hello &amp; world</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"), "kept visible heading text");
        assert!(text.contains("Hello"), "kept paragraph text");
        assert!(text.contains("world"), "kept entity-decoded text");
        assert!(!text.contains("<script>"), "script tag removed");
        assert!(!text.contains("<style>"), "style tag removed");
        assert!(!text.contains("alert"), "script body removed");
        assert!(!text.contains("color:red"), "style body removed");
    }

    /// Whitespace (including newlines + runs from tag replacement) collapses
    /// to single spaces.
    #[test]
    fn html_to_text_collapses_whitespace() {
        let html = "<p>a\n\n  b</p>";
        let text = html_to_text(html);
        assert_eq!(text, "a b");
    }

    /// Non-http schemes are rejected before any network call. This is the
    /// SSRF / local-file-read guard.
    #[tokio::test]
    async fn validate_rejects_non_http() {
        let tool = WebFetchTool::new();
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
        ] {
            let result = tool.execute(json!({ "url": bad })).await;
            assert!(!result.success, "rejected {bad}");
            assert!(
                result.output.contains("http"),
                "error mentions http for {bad}: {}",
                result.output
            );
        }
    }

    /// Regression: capping a multi-byte (CJK) body must NOT panic. A naive
    /// `text[..max_length]` byte slice panics when the cut lands inside a
    /// 3-byte char — this test builds such a string and asserts the
    /// char-boundary-safe `cap_body` returns success + a truncation note.
    #[test]
    fn cap_body_multibyte_no_panic() {
        // 3-byte CJK char repeated enough times to exceed the cap in bytes.
        let text = "漢".repeat(20_000); // 60_000 bytes
        let capped = cap_body(text, 50_000);
        assert!(capped.len() <= 50_000 + 64, "capped near the limit");
        assert!(
            capped.contains("(truncated: body exceeded max_length)"),
            "truncation note appended"
        );
        // Must be valid UTF-8 (no panic, no mid-char slice).
        assert!(
            std::str::from_utf8(capped.as_bytes()).is_ok(),
            "valid UTF-8"
        );
    }

    /// `cap_body` leaves short text untouched (no truncation note).
    #[test]
    fn cap_body_short_text_untouched() {
        let text = "hello world".to_string();
        let capped = cap_body(text.clone(), 50_000);
        assert_eq!(capped, text, "short text unchanged");
        assert!(!capped.contains("truncated"), "no truncation note");
    }

    #[test]
    fn test_is_internal_ip_rejects_loopback() {
        assert!(is_internal_ip(&"127.0.0.1".parse().unwrap()));
        assert!(is_internal_ip(&"127.255.255.255".parse().unwrap()));
        assert!(is_internal_ip(&"::1".parse().unwrap()));
    }

    #[test]
    fn test_is_internal_ip_rejects_private() {
        assert!(is_internal_ip(&"10.0.0.1".parse().unwrap()));
        assert!(is_internal_ip(&"10.255.255.255".parse().unwrap()));
        assert!(is_internal_ip(&"172.16.0.1".parse().unwrap()));
        assert!(is_internal_ip(&"172.31.255.255".parse().unwrap()));
        assert!(is_internal_ip(&"192.168.1.1".parse().unwrap()));
        assert!(is_internal_ip(&"192.168.0.0".parse().unwrap()));
    }

    #[test]
    fn test_is_internal_ip_rejects_link_local() {
        assert!(is_internal_ip(&"169.254.169.254".parse().unwrap()));
        assert!(is_internal_ip(&"169.254.0.1".parse().unwrap()));
    }

    #[test]
    fn test_is_internal_ip_allows_public() {
        assert!(!is_internal_ip(&"8.8.8.8".parse().unwrap()));
        assert!(!is_internal_ip(&"1.1.1.1".parse().unwrap()));
        assert!(!is_internal_ip(&"172.32.0.1".parse().unwrap()));
        assert!(!is_internal_ip(&"11.0.0.1".parse().unwrap()));
        assert!(!is_internal_ip(&"192.169.1.1".parse().unwrap()));
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(
            extract_host("https://example.com/path"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_host("http://example.com:8080/path"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_host("https://example.com"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_host("https://example.com?q=1"),
            Some("example.com".to_string())
        );
        assert_eq!(
            extract_host("https://example.com#frag"),
            Some("example.com".to_string())
        );
        assert_eq!(extract_host("not a url"), None);
    }

    #[test]
    fn test_extract_host_userinfo_bypass() {
        // Userinfo must not fool the host extraction — the host is 127.0.0.1,
        // not "user:pass@127.0.0.1" (the old hand-rolled parser would have
        // returned the whole string up to the first `:`).
        assert_eq!(
            extract_host("https://user:pass@127.0.0.1/path"),
            Some("127.0.0.1".to_string())
        );
    }

    #[test]
    fn test_extract_host_ipv6_bracket() {
        // Bracketed IPv6 literals must be parsed correctly.
        assert_eq!(extract_host("http://[::1]/path"), Some("[::1]".to_string()));
        assert_eq!(
            extract_host("http://[fe80::1]:8080/path"),
            Some("[fe80::1]".to_string())
        );
    }

    #[test]
    fn test_is_internal_ip_rejects_ipv4_mapped_ipv6() {
        // IPv4-mapped IPv6 (::ffff:a.b.c.d) must be detected as internal
        // when the embedded v4 address is internal.
        assert!(is_internal_ip(&"::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_internal_ip(&"::ffff:10.0.0.1".parse().unwrap()));
        assert!(is_internal_ip(&"::ffff:169.254.169.254".parse().unwrap()));
        assert!(is_internal_ip(&"::ffff:192.168.1.1".parse().unwrap()));
        // A public v4 mapped into v6 should NOT be internal.
        assert!(!is_internal_ip(&"::ffff:8.8.8.8".parse().unwrap()));
    }

    // ---- Redirect handling: SSRF re-validation on every hop ----

    /// A canned response served by [`RedirectStubServer`].
    #[derive(Clone, Copy)]
    struct StubResponse {
        status: u16,
        /// Value of the `location` header, if any.
        location: Option<&'static str>,
        body: &'static str,
    }

    /// A minimal HTTP/1.1 server for the redirect tests: serves one canned
    /// response per connection (in order) and records each request's
    /// request-line, so tests can assert exactly how many requests were made
    /// before the tool refused (or followed) a redirect. Modeled on the
    /// `StubServer` in `src/provider/openai/tests.rs` — one response per
    /// connection; `connection: close` forces a fresh connection per request,
    /// so the accept loop stays trivial.
    struct RedirectStubServer {
        addr: std::net::SocketAddr,
        /// Request-lines received, in request order.
        requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl RedirectStubServer {
        /// Start the server. `responses[i]` is served for the i-th request;
        /// the server task ends after the last one (further connections are
        /// refused — a client that over-fetches fails fast instead of
        /// hanging).
        async fn start(responses: Vec<StubResponse>) -> Self {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind test server");
            let addr = listener.local_addr().expect("test server local addr");
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let requests_task = std::sync::Arc::clone(&requests);
            tokio::spawn(async move {
                for response in responses {
                    let (mut socket, _peer) = listener.accept().await.expect("accept");
                    // Read the request head (up to the \r\n\r\n terminator).
                    // web_fetch only sends GETs — no request body to drain.
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = socket.read(&mut chunk).await.expect("read head");
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let head_end = buf
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| p + 4)
                        .expect("request head terminator");
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                    let request_line = head.lines().next().unwrap_or_default().to_string();
                    requests_task
                        .lock()
                        .expect("requests lock")
                        .push(request_line);

                    let reason = match response.status {
                        200 => "OK",
                        301 => "Moved Permanently",
                        302 => "Found",
                        303 => "See Other",
                        307 => "Temporary Redirect",
                        308 => "Permanent Redirect",
                        _ => "Redirect",
                    };
                    let location_header = response
                        .location
                        .map(|l| format!("location: {l}\r\n"))
                        .unwrap_or_default();
                    let head = format!(
                        "HTTP/1.1 {} {}\r\n{location_header}content-length: {}\r\nconnection: close\r\n\r\n",
                        response.status,
                        reason,
                        response.body.len(),
                    );
                    socket.write_all(head.as_bytes()).await.expect("write head");
                    socket
                        .write_all(response.body.as_bytes())
                        .await
                        .expect("write body");
                    socket.shutdown().await.expect("shutdown");
                }
            });
            Self { addr, requests }
        }

        /// The request-lines received so far, in request order.
        fn received(&self) -> Vec<String> {
            self.requests.lock().expect("requests lock").clone()
        }

        /// The server's base URL (`http://127.0.0.1:{port}`).
        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }
    }

    /// The production hostname gate with the mock server's loopback host
    /// exempted: tests can reach the stub while every other internal
    /// address (the redirect targets) is blocked exactly as in production.
    fn loopback_exempt_gate(host: &str, port: u16) -> Result<HostPin, String> {
        if host == "127.0.0.1" {
            Ok(None)
        } else {
            resolve_and_gate_host(host, port)
        }
    }

    /// Regression (SSRF redirect bypass): a redirect whose Location points at
    /// an internal address — here the cloud-metadata IP 169.254.169.254 —
    /// must be refused with the same fail-closed SSRF error as the initial
    /// URL check, never followed. Before the fix, reqwest's
    /// `Policy::limited(5)` auto-followed redirect targets without re-running
    /// the blocklist.
    #[tokio::test]
    async fn redirect_to_internal_address_is_refused() {
        let server = RedirectStubServer::start(vec![StubResponse {
            status: 302,
            location: Some("http://169.254.169.254/latest/meta-data"),
            body: "",
        }])
        .await;
        let start = reqwest::Url::parse(&server.url()).expect("parse start url");
        let msg = match fetch_following_redirects(start, Arc::new(loopback_exempt_gate)).await {
            Err(msg) => msg,
            Ok((url, resp)) => panic!(
                "redirect to an internal address must be refused, got {url} → {}",
                resp.status()
            ),
        };
        assert!(
            msg.contains("SSRF") && msg.contains("internal"),
            "refusal carries the SSRF error: {msg}"
        );
        // Exactly one request — to the mock. The internal target is never
        // contacted: the gate refuses before the hop is requested.
        assert_eq!(server.received().len(), 1, "one request to the mock");
    }

    /// A redirect whose Location switches scheme (file://) must be refused —
    /// the per-hop gate re-runs the scheme check, not just the host check.
    #[tokio::test]
    async fn redirect_to_non_http_scheme_is_refused() {
        let server = RedirectStubServer::start(vec![StubResponse {
            status: 302,
            location: Some("file:///etc/passwd"),
            body: "",
        }])
        .await;
        let start = reqwest::Url::parse(&server.url()).expect("parse start url");
        let msg = match fetch_following_redirects(start, Arc::new(loopback_exempt_gate)).await {
            Err(msg) => msg,
            Ok((url, resp)) => panic!(
                "redirect to file:// must be refused, got {url} → {}",
                resp.status()
            ),
        };
        assert!(
            msg.contains("http") && msg.contains("https"),
            "refusal mentions the scheme restriction: {msg}"
        );
    }

    /// A redirect whose Location cannot be joined (invalid port) returns the
    /// 3xx response as-is — reqwest parity ("the original response is still
    /// valid"), not an error.
    #[tokio::test]
    async fn unjoinable_redirect_location_returns_response_as_is() {
        let server = RedirectStubServer::start(vec![StubResponse {
            status: 302,
            // Port 70000 is outside the u16 range — `Url::join` fails.
            location: Some("http://127.0.0.1:70000/"),
            body: "",
        }])
        .await;
        let start = reqwest::Url::parse(&server.url()).expect("parse start url");
        let (final_url, resp) =
            fetch_following_redirects(start, Arc::new(|_: &str, _: u16| Ok(None)))
                .await
                .expect("an unjoinable Location is not an error");
        assert_eq!(resp.status(), 302);
        assert_eq!(final_url.as_str(), format!("{}/", server.url()));
        assert_eq!(server.received().len(), 1, "no follow attempted");
    }

    /// More than MAX_REDIRECTS hops → a clear too-many-redirects error, with
    /// exactly initial + 5 follow-up requests (parity with the old
    /// `Policy::limited(5)`).
    #[tokio::test]
    async fn too_many_redirects_errors() {
        let redirect = StubResponse {
            status: 302,
            // Relative Location — resolves back to the mock itself.
            location: Some("/loop"),
            body: "",
        };
        let server = RedirectStubServer::start(vec![redirect; 6]).await;
        let start = reqwest::Url::parse(&server.url()).expect("parse start url");
        let msg = match fetch_following_redirects(start, Arc::new(|_: &str, _: u16| Ok(None))).await
        {
            Err(msg) => msg,
            Ok((url, resp)) => panic!(
                "a redirect loop must error, got {url} → {}",
                resp.status()
            ),
        };
        assert!(
            msg.contains("too many redirects"),
            "clear too-many-redirects error: {msg}"
        );
        assert_eq!(
            server.received().len(),
            6,
            "initial request + 5 followed hops"
        );
    }

    /// A redirect to an allowed URL is followed and the final URL + body are
    /// returned — legitimate redirects keep working.
    #[tokio::test]
    async fn redirect_to_public_address_succeeds() {
        let server = RedirectStubServer::start(vec![
            StubResponse {
                status: 302,
                location: Some("/final"),
                body: "",
            },
            StubResponse {
                status: 200,
                location: None,
                body: "hello",
            },
        ])
        .await;
        let start = reqwest::Url::parse(&server.url()).expect("parse start url");
        let (final_url, resp) = fetch_following_redirects(start, Arc::new(loopback_exempt_gate))
            .await
            .expect("public redirect is followed");
        assert_eq!(resp.status(), 200);
        assert_eq!(final_url.as_str(), format!("{}/final", server.url()));
        assert_eq!(server.received().len(), 2, "initial + one follow");
        let body = resp.text().await.expect("read body");
        assert_eq!(body, "hello");
    }

    /// Regression (security review 2026-09-09 LOW-1, DNS-rebinding TOCTOU):
    /// the gate's validated address must be the address the connection uses.
    /// A host the system resolver cannot answer (the RFC 2606-reserved
    /// `.invalid` TLD, trailing dot so no search-suffix rewriting applies)
    /// but that the gate pins to a validated address must be fetchable —
    /// before the fix, the connection re-resolved the hostname (reqwest's
    /// connector), so the fetch failed with a DNS error and a rebinding
    /// server could swap the address between the gate's check and the
    /// connect.
    #[tokio::test]
    async fn connection_uses_the_gate_validated_address() {
        let server = RedirectStubServer::start(vec![StubResponse {
            status: 200,
            location: None,
            body: "pinned",
        }])
        .await;
        let url = format!("http://rebind.invalid.:{}", server.addr.port());
        let start = reqwest::Url::parse(&url).expect("parse rebind url");
        // The gate pins the (unresolvable) host to the validated test-server
        // address: the connection MUST use that address — reqwest never
        // re-resolves, so a rebinding DNS server's second answer is
        // irrelevant. Before the fix the fetch failed with a DNS error.
        let server_addr = server.addr;
        let (final_url, resp) = fetch_following_redirects(
            start,
            Arc::new(move |host: &str, port: u16| {
                assert_eq!(host, "rebind.invalid.", "the gated host");
                assert_eq!(port, server_addr.port(), "the gated port");
                Ok(Some((host.to_string(), server_addr)))
            }),
        )
        .await
        .expect("the pinned connection succeeds");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            final_url.as_str(),
            format!("http://rebind.invalid.:{}/", server.addr.port())
        );
        let body = resp.text().await.expect("read body");
        assert_eq!(body, "pinned");
    }

    /// The per-hop gate itself: internal IP literals are refused, public
    /// literals pass with no pin (the URL already targets the IP), and
    /// non-http schemes are refused — all DNS-free (IP literals resolve
    /// without a lookup, so the test is deterministic).
    #[tokio::test]
    async fn gate_and_pin_url_gates_ips_and_schemes() {
        let internal = reqwest::Url::parse("http://169.254.169.254/x").unwrap();
        let err = gate_and_pin_url(&internal, Arc::new(resolve_and_gate_host))
            .await
            .unwrap_err();
        assert!(err.contains("SSRF"), "SSRF message: {err}");

        let public = reqwest::Url::parse("http://8.8.8.8/x").unwrap();
        let pin = gate_and_pin_url(&public, Arc::new(resolve_and_gate_host))
            .await
            .expect("public IP literal passes");
        assert_eq!(pin, None, "an IP literal needs no pin");

        let file = reqwest::Url::parse("file:///etc/passwd").unwrap();
        assert!(
            gate_and_pin_url(&file, Arc::new(resolve_and_gate_host))
                .await
                .is_err()
        );

        let https = reqwest::Url::parse("https://1.1.1.1/x").unwrap();
        assert!(
            gate_and_pin_url(&https, Arc::new(resolve_and_gate_host))
                .await
                .is_ok()
        );
    }

    /// The production gate: internal IP literals are refused without any
    /// DNS lookup (the literal is validated directly), public literals pass
    /// with no pin, and an unresolvable hostname is refused fail-closed —
    /// previously fail-open (the request then failed at connect), which let
    /// an attacker serve SERVFAIL to the gate and a private address to the
    /// connector.
    #[test]
    fn resolve_and_gate_host_validates_literals_and_fails_closed() {
        // Internal literals — refused without any DNS lookup.
        for host in [
            "127.0.0.1",
            "[::1]",
            "10.0.0.1",
            "192.168.1.1",
            "169.254.169.254",
        ] {
            let res = resolve_and_gate_host(host, 80);
            let err = res.expect_err("internal literal must be refused");
            assert!(err.contains("SSRF"), "SSRF message: {err}");
        }
        // Public literals — pass, nothing to pin (the URL targets the IP).
        for host in ["93.184.216.34", "[2606:2800:220:1:248:1893:25c8:1946]"] {
            let res = resolve_and_gate_host(host, 443);
            assert_eq!(res.expect("public literal passes"), None, "no pin");
        }
        // Unresolvable hostname — fail-closed (the .invalid TLD is reserved;
        // the trailing dot prevents search-suffix rewriting).
        let err = resolve_and_gate_host("nonexistent.invalid.", 80)
            .expect_err("unresolvable host must be refused");
        assert!(err.contains("fail-closed"), "{err}");
    }
}
