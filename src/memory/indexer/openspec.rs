// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! OpenSpec requirement indexing — one derived `SPEC:` memory per requirement
//! from `.coding/openspec/specs/<capability>/spec.md`.

use std::path::Path;

use crate::memory::{MemoryRecordType, MemorySearchConfig};
use crate::openspec::{Requirement, Spec};

use super::{budgeted_digest, truncate_title, SourceRecord};

/// Scan all current-state specs into requirement digest sources. Capability
/// metadata, schemas, and archived deltas are deliberately not current-state
/// sources and are therefore skipped.
pub(super) fn scan_openspec(
    specs_dir: &Path,
    out: &mut Vec<SourceRecord>,
    errors: &mut Vec<String>,
    config: &MemorySearchConfig,
) {
    let Ok(entries) = std::fs::read_dir(specs_dir) else {
        return;
    };
    let mut capabilities: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    capabilities.sort();
    for capability_dir in capabilities {
        if !capability_dir.is_dir() {
            continue;
        }
        let capability = capability_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if capability.is_empty() {
            continue;
        }
        let path = capability_dir.join("spec.md");
        if !path.is_file() {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match Spec::parse(&text) {
                Ok(spec) => {
                    for requirement in spec.requirements() {
                        out.push(requirement_record(capability, requirement, config));
                    }
                }
                Err(error) => errors.push(format!("openspec {}: {error}", path.display())),
            },
            Err(error) => {
                errors.push(format!("openspec {}: unreadable: {error}", path.display()));
            }
        }
    }
}

fn requirement_record(
    capability: &str,
    requirement: &Requirement,
    config: &MemorySearchConfig,
) -> SourceRecord {
    let mut gist = format!(
        "Capability: {capability}\nRequirement: {}\n{}",
        requirement.name,
        requirement.text.trim()
    );
    for scenario in &requirement.scenarios {
        gist.push_str(&format!("\nScenario: {}", scenario.name));
        for (label, values) in [
            ("WHEN", &scenario.when),
            ("THEN", &scenario.then),
            ("AND", &scenario.and),
        ] {
            for value in values {
                gist.push_str(&format!("\n{label}: {value}"));
            }
        }
    }
    let pointer = format!("path .coding/openspec/specs/{capability}/spec.md");
    let budget = config.digest_budget(MemoryRecordType::Spec).unwrap_or(500);
    SourceRecord {
        key: format!("openspec:{capability}/{}", requirement.name),
        title: truncate_title(&format!("SPEC: {capability}/{}", requirement.name)),
        content: budgeted_digest(&gist, &pointer, budget),
    }
}
