// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `projects.toml` — the known-projects registry (name → path).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// The wire-format file: a list of `[[project]]` tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    #[serde(default)]
    pub project: Vec<ProjectEntry>,
}

/// A registered project: a name → path mapping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub name: String,
    pub path: String,
}

/// The known-projects registry.
#[derive(Debug, Clone, Default)]
pub struct ProjectRegistry {
    entries: Vec<ProjectEntry>,
}

impl ProjectRegistry {
    /// Load from `projects.toml`, or return an empty registry if missing.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let file: ProjectsFile = toml::from_str(&text)?;
        Ok(Self {
            entries: file.project,
        })
    }

    /// Save the registry to `projects.toml`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let file = ProjectsFile {
            project: self.entries.clone(),
        };
        let text = toml::to_string_pretty(&file)?;
        fs::write(path, text)?;
        Ok(())
    }

    /// Look up a project path by name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.path.as_str())
    }

    /// Add or update a project entry. Returns `true` if a new entry was added.
    pub fn add(&mut self, name: &str, path: &str) -> bool {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) {
            entry.path = path.to_string();
            false
        } else {
            self.entries.push(ProjectEntry {
                name: name.to_string(),
                path: path.to_string(),
            });
            true
        }
    }

    /// Remove a project by name. Returns `true` if it existed.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.name != name);
        before != self.entries.len()
    }

    /// Number of registered projects.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over all entries.
    pub fn iter(&self) -> impl Iterator<Item = &ProjectEntry> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_registry() {
        let text = r#"
[[project]]
name = "myproject"
path = "C:/myProject"

[[project]]
name = "side-app"
path = "D:/code/side-app"
"#;
        let file: ProjectsFile = toml::from_str(text).unwrap();
        assert_eq!(file.project.len(), 2);
        assert_eq!(file.project[0].name, "myproject");
        assert_eq!(file.project[1].path, "D:/code/side-app");
    }

    #[test]
    fn add_and_get() {
        let mut reg = ProjectRegistry::default();
        assert!(reg.add("foo", "/path/foo"));
        assert_eq!(reg.get("foo"), Some("/path/foo"));
        // Update existing → returns false.
        assert!(!reg.add("foo", "/path/foo2"));
        assert_eq!(reg.get("foo"), Some("/path/foo2"));
    }

    #[test]
    fn remove() {
        let mut reg = ProjectRegistry::default();
        reg.add("a", "/a");
        reg.add("b", "/b");
        assert!(reg.remove("a"));
        assert!(!reg.remove("a"));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("b"), Some("/b"));
    }

    #[test]
    fn save_and_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        let mut reg = ProjectRegistry::default();
        reg.add("myproject", "C:/myProject");
        reg.save(&path).unwrap();

        let reloaded = ProjectRegistry::load_or_default(&path).unwrap();
        assert_eq!(reloaded.get("myproject"), Some("C:/myProject"));
    }
}
