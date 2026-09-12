// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Load + parse the two `agent.md` constitutions (global + project).
//!
//! The constitution is non-negotiable context loaded before every LLM turn.
//! It is re-read from disk each turn (cheap; small files), so editing
//! `agent.md` takes effect on the next turn without restarting.

use std::path::{Path, PathBuf};

use crate::error::Result;

/// The two constitution files, in load order: global first, then project.
#[derive(Debug, Clone, Default)]
pub struct Constitution {
    /// Global `~/.mnemo/agent.md` — cross-project hard rules. Read first.
    pub global: String,
    /// Project `<project>/agent.md` — this project's hard rules. Read second.
    pub project: String,
}

impl Constitution {
    /// Whether both constitutions are empty.
    pub fn is_empty(&self) -> bool {
        self.global.trim().is_empty() && self.project.trim().is_empty()
    }
}

/// A live constitution source that re-reads from disk when a file changes.
///
/// The constitution is non-negotiable context loaded before every LLM turn.
/// Rather than re-reading both files unconditionally on every turn, this caches
/// the parsed `Constitution` and the last-seen mtimes of the two source files.
/// [`reload_if_changed`](Self::reload_if_changed) stat's the files and re-reads
/// only those whose mtime advanced since the last read — so editing
/// `agent.md` takes effect on the next turn without restarting, at the cost of
/// two cheap `stat` calls per turn instead of two file reads.
#[derive(Debug, Clone)]
pub struct ConstitutionSource {
    global_path: PathBuf,
    project_path: PathBuf,
    cached: Constitution,
    global_mtime: Option<std::time::SystemTime>,
    project_mtime: Option<std::time::SystemTime>,
}

impl ConstitutionSource {
    /// Create a source backed by the two `agent.md` paths. Reads both files
    /// immediately so the first turn doesn't pay the read cost.
    pub fn new(global_path: impl Into<PathBuf>, project_path: impl Into<PathBuf>) -> Result<Self> {
        let global_path = global_path.into();
        let project_path = project_path.into();
        let (cached, global_mtime, project_mtime) = load_with_mtimes(&global_path, &project_path)?;
        Ok(Self {
            global_path,
            project_path,
            cached,
            global_mtime,
            project_mtime,
        })
    }

    /// The cached constitution (re-read on the last `reload_if_changed`).
    pub fn constitution(&self) -> &Constitution {
        &self.cached
    }

    /// Re-read both files if their mtimes have advanced since the last read.
    ///
    /// Returns `true` if anything changed. Best-effort: FS errors keep the
    /// previous cache so a transient failure can't break the agent loop.
    ///
    /// # Why not `spawn_blocking`? (review L3)
    ///
    /// `mtime_of` issues `std::fs::metadata` stats — microseconds-fast syscalls
    /// on OS-cached inodes. Wrapping them in `spawn_blocking` would add
    /// task-allocation + scheduling overhead that may *exceed* the stats
    /// themselves (a well-known anti-pattern for trivially-fast blocking ops).
    /// This is called once per turn via `ConstitutionHolder::stable_head`, and
    /// each turn already does an LLM API call (seconds) — two stats are noise
    /// against that. Accepted as a documented trade-off, not an oversight.
    pub fn reload_if_changed(&mut self) -> bool {
        let mut changed = false;
        if let Some(mtime) = mtime_of(&self.global_path) {
            if self.global_mtime != Some(mtime) {
                if let Ok(content) = read_optional(&self.global_path) {
                    self.cached.global = content;
                    self.global_mtime = Some(mtime);
                    changed = true;
                }
            }
        } else if self.global_mtime.is_some() {
            // File was deleted — clear it.
            self.cached.global.clear();
            self.global_mtime = None;
            changed = true;
        }
        if let Some(mtime) = mtime_of(&self.project_path) {
            if self.project_mtime != Some(mtime) {
                if let Ok(content) = read_optional(&self.project_path) {
                    self.cached.project = content;
                    self.project_mtime = Some(mtime);
                    changed = true;
                }
            }
        } else if self.project_mtime.is_some() {
            self.cached.project.clear();
            self.project_mtime = None;
            changed = true;
        }
        changed
    }
}

/// Load both files + capture their mtimes in one pass.
fn load_with_mtimes(
    global_path: &Path,
    project_path: &Path,
) -> Result<(
    Constitution,
    Option<std::time::SystemTime>,
    Option<std::time::SystemTime>,
)> {
    let global = read_optional(global_path)?;
    let project = read_optional(project_path)?;
    let global_mtime = mtime_of(global_path);
    let project_mtime = mtime_of(project_path);
    Ok((
        Constitution { global, project },
        global_mtime,
        project_mtime,
    ))
}

/// The mtime of a path, or `None` if it doesn't exist / can't be stat'd.
fn mtime_of(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The default template written into a fresh project's `agent.md`.
pub const DEFAULT_PROJECT_TEMPLATE: &str = "\
# Project Constitution

Hard rules for this project. The agent cannot ignore these. They are loaded
before every LLM turn, after the global constitution.

## Examples (replace these)

- Follow the existing code style in this repository.
- All public functions must have doc comments.
- Run `cargo test` before marking a workflow step complete.
- Never commit directly to the main branch.
";

/// Load both constitution files. Missing files are not an error — that section
/// is simply empty.
pub fn load(global_path: &Path, project_path: &Path) -> Result<Constitution> {
    let global = read_optional(global_path)?;
    let project = read_optional(project_path)?;
    Ok(Constitution { global, project })
}

/// Read a file, returning an empty string if it doesn't exist.
fn read_optional(path: &Path) -> Result<String> {
    if !path.exists() {
        return Ok(String::new());
    }
    Ok(std::fs::read_to_string(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_both_files_in_order() {
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let gpath = g.path().join("agent.md");
        let ppath = p.path().join("agent.md");
        std::fs::write(&gpath, "GLOBAL RULES").unwrap();
        std::fs::write(&ppath, "PROJECT RULES").unwrap();

        let c = load(&gpath, &ppath).unwrap();
        assert_eq!(c.global, "GLOBAL RULES");
        assert_eq!(c.project, "PROJECT RULES");
        assert!(!c.is_empty());
    }

    #[test]
    fn missing_files_are_empty_not_error() {
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let c = load(&g.path().join("nope.md"), &p.path().join("nope.md")).unwrap();
        assert!(c.global.is_empty());
        assert!(c.project.is_empty());
        assert!(c.is_empty());
    }

    #[test]
    fn one_missing_one_present() {
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let ppath = p.path().join("agent.md");
        std::fs::write(&ppath, "PROJECT ONLY").unwrap();
        let c = load(&g.path().join("nope.md"), &ppath).unwrap();
        assert!(c.global.is_empty());
        assert_eq!(c.project, "PROJECT ONLY");
    }

    #[test]
    fn default_template_is_nonempty() {
        assert!(!DEFAULT_PROJECT_TEMPLATE.is_empty());
        assert!(DEFAULT_PROJECT_TEMPLATE.contains("Constitution"));
    }

    #[test]
    fn source_reads_initial_content() {
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let gpath = g.path().join("agent.md");
        let ppath = p.path().join("agent.md");
        std::fs::write(&gpath, "GLOBAL v1").unwrap();
        std::fs::write(&ppath, "PROJECT v1").unwrap();

        let src = ConstitutionSource::new(&gpath, &ppath).unwrap();
        let c = src.constitution();
        assert_eq!(c.global, "GLOBAL v1");
        assert_eq!(c.project, "PROJECT v1");
    }

    #[test]
    fn source_rereads_after_mtime_change() {
        // Editing agent.md must be picked up on the next reload_if_changed
        // without restarting — the core requirement of this fix.
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let gpath = g.path().join("agent.md");
        let ppath = p.path().join("agent.md");
        std::fs::write(&gpath, "GLOBAL v1").unwrap();
        std::fs::write(&ppath, "PROJECT v1").unwrap();

        let mut src = ConstitutionSource::new(&gpath, &ppath).unwrap();
        assert_eq!(src.constitution().global, "GLOBAL v1");

        // Write new content and bump the mtime EXPLICITLY. Relying on the
        // wall clock is racy: coarse filesystem timestamp granularity (2s on
        // FAT, 100ns on NTFS) or a backwards clock step (NTP slew) can leave
        // the second write with the SAME mtime as the first, and
        // reload_if_changed compares `SystemTime` for exact equality — so the
        // change is missed (a real flake seen on Windows). Setting the mtime
        // directly makes the test deterministic on every filesystem.
        std::fs::write(&gpath, "GLOBAL v2").unwrap();
        {
            // Write access is required for set_times on Windows (a read-only
            // handle lacks FILE_WRITE_ATTRIBUTES).
            let f = std::fs::File::options().write(true).open(&gpath).unwrap();
            let times = std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60));
            f.set_times(times).unwrap();
        }

        let changed = src.reload_if_changed();
        assert!(changed, "reload should detect the mtime change");
        assert_eq!(src.constitution().global, "GLOBAL v2");
        // The project file was untouched.
        assert_eq!(src.constitution().project, "PROJECT v1");
    }

    #[test]
    fn source_no_change_is_noop() {
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let gpath = g.path().join("agent.md");
        let ppath = p.path().join("agent.md");
        std::fs::write(&gpath, "GLOBAL").unwrap();
        std::fs::write(&ppath, "PROJECT").unwrap();

        let mut src = ConstitutionSource::new(&gpath, &ppath).unwrap();
        // No file changed → reload reports no change and keeps the cache.
        assert!(!src.reload_if_changed());
        assert_eq!(src.constitution().global, "GLOBAL");
        assert_eq!(src.constitution().project, "PROJECT");
    }

    #[test]
    fn source_handles_missing_files() {
        // A source pointing at non-existent files starts empty and doesn't
        // error; if the files appear later, they're picked up.
        let g = tempdir().unwrap();
        let p = tempdir().unwrap();
        let gpath = g.path().join("agent.md");
        let ppath = p.path().join("agent.md");

        let mut src = ConstitutionSource::new(&gpath, &ppath).unwrap();
        assert!(src.constitution().is_empty());

        std::fs::write(&ppath, "NEW PROJECT").unwrap();
        // No sleep needed: the file went from missing (mtime None) to present
        // (Some), which reload_if_changed detects regardless of timestamp
        // ticks — unlike the same-mtime-same-tick case the other test covers.
        assert!(src.reload_if_changed());
        assert_eq!(src.constitution().project, "NEW PROJECT");
    }
}
