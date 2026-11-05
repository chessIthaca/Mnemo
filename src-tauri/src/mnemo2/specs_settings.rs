// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Specs settings page's commands: OpenSpec schema management for the
//! open project — the schema list with its active pick, adding a schema
//! from a folder or a git URL, forking one (the only way to edit the
//! read-only built-in), editing a project schema's artifact templates and
//! instructions, the project `config.yaml` (worker context and artifact
//! rules), the plan preview a lead can expect under a schema, the specs
//! status panel, and the optional `openspec validate` run.
//!
//! Every command reads and writes the project fresh from disk, like the
//! backlog commands; `.coding/openspec/` stays the one source of truth.
//! Schema folders are data, never code: nothing here runs anything found
//! in a schema, and the only external processes are the `git clone` of an
//! explicitly requested URL add and the user's own `openspec` CLI, run
//! only when the page's button asks for it.
//!
//! The pure functions take paths so the tests can exercise a whole temp
//! project without a Tauri host; the `#[tauri::command]` wrappers only
//! resolve the open project and delegate.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_norway::Value;
use tauri::State;

use mnemo::app::openspec::schema::{OpenSpecConfig, Schema};
use mnemo::app::plan_board::{BoardStatus, PlanBoard};
use mnemo::openspec::Spec;

use crate::mnemo2::ipc::Mnemo2State;

// ── Wire shapes ──────────────────────────────────────────────────────────────

/// One capability of the Specs status panel: its requirement count, or the
/// format problem that kept `spec.md` from parsing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityStatus {
    /// The capability folder under `.coding/openspec/specs/`.
    pub name: String,
    /// Parsed requirements in `spec.md`; 0 when it did not parse.
    pub requirements: usize,
    /// The format-check result; `None` when `spec.md` is well formed.
    pub format_error: Option<String>,
}

/// One open plan whose spec changes have not been applied yet (they are
/// applied when the plan closes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSpecPlan {
    /// The 8-hex plan id.
    pub plan_id: String,
    /// The plan title.
    pub title: String,
    /// The distinct capabilities the plan's deltas touch, sorted.
    pub capabilities: Vec<String>,
}

/// The Specs status panel's data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecsStatus {
    /// Every capability folder, sorted by name.
    pub capabilities: Vec<CapabilityStatus>,
    /// Open plans carrying spec changes; a board that cannot be read lists
    /// no plans (the Plans tab reports its own errors).
    pub pending_plans: Vec<PendingSpecPlan>,
    /// Whether the `openspec` CLI is on PATH.
    pub openspec_cli: bool,
}

/// One schema of the list, loadable or not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaSummary {
    /// The schema's folder name — the pick key the board resolves by.
    pub name: String,
    /// `name@version` of the loaded schema.
    pub reference: String,
    /// The schema's description.
    pub description: String,
    /// `built-in`, `project`, or `user`.
    pub origin: String,
    /// Where it resolves from: `(built-in)` or the folder path.
    pub path: String,
    /// The artifact ids in authoring order.
    pub artifacts: Vec<String>,
    /// Non-fatal unknown-field warnings from loading.
    pub warnings: Vec<String>,
    /// Why the folder did not load (or is shadowed), when it cannot be
    /// picked; `None` for a healthy schema.
    pub error: Option<String>,
}

/// The schema list plus which schema is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaList {
    /// The built-in first, then the project's and the user's custom
    /// schemas, sorted by folder name.
    pub schemas: Vec<SchemaSummary>,
    /// The active schema key: what `config.yaml` records, or
    /// `spec-driven` when it records nothing.
    pub active: String,
    /// Why resolving the active schema failed, when it did.
    pub active_error: Option<String>,
}

/// One artifact of the template editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactView {
    /// The stable artifact id.
    pub id: String,
    /// What the artifact generates.
    pub generates: String,
    /// The template filename beneath `templates/`.
    pub template: String,
    /// The instruction included in worker briefs.
    pub instruction: String,
    /// The artifacts that must exist first.
    pub requires: Vec<String>,
    /// The template body as read from disk — the editor's working text.
    pub template_text: String,
}

/// The artifact editor's data for one schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactsView {
    /// The schema's folder name (the pick key).
    pub name: String,
    /// `name@version`.
    pub reference: String,
    /// `built-in`, `project`, or `user`. Only `project` schemas are
    /// editable; the others must be forked first.
    pub origin: String,
    /// The schema description.
    pub description: String,
    /// The artifacts in authoring order.
    pub artifacts: Vec<ArtifactView>,
}

/// The project's `config.yaml` as the page edits it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecsConfig {
    /// The active schema key recorded in `config.yaml`; `None` when the
    /// file records none (the built-in applies).
    pub schema: Option<String>,
    /// The project context included in every worker brief.
    pub context: String,
    /// Additional rules keyed by artifact id.
    pub rules: BTreeMap<String, Vec<String>>,
    /// Non-fatal unknown-key warnings from loading `config.yaml`.
    pub warnings: Vec<String>,
}

/// One plan section of the preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPreviewSection {
    /// The artifact (and plan section) id.
    pub id: String,
    /// What the artifact generates.
    pub generates: String,
    /// The artifacts that must exist first.
    pub requires: Vec<String>,
    /// The instruction the lead fills the section under.
    pub instruction: String,
    /// The template body the section starts from.
    pub template_text: String,
    /// Whether `team_dispatch` is refused until this section is filled
    /// (`apply.requires` and what those artifacts require).
    pub required_for_dispatch: bool,
}

/// The plan preview: what a new plan under this schema must contain and
/// the context its workers would receive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPreview {
    /// The schema's own name from `schema.yaml`.
    pub name: String,
    /// `name@version`.
    pub reference: String,
    /// The schema description.
    pub description: String,
    /// The plan sections in authoring order.
    pub sections: Vec<PlanPreviewSection>,
    /// The ids a plan must have filled before `team_dispatch` runs.
    pub required: Vec<String>,
    /// The `## Project context` block a worker brief carries under this
    /// schema and config — composed exactly as the board composes it.
    pub worker_context: String,
}

/// The `openspec validate` output, once it ran.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidateOutput {
    /// The CLI's exit code; `None` when the process could not be waited
    /// for.
    pub exit_code: Option<i32>,
    /// The CLI's stdout.
    pub stdout: String,
    /// The CLI's stderr.
    pub stderr: String,
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// The open project's `.coding/openspec/` directory.
fn openspec_dir(root: &Path) -> PathBuf {
    root.join(".coding").join("openspec")
}

/// The user-level OpenSpec schema directory, mirroring the kernel's
/// resolution (`$HOME` or `$USERPROFILE`, then `.local/share/openspec/`).
fn user_schema_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".local").join("share").join("openspec").join("schemas"))
}

/// A single safe path component — the same rule the kernel's schema
/// resolution applies to schema names.
fn safe_component(name: &str) -> Option<&str> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(component)), None) if component == name => Some(name),
        _ => None,
    }
}

/// A relative path that stays inside the schema folder — the rule
/// `Schema::load` applies to template names.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        None
    } else {
        Some(path.to_path_buf())
    }
}

/// The name part of a `name` or `name@version` pick, with the kernel's
/// validation of the recorded form.
fn lookup_name(recorded: &str) -> Result<String, String> {
    let trimmed = recorded.trim();
    let (name, version) = trimmed
        .rsplit_once('@')
        .map(|(name, version)| (name, Some(version)))
        .unwrap_or((trimmed, None));
    if name.trim().is_empty() || version.is_some_and(|version| version.trim().is_empty()) {
        return Err(format!(
            "invalid OpenSpec schema '{recorded}': expected name@version"
        ));
    }
    Ok(name.trim().to_string())
}

/// The subdirectories of `dir`, sorted by name; a missing or unreadable
/// `dir` lists empty.
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The folder a named non-built-in schema resolves from: the project's
/// `schemas/` first, then the user-level OpenSpec directory — the same
/// order `Schema::resolve` resolves in.
fn resolve_schema_folder(openspec_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let Some(safe_name) = safe_component(name) else {
        return Err(format!(
            "invalid OpenSpec schema name '{name}': expected one path component"
        ));
    };
    let project = openspec_dir.join("schemas").join(safe_name);
    if project.join("schema.yaml").is_file() {
        return Ok(project);
    }
    if let Some(user) = user_schema_dir(safe_name) {
        if user.join("schema.yaml").is_file() {
            return Ok(user);
        }
        return Err(format!(
            "OpenSpec schema '{name}' was not found at '{}' or '{}'",
            project.display(),
            user.display()
        ));
    }
    Err(format!(
        "OpenSpec schema '{name}' was not found at '{}'",
        project.display()
    ))
}

/// The user-level directory of one named schema (the kernel keeps its own
/// copy private; the path is a documented OpenSpec convention).
fn user_schema_dir(name: &str) -> Option<PathBuf> {
    user_schema_root().map(|root| root.join(name))
}

// ── Specs status ─────────────────────────────────────────────────────────────

/// The Specs status panel's data: every capability under
/// `.coding/openspec/specs/` with its requirement count or format error,
/// the open plans whose spec changes have not been applied yet, and
/// whether the `openspec` CLI is on PATH.
pub fn status(root: &Path) -> SpecsStatus {
    let dir = openspec_dir(root);
    let mut capabilities = Vec::new();
    for folder in child_dirs(&dir.join("specs")) {
        let name = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let spec_path = folder.join("spec.md");
        let (requirements, format_error) = match std::fs::read_to_string(&spec_path) {
            Ok(text) => match Spec::parse(&text) {
                Ok(spec) => (spec.requirements().len(), None),
                Err(error) => (0, Some(error.to_string())),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (0, Some("spec.md is missing".to_string()))
            }
            Err(error) => (0, Some(format!("could not read spec.md: {error}"))),
        };
        capabilities.push(CapabilityStatus {
            name,
            requirements,
            format_error,
        });
    }
    let board = PlanBoard::new(
        root.join(".coding").join("plans"),
        root.join(".coding").join("reviews"),
    );
    let pending_plans = match board.list() {
        Ok(plans) => plans
            .into_iter()
            .filter(|plan| plan.status == BoardStatus::Open && !plan.spec_deltas.is_empty())
            .map(|plan| {
                let mut touched: Vec<String> = plan
                    .spec_deltas
                    .iter()
                    .map(|delta| delta.capability.clone())
                    .collect();
                touched.sort();
                touched.dedup();
                PendingSpecPlan {
                    plan_id: plan.id,
                    title: plan.title,
                    capabilities: touched,
                }
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    SpecsStatus {
        capabilities,
        pending_plans,
        openspec_cli: find_openspec_cli().is_some(),
    }
}

// ── Current spec ─────────────────────────────────────────────────────────────

/// The current spec of one capability, or None when it has none yet.
/// The read refuses links — a linked capability folder or `spec.md`
/// whose target sits outside the project would serve bytes the project
/// never held.
fn capability_spec(root: &Path, capability: &str) -> Result<Option<String>, String> {
    mnemo::app::openspec::read_capability_spec(&openspec_dir(root), capability)
}

// ── Schema list and activation ───────────────────────────────────────────────

/// Every schema the project can pick, with where it resolves and which
/// one is active: the built-in, then the project's and the user's custom
/// schemas sorted by folder name. A folder that does not load is listed
/// with its error so the page can show why it cannot be picked; a user
/// schema shadowed by a same-named project schema is skipped.
pub fn schemas(openspec_dir: &Path) -> SchemaList {
    let mut list = vec![builtin_summary()];
    let project_root = openspec_dir.join("schemas");
    let mut project_names = BTreeSet::new();
    for folder in child_dirs(&project_root) {
        let Some(name) = folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        let mut summary = folder_summary("project", &folder);
        if name.eq_ignore_ascii_case("spec-driven") {
            summary.error =
                Some("shadowed by the built-in schema; this folder is never resolved".into());
        }
        project_names.insert(name.to_lowercase());
        list.push(summary);
    }
    if let Some(user) = user_schema_root() {
        for folder in child_dirs(&user) {
            let Some(name) = folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
            else {
                continue;
            };
            if project_names.contains(&name.to_lowercase()) {
                // The project schema wins in resolution; this copy is
                // unreachable.
                continue;
            }
            let mut summary = folder_summary("user", &folder);
            if name.eq_ignore_ascii_case("spec-driven") {
                summary.error =
                    Some("shadowed by the built-in schema; this folder is never resolved".into());
            }
            list.push(summary);
        }
    }
    let active = OpenSpecConfig::load(openspec_dir)
        .ok()
        .and_then(|config| config.schema)
        .map(|schema| schema.trim().to_string())
        .filter(|schema| !schema.is_empty())
        .unwrap_or_else(|| "spec-driven".to_string());
    let active_error = Schema::resolve(openspec_dir, None).err();
    SchemaList {
        schemas: list,
        active,
        active_error,
    }
}

/// The built-in `spec-driven` schema summary — read-only, forked to edit.
fn builtin_summary() -> SchemaSummary {
    let schema = Schema::spec_driven();
    SchemaSummary {
        name: schema.name.clone(),
        reference: schema.reference(),
        description: schema.description.clone(),
        origin: "built-in".to_string(),
        path: "(built-in)".to_string(),
        artifacts: schema
            .artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect(),
        warnings: Vec::new(),
        error: None,
    }
}

/// One schema folder summarized: the folder name is the pick key (the
/// kernel resolves by folder), and the load error is surfaced so the page
/// can show why the schema cannot be picked.
fn folder_summary(origin: &str, folder: &Path) -> SchemaSummary {
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = folder.display().to_string();
    match Schema::load(folder) {
        Ok(schema) => SchemaSummary {
            name,
            reference: schema.reference(),
            description: schema.description.clone(),
            origin: origin.to_string(),
            path,
            artifacts: schema
                .artifacts
                .iter()
                .map(|artifact| artifact.id.clone())
                .collect(),
            warnings: schema.warnings,
            error: None,
        },
        Err(error) => SchemaSummary {
            name,
            reference: String::new(),
            description: String::new(),
            origin: origin.to_string(),
            path,
            artifacts: Vec::new(),
            warnings: Vec::new(),
            error: Some(error),
        },
    }
}

/// Make `name` the project's default schema: it must resolve (so a pick
/// can never leave the board stranded), then `config.yaml`'s `schema` key
/// is rewritten with context, rules, and unknown keys kept. Returns the
/// refreshed list.
pub fn activate_schema(openspec_dir: &Path, name: &str) -> Result<SchemaList, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("pick a schema to make active".to_string());
    }
    Schema::resolve(openspec_dir, Some(trimmed))?;
    let mut config = OpenSpecConfig::load(openspec_dir)?;
    config.schema = Some(trimmed.to_string());
    write_config(openspec_dir, &config)?;
    Ok(schemas(openspec_dir))
}

// ── config.yaml read/write ───────────────────────────────────────────────────

/// The serialization shape of `config.yaml`: like `OpenSpecConfig`, but a
/// missing schema and empty context and rules are omitted rather than
/// written as `null`/empty keys. Unknown keys ride along through
/// `extra`.
#[derive(Serialize)]
struct ConfigFile<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<&'a str>,
    #[serde(skip_serializing_if = "str::is_empty")]
    context: &'a str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    rules: &'a BTreeMap<String, Vec<String>>,
    #[serde(flatten)]
    extra: &'a BTreeMap<String, Value>,
}

impl<'a> ConfigFile<'a> {
    /// The write shape of a loaded config.
    fn of(config: &'a OpenSpecConfig) -> Self {
        Self {
            schema: config.schema.as_deref(),
            context: &config.context,
            rules: &config.rules,
            extra: &config.extra,
        }
    }
}

/// Write `config.yaml` beneath the OpenSpec directory, creating parent
/// folders as needed. The file is re-serialized, so YAML comments inside
/// it are lost; `schema`, `context`, `rules`, and every unknown key keep
/// their values.
fn write_config(openspec_dir: &Path, config: &OpenSpecConfig) -> Result<(), String> {
    let path = openspec_dir.join("config.yaml");
    let text = serde_norway::to_string(&ConfigFile::of(config))
        .map_err(|error| format!("could not serialize '{}': {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!("could not create '{}': {error}", parent.display())
        })?;
    }
    std::fs::write(&path, text)
        .map_err(|error| format!("could not write '{}': {error}", path.display()))
}

/// The project's `config.yaml`: the active schema key, the worker
/// context, the artifact rules, and unknown-key warnings.
pub fn config(openspec_dir: &Path) -> Result<SpecsConfig, String> {
    let config = OpenSpecConfig::load(openspec_dir)?;
    Ok(SpecsConfig {
        schema: config.schema,
        context: config.context,
        rules: config.rules,
        warnings: config.warnings,
    })
}

/// Save the worker context and the artifact rules. The active schema and
/// unknown keys keep their values; the file is re-serialized, so YAML
/// comments inside it are lost. Returns the persisted state.
pub fn store_config(
    openspec_dir: &Path,
    context: &str,
    rules: &BTreeMap<String, Vec<String>>,
) -> Result<SpecsConfig, String> {
    let mut current = OpenSpecConfig::load(openspec_dir)?;
    current.context = context.to_string();
    current.rules = rules.clone();
    write_config(openspec_dir, &current)?;
    config(openspec_dir)
}

// ── Adding a schema ──────────────────────────────────────────────────────────

/// A staged `add_schema` source: the folder the user pointed at, or a
/// fresh clone in a temp dir that must outlive the copy into the project.
enum StagedSource {
    /// The user's own folder; nothing to clean up.
    Folder(PathBuf),
    /// A shallow clone in a dir that is removed when dropped.
    Clone(tempfile::TempDir),
}

impl StagedSource {
    /// The root the schema folder is located beneath.
    fn path(&self) -> &Path {
        match self {
            Self::Folder(path) => path,
            Self::Clone(dir) => dir.path(),
        }
    }
}

/// Whether `source` names a remote (cloned with git) rather than a folder.
fn is_git_source(source: &str) -> bool {
    let source = source.trim();
    ["https://", "http://", "git@", "ssh://", "git://"]
        .iter()
        .any(|prefix| source.starts_with(prefix))
        || source.ends_with(".git")
}

/// Add a schema from a folder path or a git URL (cloned shallowly into a
/// temp dir). The schema must load and validate — a missing template or
/// malformed `schema.yaml` is an error and copies nothing — and it lands
/// under `.coding/openspec/schemas/<schema name>/`. A source holding a
/// symbolic link (its target may sit anywhere on disk) is refused the
/// same way. Returns the refreshed list.
pub fn add_schema_from(openspec_dir: &Path, source: &str) -> Result<SchemaList, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("enter a schema folder or git URL to add".to_string());
    }
    let staged = if is_git_source(source) {
        let clone = tempfile::TempDir::new()
            .map_err(|error| format!("could not create a temp dir for the clone: {error}"))?;
        clone_repo(source, clone.path())?;
        StagedSource::Clone(clone)
    } else {
        let folder = PathBuf::from(source);
        if !folder.is_dir() {
            return Err(format!("'{}' is not a folder", folder.display()));
        }
        StagedSource::Folder(folder)
    };
    let schema_folder = locate_schema_folder(staged.path())?;
    let schema = Schema::load(&schema_folder)?;
    refuse_symlinked_schema(&schema_folder, &schema)?;
    let name = schema.name.trim().to_string();
    if name.eq_ignore_ascii_case("spec-driven") {
        return Err("'spec-driven' is the built-in schema; fork it under a new name".to_string());
    }
    if safe_component(&name).is_none() {
        return Err(format!(
            "schema name '{name}' must be a single folder name without separators"
        ));
    }
    let dest = openspec_dir.join("schemas").join(&name);
    if dest.exists() {
        return Err(format!("schema '{name}' already exists at '{}'", dest.display()));
    }
    copy_tree(&schema_folder, &dest)?;
    Ok(schemas(openspec_dir))
}

/// The refusal message for a symbolic link at `path`, naming it.
fn symlink_refused(path: &Path) -> String {
    format!(
        "schema sources may not contain symbolic links: '{}'",
        path.display()
    )
}

/// Refuse a schema source whose located folder, its `schema.yaml`, or
/// any artifact template is a symbolic link. `Schema::load`'s reads
/// follow links, so without this check a link whose target sits
/// outside the source — a private key posing as a template — would
/// load, and the copy would bring the target's bytes into the project.
fn refuse_symlinked_schema(folder: &Path, schema: &Schema) -> Result<(), String> {
    let refuse = |path: &Path| {
        if std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            Err(symlink_refused(path))
        } else {
            Ok(())
        }
    };
    refuse(folder)?;
    refuse(&folder.join("schema.yaml"))?;
    for artifact in &schema.artifacts {
        // `Schema::load` already rejected unsafe template paths.
        if let Some(template) = safe_relative(&artifact.template) {
            refuse(&folder.join("templates").join(template))?;
        }
    }
    Ok(())
}

/// Clone `url` shallowly into `target` with git, with
/// `core.symlinks=false` so a crafted repository cannot check a
/// symbolic link out; a refusal is reported with git's own stderr.
fn clone_repo(url: &str, target: &Path) -> Result<(), String> {
    let output = Command::new("git")
        .arg("clone")
        .arg("-c")
        .arg("core.symlinks=false")
        .arg("--depth")
        .arg("1")
        .arg("--")
        .arg(url)
        .arg(target)
        .output()
        .map_err(|error| format!("could not run git to clone '{url}': {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stdout.is_empty() {
            stderr
        } else {
            format!("{stderr}\n{stdout}")
        };
        return Err(format!(
            "git clone of '{url}' failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            detail
        ));
    }
    Ok(())
}

/// Locate the schema folder to add beneath `root`: the root itself when
/// it holds `schema.yaml`, or the single `schemas/<name>/` entry that
/// does. Multiple candidates or none are errors that name what was
/// found, so the page can show why nothing was copied.
fn locate_schema_folder(root: &Path) -> Result<PathBuf, String> {
    if root.join("schema.yaml").is_file() {
        return Ok(root.to_path_buf());
    }
    let nested = root.join("schemas");
    if nested.is_dir() {
        let candidates: Vec<PathBuf> = child_dirs(&nested)
            .into_iter()
            .filter(|folder| folder.join("schema.yaml").is_file())
            .collect();
        match candidates.len() {
            0 => {}
            1 => return Ok(candidates[0].clone()),
            _ => {
                let names: Vec<String> = candidates
                    .iter()
                    .filter_map(|folder| {
                        folder
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                    })
                    .collect();
                return Err(format!(
                    "the source holds {} schemas ({}); point at one schema folder",
                    names.len(),
                    names.join(", ")
                ));
            }
        }
    }
    Err(format!(
        "no schema.yaml found in '{}' (looked in the folder and in its schemas/ subfolders)",
        root.display()
    ))
}

/// Walk the whole copy source and refuse anything whose link would be
/// followed: a symbolic link (file or directory), or an entry that is
/// neither a file nor a folder. `entry.file_type()` reads the link
/// itself, never its target, and `.git` is skipped exactly as the copy
/// skips it. The walk runs before `copy_tree` writes anything, so a
/// refused source copies nothing.
fn check_copyable(from: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(from)
        .map_err(|error| format!("could not read '{}': {error}", from.display()))?
    {
        let entry =
            entry.map_err(|error| format!("could not read '{}': {error}", from.display()))?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let source = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not read '{}': {error}", source.display()))?;
        if file_type.is_symlink() {
            return Err(symlink_refused(&source));
        }
        if file_type.is_dir() {
            check_copyable(&source)?;
        } else if !file_type.is_file() {
            return Err(format!(
                "schema sources may only contain files and folders, not '{}'",
                source.display()
            ));
        }
    }
    Ok(())
}

/// Copy a schema folder tree into a fresh `dest`, skipping `.git`. The
/// whole source is walked for links first ([`check_copyable`]), so a
/// refused source copies nothing at all; any failure aborts with an
/// error, only the `dest` (which never existed before) is left partial,
/// and re-adding after deleting it starts clean.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(from)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(symlink_refused(from));
    }
    check_copyable(from)?;
    copy_tree_entries(from, to)
}

/// The recursive copy half of [`copy_tree`], guarded by its link walk.
/// `entry.file_type()` re-refuses a link per entry, so one that
/// appeared between the walk and the copy is not followed either.
fn copy_tree_entries(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to)
        .map_err(|error| format!("could not create '{}': {error}", to.display()))?;
    for entry in std::fs::read_dir(from)
        .map_err(|error| format!("could not read '{}': {error}", from.display()))?
    {
        let entry =
            entry.map_err(|error| format!("could not read '{}': {error}", from.display()))?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let source = entry.path();
        let target = to.join(&name);
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not read '{}': {error}", source.display()))?;
        if file_type.is_symlink() {
            return Err(symlink_refused(&source));
        }
        if file_type.is_dir() {
            copy_tree_entries(&source, &target)?;
        } else if file_type.is_file() {
            std::fs::copy(&source, &target).map_err(|error| {
                format!(
                    "could not copy '{}' to '{}': {error}",
                    source.display(),
                    target.display()
                )
            })?;
        } else {
            return Err(format!(
                "schema sources may only contain files and folders, not '{}'",
                source.display()
            ));
        }
    }
    Ok(())
}

// ── Forking a schema ──────────────────────────────────────────────────────────

/// Fork the schema `from` (the built-in included) into a new project
/// schema `to`: the fork is editable in place, the source is not touched.
/// The `name` in the forked `schema.yaml` is rewritten, so the fork
/// resolves under its new key. Returns the refreshed list.
pub fn fork_schema_into(
    openspec_dir: &Path,
    from: &str,
    to: &str,
) -> Result<SchemaList, String> {
    let from_name = lookup_name(from)?;
    if from_name.is_empty() {
        return Err("enter the schema to fork".to_string());
    }
    let to_name = to.trim().to_string();
    if to_name.is_empty() {
        return Err("enter the new schema name".to_string());
    }
    if to_name.eq_ignore_ascii_case("spec-driven") {
        return Err("'spec-driven' is the built-in schema; pick a different name".to_string());
    }
    if safe_component(&to_name).is_none() {
        return Err(format!(
            "schema name '{to_name}' must be a single folder name without separators"
        ));
    }
    let dest = openspec_dir.join("schemas").join(&to_name);
    if dest.exists() {
        return Err(format!(
            "schema '{to_name}' already exists at '{}'",
            dest.display()
        ));
    }
    if from_name.eq_ignore_ascii_case("spec-driven") {
        let mut schema = Schema {
            name: to_name.clone(),
            ..Schema::spec_driven()
        };
        for artifact in &mut schema.artifacts {
            if artifact.template.contains('*') {
                artifact.template = literal_template_name(&artifact.id, &artifact.template);
            }
        }
        write_schema_files(&dest, &schema)?;
    } else {
        let folder = resolve_schema_folder(openspec_dir, &from_name)?;
        Schema::load(&folder)?;
        copy_tree(&folder, &dest)?;
        rewrite_schema_name(&dest, &to_name)?;
    }
    // The fork must end valid; anything else refused before it can be
    // picked.
    Schema::load(&dest)?;
    Ok(schemas(openspec_dir))
}

/// A literal template filename for a glob template: the artifact id with
/// the glob's extension. The built-in's `specs/**/*.md` forks as
/// `specs.md` — a folder on disk cannot hold a glob name (Windows forbids
/// `*` outright), and the fork is editable in place.
fn literal_template_name(id: &str, template: &str) -> String {
    let extension = Path::new(template.rsplit(['/', '\\']).next().unwrap_or(template))
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    format!("{id}{extension}")
}

/// Rewrite the forked schema's `name` in its `schema.yaml`. The file is
/// parsed and re-serialized, so YAML comments inside it are lost; every
/// other key, including unrecognized ones, keeps its value and order.
fn rewrite_schema_name(dest: &Path, name: &str) -> Result<(), String> {
    let path = dest.join("schema.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read '{}': {error}", path.display()))?;
    let mut value: Value = serde_norway::from_str(&text)
        .map_err(|error| format!("could not parse '{}': {error}", path.display()))?;
    let Value::Mapping(mapping) = &mut value else {
        return Err(format!("'{}' is not a schema mapping", path.display()));
    };
    mapping.insert(Value::String("name".into()), Value::String(name.into()));
    let out = serde_norway::to_string(&value)
        .map_err(|error| format!("could not serialize '{}': {error}", path.display()))?;
    std::fs::write(&path, out)
        .map_err(|error| format!("could not write '{}': {error}", path.display()))
}

/// Materialize a schema folder from an in-memory schema: `schema.yaml`
/// plus one template file per artifact. Used for the built-in fork,
/// which has no folder on disk.
fn write_schema_files(dest: &Path, schema: &Schema) -> Result<(), String> {
    std::fs::create_dir_all(dest.join("templates")).map_err(|error| {
        format!(
            "could not create '{}': {error}",
            dest.join("templates").display()
        )
    })?;
    let yaml = serde_norway::to_string(schema)
        .map_err(|error| format!("could not serialize schema.yaml: {error}"))?;
    std::fs::write(dest.join("schema.yaml"), yaml)
        .map_err(|error| format!("could not write schema.yaml: {error}"))?;
    for artifact in &schema.artifacts {
        let Some(template) = safe_relative(&artifact.template) else {
            return Err(format!(
                "artifact '{}' template must be a relative path inside templates/",
                artifact.id
            ));
        };
        let path = dest.join("templates").join(template);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!("could not create '{}': {error}", parent.display())
            })?;
        }
        std::fs::write(&path, &artifact.template_text).map_err(|error| {
            format!("could not write '{}': {error}", path.display())
        })?;
    }
    Ok(())
}

// ── Artifact templates and instructions ──────────────────────────────────────

/// One schema's artifact list for the template editor: the artifacts
/// with their template bodies and instructions, and where the schema
/// resolves from.
pub fn artifacts(openspec_dir: &Path, name: &str) -> Result<ArtifactsView, String> {
    let key = lookup_name(name)?;
    let resolved = Schema::resolve(openspec_dir, Some(name.trim()))?;
    let origin = schema_origin(openspec_dir, &key);
    let schema = resolved.schema;
    Ok(ArtifactsView {
        name: key,
        reference: schema.reference(),
        origin: origin.to_string(),
        description: schema.description,
        artifacts: schema
            .artifacts
            .iter()
            .map(|artifact| ArtifactView {
                id: artifact.id.clone(),
                generates: artifact.generates.clone(),
                template: artifact.template.clone(),
                instruction: artifact.instruction.clone(),
                requires: artifact.requires.clone(),
                template_text: artifact.template_text.clone(),
            })
            .collect(),
    })
}

/// Where a named schema resolves from: `built-in`, `project`, or `user`.
fn schema_origin(openspec_dir: &Path, name: &str) -> &'static str {
    if name.eq_ignore_ascii_case("spec-driven") {
        "built-in"
    } else if openspec_dir
        .join("schemas")
        .join(name)
        .join("schema.yaml")
        .is_file()
    {
        "project"
    } else {
        "user"
    }
}

/// Save one artifact's template body and instruction in a project schema.
/// The template file and the `instruction` in `schema.yaml` are written,
/// then the whole schema is reloaded: an invalid result is refused and the
/// previous files are restored. Only project schemas are editable; the
/// built-in and user schemas are forked instead. Returns the reloaded
/// artifact view.
pub fn store_artifact(
    openspec_dir: &Path,
    name: &str,
    id: &str,
    template: &str,
    instruction: &str,
) -> Result<ArtifactsView, String> {
    let key = lookup_name(name)?;
    if key.eq_ignore_ascii_case("spec-driven") {
        return Err("the built-in schema is read-only; fork it to edit its artifacts".to_string());
    }
    let folder = openspec_dir.join("schemas").join(&key);
    if !folder.is_dir() {
        return Err(format!(
            "schema '{key}' is not a project schema; fork it to edit it"
        ));
    }
    // Load first: refuse to edit a broken schema, and capture the bytes
    // to restore when the edited result does not validate.
    let schema = Schema::load(&folder)?;
    let Some(artifact) = schema.artifact(id.trim()) else {
        return Err(format!("schema '{key}' has no artifact '{id}'"));
    };
    let Some(relative) = safe_relative(&artifact.template) else {
        return Err(format!(
            "artifact '{}' template must be a relative path inside templates/",
            artifact.id
        ));
    };
    let template_path = folder.join("templates").join(relative);
    let schema_path = folder.join("schema.yaml");
    let old_template = std::fs::read_to_string(&template_path).map_err(|error| {
        format!("could not read '{}': {error}", template_path.display())
    })?;
    let old_schema = std::fs::read_to_string(&schema_path)
        .map_err(|error| format!("could not read '{}': {error}", schema_path.display()))?;
    std::fs::write(&template_path, template).map_err(|error| {
        format!("could not write '{}': {error}", template_path.display())
    })?;
    if let Err(error) = store_instruction(&folder, id.trim(), instruction) {
        let _ = std::fs::write(&template_path, &old_template);
        return Err(error);
    }
    if let Err(error) = Schema::load(&folder) {
        let _ = std::fs::write(&template_path, &old_template);
        let _ = std::fs::write(&schema_path, &old_schema);
        return Err(format!("the edited schema is invalid: {error}"));
    }
    artifacts(openspec_dir, name)
}

/// Set one artifact's `instruction` in the schema's `schema.yaml`. The
/// file is parsed and re-serialized (comments are lost; every other key
/// keeps its value and order).
fn store_instruction(schema_dir: &Path, id: &str, instruction: &str) -> Result<(), String> {
    let path = schema_dir.join("schema.yaml");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read '{}': {error}", path.display()))?;
    let mut value: Value = serde_norway::from_str(&text)
        .map_err(|error| format!("could not parse '{}': {error}", path.display()))?;
    let Some(entries) = value.get_mut("artifacts") else {
        return Err(format!("'{}' has no artifacts list", path.display()));
    };
    let Value::Sequence(entries) = entries else {
        return Err(format!("'{}': artifacts is not a list", path.display()));
    };
    for entry in entries.iter_mut() {
        let Value::Mapping(mapping) = entry else {
            continue;
        };
        let matches = mapping
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|found| found.trim() == id);
        if matches {
            mapping.insert(
                Value::String("instruction".into()),
                Value::String(instruction.to_string()),
            );
            let out = serde_norway::to_string(&value).map_err(|error| {
                format!("could not serialize '{}': {error}", path.display())
            })?;
            std::fs::write(&path, out)
                .map_err(|error| format!("could not write '{}': {error}", path.display()))?;
            return Ok(());
        }
    }
    Err(format!("'{}' has no artifact '{id}'", path.display()))
}

// ── Plan preview ──────────────────────────────────────────────────────────────

/// Preview a new plan under one schema: the sections it gets (and which
/// of them `team_dispatch` insists on), and the exact `## Project context`
/// block a worker brief would carry with the current config.
pub fn preview(openspec_dir: &Path, name: &str) -> Result<PlanPreview, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("pick a schema to preview".to_string());
    }
    let resolved = Schema::resolve(openspec_dir, Some(trimmed))?;
    let schema = &resolved.schema;
    let required = required_ids(schema);
    let sections = schema
        .artifacts
        .iter()
        .map(|artifact| PlanPreviewSection {
            id: artifact.id.clone(),
            generates: artifact.generates.clone(),
            requires: artifact.requires.clone(),
            instruction: artifact.instruction.clone(),
            template_text: artifact.template_text.clone(),
            required_for_dispatch: required.contains(&artifact.id),
        })
        .collect();
    Ok(PlanPreview {
        name: schema.name.clone(),
        reference: schema.reference(),
        description: schema.description.clone(),
        sections,
        required,
        worker_context: worker_context(resolved.config.context.trim(), schema),
    })
}

/// The ids a plan must have filled before `team_dispatch`:
/// `apply.requires` plus everything those artifacts require,
/// transitively, in resolution order.
fn required_ids(schema: &Schema) -> Vec<String> {
    let mut required: Vec<String> = schema.apply.requires.clone();
    let mut index = 0;
    while index < required.len() {
        if let Some(artifact) = schema.artifact(&required[index]) {
            for id in &artifact.requires {
                if !required.iter().any(|known| known == id) {
                    required.push(id.clone());
                }
            }
        }
        index += 1;
    }
    required
}

/// The `## Project context` block a worker brief carries — composed
/// exactly like `PlanBoard::worker_context`, so the preview shows the
/// real thing.
fn worker_context(context: &str, schema: &Schema) -> String {
    let mut out = String::new();
    if !context.is_empty() {
        out.push_str(&format!("\n{context}\n"));
    }
    for artifact in &schema.artifacts {
        let instruction = artifact.instruction.trim();
        if !instruction.is_empty() {
            out.push_str(&format!("\n### Artifact: {}\n{instruction}\n", artifact.id));
        }
    }
    if out.is_empty() {
        out
    } else {
        format!("\n## Project context\n{out}")
    }
}

// ── openspec validate ─────────────────────────────────────────────────────────

/// Locate the `openspec` CLI on PATH: any of the names the npm installer
/// lays out (Windows file lookups are case-insensitive, so the lowercase
/// candidates cover the uppercase shims too).
fn find_openspec_cli() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_cli_in_paths(&path)
}

/// Locate the `openspec` CLI among the entries of one `PATH` value.
fn find_cli_in_paths(paths: &OsStr) -> Option<PathBuf> {
    for dir in std::env::split_paths(paths) {
        for candidate in ["openspec", "openspec.exe", "openspec.cmd", "openspec.bat"] {
            let hit = dir.join(candidate);
            if hit.is_file() {
                return Some(hit);
            }
        }
    }
    None
}

/// Run `openspec validate` in the open project's `.coding/` directory —
/// only ever with the CLI the user installed on PATH, and only because
/// the page's button asked for it. Nothing from a schema runs.
pub fn validate(root: &Path) -> Result<ValidateOutput, String> {
    validate_with(root, find_openspec_cli())
}

/// The tested core of [`validate`]: with the CLI already resolved (or
/// known absent), refuse without it, then run it with `.coding/` as the
/// working directory and collect its output whatever its exit code is.
fn validate_with(root: &Path, cli: Option<PathBuf>) -> Result<ValidateOutput, String> {
    let Some(cli) = cli else {
        return Err("the openspec CLI is not on PATH".to_string());
    };
    let cwd = root.join(".coding");
    if !cwd.is_dir() {
        return Err(format!(
            "'{}' does not exist; open a project first",
            cwd.display()
        ));
    }
    run_cli(&cli, &cwd, &["validate"])
}

/// Run the resolved CLI with `args` in `cwd`, collecting its output
/// whatever its exit code is.
fn run_cli(cli: &Path, cwd: &Path, args: &[&str]) -> Result<ValidateOutput, String> {
    let output = cli_command(cli, cwd, args)?;
    Ok(ValidateOutput {
        exit_code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Spawn the resolved CLI. On Windows the npm shim is a `.cmd` script,
/// which `Command` alone can neither resolve nor execute, so `cmd /S /C`
/// runs it with the CLI's full path quoted and the args appended.
fn cli_command(cli: &Path, cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    #[cfg(windows)]
    let mut run = {
        let script = format!("\"{}\" {}", cli.display(), args.join(" "));
        let mut run = Command::new("cmd");
        run.args(["/S", "/C"]).arg(script);
        run
    };
    #[cfg(not(windows))]
    let mut run = {
        let mut run = Command::new(cli);
        run.args(args);
        run
    };
    run.current_dir(cwd).output().map_err(|error| {
        format!(
            "could not run the openspec CLI at '{}': {error}",
            cli.display()
        )
    })
}

// ── Tauri commands ────────────────────────────────────────────────────────────

/// The open lead's workspace root, with the same clear error the backlog
/// commands return when no lead is open.
async fn project_root(state: &Mnemo2State) -> Result<PathBuf, String> {
    let lead = state
        .open_lead()
        .await
        .ok_or_else(|| "Mnemo2 has no open lead; call lead_open first".to_string())?;
    Ok(lead.workspace_root().to_path_buf())
}

/// The Specs status panel.
#[tauri::command]
pub async fn specs_status(state: State<'_, Mnemo2State>) -> Result<SpecsStatus, String> {
    let root = project_root(&state).await?;
    Ok(status(&root))
}

/// The current spec of one capability — `None` when the capability has
/// no spec yet.
#[tauri::command]
pub async fn read_capability_spec(
    state: State<'_, Mnemo2State>,
    capability: String,
) -> Result<Option<String>, String> {
    let root = project_root(&state).await?;
    capability_spec(&root, &capability)
}

/// Every schema the project can pick, with the active one.
#[tauri::command]
pub async fn list_schemas(state: State<'_, Mnemo2State>) -> Result<SchemaList, String> {
    let root = project_root(&state).await?;
    Ok(schemas(&openspec_dir(&root)))
}

/// Make `name` the project's default schema.
#[tauri::command]
pub async fn set_active_schema(
    state: State<'_, Mnemo2State>,
    name: String,
) -> Result<SchemaList, String> {
    let root = project_root(&state).await?;
    activate_schema(&openspec_dir(&root), &name)
}

/// Add a schema from a folder path or a git URL.
#[tauri::command]
pub async fn add_schema(
    state: State<'_, Mnemo2State>,
    source: String,
) -> Result<SchemaList, String> {
    let root = project_root(&state).await?;
    add_schema_from(&openspec_dir(&root), &source)
}

/// Fork a schema (the built-in included) into a new project schema.
#[tauri::command]
pub async fn fork_schema(
    state: State<'_, Mnemo2State>,
    from: String,
    to: String,
) -> Result<SchemaList, String> {
    let root = project_root(&state).await?;
    fork_schema_into(&openspec_dir(&root), &from, &to)
}

/// One schema's artifact list for the template editor.
#[tauri::command]
pub async fn read_artifacts(
    state: State<'_, Mnemo2State>,
    name: String,
) -> Result<ArtifactsView, String> {
    let root = project_root(&state).await?;
    artifacts(&openspec_dir(&root), &name)
}

/// Save one artifact's template body and instruction in a project schema.
#[tauri::command]
pub async fn save_artifact(
    state: State<'_, Mnemo2State>,
    name: String,
    id: String,
    template: String,
    instruction: String,
) -> Result<ArtifactsView, String> {
    let root = project_root(&state).await?;
    store_artifact(&openspec_dir(&root), &name, &id, &template, &instruction)
}

/// The project's `config.yaml` (worker context and artifact rules).
#[tauri::command]
pub async fn read_config(state: State<'_, Mnemo2State>) -> Result<SpecsConfig, String> {
    let root = project_root(&state).await?;
    config(&openspec_dir(&root))
}

/// Save the worker context and the artifact rules.
#[tauri::command]
pub async fn save_config(
    state: State<'_, Mnemo2State>,
    context: String,
    rules: BTreeMap<String, Vec<String>>,
) -> Result<SpecsConfig, String> {
    let root = project_root(&state).await?;
    store_config(&openspec_dir(&root), &context, &rules)
}

/// Preview a new plan under one schema.
#[tauri::command]
pub async fn plan_preview(
    state: State<'_, Mnemo2State>,
    name: String,
) -> Result<PlanPreview, String> {
    let root = project_root(&state).await?;
    preview(&openspec_dir(&root), &name)
}

/// Run `openspec validate` with `.coding/` as the working directory; an
/// error when the CLI is not on PATH.
#[tauri::command]
pub async fn run_openspec_validate(
    state: State<'_, Mnemo2State>,
) -> Result<ValidateOutput, String> {
    let root = project_root(&state).await?;
    validate(&root)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::app::plan_board::PlanDraft;
    use mnemo::openspec::{Delta, Requirement, Scenario};
    use std::fs;

    /// A temp project with `.coding/openspec/` already in place.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp project");
        fs::create_dir_all(openspec_dir(dir.path())).expect("openspec dir");
        dir
    }

    /// A minimal valid schema folder named `custom`, with one research
    /// artifact and its template on disk.
    fn write_schema(dir: &Path) {
        fs::create_dir_all(dir.join("templates")).expect("templates dir");
        fs::write(
            dir.join("schema.yaml"),
            "name: custom\nversion: \"1\"\ndescription: A test schema.\nartifacts:\n  - id: research\n    generates: research.md\n    template: research.md\n    instruction: Do the research.\napply:\n  requires: [research]\n",
        )
        .expect("schema.yaml");
        fs::write(
            dir.join("templates").join("research.md"),
            "# Research\n\nReplace with findings.\n",
        )
        .expect("template");
    }

    /// A hand-written current-state spec with `count` requirements — the
    /// format `Spec::parse` accepts.
    fn write_spec(openspec_dir: &Path, capability: &str, count: usize) {
        let folder = openspec_dir.join("specs").join(capability);
        fs::create_dir_all(&folder).expect("spec folder");
        let mut text = format!("# {capability}\n\n## Purpose\n\nWhat it does.\n\n## Requirements\n");
        for index in 0..count {
            text.push_str(&format!(
                "\n### Requirement: Rule {index}\n\nThe system SHALL do {index}.\n\n#### Scenario: Works {index}\n\n- **WHEN** the contract is exercised\n- **THEN** the behavior is observable\n"
            ));
        }
        fs::write(folder.join("spec.md"), text).expect("spec.md");
    }

    /// An ADDED-delta for one requirement — the shape the board validates
    /// on plan create.
    fn added_delta(capability: &str, requirement_name: &str) -> Delta {
        let mut delta = Delta {
            capability: capability.to_string(),
            ..Delta::default()
        };
        delta.added.push(Requirement::new(
            requirement_name,
            "The system SHALL keep the observable contract.",
            vec![Scenario::new(
                "Contract exercised",
                vec!["the contract is exercised".to_string()],
                vec!["the behavior is observable".to_string()],
                vec![],
            )],
        ));
        delta
    }

    /// A draft that passes the board's resumability gate, with the given
    /// spec deltas.
    fn draft(title: &str, spec_deltas: Vec<Delta>) -> PlanDraft {
        PlanDraft {
            title: title.to_string(),
            goal: "Ship the importer fix".to_string(),
            context: "Why: the importer drops rows (src/import.rs).\n## Not a section\nConstraint: keep the CLI flags. Verify: cargo test.".to_string(),
            steps: vec![
                "**Fix parser** — edit src/import.rs".to_string(),
                "Add tests in tests/import.rs".to_string(),
            ],
            spec_deltas,
            ..Default::default()
        }
    }

    #[test]
    fn add_schema_with_missing_template_copies_nothing() {
        let project = project();
        let dir = openspec_dir(project.path());
        let source = project.path().join("incoming");
        fs::create_dir_all(source.join("templates")).expect("templates dir");
        fs::write(
            source.join("schema.yaml"),
            "name: custom\nversion: \"1\"\ndescription: Broken.\nartifacts:\n  - id: research\n    generates: research.md\n    template: research.md\n    instruction: Do the research.\napply:\n  requires: [research]\n",
        )
        .expect("schema.yaml");
        let error = add_schema_from(&dir, source.to_str().unwrap()).unwrap_err();
        assert!(error.contains("research.md"), "{error}");
        assert!(
            !dir.join("schemas").exists(),
            "an invalid schema must copy nothing"
        );
    }

    #[test]
    fn add_schema_refuses_a_source_with_several_schemas() {
        let project = project();
        let dir = openspec_dir(project.path());
        let source = project.path().join("repo");
        for name in ["alpha", "beta"] {
            let folder = source.join("schemas").join(name);
            fs::create_dir_all(&folder).expect("schema dir");
            fs::write(folder.join("schema.yaml"), "name: x\nversion: \"1\"\n").expect("yaml");
        }
        let error = add_schema_from(&dir, source.to_str().unwrap()).unwrap_err();
        assert!(error.contains("alpha") && error.contains("beta"), "{error}");
        assert!(!dir.join("schemas").exists());
    }

    #[test]
    fn add_schema_from_a_folder_copies_and_resolves() {
        let project = project();
        let dir = openspec_dir(project.path());
        let source = project.path().join("incoming");
        write_schema(&source);
        let list = add_schema_from(&dir, source.to_str().unwrap()).expect("add");
        let added = list
            .schemas
            .iter()
            .find(|schema| schema.name == "custom")
            .expect("custom listed");
        assert_eq!(added.origin, "project");
        assert_eq!(added.artifacts, vec!["research".to_string()]);
        assert!(added.error.is_none());
        assert!(dir.join("schemas").join("custom").join("templates").join("research.md").is_file());
        // The added schema resolves under its new key.
        let resolved = Schema::resolve(&dir, Some("custom")).expect("resolve");
        assert_eq!(resolved.schema.name, "custom");
    }

    /// Create the file symlink `link` pointing at `target`. Errors when
    /// the OS refuses to create links (Windows without developer mode
    /// or the SeCreateSymbolicLink privilege); the caller then skips
    /// the test instead of failing it.
    fn make_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlinks are not supported on this platform",
            ))
        }
    }

    /// Create the directory symlink `link` pointing at the directory
    /// `target` — on Windows a directory link needs its own reparse
    /// point kind, so `symlink_file` would not resolve as a folder.
    fn make_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlinks are not supported on this platform",
            ))
        }
    }

    #[test]
    fn add_schema_refuses_a_symlinked_template_and_copies_nothing() {
        let project = project();
        let dir = openspec_dir(project.path());
        let source = project.path().join("incoming");
        write_schema(&source);
        // The template becomes a link to a file outside the source, so
        // following it would copy bytes the source never contained.
        let secret = project.path().join("secret.md");
        fs::write(&secret, "a private file\n").expect("secret");
        let template = source.join("templates").join("research.md");
        fs::remove_file(&template).expect("remove the real template");
        if let Err(error) = make_file_symlink(&secret, &template) {
            eprintln!("skipped: the OS refused to create the symlink: {error}");
            return;
        }
        let error = add_schema_from(&dir, source.to_str().unwrap()).unwrap_err();
        assert!(error.contains("symbolic links"), "{error}");
        assert!(error.contains("research.md"), "{error}");
        assert!(
            !dir.join("schemas").exists(),
            "a refused source must copy nothing"
        );
    }

    #[test]
    fn add_schema_refuses_a_symlinked_schema_folder() {
        let project = project();
        let dir = openspec_dir(project.path());
        let real = project.path().join("real-schema");
        write_schema(&real);
        let source = project.path().join("repo");
        fs::create_dir_all(source.join("schemas")).expect("schemas dir");
        let linked = source.join("schemas").join("custom");
        if let Err(error) = make_dir_symlink(&real, &linked) {
            eprintln!("skipped: the OS refused to create the symlink: {error}");
            return;
        }
        let error = add_schema_from(&dir, source.to_str().unwrap()).unwrap_err();
        assert!(error.contains("symbolic links"), "{error}");
        assert!(error.contains("custom"), "{error}");
        assert!(
            !dir.join("schemas").exists(),
            "a refused source must copy nothing"
        );
    }

    // A directory junction is the link kind any Windows user can create
    // without a privilege (`mklink /J`), so it must be refused exactly
    // like a symlink — and unlike the two tests above it also runs on
    // a Windows box without developer mode, where true symlinks cannot
    // be created. `is_symlink()` reports junctions too, so the same
    // refusal fires.
    #[test]
    #[cfg(windows)]
    fn add_schema_refuses_a_junctioned_schema_folder() {
        let project = project();
        let dir = openspec_dir(project.path());
        let real = project.path().join("real-schema");
        write_schema(&real);
        let source = project.path().join("repo");
        fs::create_dir_all(source.join("schemas")).expect("schemas dir");
        let linked = source.join("schemas").join("custom");
        let mklink = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&linked)
            .arg(&real)
            .output()
            .expect("mklink");
        assert!(
            mklink.status.success(),
            "mklink: {}",
            String::from_utf8_lossy(&mklink.stderr)
        );
        // The junction lives inside the temp project, so dropping it
        // at the end of the test cleans the link up too.
        let error = add_schema_from(&dir, source.to_str().unwrap()).unwrap_err();
        assert!(error.contains("symbolic links"), "{error}");
        assert!(error.contains("custom"), "{error}");
        assert!(
            !dir.join("schemas").exists(),
            "a refused source must copy nothing"
        );
    }

    #[test]
    fn classifies_git_sources() {
        assert!(is_git_source("https://example.com/schema.git"));
        assert!(is_git_source("http://example.com/schema"));
        assert!(is_git_source("git@github.com:team/schema.git"));
        assert!(is_git_source("ssh://git@example.com/team/schema.git"));
        assert!(!is_git_source("C:\\schemas\\custom"));
        assert!(!is_git_source(".coding/openspec/schemas"));
    }

    #[test]
    fn fork_then_edit_an_artifact() {
        let project = project();
        let dir = openspec_dir(project.path());
        let list = fork_schema_into(&dir, "spec-driven", "my-flow").expect("fork");
        assert!(list.schemas.iter().any(|schema| schema.name == "my-flow"));
        assert_eq!(list.active, "spec-driven");
        let forked = dir.join("schemas").join("my-flow");
        let yaml = fs::read_to_string(forked.join("schema.yaml")).expect("schema.yaml");
        assert!(yaml.contains("name: my-flow"), "{yaml}");
        assert!(forked.join("templates").join("proposal.md").is_file());
        // The built-in's glob template forks as a literal file.
        assert!(forked.join("templates").join("specs.md").is_file());

        // The built-in stays read-only.
        let error = store_artifact(&dir, "spec-driven", "proposal", "x", "y").unwrap_err();
        assert!(error.contains("read-only"), "{error}");

        // The fork is editable in place and survives a reload.
        let view = store_artifact(&dir, "my-flow", "proposal", "Body line.", "New instruction.")
            .expect("edit");
        let proposal = view
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "proposal")
            .expect("proposal");
        assert_eq!(proposal.template_text, "Body line.");
        assert_eq!(proposal.instruction, "New instruction.");
        assert_eq!(view.origin, "project");
        let reloaded = artifacts(&dir, "my-flow").expect("reload");
        let proposal = reloaded
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "proposal")
            .expect("proposal");
        assert_eq!(proposal.template_text, "Body line.");
        assert_eq!(proposal.instruction, "New instruction.");

        // Activating the fork records it in config.yaml.
        let list = activate_schema(&dir, "my-flow").expect("activate");
        assert_eq!(list.active, "my-flow");
        assert!(list.active_error.is_none());
        let stored = config(&dir).expect("config");
        assert_eq!(stored.schema.as_deref(), Some("my-flow"));
    }

    #[test]
    fn store_artifact_restores_the_files_on_an_invalid_result() {
        let project = project();
        let dir = openspec_dir(project.path());
        let source = project.path().join("incoming");
        write_schema(&source);
        add_schema_from(&dir, source.to_str().unwrap()).expect("add");
        // An edit naming a missing artifact must refuse and leave the
        // schema untouched.
        let before = fs::read_to_string(dir.join("schemas").join("custom").join("schema.yaml"))
            .expect("schema.yaml");
        let error =
            store_artifact(&dir, "custom", "no-such-artifact", "Body.", "Instruction.")
                .unwrap_err();
        assert!(error.contains("no-such-artifact"), "{error}");
        let after = fs::read_to_string(dir.join("schemas").join("custom").join("schema.yaml"))
            .expect("schema.yaml");
        assert_eq!(before, after, "a refused edit must not persist");
        // The real edit still works afterwards.
        store_artifact(&dir, "custom", "research", "Body.", "Instruction.")
            .expect("edit");
    }

    #[test]
    fn save_config_round_trips_and_keeps_unknown_keys() {
        let project = project();
        let dir = openspec_dir(project.path());
        fs::write(
            dir.join("config.yaml"),
            "custom_key: kept\ncontext: Old context.\n",
        )
        .expect("config.yaml");
        let rules = BTreeMap::from([
            ("research".to_string(), vec!["Cite sources.".to_string()]),
            (
                "proposal".to_string(),
                vec![
                    "Name the blast radius.".to_string(),
                    "Name the risk.".to_string(),
                ],
            ),
        ]);
        let saved = store_config(&dir, "The project's conventions.", &rules).expect("save");
        assert_eq!(saved.context, "The project's conventions.");
        assert_eq!(saved.rules, rules);
        assert_eq!(saved.schema, None);

        let reloaded = config(&dir).expect("reload");
        assert_eq!(reloaded.context, "The project's conventions.");
        assert_eq!(reloaded.rules, rules);

        let raw = fs::read_to_string(dir.join("config.yaml")).expect("config.yaml");
        assert!(raw.contains("custom_key: kept"), "{raw}");
        assert!(raw.contains("rules:"), "{raw}");

        // A schema recorded before the save survives it.
        activate_schema(&dir, "spec-driven").expect("activate");
        let saved = store_config(&dir, "New context.", &rules).expect("save");
        assert_eq!(saved.schema.as_deref(), Some("spec-driven"));
    }

    #[test]
    fn status_counts_requirements_and_pending_plans() {
        let project = project();
        let dir = openspec_dir(project.path());
        write_spec(&dir, "auth", 2);
        write_spec(&dir, "importer", 1);
        let broken = dir.join("specs").join("broken");
        fs::create_dir_all(&broken).expect("spec folder");
        fs::write(broken.join("spec.md"), "# Broken\n\nNot a spec.\n").expect("spec.md");

        let board = PlanBoard::new(
            project.path().join(".coding").join("plans"),
            project.path().join(".coding").join("reviews"),
        );
        board
            .create(draft("Rate limit", vec![added_delta("auth", "Login rate limit")]))
            .expect("create");
        board.create(draft("No deltas", vec![])).expect("create");

        let status = status(project.path());
        let auth = status
            .capabilities
            .iter()
            .find(|capability| capability.name == "auth")
            .expect("auth listed");
        assert_eq!(auth.requirements, 2);
        assert!(auth.format_error.is_none());
        let broken_status = status
            .capabilities
            .iter()
            .find(|capability| capability.name == "broken")
            .expect("broken listed");
        assert_eq!(broken_status.requirements, 0);
        assert!(broken_status.format_error.is_some());
        assert_eq!(status.pending_plans.len(), 1, "only the plan with deltas");
        assert_eq!(status.pending_plans[0].title, "Rate limit");
        assert_eq!(status.pending_plans[0].capabilities, vec!["auth".to_string()]);
    }

    #[test]
    fn capability_spec_reads_the_current_spec() {
        let project = project();
        let folder = openspec_dir(project.path()).join("specs").join("tier-ladder");
        fs::create_dir_all(&folder).expect("spec folder");
        fs::write(folder.join("spec.md"), "# Tier ladder\n").expect("spec.md");
        assert_eq!(
            capability_spec(project.path(), "tier-ladder"),
            Ok(Some("# Tier ladder\n".to_string()))
        );
    }

    #[test]
    fn capability_spec_is_none_without_a_spec() {
        let project = project();
        assert_eq!(capability_spec(project.path(), "tier-ladder"), Ok(None));
    }

    #[test]
    fn capability_spec_refuses_bad_names() {
        let project = project();
        for name in ["../x", "a/b", "Tier", "-a", "a-", "a--b", ""] {
            let error = capability_spec(project.path(), name).unwrap_err();
            assert!(error.contains("invalid capability name"), "{name}: {error}");
        }
    }

    // A spec.md linked outside the specs tree would serve another file's
    // bytes as the capability's current spec, so the read must refuse it.
    // Unix only — the link creation needs the OS's unprivileged symlink.
    #[test]
    #[cfg(unix)]
    fn capability_spec_refuses_a_linked_spec() {
        let project = project();
        let folder = openspec_dir(project.path()).join("specs").join("tier-ladder");
        fs::create_dir_all(&folder).expect("spec folder");
        let secret = project.path().join("secret.md");
        fs::write(&secret, "a private file\n").expect("secret");
        if let Err(error) = make_file_symlink(&secret, &folder.join("spec.md")) {
            eprintln!("skipped: the OS refused to create the symlink: {error}");
            return;
        }
        let error = capability_spec(project.path(), "tier-ladder").unwrap_err();
        assert!(error.contains("link"), "{error}");
    }

    #[test]
    fn validate_never_runs_without_the_cli() {
        let project = project();
        let error = validate_with(project.path(), None).unwrap_err();
        assert!(error.contains("not on PATH"), "{error}");
    }

    #[test]
    fn finds_the_cli_among_path_entries() {
        let dir = tempfile::tempdir().expect("temp path dir");
        fs::write(dir.path().join("openspec.cmd"), "@echo off\n").expect("shim");
        let paths = std::env::join_paths([dir.path()]).expect("join paths");
        assert_eq!(
            find_cli_in_paths(&paths),
            Some(dir.path().join("openspec.cmd"))
        );
        assert_eq!(find_cli_in_paths(OsStr::new(r"C:\definitely\missing")), None);
    }

    #[test]
    fn preview_composes_sections_and_worker_context() {
        let project = project();
        let dir = openspec_dir(project.path());
        fork_schema_into(&dir, "spec-driven", "my-flow").expect("fork");
        activate_schema(&dir, "my-flow").expect("activate");
        store_config(&dir, "The project's rules.", &BTreeMap::new()).expect("context");

        let preview = preview(&dir, "my-flow").expect("preview");
        assert_eq!(preview.name, "my-flow");
        assert_eq!(preview.reference, "my-flow@1");
        let proposal = preview
            .sections
            .iter()
            .find(|section| section.id == "proposal")
            .expect("proposal section");
        assert!(proposal.required_for_dispatch);
        assert_eq!(preview.required.first().map(String::as_str), Some("tasks"));
        assert!(preview.required.contains(&"specs".to_string()));
        assert!(
            preview.worker_context.contains("\n## Project context\n"),
            "{}",
            preview.worker_context
        );
        assert!(preview.worker_context.contains("The project's rules."));
        assert!(preview.worker_context.contains("### Artifact: proposal"));
    }
}
