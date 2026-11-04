// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo-only capability path metadata (`.mnemo.yaml`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A capability and the project-relative globs covered by its specification.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// Kebab-case capability folder name.
    pub name: String,
    /// Project-relative glob patterns from `.mnemo.yaml`.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// Failures while loading capability path metadata.
#[derive(Debug, Error)]
pub enum CapabilityError {
    /// A capability folder name must be representable in user-facing paths.
    #[error("capability folder '{0}' has a non-UTF-8 name")]
    InvalidName(PathBuf),
    /// Metadata could not be read.
    #[error("could not read capability metadata '{path}': {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A specs directory entry could not be inspected.
    #[error("could not list capabilities under '{path}': {source}")]
    List {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// `.mnemo.yaml` is not valid YAML of the expected shape.
    #[error("could not parse capability metadata '{path}': {source}")]
    Yaml {
        path: PathBuf,
        #[source]
        source: serde_norway::Error,
    },
    /// A path pattern must be a valid glob.
    #[error("invalid capability glob '{glob}' in '{path}': {source}")]
    Glob {
        path: PathBuf,
        glob: String,
        #[source]
        source: glob::PatternError,
    },
}

impl Capability {
    /// Load one capability folder. A missing `.mnemo.yaml` simply means the
    /// capability has no path associations.
    pub fn load(capability_dir: &Path) -> Result<Self, CapabilityError> {
        let name = capability_dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| CapabilityError::InvalidName(capability_dir.to_path_buf()))?
            .to_string();
        let metadata_path = capability_dir.join(".mnemo.yaml");
        let text = match std::fs::read_to_string(&metadata_path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    name,
                    paths: vec![],
                });
            }
            Err(source) => {
                return Err(CapabilityError::Read {
                    path: metadata_path,
                    source,
                });
            }
        };
        let parsed: CapabilityFile =
            serde_norway::from_str(&text).map_err(|source| CapabilityError::Yaml {
                path: metadata_path.clone(),
                source,
            })?;
        for pattern in &parsed.paths {
            glob::Pattern::new(pattern).map_err(|source| CapabilityError::Glob {
                path: metadata_path.clone(),
                glob: pattern.clone(),
                source,
            })?;
        }
        Ok(Self {
            name,
            paths: parsed.paths,
        })
    }

    /// List every capability folder directly beneath `.coding/openspec/specs/`.
    /// Results are sorted by name; a missing specs directory is an empty list.
    pub fn load_all(specs_dir: &Path) -> Result<Vec<Self>, CapabilityError> {
        let entries = match std::fs::read_dir(specs_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(source) => {
                return Err(CapabilityError::List {
                    path: specs_dir.to_path_buf(),
                    source,
                });
            }
        };
        let mut capabilities = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| CapabilityError::List {
                path: specs_dir.to_path_buf(),
                source,
            })?;
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                capabilities.push(Self::load(&entry.path())?);
            }
        }
        capabilities.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(capabilities)
    }

    /// Whether a project-relative path is covered by any capability glob.
    /// Windows separators are normalized to `/` before matching.
    pub fn matches(&self, path: impl AsRef<Path>) -> bool {
        let normalized = path.as_ref().to_string_lossy().replace('\\', "/");
        let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
        self.paths.iter().any(|pattern| {
            glob::Pattern::new(pattern)
                .map(|pattern| pattern.matches_path(Path::new(normalized)))
                .unwrap_or(false)
        })
    }
}

#[derive(Debug, Deserialize)]
struct CapabilityFile {
    #[serde(default)]
    paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_paths_match_normalized_project_paths() {
        let capability = Capability {
            name: "dispatch-routing".to_string(),
            paths: vec![
                "src/runtime/**/*.rs".to_string(),
                "mnemo2/src/**/*.svelte".to_string(),
            ],
        };
        assert!(capability.matches("src/runtime/worker.rs"));
        assert!(capability.matches(r"src\runtime\team\dispatch.rs"));
        assert!(capability.matches("./mnemo2/src/components/Team.svelte"));
        assert!(!capability.matches("src/memory/indexer.rs"));
        assert!(!capability.matches("runtime/worker.rs"));
    }

    #[test]
    fn loader_reads_paths_and_lists_every_capability() {
        let temp = tempfile::tempdir().unwrap();
        let specs = temp.path().join("specs");
        let dispatch = specs.join("dispatch-routing");
        let other = specs.join("another-capability");
        std::fs::create_dir_all(&dispatch).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(
            dispatch.join(".mnemo.yaml"),
            "paths:\n  - 'src/runtime/**/*.rs'\n  - 'src/agent/steering_stats.rs'\n",
        )
        .unwrap();
        let capabilities = Capability::load_all(&specs).unwrap();
        assert_eq!(capabilities.len(), 2);
        assert_eq!(capabilities[0].name, "another-capability");
        assert!(capabilities[0].paths.is_empty());
        assert_eq!(capabilities[1].paths.len(), 2);
        assert!(capabilities[1].matches("src/runtime/worker.rs"));
        assert!(Capability::load_all(&temp.path().join("missing"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn loader_rejects_bad_yaml_and_bad_globs() {
        let temp = tempfile::tempdir().unwrap();
        let capability = temp.path().join("bad");
        std::fs::create_dir_all(&capability).unwrap();
        let metadata = capability.join(".mnemo.yaml");
        std::fs::write(&metadata, "paths: [\n").unwrap();
        assert!(matches!(
            Capability::load(&capability),
            Err(CapabilityError::Yaml { .. })
        ));
        std::fs::write(&metadata, "paths: ['[invalid']\n").unwrap();
        assert!(matches!(
            Capability::load(&capability),
            Err(CapabilityError::Glob { .. })
        ));
    }
}
