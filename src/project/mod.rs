// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The project model — one app instance works on exactly one project.
//!
//! A project is a directory whose `.coding/` subdir holds its config, memory,
//! and plans. The `agent.md` constitution lives at the project root (visible,
//! like `CLAUDE.md` / `AGENTS.md`). All project state is self-contained.

pub mod agent_md;
pub mod git_ops;
pub mod worktrees;

pub use agent_md::{Constitution, ConstitutionSource};

use std::path::{Path, PathBuf};

use crate::config::ProjectRegistry;
use crate::error::{Error, Result};

/// A resolved project directory.
#[derive(Debug, Clone)]
pub struct Project {
    /// The project directory.
    pub root: PathBuf,
    /// `<root>/.coding`.
    pub coding_dir: PathBuf,
    /// `<root>/agent.md` — the project constitution. Lives at the project
    /// root (not in `.coding/`) so it's visible and easy to edit, like
    /// `CLAUDE.md` / `AGENTS.md` conventions.
    pub agent_md: PathBuf,
    /// `<coding_dir>/memory.db` — the project-scoped memory store.
    pub memory_db: PathBuf,
    /// `<coding_dir>/codegraph.db` — the project-scoped code knowledge graph
    /// (a derivable cache, gitignored, rebuilt on startup).
    pub codegraph_db: PathBuf,
    /// `<coding_dir>/plans` — plan files (the workflow source of truth).
    pub plans_dir: PathBuf,
    /// `<coding_dir>/safety.toml` — regex-based auto-approve rules.
    pub safety_toml: PathBuf,
    /// `<coding_dir>/config.toml` — optional project-specific overrides.
    pub config_toml: PathBuf,
    /// `<coding_dir>/skills` — skill definition files (`<name>.toml`), loaded
    /// at startup into a [`SkillRegistry`](crate::skill::SkillRegistry).
    pub skills_dir: PathBuf,
    /// `<coding_dir>/mcp.toml` — optional per-project MCP server overrides,
    /// union-merged over the global `~/.mnemo/mcp.toml` with the project
    /// file winning by name (same philosophy as skills).
    pub mcp_toml: PathBuf,
}

impl Project {
    /// The name of the `.coding/` subdir.
    pub const CODING_DIR_NAME: &'static str = ".coding";

    /// Build a `Project` from a root directory, computing all the sub-paths.
    /// Does not touch the filesystem — use [`Project::init`] to scaffold.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let coding_dir = root.join(Self::CODING_DIR_NAME);
        Self {
            agent_md: root.join("agent.md"),
            memory_db: coding_dir.join("memory.db"),
            codegraph_db: coding_dir.join("codegraph.db"),
            plans_dir: coding_dir.join("plans"),
            safety_toml: coding_dir.join("safety.toml"),
            config_toml: coding_dir.join("config.toml"),
            skills_dir: coding_dir.join("skills"),
            mcp_toml: coding_dir.join("mcp.toml"),
            coding_dir,
            root,
        }
    }

    /// Whether this project's `.coding/` directory already exists.
    pub fn is_initialized(&self) -> bool {
        self.coding_dir.is_dir()
    }

    /// Resolve a project from the launch directory + the known-projects registry.
    ///
    /// Resolution order:
    /// 1. If `launch_dir` (or an ancestor) contains `.coding/`, use it directly.
    /// 2. If the registry has exactly one entry, use it.
    /// 3. If the registry has a project whose path matches `launch_dir`, use it.
    ///
    /// Returns `Err` if no project can be resolved. The caller (CLI) handles
    /// the "browse / initialize" path.
    pub fn resolve(launch_dir: &Path, registry: &ProjectRegistry) -> Result<Self> {
        // 1. Auto-detect: walk up from launch_dir looking for `.coding/`.
        if let Some(found) = Self::find_coding_dir_ancestor(launch_dir) {
            return Ok(Self::from_root(found));
        }
        // 2. If the registry has entries, try to match the launch dir, else
        //    fall back to the first entry.
        if !registry.is_empty() {
            // Exact path match first.
            for entry in registry.iter() {
                if Path::new(&entry.path) == launch_dir {
                    return Ok(Self::from_root(&entry.path));
                }
            }
            // Otherwise, if there's exactly one registered project, use it.
            if registry.len() == 1 {
                let entry = registry.iter().next().unwrap();
                return Ok(Self::from_root(&entry.path));
            }
        }
        Err(Error::Project(format!(
            "no project found at or above '{}'; initialize one with `mnemo project init <path>`",
            launch_dir.display()
        )))
    }

    /// Walk up from `start` looking for a directory containing `.coding/`.
    /// Returns the project root (the parent of `.coding/`) if found.
    ///
    /// Public so the Tauri shell can run the "is the cwd already a project?"
    /// check directly (the startup picker is skipped when this returns `Some`).
    pub fn find_coding_dir_ancestor(start: &Path) -> Option<PathBuf> {
        let mut current = Some(start);
        while let Some(dir) = current {
            if dir.join(Self::CODING_DIR_NAME).is_dir() {
                return Some(dir.to_path_buf());
            }
            current = dir.parent();
        }
        None
    }

    /// Initialize a project directory: scaffold `.coding/` (for memory, plans,
    /// and config) + the root-level `agent.md` constitution template.
    ///
    /// If `.coding/` already exists, this re-scaffolds only the missing
    /// pieces: a missing `agent.md` is re-written and the shipped skills
    /// (`.coding/skills/`, compile-embedded) are re-seeded write-if-missing
    /// so deleted skill files self-heal — user-modified files are never
    /// overwritten.
    pub fn init(dir: &Path) -> Result<Self> {
        let project = Self::from_root(dir);
        if project.is_initialized() {
            // Still ensure the root agent.md exists if it was removed.
            if !project.agent_md.exists() {
                std::fs::write(&project.agent_md, agent_md::DEFAULT_PROJECT_TEMPLATE)?;
            }
            // Self-heal the skills dir too: a project whose shipped skill
            // files (or the whole .coding/skills/ directory) were deleted
            // gets them back on the next init — write-if-missing, so existing
            // user-modified skills are never touched.
            crate::skill::seed_skills(&project.skills_dir).map_err(|e| {
                Error::Project(format!(
                    "failed to seed skills into {}: {e}",
                    project.skills_dir.display()
                ))
            })?;
            return Ok(project);
        }
        std::fs::create_dir_all(&project.coding_dir)?;
        std::fs::create_dir_all(&project.plans_dir)?;
        // Seed the shipped skills (compile-embedded TOMLs, write-if-missing)
        // so a fresh project starts with the same baseline skill set as the
        // main installation.
        crate::skill::seed_skills(&project.skills_dir).map_err(|e| {
            Error::Project(format!(
                "failed to seed skills into {}: {e}",
                project.skills_dir.display()
            ))
        })?;
        // Write the agent.md template at the project root if it doesn't exist.
        if !project.agent_md.exists() {
            std::fs::write(&project.agent_md, agent_md::DEFAULT_PROJECT_TEMPLATE)?;
        }
        Ok(project)
    }

    /// Eagerly seed the project's on-disk stores after scaffolding: create
    /// `.coding/memory.db` with its schema applied, and — when the codegraph
    /// is enabled — run the first source index so semantic + source search
    /// are ready the moment the first session opens (without this, both are
    /// created lazily on the next app startup, leaving the first session
    /// without them).
    ///
    /// Idempotent on an already-initialized project: an existing memory DB is
    /// simply re-opened (schema application is a no-op) and the graph index
    /// is incremental (content-hash based — unchanged files are skipped). The
    /// embedder is inert here: no memories are written, so no vectors or
    /// fingerprints land in the DB and the real embedder swap at startup is
    /// unaffected.
    ///
    /// `progress`, when given, is forwarded to the codegraph index pass —
    /// called after each walked file with `(files_processed, files_total)`;
    /// the create-project IPC command streams it to the open-project overlay
    /// as IndexProgressEvent ticks.
    pub fn seed_stores(
        &self,
        codegraph_enabled: bool,
        progress: Option<&dyn Fn(usize, usize)>,
    ) -> Result<()> {
        crate::memory::MemoryStore::open(
            &self.memory_db,
            std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new()),
        )
        .map_err(|e| {
            Error::Project(format!(
                "failed to seed memory store at {}: {e}",
                self.memory_db.display()
            ))
        })?;
        if codegraph_enabled {
            let graph = crate::codegraph::CodeGraph::open(self.root.clone(), &self.codegraph_db)
                .map_err(|e| {
                    Error::Project(format!(
                        "failed to seed codegraph at {}: {e}",
                        self.codegraph_db.display()
                    ))
                })?;
            graph.index(progress).map_err(|e| {
                Error::Project(format!(
                    "failed to index codegraph for {}: {e}",
                    self.root.display()
                ))
            })?;
        }
        Ok(())
    }

    /// Load both constitutions (global + project) in order.
    pub fn load_constitutions(&self, global_agent_md: &Path) -> Result<Constitution> {
        agent_md::load(global_agent_md, &self.agent_md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn from_root_computes_paths() {
        let p = Project::from_root("/proj");
        assert_eq!(p.root, PathBuf::from("/proj"));
        assert_eq!(p.coding_dir, PathBuf::from("/proj/.coding"));
        assert_eq!(p.agent_md, PathBuf::from("/proj/agent.md"));
        assert_eq!(p.memory_db, PathBuf::from("/proj/.coding/memory.db"));
        assert_eq!(p.plans_dir, PathBuf::from("/proj/.coding/plans"));
        assert_eq!(p.safety_toml, PathBuf::from("/proj/.coding/safety.toml"));
        assert_eq!(p.config_toml, PathBuf::from("/proj/.coding/config.toml"));
        assert_eq!(p.skills_dir, PathBuf::from("/proj/.coding/skills"));
        assert_eq!(p.mcp_toml, PathBuf::from("/proj/.coding/mcp.toml"));
    }

    #[test]
    fn init_scaffolds_coding_dir() {
        let dir = tempdir().unwrap();
        let project = Project::init(dir.path()).unwrap();
        assert!(project.coding_dir.is_dir());
        assert!(project.plans_dir.is_dir());
        assert!(project.agent_md.is_file());
        let content = std::fs::read_to_string(&project.agent_md).unwrap();
        assert!(!content.is_empty());
        // Shipped skills are seeded at init: the skills dir exists and every
        // embedded shipped skill landed as a parseable TOML file.
        assert!(project.skills_dir.is_dir(), "skills dir scaffolded");
        for (name, _) in crate::skill::SHIPPED_SKILLS {
            let path = project.skills_dir.join(format!("{name}.toml"));
            assert!(path.is_file(), "shipped skill {name} seeded at init");
        }
    }

    #[test]
    fn init_is_idempotent() {
        let dir = tempdir().unwrap();
        Project::init(dir.path()).unwrap();
        // Second init should not error and should preserve existing agent.md.
        let project = Project::init(dir.path()).unwrap();
        let content = std::fs::read_to_string(&project.agent_md).unwrap();
        assert!(!content.is_empty());
    }

    #[test]
    fn init_seeded_skills_never_overwrite_user_edits() {
        // A user-modified (or project-specific) skill file must survive a
        // re-init — seeding is write-if-missing, never an overwrite.
        let dir = tempdir().unwrap();
        Project::init(dir.path()).unwrap();
        let skill_path = dir.path().join(".coding/skills/merge_to_main.toml");
        let custom = r#"name = "merge_to_main"
available_in = ["complete"]
target_state = "planning"
tools = ["git"]
prompt = "Custom project-specific merge flow."
"#;
        std::fs::write(&skill_path, custom).unwrap();
        Project::init(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&skill_path).unwrap(),
            custom,
            "user-edited skill survived re-init"
        );
    }

    #[test]
    fn init_self_heals_missing_skill_files() {
        // Deleting the shipped skill files (or the whole skills directory)
        // is repaired on the next Project::init — mirroring the agent.md
        // self-heal — so the baseline skill set always comes back.
        let dir = tempdir().unwrap();
        Project::init(dir.path()).unwrap();
        std::fs::remove_dir_all(dir.path().join(".coding/skills")).unwrap();
        Project::init(dir.path()).unwrap();
        for (name, _) in crate::skill::SHIPPED_SKILLS {
            let path = dir
                .path()
                .join(".coding/skills")
                .join(format!("{name}.toml"));
            assert!(path.is_file(), "shipped skill {name} self-healed");
        }
    }

    #[test]
    fn seed_stores_creates_memory_db_and_indexes_graph() {
        let dir = tempdir().unwrap();
        let project = Project::init(dir.path()).unwrap();
        // A Rust source file the graph walker can parse.
        std::fs::write(dir.path().join("lib.rs"), "pub fn seeded_symbol() {}\n").unwrap();
        project.seed_stores(true, None).unwrap();
        // memory.db exists and re-opens (schema already applied by seeding).
        assert!(project.memory_db.is_file());
        crate::memory::MemoryStore::open(
            &project.memory_db,
            std::sync::Arc::new(crate::memory::embedder::HashEmbedder::new()),
        )
        .unwrap();
        // codegraph.db exists and the symbol is queryable via a fresh open.
        assert!(project.codegraph_db.is_file());
        let graph =
            crate::codegraph::CodeGraph::open(dir.path().to_path_buf(), &project.codegraph_db)
                .unwrap();
        let view = graph.view().unwrap();
        assert!(view
            .resolve("seeded_symbol")
            .iter()
            .any(|s| s.name == "seeded_symbol"));
        // The FTS content index is populated by the same seeding pass (over
        // EVERY searchable file — the scaffolded agent.md included), so the
        // `search` tool's index engine is ready from the first session.
        // Coverage must be complete: every indexed file has content rows.
        let (files, with_content) = graph.content_coverage().unwrap();
        assert!(
            files >= 1 && files == with_content,
            "content coverage complete: ({files}, {with_content})"
        );
        assert!(
            !graph
                .search_content("seeded_symbol", 10)
                .unwrap()
                .is_empty(),
            "content index populated at project creation"
        );
    }

    #[test]
    fn seed_stores_skips_codegraph_when_disabled() {
        let dir = tempdir().unwrap();
        let project = Project::init(dir.path()).unwrap();
        project.seed_stores(false, None).unwrap();
        assert!(project.memory_db.is_file());
        assert!(!project.codegraph_db.exists());
    }

    #[test]
    fn seed_stores_is_idempotent() {
        let dir = tempdir().unwrap();
        let project = Project::init(dir.path()).unwrap();
        project.seed_stores(true, None).unwrap();
        // A second seeding pass succeeds and keeps both DBs valid.
        project.seed_stores(true, None).unwrap();
        assert!(project.memory_db.is_file());
        assert!(project.codegraph_db.is_file());
    }

    /// The seed pass forwards the index progress callback: the counter walks
    /// 1..=total and the final tick reports completion. This is the wire the
    /// create-project IPC command streams to the open-project overlay as
    /// "N/M files indexed" ticks.
    #[test]
    fn seed_stores_forwards_progress_ticks() {
        let dir = tempdir().unwrap();
        let project = Project::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("lib.rs"), "pub fn progress_symbol() {}\n").unwrap();
        let ticks: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
        let record = |done: usize, total: usize| {
            ticks.lock().unwrap().push((done, total));
        };
        project.seed_stores(true, Some(&record)).unwrap();
        let recorded = ticks.into_inner().unwrap();
        assert!(
            !recorded.is_empty(),
            "progress ticks forwarded to the caller"
        );
        assert_eq!(recorded.first().unwrap().0, 1, "first tick is file 1");
        let last = *recorded.last().unwrap();
        assert_eq!(last.0, last.1, "final tick reports completion: {last:?}");
    }

    #[test]
    fn resolve_auto_detects_ancestor() {
        let dir = tempdir().unwrap();
        Project::init(dir.path()).unwrap();
        // Launch from a subdirectory — should find `.coding/` above.
        let subdir = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();
        let registry = ProjectRegistry::default();
        let resolved = Project::resolve(&subdir, &registry).unwrap();
        assert_eq!(resolved.root, dir.path());
    }

    #[test]
    fn resolve_uses_single_registry_entry() {
        let dir = tempdir().unwrap();
        let mut registry = ProjectRegistry::default();
        registry.add("test", dir.path().to_str().unwrap());
        // Launch from an unrelated temp dir.
        let other = tempdir().unwrap();
        let resolved = Project::resolve(other.path(), &registry).unwrap();
        assert_eq!(resolved.root, dir.path());
    }

    #[test]
    fn resolve_fails_when_nothing_found() {
        let dir = tempdir().unwrap();
        let registry = ProjectRegistry::default();
        let result = Project::resolve(dir.path(), &registry);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_prefers_exact_registry_match() {
        let dir = tempdir().unwrap();
        let other = tempdir().unwrap();
        let mut registry = ProjectRegistry::default();
        registry.add("a", dir.path().to_str().unwrap());
        registry.add("b", other.path().to_str().unwrap());
        // Launch dir matches "a" exactly.
        let resolved = Project::resolve(dir.path(), &registry).unwrap();
        assert_eq!(resolved.root, dir.path());
    }
}
