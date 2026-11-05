// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Prepare and write the OpenSpec changes archived by a closing board plan.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use super::schema::ResolvedSchema;
use crate::app::workflow::PlanFile;
use crate::openspec::{apply, Delta, Requirement, Spec};

/// One file whose complete contents are ready to be written.
struct PreparedWrite {
    path: PathBuf,
    contents: String,
}

/// A fully validated close: every delta applied and every archive file rendered.
///
/// Nothing touches the filesystem until [`write`](Self::write), so a conflict in
/// any capability leaves all current-state specs and the archive untouched.
pub(crate) struct PreparedOpenSpecClose {
    writes: Vec<PreparedWrite>,
}

impl PreparedOpenSpecClose {
    /// Write the staged current-state specs and archive files.
    pub(crate) fn write(self) -> Result<(), String> {
        for file in self.writes {
            if let Some(parent) = file.path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!(
                        "could not create OpenSpec folder '{}': {error}",
                        parent.display()
                    )
                })?;
            }
            std::fs::write(&file.path, file.contents).map_err(|error| {
                format!(
                    "could not write OpenSpec file '{}': {error}",
                    file.path.display()
                )
            })?;
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct ArchiveMetadata<'a> {
    schema: &'a str,
    mnemo_plan: &'a str,
}

/// Apply a plan's deltas in order and render its schema's archive in memory.
pub(crate) fn prepare_close(
    openspec_dir: &Path,
    plan_id: &str,
    plan: &PlanFile,
    resolved: &ResolvedSchema,
    deltas: &[Delta],
    sections: &BTreeMap<String, String>,
    date: &str,
) -> Result<PreparedOpenSpecClose, String> {
    if deltas.is_empty() {
        return Ok(PreparedOpenSpecClose { writes: Vec::new() });
    }

    let specs_dir = openspec_dir.join("specs");
    let mut specs: BTreeMap<String, Spec> = BTreeMap::new();
    let mut archived_deltas: BTreeMap<String, Delta> = BTreeMap::new();
    for delta in deltas {
        validate_capability(&delta.capability)?;
        let capability = delta.capability.clone();
        let current = match specs.get(&capability) {
            Some(spec) => Some(spec.clone()),
            None => load_current_spec(&specs_dir, &capability)?,
        };
        let merged = apply(current.as_ref(), delta).map_err(|error| {
            format!(
                "cannot close plan {plan_id}: OpenSpec delta for capability '{capability}' no longer applies: {error}. Fix the plan's spec changes with plan_update, then retry the close."
            )
        })?;
        specs.insert(capability.clone(), merged);
        archived_deltas
            .entry(capability.clone())
            .and_modify(|combined| extend_delta(combined, delta))
            .or_insert_with(|| delta.clone());
    }

    let archive_dir = openspec_dir
        .join("changes")
        .join("archive")
        .join(format!("{date}-{}", archive_slug(plan_id, &plan.title)));
    let mut archive_files = Vec::new();
    for artifact in &resolved.schema.artifacts {
        if artifact.id == "specs" {
            for (capability, delta) in &archived_deltas {
                archive_files.push(PreparedWrite {
                    path: archive_dir.join("specs").join(capability).join("spec.md"),
                    contents: render_delta(delta),
                });
            }
            continue;
        }
        let contents = match artifact.id.as_str() {
            "proposal" => render_proposal(plan),
            "design" => render_design(plan),
            "tasks" => render_tasks(plan),
            _ => sections
                .get(&artifact.id)
                .map(|text| with_final_newline(text))
                .unwrap_or_default(),
        };
        archive_files.push(PreparedWrite {
            path: archive_dir.join(&artifact.generates),
            contents,
        });
    }
    let metadata = serde_norway::to_string(&ArchiveMetadata {
        schema: &resolved.schema.name,
        mnemo_plan: plan_id,
    })
    .map_err(|error| format!("could not render OpenSpec archive metadata: {error}"))?;
    archive_files.push(PreparedWrite {
        path: archive_dir.join(".openspec.yaml"),
        contents: metadata,
    });

    let mut writes = Vec::new();
    for (capability, spec) in specs {
        writes.push(PreparedWrite {
            path: specs_dir.join(capability).join("spec.md"),
            contents: spec.render(),
        });
    }
    writes.extend(archive_files);
    Ok(PreparedOpenSpecClose { writes })
}

fn validate_capability(capability: &str) -> Result<(), String> {
    let mut components = Path::new(capability).components();
    if capability.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(format!(
            "invalid OpenSpec capability '{capability}': expected one path component"
        ));
    }
    Ok(())
}

fn load_current_spec(specs_dir: &Path, capability: &str) -> Result<Option<Spec>, String> {
    let path = specs_dir.join(capability).join("spec.md");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read spec for capability '{capability}' at '{}': {error}",
                path.display()
            ));
        }
    };
    Spec::parse(&text)
        .map(Some)
        .map_err(|error| format!("invalid spec for capability '{capability}': {error}"))
}

fn extend_delta(combined: &mut Delta, next: &Delta) {
    combined.added.extend(next.added.clone());
    combined.modified.extend(next.modified.clone());
    combined.removed.extend(next.removed.clone());
    combined.renamed.extend(next.renamed.clone());
}

fn render_proposal(plan: &PlanFile) -> String {
    let mut out = plan.goal.trim().to_string();
    if !plan.context.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(plan.context.trim());
    }
    out.push('\n');
    out
}

fn render_design(plan: &PlanFile) -> String {
    let mut out = String::new();
    if let Some(decisions) = plan
        .decisions
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        out.push_str("## Decisions\n\n");
        out.push_str(decisions.trim());
        out.push('\n');
    }
    if let Some(interfaces) = plan
        .interfaces
        .as_deref()
        .filter(|text| !text.trim().is_empty())
    {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("## Interfaces\n\n");
        out.push_str(interfaces.trim());
        out.push('\n');
    }
    out
}

fn render_tasks(plan: &PlanFile) -> String {
    let mut out = String::new();
    for step in &plan.steps {
        out.push_str(&format!("- [x] {}\n", step.text.trim()));
    }
    out
}

fn render_delta(delta: &Delta) -> String {
    let mut out = String::new();
    push_requirements(&mut out, "ADDED", &delta.added);
    push_requirements(&mut out, "MODIFIED", &delta.modified);
    if !delta.removed.is_empty() {
        out.push_str("## REMOVED Requirements\n\n");
        for removed in &delta.removed {
            out.push_str(&format!(
                "### Requirement: {}\n{}\n\n",
                removed.requirement, removed.reason
            ));
        }
    }
    if !delta.renamed.is_empty() {
        out.push_str("## RENAMED Requirements\n\n");
        for renamed in &delta.renamed {
            out.push_str(&format!("FROM: {}\nTO: {}\n\n", renamed.from, renamed.to));
        }
    }
    out
}

fn push_requirements(out: &mut String, operation: &str, requirements: &[Requirement]) {
    if requirements.is_empty() {
        return;
    }
    out.push_str(&format!("## {operation} Requirements\n\n"));
    for requirement in requirements {
        out.push_str(&requirement.render());
        out.push('\n');
    }
}

fn with_final_newline(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn archive_slug(plan_id: &str, title: &str) -> String {
    let mut title_slug = String::new();
    let mut dash = false;
    for character in title.chars() {
        if character.is_ascii_alphanumeric() {
            if dash && !title_slug.is_empty() {
                title_slug.push('-');
            }
            title_slug.push(character.to_ascii_lowercase());
            dash = false;
        } else {
            dash = true;
        }
    }
    if title_slug.is_empty() {
        return plan_id.to_string();
    }
    let title_len = 50usize.saturating_sub(plan_id.len() + 1);
    title_slug.truncate(title_len);
    let title_slug = title_slug.trim_end_matches('-');
    if title_slug.is_empty() {
        plan_id.to_string()
    } else {
        format!("{plan_id}-{title_slug}")
    }
}
