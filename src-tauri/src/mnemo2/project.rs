// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Real project discovery for Mnemo2.
//!
//! `projects.toml` is the one registry (shared with Mnemo1): Mnemo2 lists it,
//! validates a picked folder, canonicalizes it, derives the display name from
//! the folder, and writes the entry back. No seeded paths live here.

use std::path::{Path, PathBuf};

use mnemo::config::projects::ProjectRegistry;
use mnemo::config::resolve_config_dir;
use serde::{Deserialize, Serialize};

/// One real project as the Mnemo2 UI sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecord {
    /// The display name (the folder's basename, made unique in the registry).
    pub name: String,
    /// The canonical folder path.
    pub path: String,
}

fn registry_path(config_dir: Option<PathBuf>) -> PathBuf {
    resolve_config_dir(config_dir).join("projects.toml")
}

/// List the registered projects. A missing `projects.toml` is an empty list.
pub fn list(config_dir: Option<PathBuf>) -> Result<Vec<ProjectRecord>, String> {
    let path = registry_path(config_dir);
    let registry = ProjectRegistry::load_or_default(&path)
        .map_err(|err| format!("could not read projects.toml: {err}"))?;
    Ok(registry
        .iter()
        .map(|entry| ProjectRecord {
            name: entry.name.clone(),
            path: entry.path.clone(),
        })
        .collect())
}

/// Register a real folder. The path is canonicalized, must be a directory, and
/// gets a stable display name derived from its basename.
pub fn add(config_dir: Option<PathBuf>, folder: &Path) -> Result<ProjectRecord, String> {
    let root = std::fs::canonicalize(folder)
        .map_err(|err| format!("the selected folder does not exist: {err}"))?;
    if !root.is_dir() {
        return Err(format!(
            "the selected path is not a folder: {}",
            root.display()
        ));
    }
    let base = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "could not derive a project name from the folder".to_string())?
        .to_string();
    let path = root.to_string_lossy().to_string();
    let registry_path = registry_path(config_dir);
    if let Some(parent) = registry_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create the config directory: {err}"))?;
    }
    let mut registry = ProjectRegistry::load_or_default(&registry_path)
        .map_err(|err| format!("could not read projects.toml: {err}"))?;

    // A duplicated basename gets a numeric suffix rather than overwriting a
    // different project's path.
    let mut name = base.clone();
    let mut suffix = 2;
    while let Some(existing) = registry.get(&name) {
        if existing == path {
            break;
        }
        name = format!("{base}-{suffix}");
        suffix += 1;
    }
    registry.add(&name, &path);
    registry
        .save(&registry_path)
        .map_err(|err| format!("could not save projects.toml: {err}"))?;
    Ok(ProjectRecord { name, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn add_and_list_round_trip_a_real_folder() {
        let config = tempdir().expect("config");
        let workspace = tempdir().expect("workspace");
        let project = workspace.path().join("real-project");
        std::fs::create_dir(&project).expect("project dir");

        let added = add(Some(config.path().to_path_buf()), &project).expect("add");
        assert_eq!(added.name, "real-project");
        assert_eq!(
            added.path,
            std::fs::canonicalize(&project)
                .expect("canonical")
                .to_string_lossy()
        );
        let listed = list(Some(config.path().to_path_buf())).expect("list");
        assert_eq!(listed, vec![added]);
    }

    #[test]
    fn add_rejects_a_missing_folder() {
        let config = tempdir().expect("config");
        let missing = config.path().join("does-not-exist");
        let err = add(Some(config.path().to_path_buf()), &missing).expect_err("missing folder");
        assert!(err.contains("does not exist"), "{err}");
    }

    #[test]
    fn add_rejects_a_file() {
        let config = tempdir().expect("config");
        let file = config.path().join("not-a-project");
        std::fs::write(&file, "x").expect("file");
        let err = add(Some(config.path().to_path_buf()), &file).expect_err("file");
        assert!(err.contains("not a folder"), "{err}");
    }

    #[test]
    fn duplicate_basenames_get_a_suffix() {
        let config = tempdir().expect("config");
        let one = tempdir().expect("one");
        let two = tempdir().expect("two");
        let first = one.path().join("same");
        let second = two.path().join("same");
        std::fs::create_dir(&first).expect("first");
        std::fs::create_dir(&second).expect("second");

        let a = add(Some(config.path().to_path_buf()), &first).expect("first add");
        let b = add(Some(config.path().to_path_buf()), &second).expect("second add");
        assert_eq!(a.name, "same");
        assert_eq!(b.name, "same-2");
    }
}
