// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Project-init git-file scaffolding — the app-owned `.gitignore` /
//! `.gitattributes` entries a project needs for its own `.coding/` state to
//! travel through git safely.
//!
//! A project's `.coding/` side-car is meant to be committed (knowledge, plans,
//! reviews, skills, `backlog.jsonl`), while the per-machine caches and
//! ephemeral state inside it must never be — and `backlog.jsonl` must merge by
//! concatenation across worktrees instead of conflicting. Both rules live in
//! git's own files, so [`Project::init`](crate::project::Project::init) ensures
//! them here rather than leaving every new project to rediscover them:
//!
//! - a project whose first commit sweeps `.coding/memory.db` /
//!   `.coding/codegraph.db` (multi-MB SQLite caches) into its repo because no
//!   `.gitignore` rule covers them;
//! - a project whose `.coding/backlog.jsonl` conflicts across run-all
//!   worktrees because the `merge=union` driver was never declared.
//!
//! Seeding MERGES: a missing file is created with the managed block, while an
//! existing user file only receives the entries it does not already carry —
//! existing lines are never reordered, rewritten, or removed, and a fully
//! covered file is left byte-identical (see [`ensure_managed_entries`]).

use std::path::{Path, PathBuf};

/// The entries [`seed_git_files`] ensures in a project's `.gitignore`.
///
/// Deliberately limited to state the app itself owns inside `.coding/` plus
/// the run-all worktree root: language/build ignores (`target/`,
/// `node_modules/`, …) belong to the project, not to the agent. Mirrors this
/// repository's own `.gitignore`.
pub const MANAGED_GITIGNORE_ENTRIES: &[&str] = &[
    // The SQLite caches (memory store + code knowledge graph) and their
    // WAL/SHM sidecars — per-machine, rebuilt on startup, never worth a
    // commit.
    ".coding/memory.db",
    ".coding/memory.db-*",
    ".coding/codegraph.db",
    ".coding/codegraph.db-*",
    // Rotating trace / provider-error logs.
    ".coding/logs/",
    // Backlog screenshot attachments (sidecar files; the tracked JSONL
    // carries relative path refs only).
    ".coding/backlog-images/",
    // The live plan STACK (which plan is active in THIS worktree) — the
    // tracked plans/*.md are the history, the pointer is not.
    ".coding/plans/stack.json",
    // Per-instance {pid, started_at} marker (last writer wins).
    ".coding/instance.json",
    // Scratch shards from in-session payload-decomposition workarounds.
    ".coding/tmp-shard/",
    // Parallel run-all linked worktrees — disposable working trees.
    ".worktrees/",
];

/// The entries [`seed_git_files`] ensures in a project's `.gitattributes`,
/// mirroring this repository's own file.
///
/// `*.db* -text` is deliberately REPO-WIDE rather than scoped to `.coding/`:
/// `-text` (no eol/encoding normalization) is the safe default for any SQLite
/// database — the app's caches in this project and the user's own `.db` files
/// alike — and a repo-wide rule cannot miss a cache that moves. The union
/// driver on `.coding/backlog.jsonl` is what keeps concurrent backlog writes
/// from several worktrees conflict-free, the UUID item ids making the union
/// collision-free.
pub const MANAGED_GITATTRIBUTES_ENTRIES: &[&str] = &[
    "*.db -text",
    "*.db-wal -text",
    "*.db-shm -text",
    ".coding/backlog.jsonl merge=union",
];

/// Header comment written above the managed `.gitignore` entries when the
/// header is not already present.
const GITIGNORE_HEADER: &str = "# ===== Agent state (.coding/) =====\n\
# The .coding/ side-car (knowledge, plans, reviews, skills, backlog.jsonl)\n\
# IS committed; the entries below are its per-machine caches and ephemeral\n\
# state, which never are.";

/// Header comment written above the managed `.gitattributes` entries when the
/// header is not already present.
const GITATTRIBUTES_HEADER: &str = "# ===== Agent state (.coding/) =====\n\
# Every SQLite database file — the app's caches under .coding/ and any\n\
# user-owned .db — is binary: never text/eol-processed.\n\
# The backlog merges by concatenation across worktrees (UUID ids make the\n\
# union collision-free).";

/// Ensure `entries` are present in the git file at `path`, MERGING with
/// whatever the user already has there.
///
/// - File missing: created with `header` + every entry, LF endings (the app
///   pins new files to LF).
/// - File present: only the entries not already covered are appended at the
///   end — under `header` when that header is not there yet — using the
///   file's own dominant line-ending style. Existing lines are never
///   reordered, rewritten, or removed.
/// - File present and fully covered: no write at all (byte-identical no-op),
///   which is what makes re-running `Project::init` safe.
/// - File present but not valid UTF-8: skipped (returns `false`, file left
///   byte-untouched) — git's files are byte-oriented, and one unreadable byte
///   must never fail an init.
///
/// Writes never truncate an existing file in place: the merged content goes to
/// a temp sibling first and is renamed over the target (see
/// [`write_preserving`]).
///
/// An entry counts as covered when some line equals it after trimming
/// whitespace and stripping a leading `/` (git treats `/.coding/memory.db`
/// and `.coding/memory.db` as the same pattern); a commented-out line never
/// counts.
///
/// Returns whether anything was written.
pub fn ensure_managed_entries(path: &Path, header: &str, entries: &[&str]) -> std::io::Result<bool> {
    let existing = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            // Git's ignore/attribute files are byte-oriented and may legally
            // carry a stray non-UTF-8 byte (a Latin-1 comment, a mangled
            // paste). Seeding is hygiene, not correctness: such a file is left
            // byte-untouched instead of failing every future init on it.
            Err(_) => return Ok(false),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_preserving(path, &render_block(header, entries, "\n"))?;
            return Ok(true);
        }
        Err(e) => return Err(e),
    };

    let missing: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|entry| !contains_entry(&existing, entry))
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }

    // Appended lines follow the file's dominant ending style.
    let eol = if existing.contains("\r\n") { "\r\n" } else { "\n" };
    let mut out = existing;
    if !out.is_empty() {
        // Terminate a last line that has no newline, then separate our block
        // from the user's content with one blank line.
        if !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(eol);
    }
    if !header_present(&out, header) {
        out.push_str(&render_block(header, &[], eol));
    }
    out.push_str(&render_block("", &missing, eol));
    write_preserving(path, &out)?;
    Ok(true)
}

/// Ensure both managed git files (`.gitignore` + `.gitattributes`) under
/// `root` carry the app-owned entries — the seeding `Project::init` runs on
/// every initialization, so a hand-deleted entry self-heals while a
/// user-modified file is never overwritten.
pub fn seed_git_files(root: &Path) -> std::io::Result<()> {
    ensure_managed_entries(
        &root.join(".gitignore"),
        GITIGNORE_HEADER,
        MANAGED_GITIGNORE_ENTRIES,
    )?;
    ensure_managed_entries(
        &root.join(".gitattributes"),
        GITATTRIBUTES_HEADER,
        MANAGED_GITATTRIBUTES_ENTRIES,
    )?;
    Ok(())
}

/// Write `content` at `path` without ever truncating the existing file in
/// place: the bytes go to a same-directory `<name>.seed-tmp` first and are
/// renamed over `path` (atomic on Windows and Unix), so a crash between
/// truncate and write cannot eat the user's `.gitignore` — the one data-loss
/// path a plain `fs::write` has.
///
/// Best-effort by design: a temp write or rename failure falls back to the
/// direct write, because seeding is hygiene and must never fail an init on
/// account of a temp-file hiccup.
fn write_preserving(path: &Path, content: &str) -> std::io::Result<()> {
    match temp_sibling(path) {
        Some(tmp) => {
            if std::fs::write(&tmp, content).is_ok() && std::fs::rename(&tmp, path).is_ok() {
                return Ok(());
            }
            let _ = std::fs::remove_file(&tmp);
            std::fs::write(path, content)
        }
        None => std::fs::write(path, content),
    }
}

/// The `<name>.seed-tmp` sibling of `path` — same directory, so renaming it
/// over `path` is atomic — or `None` when the path has no file name.
fn temp_sibling(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let mut tmp_name = name.to_os_string();
    tmp_name.push(".seed-tmp");
    Some(path.with_file_name(tmp_name))
}

/// The block written for `header` + `entries`: the header comment lines, a
/// blank separator, then the entries — every line terminated by `eol`.
///
/// The same renderer is used for a freshly created file and for an append, so
/// both shapes are identical (a seeded file and a merged-into one look the
/// same). An empty `header` yields just the entries; an empty `entries` (the
/// header-only half of an append) yields just the header + separator.
fn render_block(header: &str, entries: &[&str], eol: &str) -> String {
    let mut out = String::new();
    push_block(&mut out, header, &[], eol);
    if !out.is_empty() {
        out.push_str(eol);
    }
    push_block(&mut out, "", entries, eol);
    out
}

/// Append `header` (a `\n`-separated comment block) and `entries` to `out`,
/// each line terminated by `eol`.
fn push_block(out: &mut String, header: &str, entries: &[&str], eol: &str) {
    let header = header.trim_end_matches('\n');
    if !header.is_empty() {
        for line in header.lines() {
            out.push_str(line);
            out.push_str(eol);
        }
    }
    for entry in entries {
        out.push_str(entry);
        out.push_str(eol);
    }
}

/// Is the first line of `header` (its marker line) already in `text`?
fn header_present(text: &str, header: &str) -> bool {
    match header.lines().next() {
        Some(marker) => text.lines().any(|line| line.trim_end() == marker.trim_end()),
        None => true,
    }
}

/// Is `entry` already covered by a line of `text` (comparison rules:
/// [`ensure_managed_entries`])?
fn contains_entry(text: &str, entry: &str) -> bool {
    let want = normalize_entry(entry);
    !want.is_empty() && text.lines().any(|line| normalize_entry(line) == want)
}

/// Whitespace-trimmed line with a leading `/` stripped (git treats
/// `/.coding/memory.db` and `.coding/memory.db` as the same path pattern).
fn normalize_entry(line: &str) -> &str {
    line.trim().trim_start_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// How many lines of `text` cover `entry` (the normalized presence rule).
    fn count_entry(text: &str, entry: &str) -> usize {
        let want = normalize_entry(entry);
        text.lines()
            .filter(|line| normalize_entry(line) == want)
            .count()
    }

    #[test]
    fn seed_git_files_creates_both_files_with_every_managed_entry() {
        let dir = tempdir().unwrap();
        seed_git_files(dir.path()).unwrap();

        let ignore = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        let attrs = std::fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
        for entry in MANAGED_GITIGNORE_ENTRIES {
            assert_eq!(count_entry(&ignore, entry), 1, "{entry} in .gitignore");
        }
        for entry in MANAGED_GITATTRIBUTES_ENTRIES {
            assert_eq!(count_entry(&attrs, entry), 1, "{entry} in .gitattributes");
        }
        // Both fresh files carry the explanatory header, and both are LF (the
        // app pins new files to LF — this repo's own .gitattributes does the
        // same repo-wide).
        assert!(ignore.contains("# ===== Agent state (.coding/) ====="));
        assert!(attrs.contains("# ===== Agent state (.coding/) ====="));
        assert!(!ignore.contains('\r'), "fresh .gitignore is LF");
        assert!(!attrs.contains('\r'), "fresh .gitattributes is LF");
    }

    #[test]
    fn merge_keeps_user_lines_and_appends_only_what_is_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".gitignore");
        // A user file: their rules, their comment, one managed entry in the
        // equivalent leading-slash form, and a COMMENTED-OUT managed rule that
        // must not count as coverage.
        let user = "# my rules\ntarget/\n/.coding/memory.db\n#.coding/codegraph.db\n";
        std::fs::write(&path, user).unwrap();

        assert!(ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap());

        let merged = std::fs::read_to_string(&path).unwrap();
        assert!(
            merged.starts_with(user),
            "existing bytes stay the file's prefix (never reordered or rewritten):
{merged}"
        );
        assert_eq!(
            count_entry(&merged, ".coding/memory.db"),
            1,
            "the leading-slash form already covered it — not appended twice"
        );
        assert_eq!(
            count_entry(&merged, ".coding/codegraph.db"),
            1,
            "a commented-out rule is not coverage — the live entry is appended"
        );
        for entry in MANAGED_GITIGNORE_ENTRIES {
            assert_eq!(count_entry(&merged, entry), 1, "{entry}");
        }
        assert!(merged.contains("# my rules"), "user comment kept");
        assert!(merged.contains("#.coding/codegraph.db"), "user line kept");
    }

    #[test]
    fn merge_preserves_crlf_style() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".gitignore");
        let user = "target/\r\nnode_modules/\r\n";
        std::fs::write(&path, user).unwrap();

        assert!(ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap());

        let merged = std::fs::read_to_string(&path).unwrap();
        assert!(merged.starts_with(user), "user CRLF lines untouched");
        let appended = &merged[user.len()..];
        assert_eq!(
            appended.matches('\n').count(),
            appended.matches("\r\n").count(),
            "every appended line ends CRLF — no bare LF introduced:\n{appended:?}"
        );
        for entry in MANAGED_GITIGNORE_ENTRIES {
            assert_eq!(count_entry(&merged, entry), 1, "{entry}");
        }
    }

    #[test]
    fn merge_terminates_the_last_line_before_appending() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".gitignore");
        std::fs::write(&path, "*.log").unwrap(); // no trailing newline

        assert!(ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap());

        let merged = std::fs::read_to_string(&path).unwrap();
        assert!(
            merged.starts_with("*.log\n"),
            "the unterminated user line is closed first: {merged:?}"
        );
        assert_eq!(count_entry(&merged, ".coding/memory.db"), 1);
    }

    #[test]
    fn a_fully_covered_file_is_left_byte_identical() {
        let dir = tempdir().unwrap();
        seed_git_files(dir.path()).unwrap();
        let ignore_path = dir.path().join(".gitignore");
        let attrs_path = dir.path().join(".gitattributes");
        let ignore = std::fs::read_to_string(&ignore_path).unwrap();
        let attrs = std::fs::read_to_string(&attrs_path).unwrap();

        assert!(
            !ensure_managed_entries(&ignore_path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES)
                .unwrap(),
            "a fully covered file reports no write"
        );
        assert!(
            !ensure_managed_entries(&attrs_path, GITATTRIBUTES_HEADER, MANAGED_GITATTRIBUTES_ENTRIES)
                .unwrap(),
            "a fully covered .gitattributes reports no write"
        );
        seed_git_files(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(&ignore_path).unwrap(),
            ignore,
            "re-seeding is byte-identical"
        );
        assert_eq!(
            std::fs::read_to_string(&attrs_path).unwrap(),
            attrs,
            "re-seeding is byte-identical"
        );
    }

    #[test]
    fn self_heals_a_deleted_entry_or_a_deleted_file() {
        let dir = tempdir().unwrap();
        seed_git_files(dir.path()).unwrap();
        let path = dir.path().join(".gitignore");

        // One entry removed (the header stays): the next seeding restores
        // exactly that line without duplicating the header.
        let original = std::fs::read_to_string(&path).unwrap();
        let trimmed: String = original
            .lines()
            .filter(|line| *line != ".coding/plans/stack.json")
            .map(|line| format!("{line}\n"))
            .collect();
        std::fs::write(&path, trimmed).unwrap();

        seed_git_files(dir.path()).unwrap();
        let repaired = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            count_entry(&repaired, ".coding/plans/stack.json"),
            1,
            "the deleted entry self-healed"
        );
        assert_eq!(
            repaired
                .matches("# ===== Agent state (.coding/) =====")
                .count(),
            1,
            "the repair does not duplicate the header"
        );

        // The whole file deleted: recreated with the full block.
        std::fs::remove_file(&path).unwrap();
        seed_git_files(dir.path()).unwrap();
        let recreated = std::fs::read_to_string(&path).unwrap();
        for entry in MANAGED_GITIGNORE_ENTRIES {
            assert_eq!(count_entry(&recreated, entry), 1, "{entry}");
        }
    }

    #[test]
    fn repo_own_git_files_carry_every_managed_entry() {
        // The app repo must obey what it seeds: an entry dropped from this
        // repo's own .gitignore/.gitattributes (or added to a managed list
        // without the repo following suit) means the seeding contract and this
        // repo's real hygiene have drifted apart.
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
        let attrs = std::fs::read_to_string(root.join(".gitattributes")).unwrap();
        for entry in MANAGED_GITIGNORE_ENTRIES {
            assert_eq!(count_entry(&ignore, entry), 1, "repo .gitignore covers {entry}");
        }
        for entry in MANAGED_GITATTRIBUTES_ENTRIES {
            assert_eq!(count_entry(&attrs, entry), 1, "repo .gitattributes covers {entry}");
        }
    }

    #[test]
    fn a_non_utf8_git_file_is_skipped_untouched() {
        // Git's ignore/attribute files are byte-oriented: a stray Latin-1 byte
        // is legal, and it must not become an init failure — the file is
        // skipped, never rewritten.
        let dir = tempdir().unwrap();
        let path = dir.path().join(".gitignore");
        let original = b"# caf\xe9 latin-1 comment\ntarget/\n".to_vec();
        std::fs::write(&path, &original).unwrap();

        assert!(
            !ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap(),
            "a non-UTF-8 file reports no write"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the skipped file is byte-untouched"
        );
    }

    #[test]
    fn an_empty_existing_git_file_gets_the_full_block() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".gitignore");
        std::fs::write(&path, "").unwrap();

        assert!(ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap());

        let seeded = std::fs::read_to_string(&path).unwrap();
        assert!(
            seeded.starts_with("# ===== Agent state (.coding/) ====="),
            "the header leads — no blank line before it: {seeded:?}"
        );
        for entry in MANAGED_GITIGNORE_ENTRIES {
            assert_eq!(count_entry(&seeded, entry), 1, "{entry}");
        }
    }

    #[test]
    fn the_merge_leaves_no_temp_sibling_behind() {
        // The preserving write goes through a `<name>.seed-tmp` sibling + a
        // rename; the temp file must never survive the merge (nor a covered
        // re-run, which writes nothing at all).
        let dir = tempdir().unwrap();
        let path = dir.path().join(".gitignore");
        std::fs::write(&path, "target/\n").unwrap();
        assert!(ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap());
        assert!(!ensure_managed_entries(&path, GITIGNORE_HEADER, MANAGED_GITIGNORE_ENTRIES).unwrap());

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![".gitignore".to_string()], "no temp litter: {names:?}");
    }
}
