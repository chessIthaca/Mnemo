// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! OAuth 2.1 authorization-code flow for remote MCP servers.
//!
//! Flow (Settings → MCP → Connect): discover the authorization-server
//! metadata (RFC 8414 well-known), open a loopback redirect listener,
//! return the authorization URL to the frontend (opened in the user's
//! browser), wait for the `?code=` redirect (state-validated), exchange
//! the code (PKCE S256 verifier) at the token endpoint, and store the
//! tokens in `keys.toml` (user-only permissions, same store as API keys —
//! each server's tokens serialize as a JSON blob under a sanitized
//! `mcp-<name>` key). Remote requests then send `Authorization: Bearer`
//! and refresh automatically on 401.
//!
//! Limitations (deliberate): no dynamic client registration — `client_id`
//! must be pre-registered with the server; the network legs (discovery,
//! browser round-trip, exchange) are exercised manually via Settings, the
//! pure parts (PKCE, URL building, request bodies, sanitization) are
//! unit-tested here.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::KeyStore;
use crate::error::{Error, Result};

use super::{HANDSHAKE_TIMEOUT, REQUEST_TIMEOUT};

/// A `reqwest` client for the OAuth legs (discovery + token exchange):
/// connect-bounded at the handshake timeout, overall at the request
/// timeout — the same posture as the transport itself.
pub fn oauth_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(HANDSHAKE_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("reqwest client builds with standard options")
}

/// A fresh unguessable `state` parameter (UUIDv4 simple form).
pub fn new_state() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Sanitize a server name into a keys.toml key: non-alphanumerics → '-'
/// (TOML table keys must survive round-trips; dots/dashes in raw names
/// would nest or break).
pub fn token_key(server: &str) -> String {
    let clean: String = server
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("mcp-{clean}")
}

/// Stored OAuth tokens — serialized as a JSON blob into keys.toml (the
/// KeyStore value is opaque to it).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Epoch seconds when the access token expires. Servers that omit
    /// `expires_in` leave this unset — the token is treated as
    /// non-expiring and the 401-refresh path still covers revocation.
    #[serde(default)]
    pub expires_at_epoch: Option<u64>,
    /// The server origin (scheme://host[:port]) the tokens were issued
    /// for. Loaders verify it matches the CURRENT server url, so
    /// re-pointing a server's url never sends old tokens to a new host
    /// (review LOW 3). Legacy blobs without it are treated as absent —
    /// a one-time re-authorization after the upgrade.
    #[serde(default)]
    pub origin: Option<String>,
}

/// The scheme://host[:port] origin of a server URL (pure — unit-tested).
pub fn origin_of(server_url: &str) -> Result<String> {
    let url =
        reqwest::Url::parse(server_url).map_err(|e| Error::Mcp(format!("bad server url: {e}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::Mcp("server url has no host".into()))?;
    Ok(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// The token payload returned by the token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

impl OAuthTokens {
    /// Convert a token-endpoint response into stored tokens, resolving
    /// `expires_in` against now. The caller stamps `origin` (the MCP
    /// server's origin — the token endpoint may differ).
    pub fn from_response(resp: TokenResponse) -> Self {
        let expires_at_epoch = resp.expires_in.map(|secs| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                + secs
        });
        Self {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_at_epoch,
            origin: None,
        }
    }
}

/// Generate a PKCE pair (RFC 7636): a 64-char base64url verifier and its
/// base64url(SHA-256) S256 challenge.
pub fn pkce() -> (String, String) {
    // 48 random bytes from UUIDv4 draws (122 random bits each) — well
    // beyond the RFC 7636 43..=128-char verifier floor once base64url
    // encoded.
    let mut bytes = [0u8; 48];
    for chunk in bytes.chunks_mut(16) {
        chunk.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// Percent-encode a query component (RFC 3986 unreserved set kept
/// literal, everything else escaped). Pure — unit-tested.
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// The subset of RFC 8414 authorization-server metadata the client needs.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthServerMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
}

/// Discover the authorization + token endpoints for a remote server URL
/// (RFC 8414: `/.well-known/oauth-authorization-server` inserted after
/// the host, before the path).
pub async fn discover(http: &reqwest::Client, server_url: &str) -> Result<AuthServerMetadata> {
    let origin = origin_of(server_url)?;
    let path = reqwest::Url::parse(server_url)
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let well_known = match path.as_str() {
        "/" | "" => format!("{origin}/.well-known/oauth-authorization-server"),
        p => format!("{origin}/.well-known/oauth-authorization-server{p}"),
    };
    let resp = http
        .get(well_known)
        .send()
        .await
        .map_err(|e| Error::Mcp(format!("OAuth discovery failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Mcp(format!(
            "OAuth discovery returned {}",
            resp.status()
        )));
    }
    resp.json::<AuthServerMetadata>()
        .await
        .map_err(|e| Error::Mcp(format!("bad discovery metadata: {e}")))
}

/// Percent-decode a query component (`%XX` sequences; a stray or
/// truncated `%` stays literal). `+` is NOT treated as a space — a
/// literal `+` in a code arrives percent-encoded as `%2B` (review LOW 4:
/// decode before re-encoding into the token request, else `%2B` becomes
/// `%252B` and the exchange fails).
pub fn urldecode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the authorization URL the user's browser opens (pure —
/// unit-tested): response_type=code, PKCE S256 challenge, state, optional
/// scope — everything else percent-encoded.
pub fn authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    code_challenge: &str,
    scopes: &[String],
) -> String {
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&\
         code_challenge_method=S256",
        authorization_endpoint,
        urlencode(client_id),
        urlencode(redirect_uri),
        urlencode(state),
        urlencode(code_challenge),
    );
    if !scopes.is_empty() {
        url.push_str(&format!("&scope={}", urlencode(&scopes.join(" "))));
    }
    url
}

/// The x-www-form-urlencoded body for the authorization-code exchange
/// (pure — unit-tested).
pub fn build_token_request(
    code: &str,
    redirect_uri: &str,
    client_id: &str,
    code_verifier: &str,
) -> String {
    format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencode(code),
        urlencode(redirect_uri),
        urlencode(client_id),
        urlencode(code_verifier),
    )
}

/// The x-www-form-urlencoded body for a refresh-token grant (pure).
pub fn build_refresh_request(refresh_token: &str, client_id: &str) -> String {
    format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencode(refresh_token),
        urlencode(client_id),
    )
}

/// POST a token-endpoint body and parse the response into stored tokens.
pub async fn token_request(
    http: &reqwest::Client,
    token_endpoint: &str,
    body: String,
) -> Result<OAuthTokens> {
    let resp = http
        .post(token_endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| Error::Mcp(format!("token request failed: {e}")))?;
    if !resp.status().is_success() {
        return Err(Error::Mcp(format!(
            "token endpoint returned {}",
            resp.status()
        )));
    }
    let parsed: TokenResponse = resp
        .json()
        .await
        .map_err(|e| Error::Mcp(format!("bad token response: {e}")))?;
    Ok(OAuthTokens::from_response(parsed))
}

/// Wait for the OAuth redirect on a loopback listener: accept ONE
/// connection, read the GET line, validate `state`, extract `code`,
/// respond with a small user-facing page, and resolve with the code.
/// State mismatch / missing code is an error (the page says so too).
pub async fn wait_for_code(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| Error::Mcp(format!("redirect listener failed: {e}")))?;
    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| Error::Mcp(format!("redirect read failed: {e}")))?;
    let request_line = String::from_utf8_lossy(&buf[..n]).to_string();
    // "GET /callback?code=...&state=... HTTP/1.1" — the query sits between
    // the first space and the second.
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let query = target.split('?').nth(1).unwrap_or("");
    let mut code: Option<String> = None;
    let mut state_ok = false;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            // LOW 4: percent-decode before use — a code containing
            // reserved characters (e.g. `+` as `%2B`) would otherwise be
            // double-encoded into the token exchange.
            "code" => code = Some(urldecode(v)),
            "state" => state_ok = urldecode(v) == expected_state,
            _ => {}
        }
    }
    let body = if state_ok && code.is_some() {
        "<html><body><h2>Authorized.</h2><p>You can close this tab and return to Mnemo.</p>\
         </body></html>"
    } else {
        "<html><body><h2>Authorization failed</h2><p>State mismatch or missing code — start \
         the flow again from Mnemo.</p></body></html>"
    };
    let _ = stream
        .write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
    let _ = stream.flush().await;
    if !state_ok {
        return Err(Error::Mcp("OAuth state mismatch — aborting".into()));
    }
    code.ok_or_else(|| Error::Mcp("no code in the OAuth redirect".into()))
}

/// Token persistence backed by `keys.toml` (user-only permissions, the
/// same store as API keys): each server's [`OAuthTokens`] serialize as a
/// JSON blob under a sanitized `mcp-<name>` key.
pub struct McpTokenStore {
    keys_path: std::path::PathBuf,
}

impl McpTokenStore {
    /// A store over the given keys.toml path.
    pub fn new(keys_path: std::path::PathBuf) -> Self {
        Self { keys_path }
    }

    /// Load the stored tokens for `server` (None when absent; a corrupt
    /// blob is treated as absent — the user simply re-authorizes).
    pub fn load(&self, server: &str) -> Result<Option<OAuthTokens>> {
        let store = KeyStore::load_or_default(&self.keys_path)?;
        Ok(store
            .get(&token_key(server))
            .and_then(|blob| serde_json::from_str(blob).ok()))
    }

    /// Persist the tokens for `server` (insert + user-only atomic save).
    pub fn store(&self, server: &str, tokens: &OAuthTokens) -> Result<()> {
        let mut store = KeyStore::load_or_default(&self.keys_path)?;
        let blob = serde_json::to_string(tokens)?;
        store.insert(token_key(server), blob);
        store.save(&self.keys_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pair_is_sha256_of_the_verifier() {
        let (verifier, challenge) = pkce();
        // RFC 7636 verifier constraints: 43..=128 chars from the unreserved
        // set (base64url is exactly that minus padding).
        assert!(
            (43..=128).contains(&verifier.len()),
            "verifier length {}",
            verifier.len()
        );
        assert!(!verifier.contains('='), "no padding");
        assert!(!verifier.contains('+') && !verifier.contains('/'));
        // The challenge is base64url(SHA-256(verifier)).
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
        // Fresh pairs differ (randomness).
        let (v2, c2) = pkce();
        assert_ne!(v2, verifier);
        assert_ne!(c2, challenge);
    }

    #[test]
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("abc-_~123"), "abc-_~123");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("ä"), "%C3%A4");
    }

    #[test]
    fn authorize_url_carries_the_required_params() {
        let url = authorize_url(
            "https://auth.example.com/authorize",
            "my-client",
            "http://127.0.0.1:53111/callback",
            "state123",
            "challenge456",
            &["read".to_string(), "write".to_string()],
        );
        assert!(
            url.starts_with("https://auth.example.com/authorize?"),
            "{url}"
        );
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=my-client"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A53111%2Fcallback"));
        assert!(url.contains("state=state123"));
        assert!(url.contains("code_challenge=challenge456"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope=read%20write"));
    }

    #[test]
    fn token_request_bodies_carry_the_grant_fields() {
        let code = build_token_request("the-code", "http://127.0.0.1:9/cb", "cid", "the-verifier");
        assert_eq!(
            code,
            "grant_type=authorization_code&code=the-code&redirect_uri=http%3A%2F%2F127.0.0.1%3A9%2Fcb&client_id=cid&code_verifier=the-verifier"
        );
        let refresh = build_refresh_request("r-tok", "cid");
        assert_eq!(
            refresh,
            "grant_type=refresh_token&refresh_token=r-tok&client_id=cid"
        );
    }

    #[test]
    fn token_key_sanitizes_and_prefixes() {
        assert_eq!(token_key("fs"), "mcp-fs");
        assert_eq!(token_key("my server.dev/x"), "mcp-my-server-dev-x");
    }

    #[test]
    fn urldecode_expands_percent_escapes() {
        assert_eq!(urldecode("plain"), "plain");
        assert_eq!(urldecode("%2B"), "+");
        assert_eq!(urldecode("a%2Bb"), "a+b");
        assert_eq!(urldecode("%C3%A4"), "ä");
        // Truncated / invalid escapes stay literal.
        assert_eq!(urldecode("%2"), "%2");
        assert_eq!(urldecode("%zz"), "%zz");
    }

    #[test]
    fn origin_of_extracts_scheme_host_port() {
        assert_eq!(
            origin_of("https://mcp.example.com/mcp").unwrap(),
            "https://mcp.example.com"
        );
        assert_eq!(
            origin_of("https://mcp.example.com:8443/mcp").unwrap(),
            "https://mcp.example.com:8443"
        );
        assert!(origin_of("not a url").is_err());
    }

    #[test]
    fn token_store_round_trips_through_keys_toml() {
        let dir = tempfile::tempdir().unwrap();
        let store = McpTokenStore::new(dir.path().join("keys.toml"));
        assert!(store.load("fs").unwrap().is_none(), "absent by default");
        let tokens = OAuthTokens {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at_epoch: Some(1_800_000_000),
            origin: Some("https://mcp.example.com".into()),
        };
        store.store("fs", &tokens).unwrap();
        let loaded = store.load("fs").unwrap().expect("round-trips");
        assert_eq!(loaded.access_token, "at");
        assert_eq!(loaded.refresh_token.as_deref(), Some("rt"));
        assert_eq!(loaded.origin.as_deref(), Some("https://mcp.example.com"));
        // A corrupt blob degrades to absent (re-authorize), not an error:
        // clobber the stored JSON value so serde_json fails to parse it.
        let keys_path = dir.path().join("keys.toml");
        let text = std::fs::read_to_string(&keys_path).unwrap();
        let corrupted: String = text
            .lines()
            .map(|l| {
                if l.starts_with("api_key") {
                    "api_key = \"{{{definitely not json\"".to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&keys_path, corrupted).unwrap();
        assert!(store.load("fs").unwrap().is_none());
    }

    #[tokio::test]
    async fn wait_for_code_parses_and_validates_state() {
        // Drive the loopback handler directly: bind, connect from a client
        // task with a crafted redirect, assert the parse + state rules.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = wait_for_code(listener, "good-state");
        let client = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            use tokio::io::AsyncWriteExt;
            s.write_all(
                b"GET /callback?code=the-code&state=good-state HTTP/1.1\r\nHost: x\r\n\r\n",
            )
            .await
            .unwrap();
            s
        });
        let code = server.await.unwrap();
        assert_eq!(code, "the-code");
        let _ = client.await;

        // A state mismatch errors even when a code is present.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = wait_for_code(listener, "good-state");
        let client = tokio::spawn(async move {
            let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
            use tokio::io::AsyncWriteExt;
            s.write_all(b"GET /callback?code=x&state=evil HTTP/1.1\r\nHost: x\r\n\r\n")
                .await
                .unwrap();
            s
        });
        assert!(server.await.is_err(), "state mismatch must error");
        let _ = client.await;
    }
}
