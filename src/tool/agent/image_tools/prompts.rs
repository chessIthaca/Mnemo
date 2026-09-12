// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-tool prompt builders for the `image_*` tools.
//!
//! Each tool shares one call path (load image â†’ vision model) but injects its
//! own prompt + fixed markdown section headers. Principles baked into every
//! prompt: answer comprehensively in ONE pass (the zoom loop is stateless
//! across calls), follow the section headers exactly, transcribe text
//! verbatim where relevant, and write "unknown" rather than guess.

use std::collections::HashMap;

/// Which image tool's prompt to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageTool {
    UiToArtifact,
    ExtractText,
    DiagnoseError,
    UnderstandDiagram,
    AnalyzeChart,
    UiDiff,
    ImageAnalysis,
}

impl ImageTool {
    /// The tool's snake_case name (matches the `Tool::name()` impl).
    pub(crate) fn name(self) -> &'static str {
        match self {
            ImageTool::UiToArtifact => "image_ui_to_artifact",
            ImageTool::ExtractText => "image_extract_text",
            ImageTool::DiagnoseError => "image_diagnose_error",
            ImageTool::UnderstandDiagram => "image_understand_diagram",
            ImageTool::AnalyzeChart => "image_analyze_chart",
            ImageTool::UiDiff => "image_ui_diff",
            ImageTool::ImageAnalysis => "image_analysis",
        }
    }
}

/// Tool-specific extras passed to the prompt builder (target, framework, lang
/// hint, code context, focus â€” whichever apply to the tool).
#[derive(Debug, Default, Clone)]
pub(crate) struct PromptArgs {
    /// The user's specific question about the image.
    pub question: Option<String>,
    /// Tool-specific extras keyed by field name (e.g. "framework", "lang_hint").
    pub extra: HashMap<String, String>,
}

/// Shared preamble â€” the quality principles every prompt leads with.
const PREAMBLE: &str = "Answer comprehensively in one pass. Follow the section headers exactly. \
Transcribe text verbatim where relevant. Write \"unknown\" rather than guess.";

/// Build the task-specific prompt for a tool, injecting the user's question +
/// extras and the fixed markdown section headers.
pub(crate) fn build_prompt(tool: ImageTool, args: &PromptArgs) -> String {
    let extra_line = |label: &str, key: &str| -> String {
        args.extra
            .get(key)
            .filter(|v| !v.trim().is_empty())
            .map(|v| format!("{label}: {}\n", v.trim()))
            .unwrap_or_default()
    };
    let question_line = || -> String {
        args.question
            .as_ref()
            .filter(|q| !q.trim().is_empty())
            .map(|q| format!("Question: {}\n", q.trim()))
            .unwrap_or_default()
    };

    let task = match tool {
        ImageTool::UiToArtifact => {
            let target = args
                .extra
                .get("target")
                .map(|s| s.as_str())
                .unwrap_or("code");
            format!(
                "Task: Convert this UI screenshot into {}.\n{}{}",
                if target == "spec" { "a spec" } else { "code" },
                extra_line("Framework", "framework"),
                question_line(),
            ) + "\nRespond with these sections:\n\
               ## UI Overview\n(what the UI is and its main regions)\n\
               ## Code\n(the implementation; use the requested framework)\n\
               ## Notes\n(assumptions, edge cases, anything unclear)"
        }
        ImageTool::ExtractText => {
            format!(
                "Task: Verbatim OCR â€” extract all visible text, preserving layout and whitespace.\n{}{}",
                extra_line("Language hint", "lang_hint"),
                question_line(),
            ) + "\nRespond with these sections:\n\
               ## Extracted Text\n(the text, verbatim, preserving layout)\n\
               ## Notes\n(anything illegible or ambiguous)"
        }
        ImageTool::DiagnoseError => {
            format!(
                "Task: Diagnose the error/exception shown.\n{}{}",
                extra_line("Relevant code/context", "code_context"),
                question_line(),
            ) + "\nRespond with these sections:\n\
               ## Root Cause\n## Verbatim Error\n(the exact text, verbatim)\n\
               ## Location\n(file, line, component if visible)\n\
               ## Fix Steps\n(concrete, actionable)"
        }
        ImageTool::UnderstandDiagram => {
            format!(
                "Task: Understand this technical diagram (architecture/flow/UML/ER/sequence).\n{}",
                question_line(),
            ) + "\nRespond with these sections:\n\
               ## Overview\n## Structure\n(the components and their roles)\n\
               ## Key Relationships\n(how the parts connect)\n\
               ## Notes\n(anything ambiguous)"
        }
        ImageTool::AnalyzeChart => {
            format!(
                "Task: Read this chart/dashboard â€” extract values, trends, outliers.\n{}",
                question_line(),
            ) + "\nRespond with these sections:\n\
               ## Chart Overview\n## Data\n(the values: series/labels/numbers; trends if visible)\n\
               ## Notes\n(anything unclear)"
        }
        ImageTool::UiDiff => {
            format!(
                "Task: Compare two UI screenshots (first=A/before, second=B/after); describe what changed.\n{}{}",
                extra_line("Focus area", "focus"),
                question_line(),
            ) + "\nRespond with these sections:\n\
               ## Diff Summary\n(overview: what changed + Aâ†’B direction)\n\
               ## Details\n(each change: what + where)"
        }
        ImageTool::ImageAnalysis => {
            let q = args
                .question
                .as_ref()
                .filter(|q| !q.trim().is_empty())
                .map(|q| format!("Question: {}\n", q.trim()))
                .unwrap_or_else(|| "Question: Describe this image in detail.\n".to_string());
            format!("Task: Generic image understanding.\n{}", q)
                + "\nRespond with these sections:\n\
                   ## Overview\n(what the image shows)\n\
                   ## Notes\n(anything notable)"
        }
    };

    format!("{PREAMBLE}\n\n{task}")
}

/// The control prompt for the agentic zoom loop. The model either asks to zoom
/// into one of the server-provided regions, or declares it is confident and
/// answers. Carries the full task prompt (with its section headers) so the
/// model knows the expected answer format, then appends the vote instruction â€”
/// no contradictory "respond with sections" vs "ONLY JSON" split.
pub(crate) fn zoom_control_prompt(task_prompt: &str, region_labels: &[String]) -> String {
    let labels = region_labels.join(", ");
    format!(
        "{task_prompt}\n\n\
         You are zooming into this image to answer the task. Available regions: {labels}.\n\
         **If you are NOT at least 100% confident you can answer, pick a region to zoom into; \
         otherwise answer following the section headers above.**\n\
         Respond with ONLY a JSON object on one line, no prose:\n\
         {{\"action\":\"zoom\"|\"done\",\"region\":<region label to zoom into>,\
         \"box\":[x,y,w,h],\"confidence\":0.0-1.0,\"answer\":<your answer if done>}}\n\
         box is a normalized bbox [x,y,w,h] in 0..1 (x,y = top-left). \
         If action=\"zoom\", region + box are required and answer is empty. \
         If action=\"done\", answer is required (use the section headers above) and region/box are omitted."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(question: Option<&str>) -> PromptArgs {
        PromptArgs {
            question: question.map(String::from),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn each_tool_prompt_has_preamble_and_sections() {
        for tool in [
            ImageTool::UiToArtifact,
            ImageTool::ExtractText,
            ImageTool::DiagnoseError,
            ImageTool::UnderstandDiagram,
            ImageTool::AnalyzeChart,
            ImageTool::UiDiff,
            ImageTool::ImageAnalysis,
        ] {
            let p = build_prompt(tool, &args(None));
            assert!(p.contains(PREAMBLE), "{} missing preamble", tool.name());
            assert!(p.contains("## "), "{} missing section headers", tool.name());
        }
    }

    #[test]
    fn ui_to_artifact_includes_target_and_framework() {
        let mut a = args(Some("make it accessible"));
        a.extra.insert("target".into(), "spec".into());
        a.extra.insert("framework".into(), "react".into());
        let p = build_prompt(ImageTool::UiToArtifact, &a);
        assert!(p.contains("spec"));
        assert!(p.contains("Framework: react"));
        assert!(p.contains("Question: make it accessible"));
        assert!(p.contains("## UI Overview"));
        assert!(p.contains("## Code"));
    }

    #[test]
    fn extract_text_includes_lang_hint() {
        let mut a = args(None);
        a.extra.insert("lang_hint".into(), "zh".into());
        let p = build_prompt(ImageTool::ExtractText, &a);
        assert!(p.contains("Language hint: zh"));
        assert!(p.contains("## Extracted Text"));
    }

    #[test]
    fn diagnose_error_includes_code_context() {
        let mut a = args(None);
        a.extra
            .insert("code_context".into(), "fn main() { panic!() }".into());
        let p = build_prompt(ImageTool::DiagnoseError, &a);
        assert!(p.contains("Relevant code/context: fn main()"));
        assert!(p.contains("## Root Cause"));
        assert!(p.contains("## Verbatim Error"));
        assert!(p.contains("## Fix Steps"));
    }

    #[test]
    fn ui_diff_has_two_image_sections() {
        let mut a = args(None);
        a.extra.insert("focus".into(), "the header".into());
        let p = build_prompt(ImageTool::UiDiff, &a);
        assert!(p.contains("Focus area: the header"));
        assert!(p.contains("## Diff Summary"));
        assert!(p.contains("## Details"));
    }

    #[test]
    fn image_analysis_defaults_question_when_absent() {
        let p = build_prompt(ImageTool::ImageAnalysis, &args(None));
        assert!(p.contains("Describe this image in detail."));
        assert!(p.contains("## Overview"));
    }

    #[test]
    fn zoom_control_prompt_is_json_instruction() {
        let task = build_prompt(ImageTool::ImageAnalysis, &args(None));
        let p = zoom_control_prompt(&task, &["top-left".into(), "center".into()]);
        assert!(p.contains("top-left, center"));
        assert!(p.contains(&task));
        assert!(p.contains("\"action\":\"zoom\"|\"done\""));
        assert!(p.contains("\"box\":[x,y,w,h]"));
    }
}
