// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Skills — named, activatable overlays on the workflow.
//!
//! A skill is a file-defined bundle of {tool allow-list, prompt, entry/exit
//! states} that, when started via [`SkillStartTool`](crate::tool::workflow::skill::SkillStartTool),
//! transitions the workflow into [`WorkflowState::Skill`](crate::workflow::WorkflowState::Skill)
//! and replaces the base tool filter with the skill's allow-list + injects the
//! skill's prompt into the system prompt. The agent then drives toward the
//! goal using its tools; it exits via `skill_end` (→ target_state) or
//! `abandon_skill` (→ the pre-skill state).
//!
//! Skills are defined as TOML files in `.coding/skills/<name>.toml` and loaded
//! into a [`SkillLibrary`] — the skills dir plus the LIVE registry. Adding a
//! skill is dropping a file (or authoring one with the `skill_create` tool)
//! and calling `skill_reload`: no recompile, no app restart. Normal security
//! controls (the approval gate, per safety mode) apply to every tool call
//! inside a skill exactly as outside it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::workflow::WorkflowState;

/// The spec for one skill, loaded from a `.coding/skills/<name>.toml` file.
///
/// `available_in` gates which workflow states the skill may be started from
/// (validated by [`SkillStartTool`](crate::tool::workflow::skill::SkillStartTool)
/// at call time). `target_state` is where the workflow lands after `skill_end`.
/// `tools` is the allow-list active while the skill runs. `prompt` is the goal
/// the agent drives toward (injected into the system prompt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSpec {
    /// The skill name (matches the file stem). Selects the registry entry.
    pub name: String,
    /// The workflow states this skill may be started from (e.g.
    /// `["complete", "planning"]`). The skill is unavailable otherwise.
    pub available_in: Vec<WorkflowState>,
    /// Where the workflow lands after `skill_end`.
    pub target_state: WorkflowState,
    /// The tool allow-list active while the skill runs. Memory tools are
    /// always available regardless of this list.
    pub tools: Vec<String>,
    /// The goal the agent drives toward while the skill is active.
    pub prompt: String,
}

/// The registry of all loaded skills, keyed by name.
///
/// Built at startup from `.coding/skills/*.toml`. A missing directory yields
/// an empty registry (no skills available). A malformed file is logged and
/// skipped (one bad skill file doesn't break the others).
#[derive(Debug, Default, Clone)]
pub struct SkillRegistry {
    skills: HashMap<String, SkillSpec>,
}

impl SkillRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Insert a skill spec (used by tests). Returns the prior spec for that
    /// name, if any. (The loader in [`load_dir`](Self::load_dir) inserts
    /// directly into the map, not via this method.)
    #[cfg(test)]
    pub(crate) fn insert(&mut self, spec: SkillSpec) -> Option<SkillSpec> {
        self.skills.insert(spec.name.clone(), spec)
    }

    /// Load every `.toml` file in `dir` as a skill spec. Best-effort: a missing
    /// directory yields an empty registry; a malformed file — or one whose
    /// `target_state` is not a lifecycle state (Planning/Executing/Complete) —
    /// is logged to stderr and skipped (not fatal). Returns the populated
    /// registry.
    pub fn load_dir(dir: &Path) -> Self {
        let mut registry = Self::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            // Missing directory — no skills. Not an error.
            return registry;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => match toml::from_str::<SkillSpec>(&text) {
                    Ok(spec) => {
                        // Validate the landing state: only lifecycle states are
                        // legal skill_end targets. `Skill` is an overlay (entered
                        // via start_skill, never a landing state) and `Subagent`
                        // is a spawn-path-stamped role state whose invariant
                        // (allow-list always set) a main-agent workflow cannot
                        // satisfy — landing in either would panic `allowed_tools`
                        // on the next turn. The agent-driven `skill_start` tool
                        // already validates this (skill.rs), but the UI-initiated
                        // `enter_skill` path passes the spec's target_state
                        // straight through, so a hand-edited TOML could still
                        // land a main-agent workflow in a bad state. Rejecting
                        // at load is the strongest close: bad TOML never reaches
                        // any path (review residual, 2026-01-03, plan 3fb064c4).
                        if !matches!(
                            spec.target_state,
                            WorkflowState::Planning
                                | WorkflowState::Executing
                                | WorkflowState::Complete
                        ) {
                            eprintln!(
                                "skill: '{}' has invalid target_state '{}' \
                                 (must be \"planning\", \"executing\", or \"complete\"); skipping",
                                spec.name, spec.target_state
                            );
                            continue;
                        }
                        registry.skills.insert(spec.name.clone(), spec);
                    }
                    Err(e) => {
                        eprintln!("skill: failed to parse {}: {e}; skipping", path.display());
                    }
                },
                Err(e) => {
                    eprintln!("skill: failed to read {}: {e}; skipping", path.display());
                }
            }
        }
        registry
    }

    /// Look up a skill by name.
    pub fn get(&self, name: &str) -> Option<&SkillSpec> {
        self.skills.get(name)
    }

    /// Iterate over all loaded skills.
    pub fn iter(&self) -> impl Iterator<Item = &SkillSpec> {
        self.skills.values()
    }

    /// Whether the skill `name` is available in the given workflow state
    /// (i.e. the state is in the skill's `available_in` list).
    pub fn is_available_in(&self, name: &str, state: WorkflowState) -> bool {
        self.get(name)
            .map(|s| s.available_in.contains(&state))
            .unwrap_or(false)
    }
}

/// What a [`SkillLibrary::reload`] found on disk.
///
/// The name lists are SORTED, so the report — the `skill_reload` tool result
/// the agent reads, and the tests — is deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillReloadReport {
    /// How many skills the registry holds after the reload.
    pub total: usize,
    /// Names the reload ADDED (on disk, absent from the registry).
    pub added: Vec<String>,
    /// Names the reload REMOVED (in the registry, gone from disk — including
    /// files that stopped parsing or lost a lifecycle `target_state`).
    pub removed: Vec<String>,
    /// Every loaded skill name, sorted.
    pub names: Vec<String>,
}

/// A loaded skill library: the skills directory plus the LIVE registry.
///
/// The registry sits behind an [`RwLock`] so it can be swapped while every
/// reader keeps working — the `skill_start` tool, the UI's `enter_skill`
/// path, and the `skill_reload` / `skill_create` tools all read through
/// [`read`](Self::read). Without this the registry was loaded once at startup
/// into an immutable `Arc<SkillRegistry>`, so a skill file created or edited
/// mid-session stayed invisible until the app was restarted.
///
/// `dir` is the tree the registry was loaded from (`.coding/skills/` for the
/// main installation). `skill_create` writes there and [`reload`](Self::reload)
/// re-reads it, so authoring and loading can never disagree about where a
/// skill lives.
#[derive(Debug)]
pub struct SkillLibrary {
    dir: PathBuf,
    registry: RwLock<SkillRegistry>,
}

impl SkillLibrary {
    /// Load `dir` into a new library. A missing directory yields an empty
    /// registry (no skills available) — not an error.
    pub fn load(dir: PathBuf) -> Self {
        Self {
            registry: RwLock::new(SkillRegistry::load_dir(&dir)),
            dir,
        }
    }

    /// The skills directory this library was loaded from (`.coding/skills/`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Run `f` against the loaded registry under a read lock.
    ///
    /// The guard never escapes the closure, so a caller cannot hold it across
    /// an `.await` (a `std` guard held over a suspension point deadlocks
    /// against a writer). A POISONED lock is recovered rather than panicking:
    /// the registry is plain data, so the worst case is a stale-but-valid
    /// snapshot.
    pub fn read<R>(&self, f: impl FnOnce(&SkillRegistry) -> R) -> R {
        let guard = self.registry.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// Re-read the skills directory into the live registry, replacing its
    /// contents, and report what changed.
    ///
    /// The fresh registry is built BEFORE the write lock is taken (a slow disk
    /// never blocks readers) and the swap is a single assignment — a reader
    /// sees the old registry or the new one, never a half-loaded mix. Files
    /// that fail to parse, or that carry a non-lifecycle `target_state`, are
    /// logged and skipped by [`SkillRegistry::load_dir`]; a skill lost that way
    /// lands in `removed` rather than `names`.
    pub fn reload(&self) -> SkillReloadReport {
        let fresh = SkillRegistry::load_dir(&self.dir);
        let mut guard = self.registry.write().unwrap_or_else(|e| e.into_inner());
        let mut added: Vec<String> = fresh
            .iter()
            .filter(|s| guard.get(&s.name).is_none())
            .map(|s| s.name.clone())
            .collect();
        let mut removed: Vec<String> = guard
            .iter()
            .filter(|s| fresh.get(&s.name).is_none())
            .map(|s| s.name.clone())
            .collect();
        let mut names: Vec<String> = fresh.iter().map(|s| s.name.clone()).collect();
        let total = names.len();
        added.sort();
        removed.sort();
        names.sort();
        *guard = fresh;
        SkillReloadReport {
            total,
            added,
            removed,
            names,
        }
    }

    /// Insert (or replace) one spec in the live registry — the hot-add path
    /// `skill_create` uses, so a freshly authored skill is startable without a
    /// full reload.
    pub fn insert(&self, spec: SkillSpec) {
        let mut guard = self.registry.write().unwrap_or_else(|e| e.into_inner());
        guard.skills.insert(spec.name.clone(), spec);
    }

    /// Build a library around an in-memory registry (tests only).
    #[cfg(test)]
    pub(crate) fn from_registry(dir: PathBuf, registry: SkillRegistry) -> Self {
        Self {
            dir,
            registry: RwLock::new(registry),
        }
    }
}

/// The skills shipped with the app, compiled into the binary as
/// `(name, toml_text)` pairs via `include_str!`. [`crate::project::Project::init`]
/// seeds each into a project's `.coding/skills/` directory (write-if-missing),
/// so every new project — and an existing one whose skill files were deleted —
/// starts with the same baseline skill set as the main installation, without
/// depending on the app's source tree being present at runtime.
///
/// Adding a shipped skill takes TWO edits: drop the TOML into the repo's own
/// `.coding/skills/<name>.toml` AND add one entry here (the file is embedded
/// at compile time; nothing is read from disk at runtime).
pub const SHIPPED_SKILLS: &[(&str, &str)] = &[(
    "merge_to_main",
    include_str!("../../.coding/skills/merge_to_main.toml"),
)];

/// Is `name` one of the skills shipped with the app ([`SHIPPED_SKILLS`])?
///
/// `skill_create` refuses these names. A shipped file is the source of truth
/// for its procedure (e.g. `merge_to_main`), it is re-seeded only while the
/// file is MISSING, and `skill_create` is AutoRun — so an overwrite would
/// silently and permanently replace one with no approval prompt. Editing a
/// shipped skill stays possible through the approval-gated file tools, which is
/// the deliberate path (review LOW 2, 2027-01-16).
pub fn is_shipped_skill(name: &str) -> bool {
    SHIPPED_SKILLS.iter().any(|(n, _)| *n == name)
}

/// Seed the [`SHIPPED_SKILLS`] into `skills_dir` (a project's
/// `.coding/skills/`): the directory is created when missing and each shipped
/// skill is written as `<name>.toml` — but ONLY when that file does not
/// already exist, so a user-modified or project-specific skill is never
/// overwritten. Re-seeding a populated directory is a no-op; deleting a skill
/// file (or the whole directory) makes the next project init self-heal it.
///
/// Returns the underlying I/O error when the directory cannot be created or a
/// first write fails.
pub fn seed_skills(skills_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(skills_dir)?;
    for (name, text) in SHIPPED_SKILLS {
        let path = skills_dir.join(format!("{name}.toml"));
        if path.exists() {
            continue;
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Write a skill TOML file into `dir`.
    fn write_skill(dir: &Path, name: &str, toml: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{name}.toml")), toml).unwrap();
    }

    #[test]
    fn load_dir_loads_a_valid_skill() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "merge_to_main",
            r#"name = "merge_to_main"
available_in = ["complete", "planning"]
target_state = "planning"
tools = ["file_read", "git", "skill_end", "abandon_skill"]
prompt = "Merge the branch into main."
"#,
        );
        let reg = SkillRegistry::load_dir(dir.path());
        let spec = reg.get("merge_to_main").unwrap();
        assert_eq!(spec.name, "merge_to_main");
        assert_eq!(spec.target_state, WorkflowState::Planning);
        assert!(spec.available_in.contains(&WorkflowState::Complete));
        assert!(spec.available_in.contains(&WorkflowState::Planning));
        assert!(!spec.available_in.contains(&WorkflowState::Executing));
        assert_eq!(spec.tools.len(), 4);
        assert_eq!(spec.prompt, "Merge the branch into main.");
    }

    #[test]
    fn load_dir_missing_dir_is_empty() {
        let reg = SkillRegistry::load_dir(Path::new("/nonexistent/skills/dir"));
        assert!(reg.iter().count() == 0);
        assert!(reg.get("anything").is_none());
    }

    #[test]
    fn load_dir_skips_malformed_file_keeps_valid_ones() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "good",
            r#"name = "good"
available_in = ["complete"]
target_state = "planning"
tools = ["git"]
prompt = "Do good."
"#,
        );
        // A malformed file (missing required fields).
        write_skill(dir.path(), "bad", r#"name = "bad""#);
        let reg = SkillRegistry::load_dir(dir.path());
        assert!(reg.get("good").is_some(), "valid skill loaded");
        assert!(reg.get("bad").is_none(), "malformed skill skipped");
    }

    #[test]
    fn load_dir_skips_skill_with_non_lifecycle_target_state() {
        // Review residual (2026-01-03, plan 3fb064c4): a hand-edited skill TOML
        // with target_state = "subagent" (or "skill"/"reviewing") would land a
        // MAIN-agent workflow in a state whose allowed_tools invariant it
        // cannot satisfy — the Subagent arm panics ("must carry a tool
        // allow-list"), and Skill is an overlay with no base filter. The
        // LLM-reachable skill_start tool path was validated in 3fb064c4
        // (skill.rs), but the UI-initiated enter_skill path passes
        // spec.target_state straight through. Closing at registry load is the
        // strongest fix: bad TOML never reaches any path. WorkflowState serde
        // is rename_all = "lowercase", so "subagent"/"skill"/"reviewing" all
        // parse to valid variants — had serde rejected them the spec would
        // vanish via the parse-error arm, not this gate; green tests therefore
        // prove the rejection comes from the validation, not a parse failure.
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "good",
            r#"name = "good"
available_in = ["complete"]
target_state = "planning"
tools = ["git"]
prompt = "Do good."
"#,
        );
        for bad in ["subagent", "skill", "reviewing"] {
            write_skill(
                dir.path(),
                &format!("bad_{bad}"),
                &format!(
                    r#"name = "bad_{bad}"
available_in = ["complete"]
target_state = "{bad}"
tools = ["git"]
prompt = "Bad."
"#
                ),
            );
        }
        let reg = SkillRegistry::load_dir(dir.path());
        assert!(reg.get("good").is_some(), "valid skill loads");
        assert!(
            reg.get("bad_subagent").is_none(),
            "subagent target_state rejected at load"
        );
        assert!(
            reg.get("bad_skill").is_none(),
            "skill target_state rejected at load"
        );
        assert!(
            reg.get("bad_reviewing").is_none(),
            "reviewing target_state rejected at load"
        );
    }

    #[test]
    fn is_available_in_checks_the_list() {
        let dir = tempdir().unwrap();
        write_skill(
            dir.path(),
            "merge_to_main",
            r#"name = "merge_to_main"
available_in = ["complete", "planning"]
target_state = "planning"
tools = ["git"]
prompt = "Merge."
"#,
        );
        let reg = SkillRegistry::load_dir(dir.path());
        assert!(reg.is_available_in("merge_to_main", WorkflowState::Complete));
        assert!(reg.is_available_in("merge_to_main", WorkflowState::Planning));
        assert!(!reg.is_available_in("merge_to_main", WorkflowState::Executing));
        assert!(!reg.is_available_in("nonexistent", WorkflowState::Complete));
    }

    #[test]
    fn load_dir_ignores_non_toml_files() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(dir.path().join("readme.md"), "# not a skill").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a skill").unwrap();
        let reg = SkillRegistry::load_dir(dir.path());
        assert_eq!(reg.iter().count(), 0);
    }

    #[test]
    fn shipped_skill_files_parse_and_merge_to_main_cleans_up_memories() {
        // The repo's own `.coding/skills/*.toml` must never rot — a malformed
        // file is only logged-and-skipped at startup (the skill silently
        // vanishes). CARGO_MANIFEST_DIR is the repo root for the lib crate.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".coding/skills");
        let reg = SkillRegistry::load_dir(&dir);
        let merge = reg
            .get("merge_to_main")
            .expect("merge_to_main.toml parses and loads");
        // The merge hygiene step: branch-status memories are superseded once
        // the branch lands in main (memory tools are always available inside
        // a skill, so the tools list needs no entry).
        assert!(
            merge.prompt.contains("MERGED into main"),
            "merge_to_main prompt carries the memory-cleanup step"
        );
        // The remote sync (2026-09-13: a stale main surfaced as a rejected
        // push only after the branch had already landed). The ORDERING is the
        // point — a pull that runs after `git merge --no-ff <branch>` cannot
        // prevent that, so pin sync-before-merge, not merely present. Both
        // halves of the sync are pinned: a bare `git pull` with no prior fetch
        // pulls a remote-tracking ref that may itself be stale.
        let fetch = merge
            .prompt
            .find("git fetch origin")
            .expect("merge_to_main prompt carries the fetch half of the sync");
        let sync = merge
            .prompt
            .find("git pull --no-rebase")
            .expect("merge_to_main prompt carries the remote-sync step");
        let branch_merge = merge
            .prompt
            .find("--no-ff <branch>")
            .expect("merge_to_main prompt carries the branch merge step");
        assert!(
            fetch < sync,
            "the sync must fetch before it pulls (fetch at {fetch}, pull at {sync})"
        );
        assert!(
            sync < branch_merge,
            "the origin sync must precede the branch merge (sync at {sync}, merge at {branch_merge})"
        );
    }

    #[test]
    fn shipped_skills_embedded_entries_parse_and_carry_their_name() {
        // Every embedded shipped skill must parse as a SkillSpec and carry
        // the name it is seeded under (the pair's name must equal the spec's
        // `name` field — a mismatch would seed a file that loads under a
        // different key than documented). Also pins the baseline: the
        // merge_to_main skill must always ship.
        for (name, text) in SHIPPED_SKILLS {
            let spec: SkillSpec = toml::from_str(text)
                .unwrap_or_else(|e| panic!("shipped skill {name} does not parse: {e}"));
            assert_eq!(
                spec.name, *name,
                "embedded name must match the spec's name field"
            );
        }
        assert!(
            SHIPPED_SKILLS.iter().any(|(n, _)| *n == "merge_to_main"),
            "merge_to_main must be embedded in SHIPPED_SKILLS"
        );
    }

    /// A one-skill TOML body for the library tests (`prompt` is the dial that
    /// proves a reload picked up EDITED content, not just a file count).
    fn skill_toml(name: &str, prompt: &str) -> String {
        format!(
            "name = \"{name}\"\navailable_in = [\"complete\"]\ntarget_state = \"planning\"\n\
             tools = [\"git\"]\nprompt = \"{prompt}\"\n"
        )
    }

    #[test]
    fn shipped_skill_names_are_recognized() {
        assert!(is_shipped_skill("merge_to_main"));
        assert!(!is_shipped_skill("merge_to_main_v2"));
        assert!(!is_shipped_skill(""));
    }

    #[test]
    fn library_reload_reports_added_removed_and_changes() {
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "alpha", &skill_toml("alpha", "Do alpha."));
        write_skill(dir.path(), "beta", &skill_toml("beta", "Do beta."));
        let lib = SkillLibrary::load(dir.path().to_path_buf());

        // An unchanged directory: nothing added, nothing removed.
        let r = lib.reload();
        assert_eq!(r.total, 2, "both skills stay loaded");
        assert!(r.added.is_empty(), "nothing added: {:?}", r.added);
        assert!(r.removed.is_empty(), "nothing removed: {:?}", r.removed);
        assert_eq!(r.names, vec!["alpha".to_string(), "beta".to_string()]);

        // Add one, edit one, delete one — all three must be reported.
        write_skill(dir.path(), "gamma", &skill_toml("gamma", "Do gamma."));
        write_skill(dir.path(), "alpha", &skill_toml("alpha", "Do alpha, revised."));
        std::fs::remove_file(dir.path().join("beta.toml")).unwrap();

        let r = lib.reload();
        assert_eq!(r.total, 2);
        assert_eq!(r.added, vec!["gamma".to_string()]);
        assert_eq!(r.removed, vec!["beta".to_string()]);
        assert_eq!(r.names, vec!["alpha".to_string(), "gamma".to_string()]);
        // The EDITED file's new content is live, not just its name.
        assert_eq!(
            lib.read(|reg| reg.get("alpha").map(|s| s.prompt.clone())),
            Some("Do alpha, revised.".to_string())
        );
        assert!(lib.read(|reg| reg.get("beta").is_none()));
    }

    #[test]
    fn library_reload_drops_a_file_that_stops_parsing() {
        // A file that goes bad (malformed TOML) is logged + skipped by
        // load_dir, so the reload must report it as removed — a skill that
        // silently stays callable after its file broke would be worse.
        let dir = tempdir().unwrap();
        write_skill(dir.path(), "good", &skill_toml("good", "Do good."));
        let lib = SkillLibrary::load(dir.path().to_path_buf());
        assert!(lib.read(|reg| reg.get("good").is_some()));

        std::fs::write(dir.path().join("good.toml"), "name = \"good\"\n").unwrap();
        let r = lib.reload();
        assert_eq!(r.removed, vec!["good".to_string()]);
        assert_eq!(r.total, 0);
        assert!(lib.read(|reg| reg.get("good").is_none()));
    }

    #[test]
    fn library_read_sees_a_hot_added_spec() {
        let dir = tempdir().unwrap();
        let lib = SkillLibrary::load(dir.path().to_path_buf());
        assert!(lib.read(|reg| reg.get("hot").is_none()));
        lib.insert(SkillSpec {
            name: "hot".into(),
            available_in: vec![WorkflowState::Complete],
            target_state: WorkflowState::Planning,
            tools: vec!["git".into()],
            prompt: "Do hot.".into(),
        });
        assert!(
            lib.read(|reg| reg.get("hot").is_some()),
            "an inserted spec is visible to readers immediately"
        );
    }

    #[test]
    fn library_from_registry_exposes_an_in_memory_spec() {
        // The test-only constructor (used by the factory + tool tests) must
        // behave like a loaded library: no disk I/O, same read path.
        let dir = tempdir().unwrap();
        let mut reg = SkillRegistry::new();
        reg.insert(SkillSpec {
            name: "in_memory".into(),
            available_in: vec![WorkflowState::Executing],
            target_state: WorkflowState::Complete,
            tools: vec!["file_read".into()],
            prompt: "Do it.".into(),
        });
        let lib = SkillLibrary::from_registry(dir.path().to_path_buf(), reg);
        assert!(lib.read(|r| r.is_available_in("in_memory", WorkflowState::Executing)));
        assert_eq!(lib.read(|r| r.iter().count()), 1);
    }

    #[test]
    fn library_reload_on_missing_dir_is_empty() {
        let lib = SkillLibrary::load(PathBuf::from("C:/nonexistent/skills/dir"));
        let r = lib.reload();
        assert_eq!(r.total, 0);
        assert!(r.names.is_empty());
        assert_eq!(lib.dir(), Path::new("C:/nonexistent/skills/dir"));
    }
}
