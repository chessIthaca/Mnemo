// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Image analysis tools â€” the `image_*` family, mirroring the vision-mcp spec.
//!
//! This module hosts the **shared vision plumbing** (moved here from the old
//! `describe_image.rs`) plus the seven task-specific image tools:
//!
//! - [`mime_from_ext`] maps a file extension to a MIME type.
//! - [`magic_matches`] verifies a file's leading bytes match its claimed MIME.
//! - [`load_image_data_url`] sandbox-validates a path, sniffs magic bytes, and
//!   reads the file into a base64 data URL.
//! - [`describe_image_data_url`] sends a data URL to the vision model.
//! - [`describe_image_file`] = load + describe (used by the attachment fallback).
//!
//! The seven tools (`image_ui_to_artifact`, `image_extract_text`,
//! `image_diagnose_error`, `image_understand_diagram`, `image_analyze_chart`,
//! `image_ui_diff`, `image_analysis`) each build a task-specific prompt and run
//! it through the agentic auto-zoom loop ([`zoom::analyze_with_zoom`]) when a
//! fine/normal/auto detail level is requested, falling back to a single
//! overview pass when image decode/crop/re-encode isn't possible.
//!
//! All tools are `NeedsApproval` â€” they read a file inside the sandbox and
//! send its bytes to the configured vision endpoint over the network, so they
//! are gated behind an approval prompt (never auto-run). The MIME is content-
//! sniffed from magic bytes (not just the extension) so a non-image file
//! renamed with an image extension cannot be exfiltrated.

pub mod prompts;
pub mod tools;
pub mod zoom;

use std::path::Path;

use base64::Engine as _;

use crate::error::{Error, Result};
use crate::provider::vision::ImageDescriber;
use crate::tool::agent::sandbox::Sandbox;

/// The default prompt used when the caller doesn't ask a specific question
/// about the image. Also used by the attachment fallback so both paths
/// describe images the same way.
pub const DEFAULT_DESCRIBE_PROMPT: &str = "Describe this image in detail.";

/// Map an image file extension (lowercase, no dot) to a MIME type.
/// Returns `None` for unrecognized extensions.
pub fn mime_from_ext(ext: &str) -> Option<&'static str> {
    match ext.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// Read an image file (path relative to the project root) and return it as a
/// base64 data URL (`data:<mime>;base64,...`). The path is sandbox-validated
/// before any I/O; the extension must map to a known image MIME type.
pub fn load_image_data_url(sandbox: &Sandbox, path: &str) -> Result<String> {
    let validated = sandbox.validate(Path::new(path))?;

    let ext = validated
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| Error::InvalidInput(format!("path has no file extension: {path}")))?;
    let mime = mime_from_ext(ext).ok_or_else(|| {
        Error::InvalidInput(format!(
            "unsupported image extension '.{ext}' â€” expected png, jpg, jpeg, gif, webp, or bmp"
        ))
    })?;

    let bytes = std::fs::read(&validated)?;
    if !magic_matches(mime, &bytes) {
        return Err(Error::InvalidInput(format!(
            "file content does not match '.{ext}' (expected {mime}) â€” the magic bytes do not match"
        )));
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// Verify a file's leading bytes match the magic-number signature of the
/// claimed MIME type. This closes the exfiltration gap where a non-image file
/// renamed with an image extension (e.g. `secret.txt` â†’ `secret.png`) would be
/// read and sent to the vision endpoint: the content must actually be the
/// claimed image format.
///
/// Signatures (a prefix or fixed-offset check, whichever is cheapest):
/// - PNG: `89 50 4E 47 0D 0A 1A 0A` (`\x89PNG\r\n\x1a\n`)
/// - JPEG: `FF D8 FF`
/// - GIF: `GIF8` (`47 49 46 38`)
/// - WebP: `RIFF` at offset 0 + `WEBP` at offset 8
/// - BMP: `BM` (`42 4D`)
pub fn magic_matches(mime: &str, bytes: &[u8]) -> bool {
    let starts_with = |prefix: &[u8]| bytes.starts_with(prefix);
    match mime {
        "image/png" => starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => starts_with(b"\xff\xd8\xff"),
        "image/gif" => starts_with(b"GIF8"),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "image/bmp" => starts_with(b"BM"),
        _ => false,
    }
}

/// Describe an image given as a base64 data URL, using the vision model.
///
/// `question` is the prompt to the vision model â€” when `None` or empty,
/// [`DEFAULT_DESCRIBE_PROMPT`] is used. This is the shared entry point used
/// by both the tools (file â†’ data URL â†’ here) and the attachment fallback
/// (pasted image data URLs â†’ here).
pub async fn describe_image_data_url(
    vision: &dyn ImageDescriber,
    data_url: &str,
    question: Option<&str>,
) -> Result<String> {
    let prompt = match question {
        Some(q) if !q.trim().is_empty() => q,
        _ => DEFAULT_DESCRIBE_PROMPT,
    };
    vision.describe_image(data_url, prompt).await
}

/// Load an image file and describe it via the vision model. Convenience
/// wrapper combining [`load_image_data_url`] + [`describe_image_data_url`].
///
/// The blocking file read + sandbox validation ([`load_image_data_url`]) runs
/// on tokio's blocking pool via `spawn_blocking` (Perf H1); the vision model
/// call (`describe_image_data_url`) stays async.
pub async fn describe_image_file(
    sandbox: &Sandbox,
    vision: &dyn ImageDescriber,
    path: &str,
    question: Option<&str>,
) -> Result<String> {
    // Clone the cheap sandbox (one PathBuf) + the path so the closure owns its
    // inputs ('static + Send) and can run the blocking read off the async
    // runtime. Validation errors surface identically via `?` below.
    let sandbox = sandbox.clone();
    let path = path.to_string();
    let data_url = tokio::task::spawn_blocking(move || load_image_data_url(&sandbox, &path))
        .await
        .map_err(|e| Error::Tool(format!("describe_image load task failed: {e}")))??;
    describe_image_data_url(vision, &data_url, question).await
}

/// Load an image file to a data URL AND return its raw bytes (for the zoom
/// loop's decode/crop). The blocking work runs on tokio's blocking pool.
///
/// Returns `(data_url, bytes)` so the caller can both send the data URL to the
/// vision model and decode the bytes locally for cropping.
pub(crate) async fn load_image_data_url_and_bytes(
    sandbox: &Sandbox,
    path: &str,
) -> Result<(String, Vec<u8>)> {
    let sandbox = sandbox.clone();
    let path = path.to_string();
    tokio::task::spawn_blocking(move || -> Result<(String, Vec<u8>)> {
        let validated = sandbox.validate(Path::new(&path))?;
        let ext = validated
            .extension()
            .and_then(|e| e.to_str())
            .ok_or_else(|| {
                Error::InvalidInput(format!("path has no file extension: {path}"))
            })?;
        let mime = mime_from_ext(ext).ok_or_else(|| {
            Error::InvalidInput(format!(
                "unsupported image extension '.{ext}' â€” expected png, jpg, jpeg, gif, webp, or bmp"
            ))
        })?;
        let bytes = std::fs::read(&validated)?;
        if !magic_matches(mime, &bytes) {
            return Err(Error::InvalidInput(format!(
                "file content does not match '.{ext}' (expected {mime}) â€” the magic bytes do not match"
            )));
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_url = format!("data:{mime};base64,{b64}");
        Ok((data_url, bytes))
    })
    .await
    .map_err(|e| Error::Tool(format!("image load task failed: {e}")))?
}

// Re-export the data-URL builder used by the zoom loop's crop re-encode path.
pub(crate) fn bytes_to_data_url(mime: &str, bytes: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{b64}")
}

#[cfg(test)]
mod test_support {
    use super::*;
    use std::sync::Mutex as StdMutex;

    use async_trait::async_trait;

    /// A mock vision model: returns a canned description and records the
    /// prompts it was called with (so tests can assert default vs custom
    /// prompt) â€” no network.
    pub(crate) struct MockDescriber {
        pub description: String,
        pub calls: StdMutex<Vec<(String, String)>>,
    }

    impl MockDescriber {
        pub(crate) fn new(description: &str) -> Self {
            Self {
                description: description.to_string(),
                calls: StdMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ImageDescriber for MockDescriber {
        async fn describe_image(&self, image_url: &str, prompt: &str) -> Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push((image_url.to_string(), prompt.to_string()));
            Ok(self.description.clone())
        }

        async fn describe_images(&self, image_urls: &[String], prompt: &str) -> Result<String> {
            // Record the call with a joined URL so tests can assert it was a
            // multi-image call (the count of URLs is visible in the joined
            // string).
            let joined = image_urls.join("||");
            self.calls
                .lock()
                .unwrap()
                .push((joined, prompt.to_string()));
            Ok(self.description.clone())
        }
    }
}

#[cfg(test)]
pub(crate) use test_support::MockDescriber;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mime_from_ext_maps_known_types() {
        assert_eq!(mime_from_ext("png"), Some("image/png"));
        assert_eq!(mime_from_ext("jpg"), Some("image/jpeg"));
        assert_eq!(mime_from_ext("jpeg"), Some("image/jpeg"));
        assert_eq!(mime_from_ext("gif"), Some("image/gif"));
        assert_eq!(mime_from_ext("webp"), Some("image/webp"));
        assert_eq!(mime_from_ext("bmp"), Some("image/bmp"));
        // Case-insensitive.
        assert_eq!(mime_from_ext("PNG"), Some("image/png"));
        // Unknown / non-image.
        assert_eq!(mime_from_ext("txt"), None);
        assert_eq!(mime_from_ext("svg"), None);
        assert_eq!(mime_from_ext(""), None);
    }

    #[test]
    fn load_image_data_url_rejects_unsupported_extension() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, b"hello").unwrap();
        let err = load_image_data_url(&sandbox, "notes.txt").unwrap_err();
        assert!(err.to_string().contains("unsupported image extension"));
    }

    #[test]
    fn load_image_data_url_builds_data_url() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // A tiny PNG (just the magic bytes + padding â€” content validity
        // doesn't matter for the data URL, only the extension â†’ MIME).
        std::fs::write(dir.path().join("pic.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let url = load_image_data_url(&sandbox, "pic.png").unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        let expected = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n");
        assert!(url.ends_with(&expected));
    }

    #[test]
    fn load_image_data_url_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = load_image_data_url(&sandbox, "../../etc/passwd.png").unwrap_err();
        assert!(matches!(
            err,
            Error::PathOutsideRoot(_) | Error::InvalidInput(_) | Error::Io(_)
        ));
    }

    #[test]
    fn load_image_data_url_rejects_content_mime_mismatch() {
        // A non-image file renamed with an image extension must be rejected:
        // the magic bytes must match the claimed MIME, so a `.png` whose
        // content is plain text cannot be exfiltrated to the vision endpoint.
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("secret.png"), b"not a png at all").unwrap();
        let err = load_image_data_url(&sandbox, "secret.png").unwrap_err();
        assert!(err.to_string().contains("does not match"), "got: {err}");
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn magic_matches_known_signatures() {
        // PNG
        assert!(magic_matches("image/png", b"\x89PNG\r\n\x1a\n"));
        assert!(!magic_matches("image/png", b"not a png"));
        // JPEG
        assert!(magic_matches("image/jpeg", b"\xff\xd8\xff\xe0"));
        assert!(!magic_matches("image/jpeg", b"\x89PNG"));
        // GIF
        assert!(magic_matches("image/gif", b"GIF89a"));
        assert!(!magic_matches("image/gif", b"GIF"));
        // WebP (RIFF....WEBP)
        assert!(magic_matches("image/webp", b"RIFF\x00\x00\x00\x00WEBP"));
        assert!(!magic_matches("image/webp", b"RIFF\x00\x00\x00\x00XXXX"));
        assert!(!magic_matches("image/webp", b"short")); // too short
                                                         // BMP
        assert!(magic_matches("image/bmp", b"BM\x00\x00"));
        assert!(!magic_matches("image/bmp", b"XB"));
        // Unknown MIME
        assert!(!magic_matches("image/svg", b"<svg"));
    }

    #[tokio::test]
    async fn describe_image_data_url_uses_default_prompt_when_empty() {
        let mock = Arc::new(MockDescriber::new("a description"));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let _ = describe_image_data_url(vision.as_ref(), "data:image/png;base64,x", None).await;
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, DEFAULT_DESCRIBE_PROMPT);
    }

    #[tokio::test]
    async fn describe_image_data_url_uses_custom_question() {
        let mock = Arc::new(MockDescriber::new("a description"));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let _ = describe_image_data_url(
            vision.as_ref(),
            "data:image/png;base64,x",
            Some("what is this?"),
        )
        .await;
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[0].1, "what is this?");
    }
}
