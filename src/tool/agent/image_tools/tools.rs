// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The seven `image_*` tools â€” each builds a task-specific prompt and runs it
//! through the agentic zoom loop ([`zoom::analyze_with_zoom`]) with a graceful
//! single-pass fallback when image decode/crop fails.
//!
//! All tools are `NeedsApproval` (read a sandbox file + send its bytes to the
//! vision endpoint over the network) and `ToolCategory::Agent` (available in
//! all workflow states, like the old `describe_image`). Registered only when a
//! vision client is configured.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::vision::ImageDescriber;
use crate::provider::ToolSchema;
use crate::tool::agent::image_tools::load_image_data_url_and_bytes;
use crate::tool::agent::image_tools::prompts::{build_prompt, ImageTool, PromptArgs};
use crate::tool::agent::image_tools::zoom::{analyze_with_zoom, parse_detail_level, ZoomResult};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// The default max zoom rounds (matches the spec's `VISION_MAX_ZOOM_ROUNDS`).
const MAX_ZOOM_ROUNDS: u32 = 3;

/// Common arguments shared by every image tool: `question`, `detail_level`,
/// `region`, `thinking`. `thinking` is accepted for spec-fidelity but is
/// advisory only â€” our vision backend does not honor a reasoning mode.
#[derive(Debug, Default, Deserialize)]
struct CommonArgs {
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    detail_level: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    thinking: Option<bool>,
}

/// The shared state every image tool holds: a sandbox (for path validation)
/// and the swappable vision model.
struct ImageToolState {
    sandbox: Sandbox,
    vision: Arc<dyn ImageDescriber>,
}

impl ImageToolState {
    fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self { sandbox, vision }
    }

    /// Whether a vision model is configured. The `image_*` tools are always
    /// REGISTERED (against the shared swappable slot, so Settings can enable
    /// vision without rebuilding registries) but are only ADVERTISED when the
    /// slot is filled — otherwise every call would return "not configured",
    /// and the seven schemas would cost ~640 tokens per request to say so.
    fn vision_ready(&self) -> bool {
        self.vision.is_configured()
    }
}

/// Run one image through the zoom loop and return a `ToolResult` carrying the
/// markdown answer + structured metadata (`confidence`, `rounds`, `regions`,
/// `warnings`) in `data`.
async fn run_single(
    state: &ImageToolState,
    path: &str,
    tool: ImageTool,
    common: &CommonArgs,
    extra: std::collections::HashMap<String, String>,
) -> ToolResult {
    let (data_url, bytes) = match load_image_data_url_and_bytes(&state.sandbox, path).await {
        Ok(v) => v,
        Err(e) => return ToolResult::error(format!("{} failed: {e}", tool.name())),
    };
    let args = PromptArgs {
        question: common.question.clone(),
        extra,
    };
    let detail_level = parse_detail_level(&common.detail_level);
    let result: ZoomResult = match analyze_with_zoom(
        state.vision.as_ref(),
        &data_url,
        &bytes,
        tool,
        &args,
        detail_level,
        common.region.as_deref(),
        MAX_ZOOM_ROUNDS,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => return ToolResult::error(format!("{} failed: {e}", tool.name())),
    };
    // `thinking` is advisory â€” our vision backend doesn't honor a reasoning
    // mode, so surface a warning when the caller requested it (so the model
    // knows the flag was accepted but had no effect).
    let mut warnings = result.warnings;
    if common.thinking.unwrap_or(false) {
        warnings.push(
            "thinking=true requested but the vision backend does not honor a reasoning mode".into(),
        );
    }
    ToolResult::success(result.markdown).with_data(json!({
        "confidence": result.confidence,
        "rounds": result.rounds,
        "regions": result.regions.iter().map(|r| json!({
            "box": r.box_,
            "note": r.note,
        })).collect::<Vec<_>>(),
        "warnings": warnings,
    }))
}

/// Build the JSON schema for a single-image tool: `image` + common params +
/// any extra properties.
fn single_image_schema(
    name: &str,
    description: &str,
    extra_props: serde_json::Map<String, serde_json::Value>,
) -> ToolSchema {
    let mut props = serde_json::Map::new();
    props.insert(
        "image".into(),
        json!({
            "type": "string",
            "description": "Path to the image file, relative to the project root (png, jpg, jpeg, gif, webp, bmp)."
        }),
    );
    for (k, v) in extra_props {
        props.insert(k, v);
    }
    props.insert(
        "question".into(),
        json!({"type": "string", "description": "Optional question about the image."}),
    );
    props.insert("detail_level".into(), json!({
        "type": "string",
        "enum": ["overview", "normal", "fine", "auto"],
        "description": "Detail level: overview = single fast pass; normal/fine/auto drive the agentic zoom loop (auto zooms only when needed, early-exits when clear). Default auto."
    }));
    props.insert("region".into(), json!({
        "type": "string",
        "description": "Optional: restrict to a region â€” a named region ('top-left', 'top-right', 'bottom-left', 'bottom-right', 'center') or a bbox 'x,y,w,h' (0..1)."
    }));
    props.insert("thinking".into(), json!({
        "type": "boolean",
        "description": "Advisory: enable the backend's reasoning mode (no-op on our vision backend)."
    }));
    ToolSchema::new(
        name,
        description,
        json!({
            "type": "object",
            "properties": props,
            "required": ["image"]
        }),
    )
}

// â”€â”€ 1. image_ui_to_artifact â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// UI screenshot â†’ code or spec.
pub struct ImageUiToArtifactTool(ImageToolState);

impl ImageUiToArtifactTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct UiToArtifactArgs {
    image: String,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    framework: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageUiToArtifactTool {
    fn name(&self) -> &str {
        "image_ui_to_artifact"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        single_image_schema(
            "image_ui_to_artifact",
            "Convert a UI screenshot into code or a spec. Use this to turn a design mockup or \
             screenshot into an implementation. Set `target` to 'code' (default) or 'spec', and \
             `framework` (e.g. react/vue/html) to guide the output. detail_level drives the \
             agentic zoom loop for small text.",
            {
                let mut m = serde_json::Map::new();
                m.insert(
                    "target".into(),
                    json!({
                        "type": "string",
                        "enum": ["code", "spec"],
                        "description": "Output: 'code' (default) or 'spec'."
                    }),
                );
                m.insert(
                    "framework".into(),
                    json!({
                        "type": "string",
                        "description": "Target framework, e.g. react/vue/html."
                    }),
                );
                m
            },
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: UiToArtifactArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let mut extra = std::collections::HashMap::new();
        if let Some(t) = &args.target {
            extra.insert("target".into(), t.clone());
        }
        if let Some(f) = &args.framework {
            extra.insert("framework".into(), f.clone());
        }
        run_single(
            &self.0,
            &args.image,
            ImageTool::UiToArtifact,
            &args.common,
            extra,
        )
        .await
    }
}

// â”€â”€ 2. image_extract_text â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Verbatim OCR â€” extract all visible text, preserving layout.
pub struct ImageExtractTextTool(ImageToolState);

impl ImageExtractTextTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct ExtractTextArgs {
    image: String,
    #[serde(default)]
    lang_hint: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageExtractTextTool {
    fn name(&self) -> &str {
        "image_extract_text"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        single_image_schema(
            "image_extract_text",
            "Verbatim OCR â€” extract all visible text from a screenshot, preserving layout and \
             whitespace. Use detail_level='fine' for tiny text (the zoom loop crops and re-reads \
             the region). Set `lang_hint` (e.g. 'zh') to guide recognition.",
            {
                let mut m = serde_json::Map::new();
                m.insert(
                    "lang_hint".into(),
                    json!({
                        "type": "string",
                        "description": "Language/script hint, e.g. 'zh' or 'ä¸­æ–‡'."
                    }),
                );
                m
            },
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: ExtractTextArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let mut extra = std::collections::HashMap::new();
        if let Some(l) = &args.lang_hint {
            extra.insert("lang_hint".into(), l.clone());
        }
        run_single(
            &self.0,
            &args.image,
            ImageTool::ExtractText,
            &args.common,
            extra,
        )
        .await
    }
}

// â”€â”€ 3. image_diagnose_error â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Error/exception diagnosis â†’ root cause / verbatim / location / fix.
pub struct ImageDiagnoseErrorTool(ImageToolState);

impl ImageDiagnoseErrorTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct DiagnoseErrorArgs {
    image: String,
    #[serde(default)]
    code_context: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageDiagnoseErrorTool {
    fn name(&self) -> &str {
        "image_diagnose_error"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        single_image_schema(
            "image_diagnose_error",
            "Diagnose an error/exception from a screenshot â€” root cause, verbatim error text, \
             location, and concrete fix steps. Pass `code_context` (relevant code/snippet) to \
             improve the diagnosis.",
            {
                let mut m = serde_json::Map::new();
                m.insert(
                    "code_context".into(),
                    json!({
                        "type": "string",
                        "description": "Relevant code/snippet to help diagnose the error."
                    }),
                );
                m
            },
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: DiagnoseErrorArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let mut extra = std::collections::HashMap::new();
        if let Some(c) = &args.code_context {
            extra.insert("code_context".into(), c.clone());
        }
        run_single(
            &self.0,
            &args.image,
            ImageTool::DiagnoseError,
            &args.common,
            extra,
        )
        .await
    }
}

// â”€â”€ 4. image_understand_diagram â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Understand a technical diagram (architecture/flow/UML/ER/sequence).
pub struct ImageUnderstandDiagramTool(ImageToolState);

impl ImageUnderstandDiagramTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct UnderstandDiagramArgs {
    image: String,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageUnderstandDiagramTool {
    fn name(&self) -> &str {
        "image_understand_diagram"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        single_image_schema(
            "image_understand_diagram",
            "Understand a technical diagram (architecture, flow, UML, ER, sequence) â€” overview, \
             structure, key relationships. Use this for any schematic or diagram the main model \
             cannot parse.",
            serde_json::Map::new(),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: UnderstandDiagramArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        run_single(
            &self.0,
            &args.image,
            ImageTool::UnderstandDiagram,
            &args.common,
            std::collections::HashMap::new(),
        )
        .await
    }
}

// â”€â”€ 5. image_analyze_chart â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Read charts/dashboards â€” values, trends, outliers.
pub struct ImageAnalyzeChartTool(ImageToolState);

impl ImageAnalyzeChartTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct AnalyzeChartArgs {
    image: String,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageAnalyzeChartTool {
    fn name(&self) -> &str {
        "image_analyze_chart"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        single_image_schema(
            "image_analyze_chart",
            "Read a chart or dashboard â€” extract values, series, labels, trends, and outliers. \
             Use this for any data visualization the main model cannot read.",
            serde_json::Map::new(),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: AnalyzeChartArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        run_single(
            &self.0,
            &args.image,
            ImageTool::AnalyzeChart,
            &args.common,
            std::collections::HashMap::new(),
        )
        .await
    }
}

// â”€â”€ 6. image_ui_diff â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Compare two UI screenshots (A before / B after).
pub struct ImageUiDiffTool(ImageToolState);

impl ImageUiDiffTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct UiDiffArgs {
    image_a: String,
    image_b: String,
    #[serde(default)]
    focus: Option<String>,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageUiDiffTool {
    fn name(&self) -> &str {
        "image_ui_diff"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        let mut props = serde_json::Map::new();
        props.insert(
            "image_a".into(),
            json!({
                "type": "string",
                "description": "Path to the 'before' (A) image, relative to the project root."
            }),
        );
        props.insert(
            "image_b".into(),
            json!({
                "type": "string",
                "description": "Path to the 'after' (B) image, relative to the project root."
            }),
        );
        props.insert(
            "focus".into(),
            json!({
                "type": "string",
                "description": "Optional area/element to focus the comparison on."
            }),
        );
        props.insert(
            "question".into(),
            json!({"type": "string", "description": "Optional question about the diff."}),
        );
        props.insert(
            "detail_level".into(),
            json!({
                "type": "string",
                "enum": ["overview", "normal", "fine", "auto"],
                "description": "Detail level (applied to both images). Default auto."
            }),
        );
        props.insert("region".into(), json!({
            "type": "string",
            "description": "Optional: restrict to a region (named or bbox 'x,y,w,h' 0..1), applied to both images."
        }));
        props.insert("thinking".into(), json!({
            "type": "boolean",
            "description": "Advisory: enable the backend's reasoning mode (no-op on our vision backend)."
        }));
        ToolSchema::new(
            "image_ui_diff",
            "Compare two UI screenshots (A=before, B=after) and describe what changed â€” a diff \
             summary plus per-change details. Use this to verify a UI change or spot a regression.",
            json!({
                "type": "object",
                "properties": props,
                "required": ["image_a", "image_b"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: UiDiffArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Load both images to data URLs. Both are sent to the vision model in
        // ONE call (via describe_images) so the model actually sees both
        // screenshots together and can compare them â€” not two independent
        // descriptions concatenated.
        let (url_a, _bytes_a) =
            match load_image_data_url_and_bytes(&self.0.sandbox, &args.image_a).await {
                Ok(v) => v,
                Err(e) => return ToolResult::error(format!("image_ui_diff failed: {e}")),
            };
        let (url_b, _bytes_b) =
            match load_image_data_url_and_bytes(&self.0.sandbox, &args.image_b).await {
                Ok(v) => v,
                Err(e) => return ToolResult::error(format!("image_ui_diff failed: {e}")),
            };
        let mut extra = std::collections::HashMap::new();
        if let Some(f) = &args.focus {
            extra.insert("focus".into(), f.clone());
        }
        let prompt_args = PromptArgs {
            question: args.common.question.clone(),
            extra,
        };
        let prompt = build_prompt(ImageTool::UiDiff, &prompt_args);
        let image_urls = vec![url_a, url_b];
        let markdown = match self.0.vision.describe_images(&image_urls, &prompt).await {
            Ok(m) => m,
            Err(e) => return ToolResult::error(format!("image_ui_diff failed: {e}")),
        };
        let mut warnings: Vec<String> = Vec::new();
        if args.common.thinking.unwrap_or(false) {
            warnings.push(
                "thinking=true requested but the vision backend does not honor a reasoning mode"
                    .into(),
            );
        }
        ToolResult::success(markdown).with_data(json!({
            "confidence": null,
            "rounds": 0,
            "regions": [],
            "warnings": warnings,
        }))
    }
}

// â”€â”€ 7. image_analysis â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€â”€

/// Generic image understanding (the fallback).
pub struct ImageAnalysisTool(ImageToolState);

impl ImageAnalysisTool {
    /// Create the tool with a sandbox + the shared vision model.
    pub fn new(sandbox: Sandbox, vision: Arc<dyn ImageDescriber>) -> Self {
        Self(ImageToolState::new(sandbox, vision))
    }
}

#[derive(Debug, Deserialize)]
struct ImageAnalysisArgs {
    image: String,
    #[serde(flatten)]
    common: CommonArgs,
}

#[async_trait]
impl Tool for ImageAnalysisTool {
    fn name(&self) -> &str {
        "image_analysis"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }
    fn available(&self) -> bool {
        self.0.vision_ready()
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("image")
    }
    fn schema(&self) -> ToolSchema {
        single_image_schema(
            "image_analysis",
            "Generic image understanding â€” describe an image or answer a question about it. Use \
             this as the fallback when no task-specific image tool fits. Pass `question` to ask \
             something specific, or omit it for a detailed description.",
            serde_json::Map::new(),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: ImageAnalysisArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        run_single(
            &self.0,
            &args.image,
            ImageTool::ImageAnalysis,
            &args.common,
            std::collections::HashMap::new(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::agent::image_tools::MockDescriber;
    use crate::tool::agent::sandbox::Sandbox;
    use image::{ImageFormat, RgbaImage};
    use std::io::Cursor;

    /// Write a tiny valid PNG into a tempdir sandbox.
    fn make_sandbox_with_png() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 255, 255]));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        std::fs::write(dir.path().join("pic.png"), &buf.into_inner()).unwrap();
        (dir, sandbox)
    }

    macro_rules! assert_tool_meta {
        ($tool:expr, $name:expr) => {{
            // Bind once so a value expression (which may move its inputs) is
            // evaluated only once, not once per assertion.
            let tool = $tool;
            assert_eq!(tool.name(), $name);
            assert_eq!(tool.category(), ToolCategory::Agent);
            assert_eq!(tool.safety(), SafetyLevel::NeedsApproval);
            let schema = tool.schema();
            assert_eq!(schema.name, $name);
        }};
    }

    #[test]
    fn schemas_have_required_image_field() {
        let (_d, sandbox) = make_sandbox_with_png();
        let vision: Arc<dyn ImageDescriber> = Arc::new(MockDescriber::new("x"));
        for tool in [
            Box::new(ImageUiToArtifactTool::new(sandbox.clone(), vision.clone())) as Box<dyn Tool>,
            Box::new(ImageExtractTextTool::new(sandbox.clone(), vision.clone())) as Box<dyn Tool>,
            Box::new(ImageDiagnoseErrorTool::new(sandbox.clone(), vision.clone())) as Box<dyn Tool>,
            Box::new(ImageUnderstandDiagramTool::new(
                sandbox.clone(),
                vision.clone(),
            )) as Box<dyn Tool>,
            Box::new(ImageAnalyzeChartTool::new(sandbox.clone(), vision.clone())) as Box<dyn Tool>,
            Box::new(ImageAnalysisTool::new(sandbox.clone(), vision.clone())) as Box<dyn Tool>,
        ] {
            let params = tool.schema().parameters;
            let required = params["required"].as_array().unwrap();
            assert!(
                required.iter().any(|r| r == "image"),
                "{} missing required image",
                tool.name()
            );
        }
    }

    #[test]
    fn ui_diff_schema_requires_both_images() {
        let (_d, sandbox) = make_sandbox_with_png();
        let vision: Arc<dyn ImageDescriber> = Arc::new(MockDescriber::new("x"));
        let tool = ImageUiDiffTool::new(sandbox, vision);
        assert_eq!(tool.name(), "image_ui_diff");
        assert_eq!(tool.category(), ToolCategory::Agent);
        assert_eq!(tool.safety(), SafetyLevel::NeedsApproval);
        let schema = tool.schema();
        assert_eq!(schema.name, "image_ui_diff");
        let required = schema.parameters["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "image_a"));
        assert!(required.iter().any(|r| r == "image_b"));
    }

    #[test]
    fn all_seven_tool_names_and_meta() {
        let (_d, sandbox) = make_sandbox_with_png();
        let vision: Arc<dyn ImageDescriber> = Arc::new(MockDescriber::new("x"));
        assert_tool_meta!(
            ImageUiToArtifactTool::new(sandbox.clone(), vision.clone()),
            "image_ui_to_artifact"
        );
        assert_tool_meta!(
            ImageExtractTextTool::new(sandbox.clone(), vision.clone()),
            "image_extract_text"
        );
        assert_tool_meta!(
            ImageDiagnoseErrorTool::new(sandbox.clone(), vision.clone()),
            "image_diagnose_error"
        );
        assert_tool_meta!(
            ImageUnderstandDiagramTool::new(sandbox.clone(), vision.clone()),
            "image_understand_diagram"
        );
        assert_tool_meta!(
            ImageAnalyzeChartTool::new(sandbox.clone(), vision.clone()),
            "image_analyze_chart"
        );
        assert_tool_meta!(
            ImageUiDiffTool::new(sandbox.clone(), vision.clone()),
            "image_ui_diff"
        );
        assert_tool_meta!(ImageAnalysisTool::new(sandbox, vision), "image_analysis");
    }

    #[tokio::test]
    async fn image_analysis_execute_success() {
        let (_d, sandbox) = make_sandbox_with_png();
        let mock = Arc::new(MockDescriber::new(
            "{\"action\":\"done\",\"answer\":\"a blue square\",\"confidence\":0.9}",
        ));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let tool = ImageAnalysisTool::new(sandbox, vision);
        let result = tool.execute(json!({"image": "pic.png"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("a blue square"));
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
        // data carries structured metadata.
        assert!(result.data.is_some());
    }

    #[tokio::test]
    async fn image_analysis_execute_errors_on_bad_extension() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        let vision: Arc<dyn ImageDescriber> = Arc::new(MockDescriber::new("unused"));
        let tool = ImageAnalysisTool::new(sandbox, vision);
        let result = tool.execute(json!({"image": "a.txt"})).await;
        assert!(!result.success);
        assert!(result.output.contains("image_analysis failed"));
    }

    #[tokio::test]
    async fn image_analysis_execute_errors_on_invalid_args() {
        let (_d, sandbox) = make_sandbox_with_png();
        let vision: Arc<dyn ImageDescriber> = Arc::new(MockDescriber::new("unused"));
        let tool = ImageAnalysisTool::new(sandbox, vision);
        let result = tool.execute(json!({"nope": 1})).await;
        assert!(!result.success);
        // Sanitized (plan 21118961): names the tool, no raw serde text.
        assert!(
            result
                .output
                .starts_with("Error: The tool 'image_analysis' failed"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("missing field"));
    }

    #[tokio::test]
    async fn image_ui_diff_execute_success() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).unwrap();
        let png_bytes = buf.into_inner();
        std::fs::write(dir.path().join("a.png"), &png_bytes).unwrap();
        std::fs::write(dir.path().join("b.png"), &png_bytes).unwrap();
        // The mock returns a canned diff answer; image_ui_diff sends BOTH images
        // in one describe_images call (not two separate describe_image calls).
        let mock = Arc::new(MockDescriber::new("## Diff Summary\nthe button moved"));
        let vision: Arc<dyn ImageDescriber> = mock.clone();
        let tool = ImageUiDiffTool::new(sandbox, vision);
        let result = tool
            .execute(json!({"image_a": "a.png", "image_b": "b.png"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("Diff Summary"));
        // Exactly one vision call (the multi-image describe_images path).
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
        // The call carried both image URLs (joined with || by the mock).
        let url = &mock.calls.lock().unwrap()[0].0;
        assert!(
            url.contains("||"),
            "describe_images should receive both URLs"
        );
    }
}
