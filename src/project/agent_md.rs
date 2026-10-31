// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Load + parse the two `agent.md` constitutions (global + project).
//!
//! The constitution is non-negotiable context loaded before every LLM turn.
//! It is re-read from disk each turn (cheap; small files), so editing
//! `agent.md` takes effect on the next turn without restarting.

use std::path::{Path, PathBuf};

use crate::agent::prompt::ConstitutionMode;
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

    /// The constitution restricted to the prompt spec's constitution mode
    /// (2c.1/G1, §5.9.1 :621): the excluded sources drop to empty; `Both`
    /// returns an exact copy (today's behavior — the byte-golden's path).
    /// Applied by the free head's assembler (`assemble_spec_head`), so the
    /// spec field always governs its own head.
    pub fn restricted(&self, mode: ConstitutionMode) -> Constitution {
        match mode {
            ConstitutionMode::Both => self.clone(),
            ConstitutionMode::Global => Constitution {
                global: self.global.clone(),
                project: String::new(),
            },
            ConstitutionMode::Project => Constitution {
                global: String::new(),
                project: self.project.clone(),
            },
            ConstitutionMode::None => Constitution::default(),
        }
    }

    /// The constitution as a session in `scope` reads it
    /// (`.coding/design/mnemo2.md` Phase 5.2): text wrapped in
    /// `<!-- mnemo:classic -->` … `<!-- /mnemo:classic -->` applies only to
    /// Mnemo1's Classic workflow, text wrapped in `<!-- mnemo:team -->` …
    /// `<!-- /mnemo:team -->` only to Mnemo2 (its lead and workers), and
    /// unmarked text to everyone.
    pub fn scoped(&self, scope: ConstitutionScope) -> Constitution {
        Constitution {
            global: scope_text(&self.global, scope),
            project: scope_text(&self.project, scope),
        }
    }
}

/// Which workflow's marked `agent.md` sections a session reads
/// ([`Constitution::scoped`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstitutionScope {
    /// Mnemo1's plan-first workflow: drops `mnemo:team` sections.
    Classic,
    /// The Mnemo2 lead and its executor workers: drops `mnemo:classic`
    /// sections.
    Team,
}

/// Marker lines of the two scoped section kinds: `(open, close)`.
const CLASSIC_MARKERS: (&str, &str) = ("<!-- mnemo:classic -->", "<!-- /mnemo:classic -->");
const TEAM_MARKERS: (&str, &str) = ("<!-- mnemo:team -->", "<!-- /mnemo:team -->");

/// What a marker line (trimmed) is.
enum Marker {
    Open(ConstitutionScope),
    Close(ConstitutionScope),
}

fn parse_marker(line: &str) -> Option<Marker> {
    let line = line.trim();
    if !line.starts_with("<!-- ") {
        return None;
    }
    if line == CLASSIC_MARKERS.0 {
        Some(Marker::Open(ConstitutionScope::Classic))
    } else if line == CLASSIC_MARKERS.1 {
        Some(Marker::Close(ConstitutionScope::Classic))
    } else if line == TEAM_MARKERS.0 {
        Some(Marker::Open(ConstitutionScope::Team))
    } else if line == TEAM_MARKERS.1 {
        Some(Marker::Close(ConstitutionScope::Team))
    } else {
        None
    }
}

/// Filter one constitution text for `scope`. Text with no marker line comes
/// back byte-identical. Marker lines are always dropped; a section of the
/// other scope is dropped with its body; an unterminated open marker keeps
/// its body (over-include rather than silently lose rules). A removed
/// section never leaves a run of 3+ blank lines.
fn scope_text(text: &str, scope: ConstitutionScope) -> String {
    if !text.lines().any(|l| parse_marker(l).is_some()) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    // The section kind we're inside, if any (sections don't nest).
    // The section kind we're inside, if any (sections don't nest).
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut inside: Option<ConstitutionScope> = None;
    let mut blank_run = 0usize;
    for (i, line) in lines.iter().enumerate() {
        match parse_marker(line) {
            Some(Marker::Open(kind)) => {
                // Only a terminated section can drop text.
                let closed = lines[i + 1..]
                    .iter()
                    .any(|l| matches!(parse_marker(l), Some(Marker::Close(k)) if k == kind));
                inside = closed.then_some(kind);
                continue;
            }
            Some(Marker::Close(kind)) => {
                if inside == Some(kind) {
                    inside = None;
                }
                continue;
            }
            None => {}
        }
        if matches!(inside, Some(kind) if kind != scope) {
            continue;
        }
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 2 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line);
    }
    out
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

    const MIXED: &str = "intro\n\n<!-- mnemo:classic -->\nCLASSIC RULE\n<!-- /mnemo:classic -->\n\n<!-- mnemo:team -->\nTEAM RULE\n<!-- /mnemo:team -->\n\noutro\n";

    #[test]
    fn scoped_classic_drops_team_sections() {
        let c = Constitution { global: MIXED.into(), project: String::new() };
        let g = c.scoped(ConstitutionScope::Classic).global;
        assert!(g.contains("CLASSIC RULE") && !g.contains("TEAM RULE"));
        assert!(g.contains("intro") && g.contains("outro"));
        assert!(!g.contains("mnemo:"), "marker lines are dropped: {g}");
    }

    #[test]
    fn scoped_team_drops_classic_sections() {
        let c = Constitution { global: MIXED.into(), project: String::new() };
        let g = c.scoped(ConstitutionScope::Team).global;
        assert!(g.contains("TEAM RULE") && !g.contains("CLASSIC RULE"));
        assert!(!g.contains("mnemo:"));
    }

    #[test]
    fn scoped_without_markers_is_identity() {
        let text = "a\r\n\n\n\n\nb <!-- not a marker line -->\n";
        let c = Constitution { global: text.into(), project: "p".into() };
        for scope in [ConstitutionScope::Classic, ConstitutionScope::Team] {
            let s = c.scoped(scope);
            assert_eq!(s.global, text);
            assert_eq!(s.project, "p");
        }
    }

    #[test]
    fn scoped_unterminated_marker_keeps_text() {
        let c = Constitution {
            global: "a\n<!-- mnemo:team -->\nKEEP ME\n".into(),
            project: String::new(),
        };
        assert_eq!(c.scoped(ConstitutionScope::Classic).global, "a\nKEEP ME\n");
        assert_eq!(c.scoped(ConstitutionScope::Team).global, "a\nKEEP ME\n");
    }

    #[test]
    fn scoped_tolerates_marker_whitespace_and_crlf() {
        let c = Constitution {
            global: "a\r\n  <!-- mnemo:classic -->  \r\nC\r\n\t<!-- /mnemo:classic -->\r\nb\r\n".into(),
            project: String::new(),
        };
        assert_eq!(c.scoped(ConstitutionScope::Classic).global, "a\r\nC\r\nb\r\n");
        assert_eq!(c.scoped(ConstitutionScope::Team).global, "a\r\nb\r\n");
    }

    #[test]
    fn scoped_filters_global_and_project() {
        let c = Constitution { global: MIXED.into(), project: MIXED.into() };
        let t = c.scoped(ConstitutionScope::Team);
        assert!(!t.global.contains("CLASSIC RULE") && !t.project.contains("CLASSIC RULE"));
        assert!(t.global.contains("TEAM RULE") && t.project.contains("TEAM RULE"));
    }

    #[test]
    fn scoped_blank_runs_collapse_after_removal() {
        let c = Constitution {
            global: "a\n\n\n\n<!-- mnemo:team -->\nT\n<!-- /mnemo:team -->\n\n\n\nb\n".into(),
            project: String::new(),
        };
        assert_eq!(c.scoped(ConstitutionScope::Classic).global, "a\n\n\nb\n");
    }

    #[test]
    fn real_agent_md_scopes_for_team() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/agent.md");
        let text = std::fs::read_to_string(path).unwrap();
        let c = Constitution { global: String::new(), project: text };
        let team = c.scoped(ConstitutionScope::Team).project;
        let classic = c.scoped(ConstitutionScope::Classic).project;
        assert!(!team.contains("update_plan"), "classic-only rule leaked");
        assert!(!team.contains("Review expectations"));
        assert!(team.contains("File mutation policy"), "universal rule lost");
        assert!(team.contains("### Mnemo2 team merge policy"));
        assert!(classic.contains("Review expectations"));
        assert!(!classic.contains("### Mnemo2 team merge policy"));
        assert!(!team.contains("mnemo:") && !classic.contains("mnemo:"));
    }
}
