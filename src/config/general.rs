// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `config.toml` — general preferences (default provider/model, safety, UI,
//! context, per-context model overrides, markdown viewer, embeddings).
//!
//! The file has sections: `[general]` (with optional `[general.vision_model]`
//! and `[general.embedding_model]` sub-tables), `[context]`, `[ui]`,
//! `[models]`, `[markdown]`, `[git]`, `[trace]`, `[shell_filter]` (with
//! optional `[[shell_filter.override]]` entries).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// General preferences from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// The `[general]` section.
    pub general: GeneralSection,
    /// The `[context]` section.
    pub context: ContextConfig,
    /// The `[ui]` section.
    pub ui: UiConfig,
    /// The `[models]` section — per-context model overrides (skill / workflow
    /// state / subagent). Each entry is a [`ModelRef`] naming an endpoint +
    /// model; `None` (the default) means "use the main agent's default model".
    pub models: ModelsConfig,
    /// The `[markdown]` section — the Markdown viewer's skip-directory list.
    pub markdown: MarkdownConfig,
    /// The `[git]` section — which git subcommands are *core operations*
    /// that always force the interactive approval prompt (even in Autonomous
    /// mode or under a matching safety rule). Defaults to `["merge", "push"]`.
    pub git: GitConfig,
    /// The `[trace]` section — the in-memory LLM trace log's memory limits.
    pub trace: TraceConfig,
    /// The `[memory]` section — the memory store's retrieval/indexing scale
    /// knobs (recency decay, per-query cap, derived per-class cap, digest
    /// auto-truncation lengths). Read live by recall + the indexer from the
    /// store's snapshot.
    pub memory: crate::memory::MemorySearchConfig,
    /// The `[shell_filter]` section — command-aware filtering of shell tool
    /// output before it reaches the LLM transcript (errors and summaries are
    /// always kept; raw output stays in the tool result's `data` field).
    pub shell_filter: ShellFilterConfig,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            general: GeneralSection::default(),
            context: ContextConfig::default(),
            ui: UiConfig::default(),
            models: ModelsConfig::default(),
            markdown: MarkdownConfig::default(),
            git: GitConfig::default(),
            trace: TraceConfig::default(),
            memory: crate::memory::MemorySearchConfig::default(),
            shell_filter: ShellFilterConfig::default(),
        }
    }
}

/// The `[general]` section of `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSection {
    /// Default provider name (matches an endpoint name in `endpoints.toml`).
    pub default_provider: Option<String>,
    /// Default model id.
    pub default_model: Option<String>,
    /// Safety / approval mode.
    pub safety: SafetyMode,
    /// A separate endpoint + model to use for image-to-text when the active
    /// main model is not multimodal (its endpoint's `multimodal` flag, or a
    /// per-model `ModelSpec::multimodal` override). A vision-capable main
    /// model never triggers this fallback — it receives image blocks
    /// directly. The vision endpoint can be a different OpenAI-compatible
    /// source (different base_url + api_key) than the main provider. `None`
    /// when no vision fallback is configured.
    pub vision_model: Option<VisionModel>,
    /// An optional embedding model for memory recall — an OpenAI-compatible
    /// `/v1/embeddings` endpoint (Ollama, OpenAI, vLLM, LM Studio). `None`
    /// means the store uses the built-in deterministic `HashEmbedder`
    /// (offline, keyword-overlap only). When set, recall ranks by genuine
    /// semantic similarity (with a zero-vector fallback if the service is
    /// down, so recall never fails).
    pub embedding_model: Option<EmbeddingModel>,
    /// A bundled in-process embedding model id (e.g. `"all-MiniLM-L6-v2"`)
    /// backed by `fastembed` (ONNX Runtime). When set, recall ranks by genuine
    /// semantic similarity with no external service (no Ollama, no cloud, no
    /// API key) — the model downloads on first use + caches under the app data
    /// dir. Takes precedence over `embedding_model` (the legacy remote path).
    ///
    /// An ABSENT key resolves to the default model
    /// ([`default_bundled_embedding_model`], MiniLM) — semantic recall is the
    /// out-of-box behavior. Note the upgrade implication: a pre-existing
    /// config with no key silently downloads the default model (~90 MB) on
    /// the first startup after upgrading. The sentinel value `"hash"` is the
    /// explicit keyword-only opt-out (the built-in `HashEmbedder`, no
    /// download): TOML has no absent/None distinction once a default exists,
    /// so `"hash"` is how a user says "I really want keyword-only".
    #[serde(default = "default_bundled_embedding_model")]
    pub bundled_embedding_model: Option<String>,
    /// **Laya classifier (opt-in).** When enabled, the app may consult a
    /// running `laya-serve` instance (see [`LayaConfig`]) for fast,
    /// calibrated "System 1" decisions. Disabled by default — an absent or
    /// disabled section changes nothing: no classifier calls, no startup
    /// cost, no new failure modes. Omitted from the saved config while every
    /// field holds its default, so configs that never touched Laya keep no
    /// trace of it (mirrors `[ui.steering_notes]`).
    #[serde(default, skip_serializing_if = "LayaConfig::is_default")]
    pub laya: LayaConfig,
    /// **Token-optimizer levers (opt-in, all default-off; backlog
    /// e4a50d22).** The context levers that cut re-reads and command-output
    /// waste at the tool dispatch layer: delta/skeleton re-reads, semantic
    /// command-output compression, archive/expand progressive disclosure,
    /// compaction survival, the S-F quality score, and the lean-output
    /// nudge. An absent section means every lever off — behavior stays
    /// byte-identical. Omitted from the saved config while every field
    /// holds its default, so untouched configs keep no optimizer trace
    /// (mirrors `[general.laya]`).
    #[serde(default, skip_serializing_if = "OptimizerConfig::is_default")]
    pub optimizer: OptimizerConfig,
    /// **Agent browser inspection (opt-in, security tradeoff).** When `true`,
    /// the app exposes the WebView2 Chrome DevTools Protocol on
    /// `localhost:9222` (the next free port when several instances run) so
    /// the agent's `browser_*` tools can attach to the live
    /// Browser tab's child webview and inspect/control it. Off by default
    /// because the CDP endpoint is **unauthenticated** — any local process on
    /// the machine could connect and run arbitrary JavaScript inside the app's
    /// webview (the same trust level as the app UI: prompts, code, secrets).
    /// Enabling is an explicit acceptance of that risk. Debug builds ignore
    /// this flag and always expose CDP (the port is dev-only there).
    pub enable_browser_inspection: bool,
    /// **CodeGraph (opt-out).** When `true` (the default), the codebase is
    /// parsed into the per-project knowledge graph
    /// (`<root>/.coding/codegraph.db`) in a background task at startup and the
    /// `graph_*` agent tools + Graph tab are available. Set `false` to skip
    /// indexing entirely (the tools and tab report "graph unavailable").
    #[serde(default = "default_codegraph_enabled")]
    pub codegraph: bool,
    /// **Run-All auto-compact (opt-in).** When `true`, a Run-All loop
    /// compacts the main agent's context between items — after each
    /// completed plan resolves and before the next backlog item is
    /// dispatched — so every item starts with a clean summarized context
    /// instead of exhausting the window mid-item overnight. Run-All only:
    /// interactive plan completions never trigger it. A failed or no-op
    /// compaction logs and proceeds (compaction is an optimization, never
    /// a blocker). Default `false`.
    #[serde(default)]
    pub auto_compact_on_plan_complete: bool,
}

/// Configuration for a separate vision model used for image-to-text when the
/// active main model is not multimodal (endpoint flag or per-model override —
/// see [`crate::config::endpoints::ModelSpec::multimodal`]).
///
/// The `endpoint` names an entry in `endpoints.toml` (which may have a
/// different base_url + api_key than the main provider). The `model` is the
/// model id to use at that endpoint (e.g. `qwen-vl`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VisionModel {
    /// The endpoint name (matches an entry in `endpoints.toml`).
    pub endpoint: String,
    /// The model id at that endpoint.
    pub model: String,
}

/// Configuration for the memory embedding model — an OpenAI-compatible
/// `/v1/embeddings` endpoint (Ollama, OpenAI, vLLM, LM Studio) used to produce
/// genuine semantic vectors for memory recall.
///
/// Mirrors [`VisionModel`] but is a distinct type so the two config surfaces
/// cannot be confused. `endpoint` names an entry in `endpoints.toml`; `model`
/// is the embedding model id (e.g. `nomic-embed-text`, `text-embedding-3-small`).
/// `None` (the default) means the store uses the built-in deterministic
/// [`HashEmbedder`](crate::memory::embedder::HashEmbedder) — offline,
/// dependency-free, but semantically blind (keyword-overlap only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmbeddingModel {
    /// The endpoint name (matches an entry in `endpoints.toml`).
    pub endpoint: String,
    /// The embedding model id at that endpoint.
    pub model: String,
}

/// How the Laya backend is provided.
///
/// `external` (the default) keeps the original contract: the user runs
/// `laya-serve` themselves and [`LayaConfig::endpoint`] names its base URL.
/// `managed` has the app own the runtime — the Settings → Classifier section
/// downloads a self-contained install (uv + venv + `laya[serve]` + the
/// checkpoint) and Mnemo starts/stops a local `laya-serve` on `127.0.0.1`
/// whenever Laya is enabled, mirroring the bundled embedding models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LayaMode {
    /// A user-run `laya-serve` at [`LayaConfig::endpoint`] — the default,
    /// and the only mode configs written before it existed know.
    #[default]
    External,
    /// Mnemo downloads, starts, and stops the backend itself
    /// (Settings → Classifier).
    Managed,
}

/// Configuration for the Laya classifier — an opt-in "System 1" decision
/// service (`convaiinnovations/laya`) served by `laya-serve` over HTTP.
///
/// Laya is a fast, calibrated text classifier (not a generator): the app
/// asks it typed questions (choice / score / yes-no) and reads back
/// calibrated probabilities. **Disabled by default** — when
/// [`enabled`](Self::enabled) is false (or the `[general.laya]` section is
/// absent) the app behaves exactly as before: no classifier calls, no
/// startup cost, no new failure modes. In [`managed`](LayaMode::Managed)
/// mode the app runs the server itself (downloaded from Settings); in
/// [`external`](LayaMode::External) mode it talks to a user-run instance at
/// [`endpoint`](Self::endpoint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LayaConfig {
    /// Whether the Laya classifier is enabled. Off by default.
    pub enabled: bool,
    /// Base URL of a running `laya-serve` instance, e.g.
    /// `"http://127.0.0.1:8000"`. Required when `enabled` is true in
    /// [`external`](LayaMode::External) mode; `None` (or blank) means "not
    /// configured" and the classifier stays unavailable. Ignored in
    /// [`managed`](LayaMode::Managed) mode, where the app runs the server
    /// itself.
    pub endpoint: Option<String>,
    /// How the backend is provided — a user-run server
    /// ([`External`](LayaMode::External), the default) or the app-managed
    /// runtime ([`Managed`](LayaMode::Managed)). Omitted from the saved
    /// config while `external`, so untouched configs keep their exact
    /// pre-managed shape.
    #[serde(default, skip_serializing_if = "laya_mode_is_external")]
    pub mode: LayaMode,
    /// The managed-mode checkpoint to serve: `"english"` (the default when
    /// `None`) or `"multilingual"`. Only read in
    /// [`managed`](LayaMode::Managed) mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    /// Opt in to Laya auto-typing of memory records: at `memory_write`
    /// time the record's typed prefix (SPEC/DECISION/BUG/PLAN/HOW/REVIEW)
    /// is classified, and a confident answer may correct the writer's
    /// prefix (confidence-gated — a low-confidence or missing answer keeps
    /// the writer's prefix). Off by default, and meant to be enabled only
    /// against a **fine-tuned** checkpoint: base Laya checkpoints are
    /// near-chance zero-shot on this task and over-confident until
    /// fine-tuned (train on `seed_dataset`'s labeled set — it covers
    /// SPEC/DECISION/BUG/HOW; PLAN/REVIEW ship without seed examples), so
    /// this is a separate opt-in from [`enabled`](Self::enabled). Omitted
    /// from the saved config while false.
    #[serde(default, skip_serializing_if = "laya_flag_off")]
    pub auto_type_memories: bool,
    /// Opt in to Laya FAILURE TRIAGE (backlog 1a4049c1): at every
    /// failure-handling site (the tool-execution cap, the bad-JSON repair
    /// loop, and BOTH provider retry layers) the error text is classified
    /// transient/permanent/needs_user/flaky_test, and a confident answer
    /// steers the harness — a transient READ-ONLY tool failure is re-run by
    /// the harness without a model roundtrip, other classes ride classified
    /// guidance on the fed-back error, and a needs-user/permanent provider
    /// error skips the retry ladder outright. Confidence-gated (≥0.80): a
    /// low-confidence or missing answer keeps the existing rules, and the
    /// flag alone gates every classification + training-log write. Off by
    /// default, and meant to be enabled only against a **fine-tuned**
    /// checkpoint — base Laya checkpoints are near-chance zero-shot on this
    /// task, which is exactly what [`auto_finetune`](Self::auto_finetune)
    /// builds the labeled corpus for. Omitted from the saved config while
    /// false.
    #[serde(default, skip_serializing_if = "laya_flag_off")]
    pub failure_triage: bool,
    /// Opt in to the kNN OVERLAY for failure triage (item 4b): on top of
    /// the Laya checkpoint, a local kNN classifier retrieves the most
    /// similar logged failures from the failure-triage training log
    /// (embedding them with the app's existing embedding backend) and
    /// majority-votes the class, with the vote share as the confidence.
    /// Genuinely online learning — a disposition appended to the log is
    /// retrievable on the next classification, no retraining needed — and
    /// independent of the Laya endpoint: it rides the local embedder,
    /// not `laya-serve`. Still gated by the master
    /// [`failure_triage`](Self::failure_triage) flag and the same 0.80
    /// confidence gate (a below-threshold or tied vote falls back to the
    /// base classifier / the pre-classifier rules). Off by default, and
    /// omitted from the saved config while false.
    #[serde(default, skip_serializing_if = "laya_flag_off")]
    pub failure_triage_knn: bool,
    /// Opt in to the startup failure-triage FINE-TUNE (managed mode only):
    /// at startup the app compares the logged failure corpus against the
    /// last run's marker and, when enough new labeled rows accumulated, runs
    /// the Laya fine-tune on the managed venv and serves the fine-tuned
    /// checkpoint. Off by default, and never blocking startup. A "check"
    /// with no training surface installed (laya 0.3.20 ships none) exports
    /// the labeled dataset and skips cleanly, leaving the marker untouched
    /// so the next startup re-checks. Omitted from the saved config while
    /// false.
    #[serde(default, skip_serializing_if = "laya_flag_off")]
    pub auto_finetune: bool,
}

/// `skip_serializing_if` guard for [`LayaConfig::mode`]: `external` (the
/// default) stays unwritten, mirroring the endpoint field's
/// absence-while-unset behavior.
fn laya_mode_is_external(mode: &LayaMode) -> bool {
    matches!(mode, LayaMode::External)
}

/// `skip_serializing_if` guard for [`LayaConfig`]'s boolean opt-ins
/// (`auto_type_memories`, `failure_triage`, `auto_finetune`): `false` (the
/// default) stays unwritten, so untouched configs keep their exact
/// pre-consumer shape.
fn laya_flag_off(off: &bool) -> bool {
    !*off
}

impl LayaConfig {
    /// True while both fields hold their defaults — the `[general.laya]`
    /// section is then omitted from `config.toml` (mirrors
    /// [`SteeringNotesCfg`]'s empty-table skip), so untouched configs keep no
    /// Laya trace.
    fn is_default(&self) -> bool {
        !self.enabled
            && self.endpoint.is_none()
            && self.mode == LayaMode::External
            && self.checkpoint.is_none()
            && !self.auto_type_memories
            && !self.failure_triage
            && !self.failure_triage_knn
            && !self.auto_finetune
    }
}

/// The `[general.optimizer]` section — the token-optimizer levers (backlog
/// e4a50d22), all default-off. Each flag is an independent opt-in: enabling
/// one lever never turns on another, and a flag-off lever changes nothing
/// (its code path is skipped entirely — tool/dispatch behavior stays
/// byte-identical to the pre-lever build).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OptimizerConfig {
    /// **Lever 1 — delta + skeleton re-reads.** When `true`, `read_files`
    /// serves a signature/import skeleton for re-reads of unchanged files
    /// and a unified diff for changed ones, instead of the full content
    /// every time. First reads and ranged reads still serve full content.
    /// Off by default.
    pub delta_reads: bool,
    /// **Lever 2 — semantic command-output compression.** When `true`,
    /// large shell-tool output from known command families (cargo, npm,
    /// pytest, go) is collapsed to distinct error/warning lines + counts +
    /// exit status before it reaches the context. Off by default.
    pub compress_output: bool,
    /// **Lever 3 — archive/expand progressive disclosure.** When `true`,
    /// tool results above
    /// [`archive_min_chars`](Self::archive_min_chars) are archived in full
    /// to the per-project memory DB and the context carries a preview the
    /// model can expand via the `expand_result` tool — instead of a lossy
    /// head-only truncation. Off by default.
    pub archive: bool,
    /// **Lever 4 — compaction survival.** When `true`, compaction archives
    /// a pre-compaction checkpoint, injects extracted decisions as a
    /// must-preserve block, and appends a heuristic post-compaction digest
    /// (no extra LLM call). Off by default.
    pub compaction_survival: bool,
    /// **Quality score.** When `true`, the S-F context-quality grade rides
    /// the `ContextUsage` events for the ctx popup. Off by default.
    pub quality_score: bool,
    /// **Lean-output nudge.** When `true`, a cache-safe steering note
    /// appended to the volatile tail at
    /// [`lean_output_fill_pct`](Self::lean_output_fill_pct) context fill
    /// nudges the model toward concise visible output. Off by default.
    pub lean_output_nudge: bool,
    /// Minimum length (chars) of a tool result before lever 3 archives
    /// it. Results under the cap pass through the existing ingestion cap
    /// unchanged.
    pub archive_min_chars: usize,
    /// Minimum length (chars) of filtered shell output before lever 2
    /// attempts compression. Smaller outputs pass through untouched.
    pub compress_min_chars: usize,
    /// Context fill percentage that triggers the lean-output nudge.
    pub lean_output_fill_pct: u8,
    /// Requests between lean-output/quality nudges (a cooldown, so a long
    /// session is not nagged on every turn).
    pub nudge_cooldown_requests: u32,
    /// User-extensible command patterns (regexes) eligible for lever 2
    /// compression in addition to the built-in families, e.g.
    /// `"dotnet build"` or `"make .*"`. Matched against the full command
    /// line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compress_extra_commands: Vec<String>,
}

impl OptimizerConfig {
    /// Default `archive_min_chars` — results this large (and up) get
    /// archived by lever 3.
    pub const DEFAULT_ARCHIVE_MIN_CHARS: usize = 20_000;
    /// Default `compress_min_chars` — filtered output this large (and up)
    /// is compressed by lever 2.
    pub const DEFAULT_COMPRESS_MIN_CHARS: usize = 2_000;
    /// Default `lean_output_fill_pct`.
    pub const DEFAULT_LEAN_OUTPUT_FILL_PCT: u8 = 25;
    /// Default `nudge_cooldown_requests`.
    pub const DEFAULT_NUDGE_COOLDOWN_REQUESTS: u32 = 10;

    /// True while every field holds its default — the
    /// `[general.optimizer]` section is then omitted from `config.toml`,
    /// so untouched configs keep no optimizer trace (mirrors
    /// [`LayaConfig::is_default`]).
    fn is_default(&self) -> bool {
        !self.delta_reads
            && !self.compress_output
            && !self.archive
            && !self.compaction_survival
            && !self.quality_score
            && !self.lean_output_nudge
            && self.archive_min_chars == Self::DEFAULT_ARCHIVE_MIN_CHARS
            && self.compress_min_chars == Self::DEFAULT_COMPRESS_MIN_CHARS
            && self.lean_output_fill_pct == Self::DEFAULT_LEAN_OUTPUT_FILL_PCT
            && self.nudge_cooldown_requests == Self::DEFAULT_NUDGE_COOLDOWN_REQUESTS
            && self.compress_extra_commands.is_empty()
    }
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            delta_reads: false,
            compress_output: false,
            archive: false,
            compaction_survival: false,
            quality_score: false,
            lean_output_nudge: false,
            archive_min_chars: Self::DEFAULT_ARCHIVE_MIN_CHARS,
            compress_min_chars: Self::DEFAULT_COMPRESS_MIN_CHARS,
            lean_output_fill_pct: Self::DEFAULT_LEAN_OUTPUT_FILL_PCT,
            nudge_cooldown_requests: Self::DEFAULT_NUDGE_COOLDOWN_REQUESTS,
            compress_extra_commands: Vec::new(),
        }
    }
}

/// A reference to a configured endpoint + model, used by per-context model
/// overrides (the `[models]` section).
///
/// Mirrors [`VisionModel`] but is a distinct type so the two config surfaces
/// cannot be confused. `endpoint` names an entry in `endpoints.toml`; `model`
/// is the model id to use at that endpoint. Resolution (validating the endpoint
/// exists, building the provider) happens at turn time via
/// [`Config::resolve_model_ref`](crate::config::Config::resolve_model_ref).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    /// The endpoint name (matches an entry in `endpoints.toml`).
    pub endpoint: String,
    /// The model id at that endpoint.
    pub model: String,
    /// Optional per-context reasoning-effort override (`"max"`, `"high"`,
    /// `"medium"`, `"low"`, `"minimal"`, or `"off"`). When set, the provider
    /// built for this context runs at this effort instead of the model's own
    /// default chain (`ModelSpec::reasoning_effort` → the endpoint's value →
    /// the app default `"max"`). Ignored on Anthropic-kind endpoints (the
    /// Messages API has no reasoning-effort field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// Per-context model overrides (`[models]` section of `config.toml`).
///
/// Each field is an optional [`ModelRef`] that, when set, makes the matching
/// context run on that endpoint + model instead of the main agent's default.
/// Unset fields fall back to the default model — so a fresh config (or one
/// without a `[models]` section) behaves exactly as before.
///
/// Resolution priority at turn time is: skill override > subagent override >
/// workflow-state override > default — plus a plan-kind override: while the
/// active plan's kind is `bug_fixing` and the workflow is in that plan's
/// active lifecycle (Executing or Reviewing), [`Self::bug_fixing`] wins over
/// the state slot. See [`crate::model_resolver`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ModelsConfig {
    /// Model to use while the workflow is in the `Planning` state.
    pub planning: Option<ModelRef>,
    /// Model to use while the workflow is in the `Executing` state.
    pub executing: Option<ModelRef>,
    /// Model + reasoning effort for the active plan's bug-fixing lifecycle:
    /// while the active (top-of-stack) plan's kind is `bug_fixing` and the
    /// workflow is in the `Executing` or `Reviewing` state, this slot wins
    /// over [`Self::executing`] — so a bug-fixing session can run on a
    /// max-reasoning model while implementation plans keep the executing
    /// slot. Unset (or dangling — the endpoint no longer exists) falls back
    /// to the `executing` override, then the default: today's behavior.
    pub bug_fixing: Option<ModelRef>,
    /// Model for a spawned `role:"reviewer"` subagent — **reviewer-spawn-
    /// only** (2026-12-06 decision): the main agent never runs on it, keeping
    /// the `executing` model through the Reviewing-state closing sequence.
    /// Consumed solely at spawn time by
    /// [`ModelResolver::resolve_reviewer_model`](crate::model_resolver::ModelResolver::resolve_reviewer_model):
    /// unset falls back to the `executing` override (back-compat with
    /// pre-reviewing-slot configs), then `subagent`, then the default.
    pub reviewing: Option<ModelRef>,
    /// Model to use while the workflow is in the `Complete` state.
    pub complete: Option<ModelRef>,
    /// Model to use for subagents (those spawned via `spawn_agent` or the
    /// UI's spawn button with a parent). Falls back to the workflow-state
    /// override, then the default.
    pub subagent: Option<ModelRef>,
    /// Model for compaction summaries — the auto-compaction summary call and
    /// the run-all between-items compact. Unset = the turn's model (today's
    /// behavior: no config, no change). Like [`Self::reviewing`], this slot
    /// is consumed by a dedicated resolver method
    /// ([`ModelResolver::resolve_summarize_model`](crate::model_resolver::ModelResolver::resolve_summarize_model)),
    /// never by the per-turn state chain — a dangling reference (the endpoint
    /// no longer exists) falls back to the turn's model.
    pub summarize: Option<ModelRef>,
    /// Per-skill overrides, keyed by skill name (the `name` field of a
    /// `.coding/skills/<name>.toml` spec). A skill not listed here falls back
    /// to the subagent/state/default chain.
    pub skill: std::collections::HashMap<String, ModelRef>,
}

impl Default for GeneralSection {
    fn default() -> Self {
        Self {
            default_provider: None,
            default_model: None,
            safety: SafetyMode::ApproveEachAction,
            vision_model: None,
            embedding_model: None,
            bundled_embedding_model: default_bundled_embedding_model(),
            laya: LayaConfig::default(),
            optimizer: OptimizerConfig::default(),
            enable_browser_inspection: false,
            codegraph: default_codegraph_enabled(),
            auto_compact_on_plan_complete: false,
        }
    }
}

/// Sentinel `bundled_embedding_model` value meaning "explicit keyword-only
/// opt-out" — the built-in `HashEmbedder`, no bundled model, no download.
/// Used because TOML cannot distinguish an absent key from an explicit
/// default once a serde default exists: absent → MiniLM (the default),
/// `"hash"` → opted out.
pub const EMBEDDING_MODEL_SENTINEL_HASH: &str = "hash";

/// The out-of-box bundled embedding model: MiniLM gives genuine semantic
/// recall (no external service) with a small download on first use. Used as
/// the serde default for `bundled_embedding_model` so existing configs with
/// no key upgrade to semantic recall automatically.
fn default_bundled_embedding_model() -> Option<String> {
    Some("all-MiniLM-L6-v2".into())
}

/// CodeGraph is on by default — indexing runs in the background and never
/// blocks startup, so there is no cost to leaving it enabled. Named fn so the
/// serde default and the `Default` impl agree.
fn default_codegraph_enabled() -> bool {
    true
}

/// How much human approval each mutating action requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum, Default)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum SafetyMode {
    /// Every mutating action (file write, shell, etc.) requires explicit approval.
    #[default]
    ApproveEachAction,
    /// Read tools run automatically; only writes need approval.
    AutoReadApproveWrites,
    /// Mutating actions that only touch files inside the project directory are
    /// auto-approved (no prompt). Actions that can't be statically proven
    /// project-scoped (e.g. `shell`) still require approval.
    AutoApproveProject,
    /// No approval prompts — the agent runs autonomously. Use with care.
    Autonomous,
}

/// Context-management configuration (`[context]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextConfig {
    /// Summarize the conversation when it reaches this fraction of the context window.
    pub summarize_at_fill_rate: f64,
    /// Whether to use aggressive compaction when the conversation exceeds the
    /// hard ceiling (review L4/auto-continuation). When `true` (the default),
    /// the existing compaction in `run_turn` uses `keep_recent=3` instead of
    /// the normal `6` when the pre-compaction token count exceeds
    /// [`hard_ceiling`](Self::hard_ceiling) — preventing fatal
    /// `ContextWindowExceededError` on long sessions where the soft
    /// `summarize_at_fill_rate` trigger ran but didn't shrink enough. Set
    /// `false` to disable (the soft trigger still runs with `keep_recent=6`).
    pub preflight_compact: bool,
    /// Tokens reserved for the model's output when selecting the aggressive
    /// compaction strength (review L4/auto-continuation). When the
    /// pre-compaction token count exceeds `max_tokens - headroom`, compaction
    /// uses `keep_recent=3` instead of `6`. Default 32 000 — generous enough
    /// for a substantial response while leaving room for the system prompt +
    /// tools array the conversation-token count doesn't include.
    pub compact_headroom_tokens: usize,
    /// Total input tokens above which LiteLLM-class proxies drop
    /// whole-conversation prefix caching (hit rate collapses from ~99% to
    /// ~4%). When set, the context manager summarizes before
    /// `ceiling - 32_768` so long sessions stay below the proxy cliff.
    /// Default 340 000; set 0 (or omit) to disable the guard.
    pub proxy_cache_ceiling_tokens: Option<usize>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            summarize_at_fill_rate: 0.3,
            preflight_compact: true,
            compact_headroom_tokens: 32_000,
            proxy_cache_ceiling_tokens: Some(crate::provider::PROXY_CACHE_CEILING_TOKENS),
        }
    }
}

/// Per-kind display toggles for the steering notes that ride in tool output
/// (`[ui.steering_notes]`).
///
/// Each field is an OPTIONAL override: `Some(v)` wins, `None` falls back to
/// the kind's default — resolved by
/// [`UiConfig::effective_steering_note_visible`]. Absent overrides are not
/// serialized; when every field is absent the whole table is omitted from
/// `config.toml`, so a config that never touched a kind stays compact.
///
/// This is a GUI-only display filter, the same contract as
/// [`UiConfig::show_delegation_notes`]: the tool-result TEXT (the model's
/// context, including a note's re-issue escape hatch) is never modified —
/// only the chat ToolCard's rendering drops the note's line.
///
/// The keys mirror `MarkerKind::label()` (`src/agent/steering_stats.rs`, the
/// stable kebab labels surfaced on the wire), transcribed to snake_case, plus
/// `auto_delegated` — the display family covering the two `AUTO-DELEGATED`
/// blocks (the code-graph delegation and the memory delegation, the latter
/// carrying no marker substring of its own).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SteeringNotesCfg {
    /// Show the `AUTO-DELEGATED to the code graph` / `… to memory` block that
    /// `search` / `search_read` prepend when a query is auto-delegated.
    /// Defaults to [`UiConfig::show_delegation_notes`] (hidden unless that
    /// legacy toggle is on); set it explicitly to decouple the two.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_delegated: Option<bool>,
    /// Show the symbol-nudge line ("… is an indexed symbol …") that steers a
    /// symbol-shaped `search` to the graph tools. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_nudge: Option<bool>,
    /// Show the shell TIP ("TIP: for file-content search …") that steers a
    /// content search away from `shell`. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_tip: Option<bool>,
    /// Show the graph-miss line ("No symbols matched …") on an empty graph
    /// lookup. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_miss: Option<bool>,
    /// Show the auto-recall rider ("RECALLED CONTEXT …") appended to a tool
    /// result. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall_rider: Option<bool>,
    /// Show the read nudge ("SYMBOL NUDGE: …") after a file read. Default
    /// true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_nudge: Option<bool>,
    /// Show the literal-engine TIP ("TIP: pattern has no regex
    /// metacharacters …"), which points at `literal: true`. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub literal_tip: Option<bool>,
    /// Show the known-memory-hit note ("known memory hit: …") when a pattern
    /// matches a known backlog id. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_memory_hit: Option<bool>,
    /// Show the consolidation-due note ("working-memory events accumulated
    /// this session"). Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consolidation_due: Option<bool>,
    /// Show the shell-redirect TIP ("TIP: output redirection detected …").
    /// Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell_redirect: Option<bool>,
    /// Show the stale-read note ("Re-read the file …") after a failed
    /// `file_edit`. Default true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_stale_read: Option<bool>,
}

impl SteeringNotesCfg {
    /// True when no kind carries an explicit override (all `None`) — the
    /// `[ui]` field is then omitted from the serialized config entirely.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for SteeringNotesCfg {
    /// Every override absent — the effective defaults live in
    /// [`UiConfig::effective_steering_note_visible`], which also honors the
    /// legacy `show_delegation_notes` for `auto_delegated`.
    fn default() -> Self {
        Self {
            auto_delegated: None,
            search_nudge: None,
            shell_tip: None,
            graph_miss: None,
            recall_rider: None,
            read_nudge: None,
            literal_tip: None,
            known_memory_hit: None,
            consolidation_due: None,
            shell_redirect: None,
            edit_stale_read: None,
        }
    }
}

/// UI configuration (`[ui]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// `"dark"` or `"light"`.
    pub theme: String,
    /// Show token usage in the status bar.
    pub show_token_usage: bool,
    /// Render the analyzed/captured image inline below image-command results
    /// (the image_* vision tools and the browser screenshot tools) in the
    /// agent chat. Defaults to `true`; set `false` to hide them.
    pub show_tool_images: bool,
    /// Show agent-activity cards (tool calls, memory reads/writes, vision
    /// image-parsing, skill announcements) in the chat transcript. GUI-only
    /// display filter: the transcript store and the model's context echo are
    /// unaffected — the Output tab and the console still log every tool call.
    /// Defaults to `true` — tool results show in the chat out of the box
    /// (user request 2027-01-13, backlog 57687857); set `false` to hide them
    /// (an explicit `show_tool_activity = false` in config opts out and wins
    /// over the default).
    pub show_tool_activity: bool,
    /// Show knowledge-access activity cards (graph_* tool calls, memory tool
    /// calls, and auto-recall entries) in the chat transcript. GUI-only
    /// display filter: the transcript store and the model's context echo are
    /// unaffected — the console still logs every tool call.
    /// Defaults to `true` (the user wants to see graph + memory + auto-recall
    /// activity — these compact cards surface the agent's knowledge-tool usage
    /// without the noise of the full `show_tool_activity` toggle); set `false`
    /// to hide them.
    pub show_knowledge_activity: bool,
    /// Show the AUTO-DELEGATED steering line that `search` / `search_read`
    /// emit when a symbol-shaped query is auto-delegated to the code graph
    /// or memory (raw fast-path block or `note: `-prefixed prepend).
    /// GUI-only display filter: the tool result
    /// text (the model's context, including the re-issue escape-hatch hint)
    /// is unaffected — only the chat ToolCard's rendering hides the line;
    /// the delegated answer (def:/callers:/full 360° lines) always stays.
    /// Defaults to `false` (the note is model guidance, not end-user
    /// information); set `true` to show it.
    pub show_delegation_notes: bool,
    /// Per-kind display toggles for the steering notes that ride in tool
    /// output (`[ui.steering_notes]`). Each kind defaults to visible except
    /// `auto_delegated`, which inherits `show_delegation_notes` above — see
    /// [`SteeringNotesCfg`] and [`Self::effective_steering_note_visible`].
    /// Omitted from the saved config while no override is set.
    #[serde(skip_serializing_if = "SteeringNotesCfg::is_empty")]
    pub steering_notes: SteeringNotesCfg,
    /// Draw a subtle vertical thread line along consecutive activity cards
    /// (tool/memory/vision/skill) in the chat transcript, visually grouping
    /// them under the response that triggered them. GUI-only display filter;
    /// defaults to `true`; set `false` to hide the line.
    pub chat_thread_line: bool,
    /// Cap assistant prose paragraphs at ~100 columns for readability on
    /// wide windows (code blocks, diffs, and tool outputs stay full width).
    /// GUI-only display filter; defaults to `true`; set `false` to let prose
    /// span the full transcript width.
    pub chat_prose_cap: bool,
    /// Alternate a faint background band per conversation turn (prompt →
    /// response → tools) in the chat transcript. GUI-only display filter;
    /// defaults to `true`; set `false` to disable the banding.
    pub chat_turn_tint: bool,
    /// Show each transcript entry's creation time as a native hover tooltip
    /// (no permanent clutter). GUI-only display filter; defaults to `true`;
    /// set `false` to disable the tooltips.
    pub chat_hover_timestamps: bool,
    /// Play a ding when an agent's plan reaches the Complete state.
    /// Defaults to `true`; set `false` to silence the completion chime.
    pub sound_complete: bool,
    /// Play a ping when an agent needs user input (an approval request or
    /// an `ask_user` question arrives). Defaults to `true`; set `false` to
    /// silence it.
    pub sound_input_needed: bool,
    /// Play a short doom tone when repeated errors stop an agent (three
    /// consecutive errors — the backend `MAX_RETRIES` cap — abort the
    /// turn). Defaults to `true`; set `false` to silence it.
    pub sound_stopped_errors: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dark".to_string(),
            show_token_usage: true,
            show_tool_images: true,
            show_tool_activity: true,
            show_knowledge_activity: true,
            show_delegation_notes: false,
            steering_notes: SteeringNotesCfg::default(),
            chat_thread_line: true,
            chat_prose_cap: true,
            chat_turn_tint: true,
            chat_hover_timestamps: true,
            sound_complete: true,
            sound_input_needed: true,
            sound_stopped_errors: true,
        }
    }
}

impl UiConfig {
    /// The stable snake_case keys of every steering-note kind, in the order
    /// the Chat settings section lists them. Mirrors the fields of
    /// [`SteeringNotesCfg`] (a test pins the two together) and the
    /// `MarkerKind::label()` set plus `auto_delegated`.
    pub const STEERING_NOTE_KEYS: [&'static str; 11] = [
        "auto_delegated",
        "search_nudge",
        "shell_tip",
        "graph_miss",
        "recall_rider",
        "read_nudge",
        "literal_tip",
        "known_memory_hit",
        "consolidation_due",
        "shell_redirect",
        "edit_stale_read",
    ];

    /// Resolve one steering-note kind's visibility. An explicit
    /// `[ui.steering_notes]` override wins; an absent one falls back to the
    /// kind's default:
    ///
    /// - `auto_delegated` → [`Self::show_delegation_notes`] — the legacy
    ///   toggle keeps working for configs written before the per-kind table
    ///   existed (default: hidden);
    /// - every other kind → `true` (visible today, unchanged by this
    ///   feature).
    ///
    /// An unknown key resolves to `true`: an unrecognized name must never
    /// hide a note by accident.
    pub fn effective_steering_note_visible(&self, key: &str) -> bool {
        let notes = &self.steering_notes;
        match key {
            "auto_delegated" => notes.auto_delegated.unwrap_or(self.show_delegation_notes),
            "search_nudge" => notes.search_nudge.unwrap_or(true),
            "shell_tip" => notes.shell_tip.unwrap_or(true),
            "graph_miss" => notes.graph_miss.unwrap_or(true),
            "recall_rider" => notes.recall_rider.unwrap_or(true),
            "read_nudge" => notes.read_nudge.unwrap_or(true),
            "literal_tip" => notes.literal_tip.unwrap_or(true),
            "known_memory_hit" => notes.known_memory_hit.unwrap_or(true),
            "consolidation_due" => notes.consolidation_due.unwrap_or(true),
            "shell_redirect" => notes.shell_redirect.unwrap_or(true),
            "edit_stale_read" => notes.edit_stale_read.unwrap_or(true),
            _ => true,
        }
    }
}

/// Markdown viewer configuration (`[markdown]`).
///
/// Holds the list of directory names that are always skipped when enumerating
/// `.md` files for the Markdown tab's dropdown (version-control internals,
/// build artifacts, dependencies). Dot-directories (names starting with `.`)
/// are hidden separately via the viewer's "Show hidden" toggle — this list is
/// for non-dot dirs the user never wants to see, regardless of that toggle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MarkdownConfig {
    /// Directory names to always skip when walking for `.md` files (matched on
    /// the entry name, not its path, so a nested `target` is skipped too).
    pub skip_dirs: Vec<String>,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            skip_dirs: [".git", "node_modules", "target", "dist"]
                .into_iter()
                .map(String::from)
                .collect(),
        }
    }
}

/// Git configuration (`[git]`).
///
/// Holds the list of git subcommands treated as *core operations* — ones that
/// land commits on `main` (or another shared branch) or publish to a remote.
/// Core operations always go through the interactive approval gate, even under
/// [`SafetyMode::Autonomous`](crate::config::SafetyMode::Autonomous) or a
/// matching safety rule. See
/// [`Tool::never_auto_for`](crate::tool::Tool::never_auto_for).
///
/// Defaults to `["merge", "push"]` (the historical hardcoded set), so a config
/// with no `[git]` section behaves exactly as before the feature existed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct GitConfig {
    /// Git subcommands that always force the approval prompt (e.g. `merge`,
    /// `push`). Matched case-insensitively against the tool call's
    /// `subcommand` argument at dispatch time.
    pub core_operations: Vec<String>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            core_operations: vec!["merge".into(), "push".into()],
        }
    }
}

/// Trace-log memory configuration (`[trace]`).
///
/// Bounds the in-memory LLM trace log (the right-panel "Trace" tab's data
/// source, shared by every provider) so long sessions with large payloads
/// can't grow it without limit. Two knobs:
///
/// - `memory_budget_mb` caps the **total** raw-response bytes held across the
///   record ring. When the sum of all records' `response_raw` payloads
///   exceeds the budget, the OLDEST records' raw payloads are dropped first
///   (their rows stay — status, usage, timings — only the raw body goes).
/// - `request_body_cap_kb` caps each record's serialized request body. The
///   stored JSON keeps its structure (keys survive); the longest string
///   leaves are truncated until the body fits, with a `request_truncated`
///   flag on the record.
///
/// Both default to sensible mid-range values (16 MiB / 256 KiB) and are
/// clamped at load + save time so a typo in config.toml can't zero them out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TraceConfig {
    /// Total raw-response byte budget across the trace ring, in MiB.
    /// Excess evicts the oldest records' raw payloads first.
    pub memory_budget_mb: usize,
    /// Per-record request-body cap, in KiB (structure-preserving truncation).
    pub request_body_cap_kb: usize,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            memory_budget_mb: 16,
            request_body_cap_kb: 256,
        }
    }
}

/// Shell-output filtering configuration (`[shell_filter]`).
///
/// The shell tool filters its captured output before it reaches the LLM
/// transcript: ANSI codes are stripped, repeated lines are collapsed, and
/// well-known commands (`cargo build/test`, `npm test`, `npm run build`,
/// `git status`) get a dedicated noise handler that keeps errors, warnings,
/// and summaries while dropping progress lines. The hard safety rule — lines
/// carrying an error marker (`error`, `failed`, `fail`, `panicked`, `err!`,
/// and the glyphs `✗` / `✖`) are never dropped — applies to every handler
/// and every user override.
///
/// Defaults to enabled: a config with no `[shell_filter]` section filters
/// known commands out of the box. The raw unfiltered output always remains
/// available in the tool result's `data` field (the frontend Output tab).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ShellFilterConfig {
    /// Master switch. `false` disables the filter entirely (shell output
    /// passes through unchanged, exactly as before the feature existed).
    pub enabled: bool,
    /// Per-command overrides, matched in order against the full command
    /// string — the first override whose `command` regex matches wins.
    /// Serialized as `[[shell_filter.override]]` tables (singular, matching
    /// the `[[endpoint]]` convention in `endpoints.toml`).
    #[serde(default, rename = "override")]
    pub overrides: Vec<ShellFilterOverride>,
}

impl Default for ShellFilterConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            overrides: Vec::new(),
        }
    }
}

/// A user rule overriding the filter's behavior for matching commands
/// (`[[shell_filter.override]]` entries — PandaFilter's `filters.toml` idea).
///
/// Overrides are layered on top of the built-in handler for the command
/// (unless `disable_builtin` is set, which falls back to the generic
/// dedup-only pipeline). `drop` and `keep` patterns are Rust regexes matched
/// against each output line; `keep` always wins over `drop`, and the global
/// error-marker safety rule wins over both. A pattern that fails to compile
/// is skipped (the rule degrades rather than crashing the tool).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShellFilterOverride {
    /// Regex matched against the full shell command line (e.g. `"^cargo"`).
    pub command: String,
    /// Lines matching any of these regexes are dropped — unless they carry an
    /// error marker or match a `keep` pattern.
    #[serde(default)]
    pub drop: Vec<String>,
    /// Lines matching any of these regexes are always kept, even if a
    /// built-in handler would have dropped them.
    #[serde(default)]
    pub keep: Vec<String>,
    /// Disable the built-in handler for commands matching `command`; the
    /// generic fallback (ANSI strip + dedup + progress-bar strip) plus this
    /// override's own `drop`/`keep` rules apply instead.
    #[serde(default)]
    pub disable_builtin: bool,
}

impl TraceConfig {
    /// Clamp `memory_budget_mb` into `1..=512` (a zero/garbage value must not
    /// disable the trace log or unbound its memory).
    pub fn clamp_budget(mb: usize) -> usize {
        mb.clamp(1, 512)
    }

    /// Clamp `request_body_cap_kb` into `16..=8192` (a tiny cap would gut
    /// diagnosability; a huge one defeats the purpose).
    pub fn clamp_request_cap(kb: usize) -> usize {
        kb.clamp(16, 8192)
    }

    /// Return a copy with both fields clamped into range. Applied at load
    /// and after every Settings save.
    pub fn clamped(self) -> Self {
        Self {
            memory_budget_mb: Self::clamp_budget(self.memory_budget_mb),
            request_body_cap_kb: Self::clamp_request_cap(self.request_body_cap_kb),
        }
    }
}

impl GeneralConfig {
    /// Load from `config.toml`, or return defaults if the file is missing/empty.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let cfg: Self = toml::from_str(&text)?;
        // Clamp the trace memory knobs into range — a typo (e.g. 0 or a
        // missing-suffix mistake) must not unbound or disable the log. Same
        // for the [memory] retrieval knobs (a 0 cap would zero out recall).
        let clamped = cfg.trace.clone().clamped();
        Ok(Self {
            trace: clamped,
            memory: cfg.memory.clone().clamped(),
            ..cfg
        })
    }

    /// Write the general config to `config.toml`, serializing all four
    /// sections (`[general]`, `[context]`, `[memory]`, `[ui]`) from the
    /// in-memory value. Fields that are `None` (e.g. an unset
    /// `default_provider`) are omitted rather than written as empty — they
    /// re-parse back to `None` via `#[serde(default)]`. The `[general.vision_model]`
    /// sub-table is written only when present.
    ///
    /// This fully rewrites the file, so any unknown keys/sections not in the
    /// schema are dropped (the schema is the source of truth). All known
    /// sections are preserved verbatim from the loaded value, so editing one
    /// field (e.g. `default_provider`) does not clobber the others.
    pub fn save(&self, path: &Path) -> Result<()> {
        // Clamp the trace memory knobs before persisting, mirroring the
        // load-time clamp so the file never holds out-of-range values. Same
        // for the [memory] retrieval knobs.
        let clamped = Self {
            trace: self.trace.clone().clamped(),
            memory: self.memory.clone().clamped(),
            ..self.clone()
        };
        let text = toml::to_string_pretty(&clamped)?;
        crate::config::write_atomic(path, &text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_section_defaults_when_absent() {
        // Back-compat: a config.toml without a [memory] section loads with
        // the compiled-in defaults (an untouched config changes nothing).
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        let m = &cfg.memory;
        assert_eq!(m.decay_half_life_days, 7.0);
        assert_eq!(m.per_query_cap, 50);
        assert_eq!(m.derived_per_class_cap, 5);
        assert_eq!(m.plan_budget, 400);
        assert_eq!(m.bug_budget, 600);
        assert_eq!(m.spec_budget, 500);
        assert_eq!(m.decision_budget, 300);
        assert_eq!(m.review_budget, 500);
    }

    #[test]
    fn memory_section_roundtrips_and_clamps_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = r#"
[memory]
decay_half_life_days = 14.0
per_query_cap = 25
derived_per_class_cap = 3
plan_budget = 450
decision_budget = 0
"#;
        std::fs::write(&path, text).unwrap();
        let cfg = GeneralConfig::load_or_default(&path).unwrap();
        // In-range values survive; the 0 budget clamps to the floor (50).
        assert_eq!(cfg.memory.decay_half_life_days, 14.0);
        assert_eq!(cfg.memory.per_query_cap, 25);
        assert_eq!(cfg.memory.derived_per_class_cap, 3);
        assert_eq!(cfg.memory.plan_budget, 450);
        assert_eq!(cfg.memory.decision_budget, 50);
        // Save preserves the section (and clamps stay stable).
        cfg.save(&path).unwrap();
        let cfg2 = GeneralConfig::load_or_default(&path).unwrap();
        assert_eq!(cfg2.memory.per_query_cap, 25);
        assert_eq!(cfg2.memory.decision_budget, 50);
    }

    #[test]
    fn memory_config_digest_budget_mirrors_record_type() {
        let cfg = crate::memory::MemorySearchConfig::default();
        for rt in [
            crate::memory::MemoryRecordType::Plan,
            crate::memory::MemoryRecordType::Bug,
            crate::memory::MemoryRecordType::Spec,
            crate::memory::MemoryRecordType::Decision,
            crate::memory::MemoryRecordType::Review,
        ] {
            assert_eq!(
                cfg.digest_budget(rt),
                rt.content_budget(),
                "default config mirrors content_budget for {rt:?}"
            );
        }
        assert_eq!(
            cfg.digest_budget(crate::memory::MemoryRecordType::How),
            None
        );
        assert_eq!(
            cfg.digest_budget(crate::memory::MemoryRecordType::None),
            None
        );
    }

    #[test]
    fn parses_full_config() {
        let text = r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "auto-read-approve-writes"

[context]
summarize_at_fill_rate = 0.75

[ui]
theme = "light"
show_token_usage = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.general.default_provider.as_deref(), Some("openai"));
        assert_eq!(cfg.general.default_model.as_deref(), Some("gpt-4o"));
        assert_eq!(cfg.general.safety, SafetyMode::AutoReadApproveWrites);
        assert!((cfg.context.summarize_at_fill_rate - 0.75).abs() < 1e-9);
        assert_eq!(cfg.ui.theme, "light");
        assert!(!cfg.ui.show_token_usage);
    }

    #[test]
    fn defaults_when_empty() {
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.general.safety, SafetyMode::ApproveEachAction);
        assert!((cfg.context.summarize_at_fill_rate - 0.3).abs() < 1e-9);
        assert_eq!(cfg.ui.theme, "dark");
        assert!(cfg.ui.show_token_usage);
    }

    #[test]
    fn safety_mode_serde_roundtrip() {
        for mode in [
            SafetyMode::ApproveEachAction,
            SafetyMode::AutoReadApproveWrites,
            SafetyMode::AutoApproveProject,
            SafetyMode::Autonomous,
        ] {
            // Serialize as a field of a struct (toml can't serialize bare enums).
            #[derive(Serialize, Deserialize)]
            struct Wrapper {
                safety: SafetyMode,
            }
            let w = Wrapper { safety: mode };
            let s = toml::to_string(&w).unwrap();
            let back: Wrapper = toml::from_str(&s).unwrap();
            assert_eq!(mode, back.safety);
        }
    }

    #[test]
    fn vision_model_parsed() {
        let text = r#"
[general]
default_provider = "glm"
default_model = "glm-5.2"
safety = "approve-each-action"

[general.vision_model]
endpoint = "qwen-portal"
model = "qwen-vl"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.general.default_provider.as_deref(), Some("glm"));
        let vm = cfg.general.vision_model.expect("vision_model should parse");
        assert_eq!(vm.endpoint, "qwen-portal");
        assert_eq!(vm.model, "qwen-vl");
    }

    #[test]
    fn vision_model_defaults_none() {
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(
            cfg.general.vision_model.is_none(),
            "vision_model should default to None"
        );
    }

    #[test]
    fn embedding_model_parsed() {
        let text = r#"
[general]
default_provider = "ollama-local"
default_model = "qwen2.5-coder:7b"
safety = "auto-read-approve-writes"

[general.embedding_model]
endpoint = "ollama-local"
model = "nomic-embed-text"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(
            cfg.general.default_provider.as_deref(),
            Some("ollama-local")
        );
        let em = cfg
            .general
            .embedding_model
            .expect("embedding_model should parse");
        assert_eq!(em.endpoint, "ollama-local");
        assert_eq!(em.model, "nomic-embed-text");
    }

    #[test]
    fn embedding_model_defaults_none() {
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(
            cfg.general.embedding_model.is_none(),
            "embedding_model should default to None"
        );
    }

    #[test]
    fn bundled_embedding_model_defaults_to_minilm() {
        // Semantic recall is the out-of-box behavior: an ABSENT key in an
        // existing config.toml resolves to the default bundled model, so
        // users upgrade without touching the file.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert_eq!(
            cfg.general.bundled_embedding_model.as_deref(),
            Some("all-MiniLM-L6-v2")
        );
    }

    #[test]
    fn bundled_embedding_model_hash_sentinel_round_trips() {
        // "hash" is the explicit keyword-only opt-out — it must survive a
        // parse + re-serialize cycle verbatim (not silently become the
        // default model).
        let text = r#"
[general]
bundled_embedding_model = "hash"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.general.bundled_embedding_model.as_deref(), Some("hash"));
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert_eq!(
            cfg2.general.bundled_embedding_model.as_deref(),
            Some("hash")
        );
    }

    #[test]
    fn bundled_embedding_model_round_trips() {
        let text = r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "autonomous"
bundled_embedding_model = "all-MiniLM-L6-v2"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(
            cfg.general.bundled_embedding_model.as_deref(),
            Some("all-MiniLM-L6-v2")
        );
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert_eq!(
            cfg2.general.bundled_embedding_model.as_deref(),
            Some("all-MiniLM-L6-v2")
        );
    }

    #[test]
    fn laya_defaults_to_disabled() {
        // Opt-in only: an absent [general.laya] section (fresh config and
        // existing configs alike) means the classifier is disabled — zero
        // behavior change.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.general.laya.enabled);
        assert!(cfg.general.laya.endpoint.is_none());
        assert!(!cfg.general.laya.auto_type_memories);
        assert!(!cfg.general.laya.failure_triage);
        assert!(!cfg.general.laya.auto_finetune);
    }

    #[test]
    fn laya_round_trips() {
        let text = r#"
[general.laya]
enabled = true
endpoint = "http://127.0.0.1:8000"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.laya.enabled);
        assert_eq!(
            cfg.general.laya.endpoint.as_deref(),
            Some("http://127.0.0.1:8000")
        );
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.laya.enabled);
        assert_eq!(
            cfg2.general.laya.endpoint.as_deref(),
            Some("http://127.0.0.1:8000")
        );
    }

    #[test]
    fn laya_section_is_omitted_while_default_and_written_once_touched() {
        // Convention parity with [ui.steering_notes]: an untouched (default)
        // section is skipped in config.toml; enabling Laya (or a leftover
        // endpoint) writes it, and it round-trips.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("general.laya"));

        let cfg: GeneralConfig = toml::from_str(
            r#"
[general.laya]
enabled = true
endpoint = "http://127.0.0.1:8000"
"#,
        )
        .unwrap();
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("general.laya"));
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        assert!(back.general.laya.enabled);
        assert_eq!(
            back.general.laya.endpoint.as_deref(),
            Some("http://127.0.0.1:8000")
        );
    }

    #[test]
    fn laya_mode_and_checkpoint_default_like_an_absent_section() {
        // An absent [general.laya] section means external mode + no
        // checkpoint — exactly the pre-managed shape, so existing configs
        // are unaffected.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.general.laya.mode, LayaMode::External);
        assert!(cfg.general.laya.checkpoint.is_none());
        // is_default covers the new fields: managed mode alone counts as
        // touched, so the section gets written.
        let managed = LayaConfig {
            mode: LayaMode::Managed,
            ..Default::default()
        };
        assert!(!managed.is_default());
    }

    #[test]
    fn laya_auto_typing_defaults_off_and_round_trips() {
        // Absent section ⇒ off (zero behavior change); the flag round-trips
        // with the section; false never leaves a serialized trace; the flag
        // alone counts as "touched" so the section gets written.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.general.laya.auto_type_memories);

        let text = r#"
[general.laya]
enabled = true
endpoint = "http://127.0.0.1:8000"
auto_type_memories = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.laya.auto_type_memories);
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.laya.auto_type_memories);

        let untouched = LayaConfig::default();
        assert!(untouched.is_default());
        assert!(!toml::to_string(&untouched).unwrap().contains("auto_type"));
        let only_flag = LayaConfig {
            auto_type_memories: true,
            ..Default::default()
        };
        assert!(!only_flag.is_default());
    }

    #[test]
    fn optimizer_levers_default_off_and_round_trip() {
        // Absent section ⇒ every lever off (byte-identical behavior); the
        // untouched section serializes away; flags + knobs round-trip; any
        // flag alone counts as "touched" so the section gets written.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.general.optimizer.delta_reads);
        assert!(!cfg.general.optimizer.compress_output);
        assert!(!cfg.general.optimizer.archive);
        assert!(!cfg.general.optimizer.compaction_survival);
        assert!(!cfg.general.optimizer.quality_score);
        assert!(!cfg.general.optimizer.lean_output_nudge);
        assert_eq!(
            cfg.general.optimizer.archive_min_chars,
            OptimizerConfig::DEFAULT_ARCHIVE_MIN_CHARS
        );

        let text = r#"
[general.optimizer]
delta_reads = true
archive = true
archive_min_chars = 5000
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.optimizer.delta_reads);
        assert!(cfg.general.optimizer.archive);
        assert_eq!(cfg.general.optimizer.archive_min_chars, 5000);
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.optimizer.delta_reads);
        assert_eq!(cfg2.general.optimizer.archive_min_chars, 5000);

        // Untouched: no serialized trace; touched: is_default flips.
        assert!(OptimizerConfig::default().is_default());
        assert!(!toml::to_string(&GeneralConfig::default())
            .unwrap()
            .contains("optimizer"));
        let only_flag = OptimizerConfig {
            delta_reads: true,
            ..Default::default()
        };
        assert!(!only_flag.is_default());
    }

    #[test]
    fn laya_failure_triage_flags_default_off_and_round_trip() {
        // All three flags follow the same opt-in convention as auto-typing:
        // absent ⇒ false, false leaves no serialized trace, and any flag
        // alone counts as "touched" so the section gets written.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.general.laya.failure_triage);
        assert!(!cfg.general.laya.failure_triage_knn);
        assert!(!cfg.general.laya.auto_finetune);

        let text = r#"
[general.laya]
enabled = true
failure_triage = true
failure_triage_knn = true
auto_finetune = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.laya.failure_triage);
        assert!(cfg.general.laya.failure_triage_knn);
        assert!(cfg.general.laya.auto_finetune);
        let back = toml::to_string(&cfg).unwrap();
        assert!(back.contains("failure_triage"));
        assert!(back.contains("failure_triage_knn"));
        assert!(back.contains("auto_finetune"));
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.laya.failure_triage);
        assert!(cfg2.general.laya.failure_triage_knn);
        assert!(cfg2.general.laya.auto_finetune);

        let untouched = LayaConfig::default();
        assert!(untouched.is_default());
        let serialized = toml::to_string(&untouched).unwrap();
        assert!(!serialized.contains("failure_triage"));
        assert!(!serialized.contains("failure_triage_knn"));
        assert!(!serialized.contains("auto_finetune"));
        assert!(!LayaConfig {
            failure_triage: true,
            ..Default::default()
        }
        .is_default());
        assert!(!LayaConfig {
            failure_triage_knn: true,
            ..Default::default()
        }
        .is_default());
        assert!(!LayaConfig {
            auto_finetune: true,
            ..Default::default()
        }
        .is_default());
    }

    #[test]
    fn laya_managed_mode_round_trips() {
        let text = r#"
[general.laya]
enabled = true
mode = "managed"
checkpoint = "multilingual"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.laya.enabled);
        assert_eq!(cfg.general.laya.mode, LayaMode::Managed);
        assert_eq!(
            cfg.general.laya.checkpoint.as_deref(),
            Some("multilingual")
        );
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.laya.enabled);
        assert_eq!(cfg2.general.laya.mode, LayaMode::Managed);
        assert_eq!(
            cfg2.general.laya.checkpoint.as_deref(),
            Some("multilingual")
        );
    }

    #[test]
    fn laya_mode_is_omitted_while_external() {
        // The external default never writes `mode` — a serialized external
        // config stays shape-compatible with pre-managed configs.
        let external = LayaConfig {
            enabled: true,
            endpoint: Some("http://127.0.0.1:8000".into()),
            ..Default::default()
        };
        let text = toml::to_string(&external).unwrap();
        assert!(!text.contains("mode"));
        let managed = LayaConfig {
            mode: LayaMode::Managed,
            ..Default::default()
        };
        assert!(toml::to_string(&managed)
            .unwrap()
            .contains("mode = \"managed\""));
    }

    #[test]
    fn embedding_model_round_trips() {
        let cfg = GeneralConfig {
            general: GeneralSection {
                embedding_model: Some(EmbeddingModel {
                    endpoint: "ollama-local".into(),
                    model: "nomic-embed-text".into(),
                }),
                ..GeneralSection::default()
            },
            ..GeneralConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        let em = back
            .general
            .embedding_model
            .expect("embedding_model should round-trip");
        assert_eq!(em.endpoint, "ollama-local");
        assert_eq!(em.model, "nomic-embed-text");
    }

    #[test]
    fn removed_run_all_strict_success_key_is_tolerated() {
        // The opt-in run_all_strict_success flag was removed 2026-08-20: the
        // plan-loop gate (workflow must reach Complete before an item can be
        // marked Done) is now mandatory — see src-tauri/src/ipc/run_all.rs.
        // An old config.toml that still carries the key must keep loading
        // (serde ignores unknown fields), so the removal must not break
        // upgrades.
        let text = r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "autonomous"
run_all_strict_success = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.general.default_provider.as_deref(), Some("openai"));
    }

    #[test]
    fn enable_browser_inspection_defaults_false() {
        // The flag is opt-in: a fresh config must default to false so the
        // unauthenticated CDP port is never exposed unless the user opts in.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.general.enable_browser_inspection);
    }

    #[test]
    fn enable_browser_inspection_round_trips() {
        // The flag must round-trip through TOML so the user's opt-in persists.
        let text = r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "autonomous"
enable_browser_inspection = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.enable_browser_inspection);
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.enable_browser_inspection);
    }

    #[test]
    fn auto_compact_on_plan_complete_defaults_false() {
        // The flag is opt-in: a fresh config must default to false so the
        // run-all loop never spends a summarization call between items
        // unless the user opts in.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.general.auto_compact_on_plan_complete);
    }

    #[test]
    fn auto_compact_on_plan_complete_round_trips() {
        // The flag must round-trip through TOML so the opt-in persists
        // across save/reload.
        let text = r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "autonomous"
auto_compact_on_plan_complete = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.general.auto_compact_on_plan_complete);
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.general.auto_compact_on_plan_complete);
    }

    #[test]
    fn codegraph_defaults_true() {
        // CodeGraph is opt-out: a config with no key must default to enabled
        // (indexing runs in the background and never blocks startup).
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.general.codegraph);
    }

    #[test]
    fn codegraph_opt_out_round_trips() {
        // `codegraph = false` must round-trip through TOML so the opt-out
        // persists across save/reload.
        let text = r#"
[general]
codegraph = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(!cfg.general.codegraph);
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(!cfg2.general.codegraph);
    }

    #[test]
    fn show_tool_activity_defaults_true() {
        // Agent-activity cards are SHOWN out of the box (backlog 57687857 —
        // user request 2027-01-13: tool results visible by default).
        // Users opt OUT by setting show_tool_activity to false.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.ui.show_tool_activity);
    }

    #[test]
    fn sounds_default_on_and_round_trip() {
        // Notification sounds ship enabled: a fresh config defaults all three
        // to true; each can be opted out individually and the opt-out must
        // survive a save/reload round trip.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.ui.sound_complete);
        assert!(cfg.ui.sound_input_needed);
        assert!(cfg.ui.sound_stopped_errors);

        let text = r#"
[ui]
sound_complete = false
sound_input_needed = true
sound_stopped_errors = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(!cfg.ui.sound_complete);
        assert!(cfg.ui.sound_input_needed);
        assert!(!cfg.ui.sound_stopped_errors);
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(!cfg2.ui.sound_complete);
        assert!(cfg2.ui.sound_input_needed);
        assert!(!cfg2.ui.sound_stopped_errors);
    }

    #[test]
    fn show_tool_activity_round_trips() {
        // Explicit `false` (the user's opt-out) must survive a save/reload
        // round trip; the default is covered by show_tool_activity_defaults_true.
        let text = r#"
[ui]
theme = "dark"
show_token_usage = true
show_tool_activity = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(!cfg.ui.show_tool_activity);
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(!cfg2.ui.show_tool_activity);
    }

    #[test]
    fn show_delegation_notes_defaults_false() {
        // The AUTO-DELEGATED steering note is model guidance (the re-issue
        // escape hatch), not end-user information — hidden out of the box
        // (users opt in by setting it to true).
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.ui.show_delegation_notes);
    }

    #[test]
    fn show_delegation_notes_round_trips() {
        let text = r#"
[ui]
show_delegation_notes = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.ui.show_delegation_notes);
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.ui.show_delegation_notes);
    }

    #[test]
    fn steering_notes_default_to_visible_except_auto_delegated() {
        // Every steering-note kind is visible out of the box EXCEPT the
        // AUTO-DELEGATED family, which inherits the legacy
        // `show_delegation_notes` default (hidden) — an untouched config
        // therefore behaves exactly as it did before this table existed.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(!cfg.ui.effective_steering_note_visible("auto_delegated"));
        for key in UiConfig::STEERING_NOTE_KEYS {
            if key != "auto_delegated" {
                assert!(
                    cfg.ui.effective_steering_note_visible(key),
                    "{key} should default to visible"
                );
            }
        }
        // An unknown key resolves visible — never hide a note by accident.
        assert!(cfg.ui.effective_steering_note_visible("not-a-kind"));
    }

    #[test]
    fn steering_notes_explicit_value_wins() {
        let text = r#"
[ui.steering_notes]
auto_delegated = true
literal_tip = false
known_memory_hit = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        // An explicit override beats the legacy-derived default.
        assert!(cfg.ui.effective_steering_note_visible("auto_delegated"));
        assert!(!cfg.ui.effective_steering_note_visible("literal_tip"));
        assert!(!cfg.ui.effective_steering_note_visible("known_memory_hit"));
        // Untouched kinds keep their defaults.
        assert!(cfg.ui.effective_steering_note_visible("search_nudge"));
    }

    #[test]
    fn steering_notes_legacy_toggle_seeds_auto_delegated() {
        // A config written before the per-kind table existed keeps working:
        // `show_delegation_notes = true` shows the AUTO-DELEGATED family...
        let legacy: GeneralConfig = toml::from_str("[ui]\nshow_delegation_notes = true\n").unwrap();
        assert!(legacy.ui.effective_steering_note_visible("auto_delegated"));
        // ...while that legacy key stays the ONLY override (the other kinds
        // rest on their own defaults, not on the legacy toggle).
        assert!(legacy.ui.steering_notes.auto_delegated.is_none());
        assert!(legacy.ui.effective_steering_note_visible("literal_tip"));
    }

    #[test]
    fn steering_notes_every_key_maps_to_a_field() {
        // Pin STEERING_NOTE_KEYS against the struct: each key must address a
        // real override, and setting it must not disturb any other kind.
        for key in UiConfig::STEERING_NOTE_KEYS {
            let mut cfg = UiConfig::default();
            match key {
                "auto_delegated" => cfg.steering_notes.auto_delegated = Some(false),
                "search_nudge" => cfg.steering_notes.search_nudge = Some(false),
                "shell_tip" => cfg.steering_notes.shell_tip = Some(false),
                "graph_miss" => cfg.steering_notes.graph_miss = Some(false),
                "recall_rider" => cfg.steering_notes.recall_rider = Some(false),
                "read_nudge" => cfg.steering_notes.read_nudge = Some(false),
                "literal_tip" => cfg.steering_notes.literal_tip = Some(false),
                "known_memory_hit" => cfg.steering_notes.known_memory_hit = Some(false),
                "consolidation_due" => cfg.steering_notes.consolidation_due = Some(false),
                "shell_redirect" => cfg.steering_notes.shell_redirect = Some(false),
                "edit_stale_read" => cfg.steering_notes.edit_stale_read = Some(false),
                other => panic!("unknown steering-note key {other}"),
            }
            assert!(
                !cfg.effective_steering_note_visible(key),
                "{key} override should hide the note"
            );
            // Setting one override must not disturb any OTHER kind: compare
            // each against the untouched default resolution (auto_delegated
            // is hidden by default, so "unchanged" is not always "visible").
            let baseline = UiConfig::default();
            for other in UiConfig::STEERING_NOTE_KEYS {
                if other != key {
                    assert_eq!(
                        cfg.effective_steering_note_visible(other),
                        baseline.effective_steering_note_visible(other),
                        "{other} changed by the {key} override"
                    );
                }
            }
        }
    }

    #[test]
    fn steering_notes_round_trips() {
        let text = r#"
[ui.steering_notes]
auto_delegated = true
search_nudge = false
literal_tip = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        // Re-serialize: only the overrides that were set appear (no key
        // noise for the untouched kinds).
        let back = toml::to_string(&cfg).unwrap();
        assert!(
            !back.contains("shell_tip"),
            "an absent override must not be written: {back}"
        );
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(cfg2.ui.effective_steering_note_visible("auto_delegated"));
        assert!(!cfg2.ui.effective_steering_note_visible("search_nudge"));
        assert!(!cfg2.ui.effective_steering_note_visible("literal_tip"));
        assert_eq!(cfg2.ui.steering_notes, cfg.ui.steering_notes);
    }

    #[test]
    fn steering_notes_table_omitted_while_unset() {
        // An untouched config must not grow an empty [ui.steering_notes]
        // table (cosmetic parity with configs written before the feature).
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        let back = toml::to_string(&cfg).unwrap();
        assert!(
            !back.contains("steering_notes"),
            "unset table should be omitted: {back}"
        );
    }

    #[test]
    fn show_knowledge_activity_defaults_true() {
        // Knowledge-access cards (graph + memory + auto-recall) are visible
        // out of the box (backlog 68c4c9a5): the user wants to see graph +
        // memory + auto-recall activity without enabling the full
        // show_tool_activity toggle.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.ui.show_knowledge_activity);
    }

    #[test]
    fn show_knowledge_activity_round_trips() {
        let text = r#"
[ui]
theme = "dark"
show_token_usage = true
show_knowledge_activity = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(!cfg.ui.show_knowledge_activity);
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(!cfg2.ui.show_knowledge_activity);
    }

    #[test]
    fn chat_readability_defaults_true() {
        // The four chat-readability affordances (thread line, prose cap,
        // turn tint, hover timestamps — plan afa81f0a) ship enabled; each
        // can be opted out individually.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.ui.chat_thread_line);
        assert!(cfg.ui.chat_prose_cap);
        assert!(cfg.ui.chat_turn_tint);
        assert!(cfg.ui.chat_hover_timestamps);
    }

    #[test]
    fn chat_readability_round_trips() {
        let text = r#"
[ui]
theme = "dark"
show_token_usage = true
chat_thread_line = false
chat_prose_cap = true
chat_turn_tint = false
chat_hover_timestamps = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(!cfg.ui.chat_thread_line);
        assert!(cfg.ui.chat_prose_cap);
        assert!(!cfg.ui.chat_turn_tint);
        assert!(cfg.ui.chat_hover_timestamps);
        // Re-serialize + re-parse.
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(!cfg2.ui.chat_thread_line);
        assert!(cfg2.ui.chat_prose_cap);
        assert!(!cfg2.ui.chat_turn_tint);
        assert!(cfg2.ui.chat_hover_timestamps);
    }

    #[test]
    fn save_round_trips_all_sections() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = GeneralConfig {
            general: GeneralSection {
                default_provider: Some("openai".into()),
                default_model: Some("gpt-4o".into()),
                safety: SafetyMode::AutoReadApproveWrites,
                vision_model: Some(VisionModel {
                    endpoint: "qwen-portal".into(),
                    model: "qwen-vl".into(),
                }),
                embedding_model: Some(EmbeddingModel {
                    endpoint: "ollama-local".into(),
                    model: "nomic-embed-text".into(),
                }),
                bundled_embedding_model: None,
                laya: LayaConfig::default(),
                optimizer: OptimizerConfig::default(),
                enable_browser_inspection: false,
                codegraph: true,
                auto_compact_on_plan_complete: false,
            },
            context: ContextConfig {
                summarize_at_fill_rate: 0.3,
                preflight_compact: true,
                compact_headroom_tokens: 32_000,
                proxy_cache_ceiling_tokens: Some(340_000),
            },
            ui: UiConfig {
                theme: "light".into(),
                show_token_usage: false,
                show_tool_images: false,
                show_tool_activity: true,
                show_knowledge_activity: false,
                show_delegation_notes: true,
                steering_notes: SteeringNotesCfg {
                    literal_tip: Some(false),
                    ..Default::default()
                },
                chat_thread_line: false,
                chat_prose_cap: true,
                chat_turn_tint: false,
                chat_hover_timestamps: true,
                sound_complete: false,
                sound_input_needed: true,
                sound_stopped_errors: false,
            },
            models: ModelsConfig::default(),
            markdown: MarkdownConfig {
                skip_dirs: vec![".git".into(), "build".into()],
            },
            git: GitConfig::default(),
            trace: TraceConfig {
                memory_budget_mb: 32,
                request_body_cap_kb: 512,
            },
            memory: crate::memory::MemorySearchConfig::default(),
            shell_filter: ShellFilterConfig::default(),
        };
        cfg.save(&path).unwrap();

        let reloaded = GeneralConfig::load_or_default(&path).unwrap();
        assert_eq!(reloaded.general.default_provider.as_deref(), Some("openai"));
        assert_eq!(reloaded.general.default_model.as_deref(), Some("gpt-4o"));
        assert_eq!(reloaded.general.safety, SafetyMode::AutoReadApproveWrites);
        let vm = reloaded
            .general
            .vision_model
            .expect("vision_model should round-trip");
        assert_eq!(vm.endpoint, "qwen-portal");
        assert_eq!(vm.model, "qwen-vl");
        let em = reloaded
            .general
            .embedding_model
            .expect("embedding_model should round-trip");
        assert_eq!(em.endpoint, "ollama-local");
        assert_eq!(em.model, "nomic-embed-text");
        assert!((reloaded.context.summarize_at_fill_rate - 0.3).abs() < 1e-9);
        assert_eq!(reloaded.ui.theme, "light");
        assert!(!reloaded.ui.show_token_usage);
        assert!(reloaded.ui.show_tool_activity);
        assert!(!reloaded.ui.show_knowledge_activity);
        assert!(reloaded.ui.show_delegation_notes);
        // The per-kind steering-note table rides the save path too: only the
        // kind carrying an override is written, and the legacy toggle still
        // seeds auto_delegated on reload.
        assert_eq!(reloaded.ui.steering_notes.literal_tip, Some(false));
        assert_eq!(reloaded.ui.steering_notes.search_nudge, None);
        assert!(!reloaded.ui.effective_steering_note_visible("literal_tip"));
        assert!(reloaded.ui.effective_steering_note_visible("auto_delegated"));
        assert!(!reloaded.ui.chat_thread_line);
        assert!(reloaded.ui.chat_prose_cap);
        assert!(!reloaded.ui.chat_turn_tint);
        assert!(reloaded.ui.chat_hover_timestamps);
        assert_eq!(
            reloaded.markdown.skip_dirs,
            vec![".git".to_string(), "build".to_string()]
        );
        assert_eq!(reloaded.trace.memory_budget_mb, 32);
        assert_eq!(reloaded.trace.request_body_cap_kb, 512);
    }

    #[test]
    fn trace_section_defaults() {
        // No [trace] section → the 16 MiB / 256 KiB defaults, so a
        // pre-existing config behaves exactly as before the feature existed.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.trace.memory_budget_mb, 16);
        assert_eq!(cfg.trace.request_body_cap_kb, 256);
    }

    #[test]
    fn trace_section_parses_and_round_trips() {
        let text = r#"
[trace]
memory_budget_mb = 64
request_body_cap_kb = 1024
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(cfg.trace.memory_budget_mb, 64);
        assert_eq!(cfg.trace.request_body_cap_kb, 1024);
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert_eq!(cfg2.trace, cfg.trace);
    }

    #[test]
    fn trace_section_clamps_out_of_range_values() {
        // 0 and >512 MiB clamp into range at LOAD — a typo in config.toml
        // must neither disable the log nor unbound its memory. Same for the
        // request cap. (Goes through load_or_default, the real load path.)
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[trace]\nmemory_budget_mb = 0\nrequest_body_cap_kb = 1\n",
        )
        .unwrap();
        let cfg = GeneralConfig::load_or_default(&path).unwrap();
        assert_eq!(cfg.trace.memory_budget_mb, 1);
        assert_eq!(cfg.trace.request_body_cap_kb, 16);
        std::fs::write(
            &path,
            "[trace]\nmemory_budget_mb = 9999\nrequest_body_cap_kb = 9999\n",
        )
        .unwrap();
        let cfg = GeneralConfig::load_or_default(&path).unwrap();
        assert_eq!(cfg.trace.memory_budget_mb, 512);
        assert_eq!(cfg.trace.request_body_cap_kb, 8192);
    }

    #[test]
    fn save_with_unset_optionals_round_trips() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = GeneralConfig::default(); // default_provider/model = None, no vision_model
        cfg.save(&path).unwrap();
        let reloaded = GeneralConfig::load_or_default(&path).unwrap();
        assert!(reloaded.general.default_provider.is_none());
        assert!(reloaded.general.default_model.is_none());
        assert!(reloaded.general.vision_model.is_none());
    }

    #[test]
    fn models_section_defaults_empty() {
        // A config with no [models] section must parse to all-None overrides
        // (so behavior is unchanged from before the feature existed).
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.models.planning.is_none());
        assert!(cfg.models.executing.is_none());
        assert!(cfg.models.reviewing.is_none());
        assert!(cfg.models.complete.is_none());
        assert!(cfg.models.subagent.is_none());
        assert!(cfg.models.skill.is_empty());
    }

    #[test]
    fn models_section_parses_overrides() {
        let text = r#"
[models]
planning = { endpoint = "openai", model = "o3" }
executing = { endpoint = "deepseek", model = "deepseek-v4-flash" }
reviewing = { endpoint = "openai", model = "o3" }
subagent = { endpoint = "deepseek", model = "deepseek-v4-flash" }

[models.skill.merge_to_main]
endpoint = "deepseek"
model = "deepseek-v4-flash"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        let p = cfg.models.planning.expect("planning override");
        assert_eq!(p.endpoint, "openai");
        assert_eq!(p.model, "o3");
        let e = cfg.models.executing.expect("executing override");
        assert_eq!(e.endpoint, "deepseek");
        assert_eq!(e.model, "deepseek-v4-flash");
        let r = cfg.models.reviewing.expect("reviewing override");
        assert_eq!(r.endpoint, "openai");
        assert_eq!(r.model, "o3");
        assert!(cfg.models.complete.is_none());
        let s = cfg.models.subagent.expect("subagent override");
        assert_eq!(s.model, "deepseek-v4-flash");
        let m = cfg
            .models
            .skill
            .get("merge_to_main")
            .expect("skill override present");
        assert_eq!(m.endpoint, "deepseek");
        assert_eq!(m.model, "deepseek-v4-flash");
    }

    #[test]
    fn models_section_round_trips() {
        let cfg = GeneralConfig {
            general: GeneralSection::default(),
            context: ContextConfig::default(),
            ui: UiConfig::default(),
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                executing: None,
                bug_fixing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: Some("max".into()),
                }),
                reviewing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                complete: None,
                subagent: Some(ModelRef {
                    endpoint: "deepseek".into(),
                    model: "deepseek-v4-flash".into(),
                    reasoning_effort: None,
                }),
                summarize: None,
                skill: std::collections::HashMap::from([(
                    "merge_to_main".into(),
                    ModelRef {
                        endpoint: "deepseek".into(),
                        model: "deepseek-v4-flash".into(),
                        reasoning_effort: None,
                    },
                )]),
            },
            markdown: MarkdownConfig::default(),
            git: GitConfig::default(),
            trace: TraceConfig::default(),
            memory: crate::memory::MemorySearchConfig::default(),
            shell_filter: ShellFilterConfig::default(),
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.models, cfg.models);
    }

    #[test]
    fn models_section_save_round_trips_through_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cfg = GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                executing: None,
                bug_fixing: None,
                reviewing: None,
                complete: None,
                subagent: None,
                summarize: None,
                skill: std::collections::HashMap::new(),
            },
            ..GeneralConfig::default()
        };
        cfg.save(&path).unwrap();
        let reloaded = GeneralConfig::load_or_default(&path).unwrap();
        let p = reloaded
            .models
            .planning
            .expect("planning override round-tripped");
        assert_eq!(p.endpoint, "openai");
        assert_eq!(p.model, "o3");
        // Unset overrides stay unset.
        assert!(reloaded.models.subagent.is_none());
        assert!(reloaded.models.bug_fixing.is_none());
    }

    #[test]
    fn models_section_parses_reasoning_effort() {
        let text = r#"
[models]
planning = { endpoint = "openai", model = "o3", reasoning_effort = "low" }
executing = { endpoint = "deepseek", model = "deepseek-v4-flash" }

[models.skill.merge_to_main]
endpoint = "deepseek"
model = "deepseek-v4-flash"
reasoning_effort = "off"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        let p = cfg.models.planning.expect("planning override");
        assert_eq!(p.reasoning_effort.as_deref(), Some("low"));
        // Absent on a set slot → None (the model's own default applies).
        let e = cfg.models.executing.expect("executing override");
        assert_eq!(e.reasoning_effort, None);
        let m = cfg
            .models
            .skill
            .get("merge_to_main")
            .expect("skill override present");
        assert_eq!(m.reasoning_effort.as_deref(), Some("off"));
    }

    #[test]
    fn models_section_reasoning_effort_round_trips() {
        let cfg = GeneralConfig {
            models: ModelsConfig {
                planning: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: Some("low".into()),
                }),
                executing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: None,
                }),
                bug_fixing: Some(ModelRef {
                    endpoint: "openai".into(),
                    model: "o3".into(),
                    reasoning_effort: Some("max".into()),
                }),
                reviewing: None,
                complete: None,
                subagent: None,
                summarize: None,
                skill: std::collections::HashMap::new(),
            },
            ..GeneralConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        // Only the set efforts are serialized (None is skipped, so configs
        // without an effort keep their exact shape).
        assert_eq!(text.matches("reasoning_effort").count(), 2);
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.models, cfg.models);
    }

    #[test]
    fn markdown_section_defaults_when_absent() {
        // A config with no [markdown] section must parse to the default
        // skip_dirs (so behavior is unchanged from before the feature existed).
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert_eq!(
            cfg.markdown.skip_dirs,
            vec![
                ".git".to_string(),
                "node_modules".to_string(),
                "target".to_string(),
                "dist".to_string(),
            ]
        );
    }

    #[test]
    fn markdown_section_parses_skip_dirs() {
        let text = r#"
[markdown]
skip_dirs = [".git", "node_modules", "target", "dist", "build"]
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(
            cfg.markdown.skip_dirs,
            vec![
                ".git".to_string(),
                "node_modules".to_string(),
                "target".to_string(),
                "dist".to_string(),
                "build".to_string(),
            ]
        );
    }

    #[test]
    fn markdown_section_round_trips() {
        let cfg = GeneralConfig {
            markdown: MarkdownConfig {
                skip_dirs: vec![".git".into(), "build".into()],
            },
            ..GeneralConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.markdown, cfg.markdown);
    }

    #[test]
    fn git_section_defaults_merge_push() {
        // A config with no [git] section must parse to the default
        // core_operations (merge + push), so behavior is unchanged from before
        // the feature existed.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert_eq!(
            cfg.git.core_operations,
            vec!["merge".to_string(), "push".to_string()]
        );
    }

    #[test]
    fn git_section_parses_core_operations() {
        let text = r#"
[git]
core_operations = ["merge", "push", "checkout"]
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert_eq!(
            cfg.git.core_operations,
            vec![
                "merge".to_string(),
                "push".to_string(),
                "checkout".to_string(),
            ]
        );
    }

    #[test]
    fn git_section_round_trips() {
        let cfg = GeneralConfig {
            git: GitConfig {
                core_operations: vec!["merge".into(), "push".into(), "checkout".into()],
            },
            ..GeneralConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.git, cfg.git);
    }

    #[test]
    fn shell_filter_section_defaults_enabled_with_no_overrides() {
        // A config with no [shell_filter] section must parse to enabled + no
        // overrides — known commands are filtered out of the box, so behavior
        // is an improvement without any config change.
        let cfg: GeneralConfig = toml::from_str("").unwrap();
        assert!(cfg.shell_filter.enabled);
        assert!(cfg.shell_filter.overrides.is_empty());
    }

    #[test]
    fn shell_filter_section_parses_overrides() {
        let text = r#"
[shell_filter]
enabled = true

[[shell_filter.override]]
command = "^cargo"
drop = ["^\\s*Compiling"]
keep = ["Compiling my-favorite-crate"]
disable_builtin = true
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(cfg.shell_filter.enabled);
        assert_eq!(cfg.shell_filter.overrides.len(), 1);
        let ov = &cfg.shell_filter.overrides[0];
        assert_eq!(ov.command, "^cargo");
        assert_eq!(ov.drop, vec!["^\\s*Compiling".to_string()]);
        assert_eq!(ov.keep, vec!["Compiling my-favorite-crate".to_string()]);
        assert!(ov.disable_builtin);
    }

    #[test]
    fn shell_filter_disabled_round_trips() {
        // enabled = false must survive a parse + re-serialize cycle — the
        // explicit opt-out must not silently become the default.
        let text = r#"
[shell_filter]
enabled = false
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        assert!(!cfg.shell_filter.enabled);
        let back = toml::to_string(&cfg).unwrap();
        let cfg2: GeneralConfig = toml::from_str(&back).unwrap();
        assert!(!cfg2.shell_filter.enabled);
    }

    #[test]
    fn shell_filter_section_round_trips() {
        let cfg = GeneralConfig {
            shell_filter: ShellFilterConfig {
                enabled: true,
                overrides: vec![ShellFilterOverride {
                    command: "^npm run build".into(),
                    drop: vec!["^dist/".into()],
                    keep: vec!["^dist/index\\.html".into()],
                    disable_builtin: false,
                }],
            },
            ..GeneralConfig::default()
        };
        let text = toml::to_string(&cfg).unwrap();
        let back: GeneralConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.shell_filter, cfg.shell_filter);
    }

    #[test]
    fn shell_filter_override_defaults_false_and_empty() {
        // drop/keep/disable_builtin are optional keys — a minimal override
        // (command only) parses to empty drop/keep and disable_builtin=false.
        let text = r#"
[[shell_filter.override]]
command = "^git"
"#;
        let cfg: GeneralConfig = toml::from_str(text).unwrap();
        let ov = &cfg.shell_filter.overrides[0];
        assert!(ov.drop.is_empty());
        assert!(ov.keep.is_empty());
        assert!(!ov.disable_builtin);
    }
}
