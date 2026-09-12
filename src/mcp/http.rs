// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Remote transport — MCP streamable HTTP: each JSON-RPC message is a
//! `POST` to the configured URL; the response body is either plain JSON
//! or an SSE (`text/event-stream`) stream whose `data:` frames carry the
//! JSON-RPC responses. Servers may issue a `Mcp-Session-Id` header on
//! the first response — captured and echoed on every later request.
//!
//! v1 does not open the server→client GET stream: servers that push
//! requests (sampling) cannot reach the client over this transport at
//! all (over stdio such requests are declined with a JSON-RPC error
//! instead of hanging the server — see [`super::stdio`]), and a tool
//! response that never arrives simply times out. Supporting server→client
//! requests over HTTP is future work.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::mcp::McpServerDef;
use crate::error::{Error, Result};

use super::{
    extract_result, oauth, parse_call_result, parse_capabilities, parse_prompt_list,
    parse_resource_list, parse_text_content, parse_tool_list, rpc_request, CallToolOutcome,
    McpClient, McpPromptInfo, McpResourceInfo, McpToolInfo, ServerCapabilities, HANDSHAKE_TIMEOUT,
    REQUEST_TIMEOUT,
};

/// A live remote connection to one MCP server.
pub struct HttpMcpClient {
    url: String,
    /// Headers resolved at construction from `headers_env` (header name →
    /// the VALUE read from the environment; names only in config).
    headers: Vec<(String, String)>,
    /// The server-issued session id, sent as `Mcp-Session-Id` once set.
    session: Option<String>,
    next_id: u64,
    http: reqwest::Client,
    /// The owning server's config name — the OAuth token-store key.
    server: String,
    /// Pre-registered OAuth client id (auth = "oauth").
    client_id: Option<String>,
    /// The token store (auth = "oauth"): Bearer injected per request, and
    /// a 401 triggers one refresh + retry.
    tokens: Option<Arc<oauth::McpTokenStore>>,
    /// The server url's origin (scheme://host[:port]) — stored tokens are
    /// only used when their recorded origin matches, so re-pointing a
    /// server's url never sends old tokens to a new host (review LOW 3).
    expected_origin: Option<String>,
}

impl HttpMcpClient {
    /// Build the client for `def`. Header values are resolved from the
    /// environment NOW (an unset variable logs a diagnostic and the
    /// header is simply not sent — the server will reject it if it
    /// mattered). Never logs the resolved values. `tokens` is wired when
    /// the def carries `auth = "oauth"`.
    pub fn new(def: &McpServerDef, tokens: Option<Arc<oauth::McpTokenStore>>) -> Result<Self> {
        let url = def.url.clone().unwrap_or_default();
        if url.trim().is_empty() {
            return Err(Error::Mcp(format!(
                "mcp server '{}' has an empty url",
                def.name
            )));
        }
        let mut headers: Vec<(String, String)> = Vec::new();
        for (header, var) in &def.headers_env {
            match std::env::var(var) {
                Ok(v) if !v.trim().is_empty() => headers.push((header.clone(), v)),
                _ => eprintln!(
                    "mcp: server '{}' header '{header}' env var '{var}' is not set — the \
                     request goes out without it",
                    def.name
                ),
            }
        }
        let http = reqwest::Client::builder()
            .connect_timeout(HANDSHAKE_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| Error::Mcp(format!("failed to build http client: {e}")))?;
        // Only OAuth-flagged servers get the token store wired — removing
        // auth from the def must stop sending old tokens immediately, and
        // the tokens themselves are origin-bound (review LOW 3).
        let tokens = if def.auth.as_deref() == Some("oauth") {
            tokens
        } else {
            None
        };
        let expected_origin = oauth::origin_of(&url).ok();
        Ok(Self {
            url,
            headers,
            session: None,
            next_id: 0,
            http,
            server: def.name.clone(),
            client_id: def.client_id.clone(),
            tokens,
            expected_origin,
        })
    }

    /// The stored tokens for this server, filtered to the server's CURRENT
    /// origin — re-pointing the url invalidates old tokens so a foreign
    /// host never sees them (review LOW 3).
    fn stored_tokens(&self) -> Option<oauth::OAuthTokens> {
        let tokens = self.tokens.as_ref()?;
        let origin = self.expected_origin.as_deref()?;
        tokens
            .load(&self.server)
            .ok()
            .flatten()
            .filter(|t| t.origin.as_deref() == Some(origin))
    }

    /// POST one JSON-RPC request and await its response (JSON or SSE). A
    /// 401 under OAuth means the stored access token expired or was
    /// revoked: refresh and retry ONCE (a second 401 surfaces to the
    /// caller — no infinite refresh loops).
    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        // Built once — the OAuth retry re-serializes the SAME request.
        let request = rpc_request(id, method, params);
        let body = serde_json::to_string(&request)?;
        let resp = self.post(body).await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED && self.tokens.is_some() {
            self.refresh_tokens().await?;
            let body = serde_json::to_string(&request)?;
            let resp = self.post(body).await?;
            return self.read_response(resp, id).await;
        }
        self.read_response(resp, id).await
    }

    /// Status check + session capture + body parse (JSON or SSE), shared
    /// by the first attempt and the OAuth retry.
    async fn read_response(&mut self, resp: reqwest::Response, id: u64) -> Result<Value> {
        if !resp.status().is_success() {
            return Err(Error::Mcp(format!("http {}", resp.status())));
        }
        self.capture_session(&resp);
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        let text = resp
            .text()
            .await
            .map_err(|e| Error::Mcp(format!("failed to read response body: {e}")))?;
        let value = if content_type.contains("text/event-stream") {
            sse_response_with_id(&text, id).ok_or_else(|| {
                Error::Mcp("SSE stream ended without a response for this request".into())
            })?
        } else {
            serde_json::from_str(&text)?
        };
        extract_result(&value)
    }

    /// Refresh the OAuth tokens: re-discover the token endpoint, run the
    /// refresh grant, and persist the new tokens (the next post() picks
    /// them up from the store).
    async fn refresh_tokens(&mut self) -> Result<()> {
        let Some(tokens) = self.tokens.as_ref() else {
            return Err(Error::Mcp("no token store configured".into()));
        };
        let stored = self.stored_tokens().ok_or_else(|| {
            Error::Mcp(
                "no stored OAuth token for this server origin — re-authorize from Settings".into(),
            )
        })?;
        let refresh_token = stored.refresh_token.clone().ok_or_else(|| {
            Error::Mcp("no refresh token stored — re-authorize from Settings".into())
        })?;
        let meta = oauth::discover(&self.http, &self.url).await?;
        let body =
            oauth::build_refresh_request(&refresh_token, self.client_id.as_deref().unwrap_or(""));
        let mut new = oauth::token_request(&self.http, &meta.token_endpoint, body).await?;
        new.origin = self.expected_origin.clone();
        tokens
            .store(&self.server, &new)
            .map_err(|e| Error::Mcp(format!("failed to store refreshed tokens: {e}")))?;
        Ok(())
    }

    /// POST a notification (no id) — any 2xx (typically 202) is success.
    async fn notify(&mut self, method: &str) -> Result<()> {
        let note = json!({"jsonrpc": "2.0", "method": method});
        let body = serde_json::to_string(&note)?;
        let resp = self.post(body).await?;
        if !resp.status().is_success() {
            return Err(Error::Mcp(format!("http {}", resp.status())));
        }
        Ok(())
    }

    /// Issue the POST with the standard + configured + OAuth + session
    /// headers. Consumes the builder without reading the body — callers
    /// do that.
    async fn post(&self, body: String) -> Result<reqwest::Response> {
        let mut req = self
            .http
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .body(body);
        for (h, v) in &self.headers {
            req = req.header(h.as_str(), v.as_str());
        }
        // OAuth: inject the stored Bearer token, loaded PER REQUEST (and
        // origin-verified) so a refreshed token applies immediately; a
        // corrupt/foreign/absent blob simply sends without auth — the 401
        // path handles the rest.
        if let Some(tok) = self.stored_tokens() {
            req = req.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", tok.access_token),
            );
        }
        if let Some(session) = &self.session {
            req = req.header("Mcp-Session-Id", session);
        }
        req.send()
            .await
            .map_err(|e| Error::Mcp(format!("http request failed: {e}")))
    }

    fn capture_session(&mut self, resp: &reqwest::Response) {
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
        {
            self.session = Some(sid.to_string());
        }
    }
}

#[async_trait]
impl McpClient for HttpMcpClient {
    async fn initialize(&mut self) -> Result<ServerCapabilities> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "mnemo", "version": env!("CARGO_PKG_VERSION")}
        });
        let result = self.request("initialize", params).await?;
        let capabilities = parse_capabilities(&result);
        self.notify("notifications/initialized").await?;
        Ok(capabilities)
    }

    async fn list_tools(&mut self) -> Result<Vec<McpToolInfo>> {
        let result = self.request("tools/list", json!({})).await?;
        parse_tool_list(&result)
    }

    async fn call_tool(&mut self, name: &str, args: Value) -> Result<CallToolOutcome> {
        let params = json!({"name": name, "arguments": args});
        let result = self.request("tools/call", params).await?;
        parse_call_result(&result)
    }

    async fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>> {
        let result = self.request("prompts/list", json!({})).await?;
        parse_prompt_list(&result)
    }

    async fn get_prompt(&mut self, name: &str, args: Value) -> Result<String> {
        let params = json!({"name": name, "arguments": args});
        let result = self.request("prompts/get", params).await?;
        parse_text_content(&result)
    }

    async fn list_resources(&mut self) -> Result<Vec<McpResourceInfo>> {
        let result = self.request("resources/list", json!({})).await?;
        parse_resource_list(&result)
    }

    async fn read_resource(&mut self, uri: &str) -> Result<String> {
        let params = json!({"uri": uri});
        let result = self.request("resources/read", params).await?;
        parse_text_content(&result)
    }
}

/// Parse one SSE event block (its joined `data:` lines) into a JSON value.
fn parse_sse_event(block: &[String]) -> Option<Value> {
    if block.is_empty() {
        return None;
    }
    serde_json::from_str(&block.join("\n")).ok()
}

/// Extract the JSON-RPC response carrying `id` from an SSE body: events
/// are blank-line-separated blocks of `data:` lines; the first block
/// whose parsed JSON has the wanted id wins. `\r\n` line endings are
/// tolerated.
pub(crate) fn sse_response_with_id(body: &str, id: u64) -> Option<Value> {
    let wants = |v: &Value| v.get("id").and_then(Value::as_u64) == Some(id);
    let mut block: Vec<String> = Vec::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            if let Some(v) = parse_sse_event(&block) {
                if wants(&v) {
                    return Some(v);
                }
            }
            block.clear();
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            block.push(data.trim_start().to_string());
        }
    }
    // A body without a trailing blank line still holds its final event.
    if let Some(v) = parse_sse_event(&block) {
        if wants(&v) {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_picks_the_frame_with_the_request_id() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":true}}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"b\":2}}\n\n";
        let v = sse_response_with_id(body, 2).expect("frame with id 2");
        assert_eq!(v["result"]["b"], 2);
        assert!(sse_response_with_id(body, 9).is_none(), "no such id");
    }

    #[test]
    fn sse_tolerates_crlf_and_missing_trailing_blank_line() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}";
        let v = sse_response_with_id(body, 7).expect("final event without trailing blank line");
        assert!(v.get("result").is_some());
        let crlf = "data: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"ok\":1}}\r\n\r\n";
        let v = sse_response_with_id(crlf, 3).expect("crlf body");
        assert_eq!(v["result"]["ok"], 1);
    }

    #[test]
    fn sse_joins_multi_line_data_frames() {
        // JSON split across two data: lines must be joined before parsing.
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":4,\ndata: \"result\":{\"x\":9}}\n\n";
        let v = sse_response_with_id(body, 4).expect("joined frame parses");
        assert_eq!(v["result"]["x"], 9);
    }
}
