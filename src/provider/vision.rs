// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Vision client — image-to-text via a separate OpenAI-compatible model.
//!
//! When the main LLM is not multimodal (e.g. GLM-5.2), a separate vision model
//! (e.g. Qwen-VL) can be configured to describe images. The `VisionClient`
//! wraps an [`OpenAiClient`](crate::provider::openai::OpenAiClient) configured
//! for the vision model and exposes [`describe_image`](Self::describe_image),
//! which does a non-streaming POST to `/chat/completions` with a multipart user
//! message (text + image_url block) and returns the full text response.
//!
//! The vision model may come from a different OpenAI-compatible source than the
//! main LLM — the client is built from its own endpoint config (base_url +
//! api_key + model), independent of the main provider.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::json;

use crate::error::{Error, Result};
use crate::provider::openai::{OpenAiClient, OpenAiClientConfig};

/// An image-to-text model — abstracts the vision backend so the fallback and
/// tool don't depend on the concrete HTTP client (and tests can mock it,
/// mirroring how [`LlmClient`](crate::provider::LlmClient) abstracts the main
/// model).
///
/// Implemented by [`VisionClient`] (the real OpenAI-compatible vision model)
/// and [`SwappableVision`] (runtime-rewireable slot used by the Settings UI).
/// The always-run test suite uses mock implementations — unit tests must never
/// attempt a live connection (live provider tests live behind `#[ignore]` in
/// the integration-test binary).
#[async_trait]
pub trait ImageDescriber: Send + Sync {
    /// Describe an image at the given URL (`https://...` or a base64 data
    /// URI `data:image/png;base64,...`) using the vision model, prompted with
    /// `prompt`. Returns the full text response.
    async fn describe_image(&self, image_url: &str, prompt: &str) -> Result<String>;

    /// Whether a vision model is actually wired behind this describer.
    ///
    /// Default `true` — a concrete client is by definition configured. Only
    /// [`SwappableVision`] (which may hold an empty slot) overrides it, so the
    /// `image_*` tools can hide themselves while vision is off.
    fn is_configured(&self) -> bool {
        true
    }

    /// Describe one or more images together (so the model can compare them).
    /// `image_urls` carries the data URLs (or https URLs) in order; the prompt
    /// is sent as the text block preceding the image blocks. Returns the full
    /// text response.
    ///
    /// Default implementation delegates to [`describe_image`](Self::describe_image)
    /// with the first image only (back-compat for backends that don't override
    /// it); concrete clients that support multi-image (e.g. the OpenAI-compatible
    /// [`VisionClient`]) override to send all images in one multipart message.
    async fn describe_images(&self, image_urls: &[String], prompt: &str) -> Result<String> {
        match image_urls.first() {
            Some(url) => self.describe_image(url, prompt).await,
            None => Err(Error::InvalidInput(
                "describe_images requires at least one image".into(),
            )),
        }
    }
}

#[async_trait]
impl ImageDescriber for VisionClient {
    async fn describe_image(&self, image_url: &str, prompt: &str) -> Result<String> {
        // Delegate to the inherent implementation (the real HTTP POST).
        VisionClient::describe_image(self, image_url, prompt).await
    }

    async fn describe_images(&self, image_urls: &[String], prompt: &str) -> Result<String> {
        // Delegate to the inherent implementation (the real multi-image HTTP POST).
        VisionClient::describe_images(self, image_urls, prompt).await
    }
}

/// A runtime-swappable vision slot shared by the factory, every agent loop,
/// and the `describe_image` tool. Settings can call [`set`](Self::set) after
/// the user changes `[general.vision_model]` without rebuilding agents.
pub struct SwappableVision {
    inner: RwLock<Option<Arc<dyn ImageDescriber>>>,
}

impl SwappableVision {
    /// Create a slot with an optional initial client.
    pub fn new(initial: Option<Arc<dyn ImageDescriber>>) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(initial),
        })
    }

    /// Replace the active vision client (`None` disables image-to-text).
    pub fn set(&self, client: Option<Arc<dyn ImageDescriber>>) {
        *self.inner.write().expect("swappable vision lock poisoned") = client;
    }

    /// Snapshot the current client (if any).
    pub fn get(&self) -> Option<Arc<dyn ImageDescriber>> {
        self.inner
            .read()
            .expect("swappable vision lock poisoned")
            .clone()
    }

    /// Whether a vision client is currently configured.
    pub fn is_configured(&self) -> bool {
        self.inner
            .read()
            .expect("swappable vision lock poisoned")
            .is_some()
    }
}

#[async_trait]
impl ImageDescriber for SwappableVision {
    async fn describe_image(&self, image_url: &str, prompt: &str) -> Result<String> {
        let client = self.get().ok_or_else(|| {
            Error::InvalidInput(
                "no vision model configured — set [general.vision_model] in Settings".into(),
            )
        })?;
        client.describe_image(image_url, prompt).await
    }

    async fn describe_images(&self, image_urls: &[String], prompt: &str) -> Result<String> {
        let client = self.get().ok_or_else(|| {
            Error::InvalidInput(
                "no vision model configured — set [general.vision_model] in Settings".into(),
            )
        })?;
        client.describe_images(image_urls, prompt).await
    }

    /// Report the slot's real state through the trait, so `Tool::available`
    /// can hide the `image_*` tools while the slot is empty. Mirrors the
    /// inherent [`is_configured`](Self::is_configured); the trait method is
    /// the one reachable through `Arc<dyn ImageDescriber>`, which is how the
    /// image tools hold their vision handle.
    fn is_configured(&self) -> bool {
        self.inner
            .read()
            .expect("swappable vision lock poisoned")
            .is_some()
    }
}

/// A client for image-to-text via a separate vision model.
///
/// Wraps an `OpenAiClient` configured for the vision model. The client is
/// non-streaming — `describe_image` does a single POST and returns the full
/// text response, since image description is a one-shot operation (no need for
/// SSE streaming).
pub struct VisionClient {
    client: OpenAiClient,
    /// The base URL (without trailing slash) for building the completions URL.
    base_url: String,
    /// The API key for the Authorization header.
    api_key: String,
    /// The model id to send in the request body.
    model: String,
}

impl VisionClient {
    /// Build a vision client from an OpenAI-compatible client config. The
    /// config's `multimodal` flag is irrelevant here (we always send image
    /// blocks to the vision model — it's assumed to accept them).
    pub fn new(config: OpenAiClientConfig) -> Self {
        Self {
            base_url: config.base_url.trim_end_matches('/').to_string(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            client: OpenAiClient::new(config),
        }
    }

    /// A reference to the underlying `OpenAiClient` (for capabilities, etc.).
    pub fn client(&self) -> &OpenAiClient {
        &self.client
    }

    /// Describe an image at the given URL using the vision model.
    ///
    /// Sends a non-streaming POST to `/chat/completions` with a multipart user
    /// message containing a text block (the prompt) and an image_url block.
    /// Returns the full text response from the model.
    ///
    /// `image_url` can be an `https://...` URL or a base64 data URI
    /// (`data:image/png;base64,...`).
    pub async fn describe_image(&self, image_url: &str, prompt: &str) -> Result<String> {
        let body = self.build_request_json(&[image_url], prompt);

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .client
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("vision request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(
                match crate::provider::sse_util::check_response(response, &url, "vision request")
                    .await
                {
                    Ok(_) => unreachable!("status was not success"),
                    Err((_, _, error)) => error,
                },
            );
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Provider(format!("failed to parse vision response: {e}")))?;

        // Extract the text from choices[0].message.content.
        let text = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                Error::Provider(format!(
                    "vision response missing choices[0].message.content: {json}"
                ))
            })?;
        Ok(text.to_string())
    }

    /// Describe one or more images together (so the model can compare them).
    /// Sends all `image_urls` as image_url blocks in one multipart user
    /// message, preceded by the text prompt. Used by `image_ui_diff` so the
    /// model sees both screenshots in one call and can actually compare them.
    async fn describe_images(&self, image_urls: &[String], prompt: &str) -> Result<String> {
        let refs: Vec<&str> = image_urls.iter().map(|s| s.as_str()).collect();
        let body = self.build_request_json(&refs, prompt);

        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .client
            .http_client()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("vision request failed: {e}")))?;

        if !response.status().is_success() {
            return Err(
                match crate::provider::sse_util::check_response(response, &url, "vision request")
                    .await
                {
                    Ok(_) => unreachable!("status was not success"),
                    Err((_, _, error)) => error,
                },
            );
        }

        let json: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Error::Provider(format!("failed to parse vision response: {e}")))?;

        let text = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                Error::Provider(format!(
                    "vision response missing choices[0].message.content: {json}"
                ))
            })?;
        Ok(text.to_string())
    }

    /// Build the request body for a non-streaming image-description request
    /// with one or more images. Exposed for unit testing (canned JSON, no live
    /// HTTP).
    fn build_request_json(&self, image_urls: &[&str], prompt: &str) -> serde_json::Value {
        let mut content = vec![json!({"type": "text", "text": prompt})];
        for url in image_urls {
            content.push(json!({"type": "image_url", "image_url": {"url": url}}));
        }
        json!({
            "model": self.model,
            "stream": false,
            "messages": [
                {
                    "role": "user",
                    "content": content
                }
            ]
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderKind;

    fn make_vision_client() -> VisionClient {
        VisionClient::new(OpenAiClientConfig {
            base_url: "http://localhost:11434/v1/".into(),
            model: "qwen-vl".into(),
            kind: ProviderKind::Local,
            multimodal: true,
            ..OpenAiClientConfig::test_default()
        })
    }

    #[test]
    fn build_request_json_has_multipart_user_message() {
        let vc = make_vision_client();
        let body = vc.build_request_json(&["https://example.com/img.png"], "What is this?");
        assert_eq!(body["model"], "qwen-vl");
        assert_eq!(body["stream"], false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        let content = messages[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        // Text block.
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "What is this?");
        // Image URL block.
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(
            content[1]["image_url"]["url"],
            "https://example.com/img.png"
        );
    }

    #[test]
    fn build_request_json_supports_data_uri() {
        // A base64 data URI should be accepted as the image_url.
        let vc = make_vision_client();
        let data_uri = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";
        let body = vc.build_request_json(&[data_uri], "describe");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[1]["image_url"]["url"], data_uri);
    }

    #[test]
    fn build_request_json_supports_multiple_images() {
        // Two image URLs → two image_url blocks after the text block.
        let vc = make_vision_client();
        let body = vc.build_request_json(
            &["data:image/png;base64,aaa", "data:image/png;base64,bbb"],
            "compare these",
        );
        let content = body["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3); // text + 2 images
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,aaa");
        assert_eq!(content[2]["image_url"]["url"], "data:image/png;base64,bbb");
    }

    #[test]
    fn base_url_trailing_slash_stripped() {
        let vc = make_vision_client();
        assert_eq!(vc.base_url, "http://localhost:11434/v1");
    }
}
