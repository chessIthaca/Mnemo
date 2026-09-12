// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The agentic auto-zoom loop â€” grid â†’ model votes â†’ crop the full-resolution
//! original â†’ re-read, up to `max_rounds`, with early-exit when confident.
//!
//! When image decode/crop/re-encode isn't possible (unsupported format, decode
//! failure), the loop falls back to a single overview pass so the tool still
//! returns a useful answer instead of erroring.
//!
//! Crops ALWAYS come from the full-resolution decoded original (never the
//! downsampled overview), so zoom actually recovers detail that a single
//! overview pass would misread. All blocking image work (decode/resize/crop/
//! re-encode) runs on tokio's blocking pool via `spawn_blocking`; the vision
//! model calls stay async.

use std::io::Cursor;
use std::sync::Arc;

use image::{ImageFormat, RgbaImage};
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::provider::vision::ImageDescriber;

use super::bytes_to_data_url;
use super::describe_image_data_url;
use super::prompts::{build_prompt, zoom_control_prompt, ImageTool, PromptArgs};

/// How much detail to apply. `Overview` = single fast pass; `Normal`/`Fine`/
/// `Auto` drive the zoom loop (`Auto` zooms only when needed, early-exits when
/// clear).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailLevel {
    Overview,
    Normal,
    Fine,
    Auto,
}

impl Default for DetailLevel {
    fn default() -> Self {
        DetailLevel::Auto
    }
}

/// Parse a detail-level string (case-insensitive); unknown values default to
/// `Auto` (the spec's default).
pub(crate) fn parse_detail_level(s: &Option<String>) -> DetailLevel {
    match s.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("overview") => DetailLevel::Overview,
        Some("normal") => DetailLevel::Normal,
        Some("fine") => DetailLevel::Fine,
        _ => DetailLevel::Auto,
    }
}

/// A region the model zoomed into (normalized bbox + optional note).
#[derive(Debug, Clone)]
pub(crate) struct Region {
    pub box_: [f64; 4],
    pub note: Option<String>,
}

/// The zoom loop's result â€” the markdown answer + structured metadata.
#[derive(Debug, Clone)]
pub(crate) struct ZoomResult {
    /// The structured markdown answer (the primary tool output).
    pub markdown: String,
    /// The model's self-reported confidence (0â€“1), when it reported one.
    pub confidence: Option<f64>,
    /// How many zoom rounds executed (0 for a single overview pass).
    pub rounds: u32,
    /// Regions the model zoomed into (normalized bboxes).
    pub regions: Vec<Region>,
    /// Degradations / fallbacks / notices.
    pub warnings: Vec<String>,
}

/// The longest edge the overview is downsampled to (matches the spec's
/// `VISION_MAX_EDGE_PX`). Larger images are shrunk so the overview fits in one
/// vision-model pass.
const MAX_OVERVIEW_EDGE: u32 = 1568;

/// The confidence threshold at which `Auto` stops zooming (the model is sure
/// enough). Below this, the loop continues (up to `max_rounds`).
const AUTO_CONFIDENCE_THRESHOLD: f64 = 0.7;

/// The parsed JSON vote from the model during a zoom round. The `box_` field
/// is renamed to `box` for serde (the model emits `"box":[x,y,w,h]`; `box` is a
/// Rust reserved word so the field is named `box_`).
#[derive(Debug, Deserialize)]
struct ZoomVote {
    action: String,
    #[serde(default)]
    region: Option<String>,
    #[serde(default, rename = "box")]
    box_: Option<Vec<f64>>,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    answer: Option<String>,
}

/// Run the agentic zoom loop (or a single overview pass) for one image.
///
/// `data_url` is the full image as a base64 data URL (sent to the vision model
/// on the overview pass); `bytes` are the raw image bytes (decoded locally for
/// cropping). `tool` + `args` build the task-specific prompt; `region`
/// optionally restricts to a named/bbox region.
pub(crate) async fn analyze_with_zoom(
    vision: &dyn ImageDescriber,
    data_url: &str,
    bytes: &[u8],
    tool: ImageTool,
    args: &PromptArgs,
    detail_level: DetailLevel,
    region: Option<&str>,
    max_rounds: u32,
) -> Result<ZoomResult> {
    // Decode the full-resolution original on the blocking pool. On any decode
    // failure, fall back to a single overview pass (the data URL is still
    // valid for the model). Returns the decoded image + the overview data URL
    // (the original data URL when the image is small enough to skip a
    // downsample+re-encode, else a re-encoded downsampled overview).
    let data_url_owned = data_url.to_string();
    let bytes_owned = bytes.to_vec();
    let decoded = tokio::task::spawn_blocking(move || -> Option<(image::DynamicImage, String)> {
        let full_img = image::load_from_memory(&bytes_owned).ok()?;
        let overview_url =
            if full_img.width() <= MAX_OVERVIEW_EDGE && full_img.height() <= MAX_OVERVIEW_EDGE {
                // Small enough â€” send the original data URL (no re-encode).
                data_url_owned
            } else {
                let overview = full_img.resize(
                    MAX_OVERVIEW_EDGE,
                    MAX_OVERVIEW_EDGE,
                    image::imageops::FilterType::Lanczos3,
                );
                encode_png_data_url(&overview.to_rgba8())
            };
        Some((full_img, overview_url))
    })
    .await
    .map_err(|e| Error::Tool(format!("image decode task failed: {e}")))?;

    let (full_img, overview_url) = match decoded {
        Some(v) => v,
        None => {
            // Decode failed â€” single overview pass on the original data URL.
            let markdown =
                describe_image_data_url(vision, data_url, Some(&build_prompt(tool, args))).await?;
            return Ok(ZoomResult {
                markdown,
                confidence: None,
                rounds: 0,
                regions: vec![],
                warnings: vec!["image decode failed; used single-pass overview".into()],
            });
        }
    };
    let full_img = Arc::new(full_img);

    // A named/bbox region â†’ single pass on a crop of the full-res original.
    if let Some(r) = region {
        return crop_and_describe(vision, &full_img, r, tool, args).await;
    }

    // Overview detail level â†’ single pass on the (downsampled) overview.
    if detail_level == DetailLevel::Overview {
        let markdown =
            describe_image_data_url(vision, &overview_url, Some(&build_prompt(tool, args))).await?;
        return Ok(ZoomResult {
            markdown,
            confidence: None,
            rounds: 0,
            regions: vec![],
            warnings: vec![],
        });
    }

    // Normal / Fine / Auto â†’ the zoom loop.
    zoom_loop(
        vision,
        &full_img,
        &overview_url,
        tool,
        args,
        detail_level,
        max_rounds,
    )
    .await
}

/// Re-encode an RGBA image to a base64 PNG data URL.
fn encode_png_data_url(img: &RgbaImage) -> String {
    let mut buf = Cursor::new(Vec::new());
    // write_to is infallible for PNG into a Vec; surface any error as a string.
    if img.write_to(&mut buf, ImageFormat::Png).is_err() {
        // Should not happen for PNGâ†’Vec, but be defensive.
        return String::new();
    }
    bytes_to_data_url("image/png", &buf.into_inner())
}

/// Crop a region (named or bbox) from the full-res original and describe it in
/// a single pass.
async fn crop_and_describe(
    vision: &dyn ImageDescriber,
    full_img: &Arc<image::DynamicImage>,
    region: &str,
    tool: ImageTool,
    args: &PromptArgs,
) -> Result<ZoomResult> {
    let bbox = parse_region(region).ok_or_else(|| {
        Error::InvalidInput(format!(
            "invalid region '{region}' â€” use a named region (top-left, top-right, bottom-left, bottom-right, center) or a bbox 'x,y,w,h' with 0..1 values"
        ))
    })?;
    let crop_url = crop_to_data_url(full_img, &bbox).await;
    let crop_url =
        crop_url.ok_or_else(|| Error::Tool("failed to re-encode cropped region".into()))?;
    let markdown =
        describe_image_data_url(vision, &crop_url, Some(&build_prompt(tool, args))).await?;
    Ok(ZoomResult {
        markdown,
        confidence: None,
        rounds: 0,
        regions: vec![Region {
            box_: bbox,
            note: Some(region.to_string()),
        }],
        warnings: vec![],
    })
}

/// The zoom loop: overview â†’ model votes â†’ crop â†’ re-read â†’ repeat.
///
/// Each round sends the control prompt (task + sections + vote instruction).
/// The model returns JSON: `action=done` (with the section-formatted answer in
/// `answer`) or `action=zoom` (with a `box` to crop). Crops always come from
/// the full-res original; the crop URL is reassigned each round so progressive
/// zoom converges. `Auto` early-exits when confidence â‰¥ threshold.
async fn zoom_loop(
    vision: &dyn ImageDescriber,
    full_img: &Arc<image::DynamicImage>,
    overview_url: &str,
    tool: ImageTool,
    args: &PromptArgs,
    detail_level: DetailLevel,
    max_rounds: u32,
) -> Result<ZoomResult> {
    let task_prompt = build_prompt(tool, args);
    let region_labels = named_regions();
    let mut warnings = Vec::new();
    let mut regions = Vec::new();
    let mut rounds = 0u32;

    // Round 0: the overview pass. Send the overview + the control prompt (which
    // carries the task + section headers + the vote instruction â€” no separate
    // markdown-section instruction that would contradict the "ONLY JSON" vote).
    let control = zoom_control_prompt(&task_prompt, &region_labels);
    let overview_answer = describe_image_data_url(vision, overview_url, Some(&control)).await?;
    rounds += 1;

    // Parse the model's vote from the overview answer.
    let vote = parse_vote(&overview_answer);
    match vote {
        Some(v) if v.action == "done" => {
            return Ok(ZoomResult {
                markdown: v.answer.unwrap_or(overview_answer),
                confidence: v.confidence,
                rounds,
                regions,
                warnings,
            });
        }
        Some(v) => {
            // action == "zoom": crop the voted region and re-read.
            let crop = crop_vote(&v, full_img).await;
            match crop {
                Some((crop_url, region)) => {
                    regions.push(region);
                    // Re-read the crop with the control prompt.
                    let crop_answer =
                        describe_image_data_url(vision, &crop_url, Some(&control)).await?;
                    rounds += 1;

                    let crop_vote_result = parse_vote(&crop_answer);

                    // A "done" vote always returns (the model is confident) â€”
                    // for ALL detail levels, not just Auto.
                    if let Some(v) = crop_vote_result.as_ref() {
                        if v.action == "done" {
                            return Ok(ZoomResult {
                                markdown: v.answer.clone().unwrap_or(crop_answer),
                                confidence: v.confidence,
                                rounds,
                                regions,
                                warnings,
                            });
                        }
                    }

                    // Auto early-exit: if the model's confidence is high enough
                    // (even on a zoom vote), stop â€” the spec says Auto "zooms
                    // only when needed, early-exits when clear".
                    if detail_level == DetailLevel::Auto {
                        if let Some(c) = crop_vote_result.as_ref().and_then(|v| v.confidence) {
                            if c >= AUTO_CONFIDENCE_THRESHOLD {
                                return Ok(ZoomResult {
                                    markdown: crop_vote_result
                                        .as_ref()
                                        .and_then(|v| v.answer.clone())
                                        .unwrap_or(crop_answer),
                                    confidence: Some(c),
                                    rounds,
                                    regions,
                                    warnings,
                                });
                            }
                        }
                    }

                    // Fine / Normal / low-confidence Auto: continue zooming up
                    // to max_rounds. Each iteration reassigns the crop URL to
                    // the newly-voted region so progressive zoom converges.
                    let mut current_url = crop_url;
                    while rounds < max_rounds {
                        let next_control = zoom_control_prompt(&task_prompt, &region_labels);
                        let next_answer =
                            describe_image_data_url(vision, &current_url, Some(&next_control))
                                .await?;
                        rounds += 1;
                        match parse_vote(&next_answer) {
                            Some(v) if v.action == "done" => {
                                return Ok(ZoomResult {
                                    markdown: v.answer.unwrap_or(next_answer),
                                    confidence: v.confidence,
                                    rounds,
                                    regions,
                                    warnings,
                                });
                            }
                            Some(v) => {
                                match crop_vote(&v, full_img).await {
                                    Some((url, region)) => {
                                        regions.push(region);
                                        current_url = url;
                                    }
                                    None => {
                                        // Couldn't crop â€” treat the answer as done.
                                        warnings.push(
                                            "model voted a region that could not be cropped; used last view"
                                                .into(),
                                        );
                                        return Ok(ZoomResult {
                                            markdown: v.answer.unwrap_or(next_answer),
                                            confidence: v.confidence,
                                            rounds,
                                            regions,
                                            warnings,
                                        });
                                    }
                                }
                            }
                            None => {
                                // Parse fail â€” treat the raw text as the answer.
                                return Ok(ZoomResult {
                                    markdown: next_answer,
                                    confidence: None,
                                    rounds,
                                    regions,
                                    warnings,
                                });
                            }
                        }
                    }
                    // Max rounds exhausted.
                    warnings.push("max zoom rounds reached".into());
                    return Ok(ZoomResult {
                        markdown: crop_vote_result
                            .as_ref()
                            .and_then(|v| v.answer.clone())
                            .unwrap_or(crop_answer),
                        confidence: crop_vote_result.as_ref().and_then(|v| v.confidence),
                        rounds,
                        regions,
                        warnings,
                    });
                }
                None => {
                    // Couldn't crop the voted region â€” fall back to the overview answer.
                    warnings.push(
                        "model voted a region that could not be cropped; used overview".into(),
                    );
                    return Ok(ZoomResult {
                        markdown: v.answer.unwrap_or(overview_answer),
                        confidence: v.confidence,
                        rounds,
                        regions,
                        warnings,
                    });
                }
            }
        }
        None => {
            // Parse fail â€” treat the raw overview text as the answer.
            Ok(ZoomResult {
                markdown: overview_answer,
                confidence: None,
                rounds,
                regions,
                warnings,
            })
        }
    }
}

/// Crop a voted region to a data URL + the recorded region, on the blocking
/// pool. Returns `None` if the vote had no usable box or the re-encode failed.
/// The region is only returned (and thus only recorded by the caller) when the
/// crop + re-encode succeeded.
async fn crop_vote(
    vote: &ZoomVote,
    full_img: &Arc<image::DynamicImage>,
) -> Option<(String, Region)> {
    let box_ = vote.box_.as_ref().filter(|b| b.len() == 4)?;
    let bbox = [
        box_[0].clamp(0.0, 1.0),
        box_[1].clamp(0.0, 1.0),
        box_[2].clamp(0.0, 1.0),
        box_[3].clamp(0.0, 1.0),
    ];
    let region = Region {
        box_: bbox,
        note: vote.region.clone(),
    };
    let url = crop_to_data_url(full_img, &bbox).await?;
    Some((url, region))
}

/// Crop a normalized bbox out of the full-res image and re-encode to a PNG data
/// URL, on the blocking pool. Returns `None` if the re-encode failed.
async fn crop_to_data_url(full_img: &Arc<image::DynamicImage>, bbox: &[f64; 4]) -> Option<String> {
    let img = Arc::clone(full_img);
    let bbox = *bbox;
    tokio::task::spawn_blocking(move || -> Option<String> {
        let crop = crop_bbox(&img, &bbox);
        let url = encode_png_data_url(&crop);
        if url.is_empty() {
            None
        } else {
            Some(url)
        }
    })
    .await
    .ok()?
}

/// The five named regions the model can vote to zoom into.
fn named_regions() -> Vec<String> {
    [
        "top-left",
        "top-right",
        "bottom-left",
        "bottom-right",
        "center",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Parse a region spec into a normalized bbox [x, y, w, h] (0..1). Accepts a
/// named region or a `"x,y,w,h"` string.
fn parse_region(spec: &str) -> Option<[f64; 4]> {
    let s = spec.trim();
    match s.to_ascii_lowercase().as_str() {
        "top-left" => Some([0.0, 0.0, 0.5, 0.5]),
        "top-right" => Some([0.5, 0.0, 0.5, 0.5]),
        "bottom-left" => Some([0.0, 0.5, 0.5, 0.5]),
        "bottom-right" => Some([0.5, 0.5, 0.5, 0.5]),
        "center" => Some([0.25, 0.25, 0.5, 0.5]),
        _ => {
            // "x,y,w,h" with 0..1 values.
            let parts: Vec<&str> = s.split(',').collect();
            if parts.len() != 4 {
                return None;
            }
            let vals: Vec<f64> = parts.iter().filter_map(|p| p.trim().parse().ok()).collect();
            if vals.len() != 4 {
                return None;
            }
            Some([
                vals[0].clamp(0.0, 1.0),
                vals[1].clamp(0.0, 1.0),
                vals[2].clamp(0.0, 1.0),
                vals[3].clamp(0.0, 1.0),
            ])
        }
    }
}

/// Crop a normalized bbox [x, y, w, h] (0..1) out of the full-res image,
/// returning an RGBA image.
fn crop_bbox(full_img: &image::DynamicImage, bbox: &[f64; 4]) -> RgbaImage {
    let w = full_img.width();
    let h = full_img.height();
    let px = (bbox[0] * w as f64).round() as u32;
    let py = (bbox[1] * h as f64).round() as u32;
    let pw = ((bbox[2] * w as f64).round() as u32).max(1);
    let ph = ((bbox[3] * h as f64).round() as u32).max(1);
    // Clamp to image bounds.
    let px = px.min(w.saturating_sub(1));
    let py = py.min(h.saturating_sub(1));
    let pw = pw.min(w - px);
    let ph = ph.min(h - py);
    // crop_imm returns a new image (doesn't mutate the source), so there's no
    // unused-must-use warning.
    full_img.crop_imm(px, py, pw, ph).to_rgba8()
}

/// Leniently parse a JSON vote from the model's text response. Strips
/// ```json fences and trailing/leading prose, then parses.
fn parse_vote(text: &str) -> Option<ZoomVote> {
    let trimmed = text.trim();
    // Strip ```json ... ``` fences if present.
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    let body = body.strip_suffix("```").unwrap_or(body).trim();
    // Try a direct parse first.
    if let Ok(v) = serde_json::from_str::<ZoomVote>(body) {
        return Some(v);
    }
    // Fall back to extracting the first {...} block.
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<ZoomVote>(&body[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::agent::image_tools::MockDescriber;
    use std::sync::Arc;

    /// Build a tiny valid PNG (4x4 red pixel) as bytes.
    fn tiny_png_bytes() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn tiny_png_data_url() -> String {
        bytes_to_data_url("image/png", &tiny_png_bytes())
    }

    #[test]
    fn parse_detail_level_defaults_to_auto() {
        assert_eq!(parse_detail_level(&None), DetailLevel::Auto);
        assert_eq!(
            parse_detail_level(&Some("overview".into())),
            DetailLevel::Overview
        );
        assert_eq!(parse_detail_level(&Some("FINE".into())), DetailLevel::Fine);
        assert_eq!(parse_detail_level(&Some("bogus".into())), DetailLevel::Auto);
    }

    #[test]
    fn parse_region_named_and_bbox() {
        assert_eq!(parse_region("top-right"), Some([0.5, 0.0, 0.5, 0.5]));
        assert_eq!(parse_region("center"), Some([0.25, 0.25, 0.5, 0.5]));
        assert_eq!(parse_region("0.1,0.2,0.3,0.4"), Some([0.1, 0.2, 0.3, 0.4]));
        assert_eq!(parse_region("garbage"), None);
        assert_eq!(parse_region("1,2,3"), None); // only 3 parts
    }

    #[test]
    fn parse_vote_strips_fences_and_extracts_json() {
        let v = parse_vote("```json\n{\"action\":\"done\",\"answer\":\"hello\"}\n```");
        assert_eq!(v.unwrap().action, "done");

        let v = parse_vote("prose before {\"action\":\"zoom\",\"box\":[0.1,0.2,0.3,0.4]} after");
        let v = v.unwrap();
        assert_eq!(v.action, "zoom");
        // C1 regression guard: the "box" key must deserialize into box_.
        assert_eq!(v.box_, Some(vec![0.1, 0.2, 0.3, 0.4]));

        assert!(parse_vote("no json here").is_none());
    }

    #[tokio::test]
    async fn decode_fail_falls_back_to_single_pass() {
        // Corrupt bytes â†’ decode fails â†’ single overview call + warning.
        let mock = Arc::new(MockDescriber::new("overview answer"));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let result = analyze_with_zoom(
            vision.as_ref(),
            &tiny_png_data_url(),
            b"not an image",
            ImageTool::ImageAnalysis,
            &PromptArgs::default(),
            DetailLevel::Fine,
            None,
            3,
        )
        .await
        .unwrap();
        assert_eq!(result.rounds, 0);
        assert_eq!(result.markdown, "overview answer");
        assert!(result.warnings.iter().any(|w| w.contains("decode failed")));
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn overview_detail_level_is_single_pass() {
        let mock = Arc::new(MockDescriber::new("overview answer"));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let bytes = tiny_png_bytes();
        let result = analyze_with_zoom(
            vision.as_ref(),
            &tiny_png_data_url(),
            &bytes,
            ImageTool::ImageAnalysis,
            &PromptArgs::default(),
            DetailLevel::Overview,
            None,
            3,
        )
        .await
        .unwrap();
        assert_eq!(result.rounds, 0);
        assert!(result.warnings.is_empty());
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn region_crop_is_single_pass_on_crop() {
        let mock = Arc::new(MockDescriber::new("cropped answer"));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let bytes = tiny_png_bytes();
        let result = analyze_with_zoom(
            vision.as_ref(),
            &tiny_png_data_url(),
            &bytes,
            ImageTool::ImageAnalysis,
            &PromptArgs::default(),
            DetailLevel::Fine,
            Some("top-right"),
            3,
        )
        .await
        .unwrap();
        assert_eq!(result.rounds, 0);
        assert_eq!(result.markdown, "cropped answer");
        assert_eq!(result.regions.len(), 1);
        // The model was called with a cropped data URL (still image/png).
        let url = &mock.calls.lock().unwrap()[0].0;
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn zoom_loop_done_on_first_vote() {
        // The model immediately votes done â†’ rounds=1, answer returned.
        let mock = Arc::new(MockDescriber::new(
            "{\"action\":\"done\",\"answer\":\"the answer\",\"confidence\":0.95}",
        ));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let bytes = tiny_png_bytes();
        let result = analyze_with_zoom(
            vision.as_ref(),
            &tiny_png_data_url(),
            &bytes,
            ImageTool::ImageAnalysis,
            &PromptArgs::default(),
            DetailLevel::Fine,
            None,
            3,
        )
        .await
        .unwrap();
        assert_eq!(result.rounds, 1);
        assert_eq!(result.markdown, "the answer");
        assert_eq!(result.confidence, Some(0.95));
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn zoom_loop_zoom_then_done() {
        // C2 regression guard: the model votes zoom (with a box) then done.
        // Round 0: overview â†’ vote zoom. Round 1: crop â†’ vote done.
        // The mock returns the zoom vote first, then the done vote.
        let calls = Arc::new(std::sync::Mutex::new(0u32));
        let vision: Arc<dyn ImageDescriber> = Arc::new(CountingDescriber {
            calls: calls.clone(),
        });
        let bytes = tiny_png_bytes();
        let result = analyze_with_zoom(
            vision.as_ref(),
            &tiny_png_data_url(),
            &bytes,
            ImageTool::ImageAnalysis,
            &PromptArgs::default(),
            DetailLevel::Fine,
            None,
            3,
        )
        .await
        .unwrap();
        // Two vision calls: overview (zoom vote) + crop (done vote).
        assert_eq!(*calls.lock().unwrap(), 2, "overview + one crop");
        assert_eq!(result.rounds, 2);
        assert_eq!(result.markdown, "final answer");
        // The zoom vote's region was recorded (crop succeeded).
        assert_eq!(result.regions.len(), 1);
    }

    /// A mock that returns a zoom vote on the first call and a done vote on the
    /// second â€” exercises the zoomâ†’cropâ†’done path (C1 + C2).
    struct CountingDescriber {
        calls: Arc<std::sync::Mutex<u32>>,
    }

    #[async_trait::async_trait]
    impl ImageDescriber for CountingDescriber {
        async fn describe_image(&self, _image_url: &str, _prompt: &str) -> Result<String> {
            let mut n = self.calls.lock().unwrap();
            *n += 1;
            if *n == 1 {
                // First call (overview): vote to zoom into the top-right.
                Ok(
                    "{\"action\":\"zoom\",\"box\":[0.5,0.0,0.5,0.5],\"region\":\"top-right\"}"
                        .into(),
                )
            } else {
                // Second call (crop): done with an answer.
                Ok("{\"action\":\"done\",\"answer\":\"final answer\",\"confidence\":0.9}".into())
            }
        }
    }
}
