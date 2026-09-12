# vision-mcp spec — for native reimplementation

Source: https://github.com/Pelican0126/vision-mcp (TypeScript MCP server, MIT-style).
Fetched 2026-11 from `main` branch: README.md, src/tools/definitions.ts, src/prompts.ts.

## Architecture (theirs)

- An MCP server (stdio) that hands images to an OpenAI-compatible vision
  backend and returns **text + machine-readable metadata** to the host.
- One shared call path; each tool injects its own prompt + fixed markdown
  section headers.
- **Agentic auto-zoom**: for `detail_level` != overview, a loop grids the
  image → model votes which region matters → server crops that region from
  the FULL-RES original → re-reads it → early-exits when confident. Crops
  always come from the full-resolution original (never the downsampled
  overview), so zoom recovers detail. Max rounds configurable (default 3).
- **Dual output**: `content` (structured markdown) + `structuredContent`
  (confidence / regions / rounds / warnings / provider / model).
- Media input: local path (allowlisted dirs), `file://`, `http(s)://` (with
  SSRF check + download→data URI by default), `data:` URI, `"clipboard"`
  (OS clipboard read), `"latest"` (newest in a drop dir).
- Security: local-path allowlist, URL download + SSRF check (no blind
  passthrough), magic-byte validation, size caps (10 MB image / 50 MB video).

## Tools (8 total — we implement 7, skip video)

Common params (on every tool):
- `detail_level`: enum `overview` | `normal` | `fine` | `auto` (default `auto`).
  overview = single fast pass; normal/fine/auto drive the zoom loop (auto
  zooms only when needed, early-exits when clear).
- `question`: optional string — the user's specific question.
- `region`: optional string — restrict to a region; either a named region
  like `"top-right"` or a bbox `"x,y,w,h"` (0–1 normalized).
- `thinking`: optional bool — enable the backend's reasoning/thinking mode.

### 1. ui_to_artifact  →  image_ui_to_artifact
- Purpose: UI screenshot → code or spec.
- media: image (single `image` field).
- Extra params: `target` enum `code` | `spec` (optional, default code);
  `framework` string optional (e.g. react/vue/html).
- defaultThinking: true.
- Prompt sections: `## UI 概述` / `## 代码` (or spec) / `## 备注`.

### 2. extract_text_from_screenshot  →  image_extract_text
- Purpose: verbatim OCR (preserve layout/whitespace), zoom for tiny text.
- media: image.
- Extra params: `lang_hint` string optional (e.g. "zh", "中文").
- defaultThinking: false.
- Prompt sections: `## 提取文本` / `## 备注`.

### 3. diagnose_error_screenshot  →  image_diagnose_error
- Purpose: error/exception diagnosis → root cause / verbatim / location / fix.
- media: image.
- Extra params: `code_context` string optional (relevant code/snippet).
- defaultThinking: true.
- Prompt sections: `## 根因` / `## 原文(逐字)` / `## 位置` / `## 修复步骤`.

### 4. understand_technical_diagram  →  image_understand_diagram
- Purpose: architecture/flow/UML/ER/sequence diagrams.
- media: image.
- No extra params beyond common.
- defaultThinking: true.
- Prompt sections: `## 概述` / `## 结构` / `## 关键关系` / `## 备注`.

### 5. analyze_data_visualization  →  image_analyze_chart
- Purpose: charts/dashboards — read values, trends, outliers.
- media: image.
- No extra params beyond common.
- defaultThinking: true.
- Prompt sections: `## 图表概述` / `## 数据` / `## 备注`.

### 6. ui_diff_check  →  image_ui_diff
- Purpose: compare two UI screenshots (A before / B after).
- media: twoImages — `image_a` (before) + `image_b` (after), both required.
- Extra params: `focus` string optional (area/element to focus on).
- defaultThinking: true.
- Prompt sections: `## 差异概述` / `## 详情`.

### 7. image_analysis  →  image_analysis
- Purpose: generic image understanding (fallback).
- media: image.
- No extra params beyond common.
- defaultThinking: false.
- Prompt sections: `## 概述` / `## 备注`.

### 8. video_analysis  →  SKIP (video)
- Video understanding (native or ffmpeg frame-sampled). User said skip video.

## Output shape (structuredContent)

Every tool returns:
- `markdown`: string — the structured markdown answer (the `content`).
- `confidence`: number 0–1, optional.
- `rounds`: non-negative int — zoom rounds executed (0 for overview).
- `regions`: optional array of `{ box: number[], note?: string }` — where the
  model looked (normalized bbox).
- `warnings`: string[] — degradations/fallbacks/notices.
- `provider`: string.
- `model`: string.

## Mapping onto our system

- We already have `ImageDescriber` trait + `VisionClient` + `SwappableVision`
  (src/provider/vision.rs) — reuse as the vision backend.
- We already have `load_image_data_url`, `mime_from_ext`, `magic_matches`
  (src/tool/agent/describe_image.rs) — reuse for file→data-URL loading +
  sandbox validation + magic-byte sniff.
- Our tools are native (impl `Tool`), not MCP. Each tool: parse args →
  load image(s) to data URLs → build the tool-specific prompt → call
  `ImageDescriber::describe_image` → return the markdown text (our ToolResult
  is a string `output` + optional `data` JSON; map structuredContent into
  `data`).
- Tool names: prefix `image_` per user instruction. Final names:
  image_ui_to_artifact, image_extract_text, image_diagnose_error,
  image_understand_diagram, image_analyze_chart, image_ui_diff,
  image_analysis.
- Safety: like describe_image, these read a sandbox file + send bytes to the
  vision endpoint over the network → `SafetyLevel::NeedsApproval`.
- Category: `ToolCategory::Agent` (available in all workflow states like the
  old describe_image). Registered only when a vision client is configured.
- The agentic zoom loop requires image decode/crop/re-encode → needs the
  `image` crate (pure-Rust image manipulation). This is the main scope
  decision: full zoom loop vs. single-pass (overview only, detail_level
  accepted but treated as overview).
