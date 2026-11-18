// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Read receipts — the read-side half of the anchor-by-read-reference
//! edit form (backlog 3fdc605f, plan 5a21fc8f).
//!
//! `read_files` mints a receipt for every successful file read: the
//! sandbox-validated path, the first line number served, and a u64 hash
//! per served line. The id rides the read section's trailer
//! (`[read receipt rX — cite file_edit {from_read: "rX", lines: [s,e],
//! new_string}]`), and `file_edit`/`multi_edit` accept
//! `{from_read: "<id>", lines: [start, end], new_string}` anchors that
//! resolve server-side against the CURRENT file bytes — the verbatim
//! anchor blob never needs to live in the model's context, so context
//! compaction is harmless to the edit path (the observed cost of the
//! old way: ~10 re-reads in one afternoon when compaction dropped the
//! anchors' bytes).
//!
//! Validation is deliberately strict — NEVER a fuzzy apply: the id must
//! exist, the receipt's path must be the edit's path, the cited range
//! must lie inside the lines the read actually served, and every
//! current line in the range must still hash to what the read served.
//! Any miss is a hard fail carrying the fresh-read steering
//! (`with_fresh_read_nudge`, the same drift class as an old_string
//! miss, so the dispatch-level stale-read gate arms on it too).
//!
//! A cite names lines in the CITED READ's numbering, so a write that
//! changes the line count ABOVE the cited range must not invalidate it
//! (plan f89fd63d step 2 — observed: workers lost turns to "line 180
//! changed since read" right after their own earlier edit). Every
//! successful file-tool write reports itself through [`note_write`]
//! (or [`note_append`] for the streaming append), which records the
//! line shift on every live receipt of that path; [`resolve`] then
//! translates the cited range through those shifts before the hash
//! check, and refuses a range one of those writes rewrote. Exact
//! bookkeeping, not fuzz: a line the read served must still BE the
//! line at the translated position — a write outside the file tools
//! (a shell command, `cargo fmt`) is never recorded and still drifts.
//!
//! The registry is process-wide rather than per-agent (unlike
//! [`super::read_cache::ReadCache`]): the minting tool and the
//! consuming tools are separate instances, and a receipt is
//! self-defending — a stale receipt simply fails its hash check, so
//! cross-agent visibility is harmless. Per-line hashes keep a receipt
//! small (<= 500 u64s ~ 4 KB); the registry keeps the most recent
//! [`MAX_RECEIPTS`] receipts (prune-oldest by the id-embedded
//! sequence). `DefaultHasher` is deterministic within one process
//! build, which is all a receipt needs — ids never cross processes.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use super::edit_ops::with_fresh_read_nudge;

/// Maximum receipts kept. When exceeded, the oldest (by id sequence) are
/// pruned — a receipt costs ~4 KB worst case, so the cap bounds the
/// registry at ~2 MB.
const MAX_RECEIPTS: usize = 512;

/// Maximum line shifts tracked per receipt. Past this the receipt is
/// DROPPED instead (a later cite then gets the unknown-receipt drift
/// error): a receipt that has survived 64 writes to its file is far from
/// the read that minted it, and a stale cite is better steered to a fresh
/// read than silently translated through a long, wrong chain.
const MAX_SHIFTS: usize = 64;

/// One file-tool write, in the 1-indexed line coordinates of the file JUST
/// BEFORE that write: lines `start..=old_end` (`old_end = start - 1` for a
/// pure insertion) were replaced by `new_len` lines. Recorded on every live
/// receipt of the written path so a later cite, whose numbers stay in the
/// read's own coordinates, is translated to where its lines ARE now.
#[derive(Clone, Debug)]
struct LineShift {
    start: usize,
    old_end: usize,
    new_len: usize,
}

/// One minted read receipt: the sandbox-validated path, the 1-indexed
/// first line served, a hash per served line (terminator-stripped —
/// EOL-agnostic, mirroring the anchor path's EOL-agnostic matching), and
/// the file-tool writes recorded since the read.
#[derive(Clone)]
struct ReadReceipt {
    path: PathBuf,
    first_line: usize,
    line_hashes: Vec<u64>,
    /// The writes to `path` since this read, oldest first (plan f89fd63d
    /// step 2) — the translation chain for a cite that still names lines
    /// in this read's numbering.
    shifts: Vec<LineShift>,
    /// The id's embedded sequence (prune-oldest orders on this).
    seq: u64,
}

/// The process-wide registry state.
struct Registry {
    receipts: HashMap<String, ReadReceipt>,
    next_seq: u64,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        receipts: HashMap::new(),
        next_seq: 1,
    })
});

/// Mint a receipt for `lines` (terminator-stripped) of `path`, served
/// starting at 1-indexed `first_line`. Returns the short receipt id
/// ("r" + base36 sequence) that rides the read section's trailer.
pub fn mint(path: PathBuf, first_line: usize, lines: &[&str]) -> String {
    let mut reg = REGISTRY.lock().expect("read registry lock poisoned");
    let seq = reg.next_seq;
    reg.next_seq += 1;
    let id = format!("r{}", to_base36(seq));
    reg.receipts.insert(
        id.clone(),
        ReadReceipt {
            path,
            first_line,
            line_hashes: lines.iter().map(|l| hash_line(l)).collect(),
            shifts: Vec::new(),
            seq,
        },
    );
    if reg.receipts.len() > MAX_RECEIPTS {
        prune_oldest(&mut reg);
    }
    id
}

/// Revoke a receipt. The outline serve calls this with the just-minted id:
/// the model never saw those lines verbatim, so the full-serve receipt must
/// not stay anchorable. Revoking an unknown or empty id is a no-op.
pub fn revoke(id: &str) {
    if id.is_empty() {
        return;
    }
    REGISTRY
        .lock()
        .expect("read registry lock poisoned")
        .receipts
        .remove(id);
}

/// Resolve a from_read anchor against the CURRENT file bytes — the
/// server-side validation that replaces the verbatim old_string.
///
/// Checks, in order: the id exists (drift class — the fresh-read
/// steer); the receipt's path is the edit's path; the range is not
/// reversed and `[start, end]` lies within the lines the read actually
/// served (a usage error steering to read the region); the range is then
/// TRANSLATED from the read's coordinates into the current file's through
/// every file-tool write recorded since the read (a range one of those
/// writes rewrote can never be applied — drift class); and every current
/// line at the translated position still hashes to what the read served
/// (drift class — NEVER a fuzzy apply).
///
/// Returns the translated `(start, end)`: the lines to edit NOW to reach
/// the lines the read served (plan f89fd63d step 2 — a cite above an
/// edit elsewhere in the file no longer fails its hash check).
pub fn resolve(
    id: &str,
    path: &Path,
    start: usize,
    end: usize,
    content: &str,
) -> crate::error::Result<(usize, usize)> {
    if end < start {
        return Err(crate::error::Error::InvalidInput(format!(
            "lines [{start}, {end}] is a reversed range — [start, end] with end >= start, \
             1-indexed inclusive"
        )));
    }
    let receipt = REGISTRY
        .lock()
        .expect("read registry lock poisoned")
        .receipts
        .get(id)
        .cloned();
    let Some(receipt) = receipt else {
        return Err(with_fresh_read_nudge(format!(
            "unknown read receipt '{id}' — it is not in the registry (evicted by newer reads, \
             a new process, or never read)"
        )));
    };
    if receipt.path != path {
        return Err(with_fresh_read_nudge(format!(
            "read receipt '{id}' belongs to {}, not {} — cite the receipt a read of THIS \
             file minted",
            receipt.path.display(),
            path.display()
        )));
    }
    let last_served = receipt.first_line + receipt.line_hashes.len().saturating_sub(1);
    if start < receipt.first_line || end > last_served {
        return Err(crate::error::Error::InvalidInput(format!(
            "lines [{start}, {end}] were not in read '{id}'s covered range (lines {}-{}) — \
             read the region first, then cite it",
            receipt.first_line, last_served
        )));
    }
    // Translate the cite through the writes recorded since the read
    // (plan f89fd63d step 2). The shifts are ordered oldest first and each
    // is expressed in the coordinates of the file JUST BEFORE its own
    // write, so walking them in order keeps the running range in the
    // coordinates of the file as of that write.
    let (mut cur_start, mut cur_end) = (start, end);
    for shift in &receipt.shifts {
        if cur_end < shift.start {
            // Wholly above the written lines — untouched.
            continue;
        }
        if cur_start > shift.old_end {
            // Wholly below them — moves with them.
            let old_count = shift.old_end + 1 - shift.start;
            let delta = shift.new_len as isize - old_count as isize;
            cur_start = (cur_start as isize + delta) as usize;
            cur_end = (cur_end as isize + delta) as usize;
            continue;
        }
        // The range overlaps what that write replaced (or has lines
        // inserted strictly inside it): its lines are GONE, so no
        // translation can land on them. Drift class — the cited bytes no
        // longer exist in the file.
        let what = if shift.old_end < shift.start {
            format!("lines were inserted after line {} at the time", shift.old_end)
        } else {
            format!("lines {}-{} at the time", shift.start, shift.old_end)
        };
        return Err(with_fresh_read_nudge(format!(
            "lines [{start}, {end}] of read '{id}' were rewritten by an edit since that read \
             ({what})"
        )));
    }
    let lines: Vec<&str> = content.lines().collect();
    if cur_start == 0 || cur_end > lines.len() {
        return Err(with_fresh_read_nudge(format!(
            "lines [{cur_start}, {cur_end}] reach past the file's current end ({} lines) — it \
             changed since read '{id}'",
            lines.len()
        )));
    }
    for idx in start..=end {
        let receipt_idx = idx - receipt.first_line;
        let current_idx = cur_start + (idx - start);
        let Some(current) = lines.get(current_idx - 1) else {
            return Err(with_fresh_read_nudge(format!(
                "line {current_idx} is past the file's current end ({} lines) — it changed \
                 since read '{id}'",
                lines.len()
            )));
        };
        if hash_line(current) != receipt.line_hashes[receipt_idx] {
            return Err(with_fresh_read_nudge(format!(
                "line {current_idx} changed since read '{id}' — the file was modified after the \
                 read that minted the receipt"
            )));
        }
    }
    Ok((cur_start, cur_end))
}

/// Does any live receipt cover `path`? The file tools use this to skip the
/// pre-write bookkeeping entirely when nothing could ever cite the file.
pub fn tracks(path: &Path) -> bool {
    REGISTRY
        .lock()
        .expect("read registry lock poisoned")
        .receipts
        .values()
        .any(|r| r.path == path)
}

/// Record one successful file-tool write against every receipt minted for
/// `path`, so a later cite in that read's own line numbers still lands on
/// the lines the read served (plan f89fd63d step 2). `old` is the content as
/// read immediately before the write, `new` the content written; the
/// translation is derived from their common prefix and suffix lines, so it
/// is exact bookkeeping — never a fuzzy match (the hash check still decides
/// whether a cited line is the one the read served).
///
/// The one file tool that does NOT call this is `file_append` (documented
/// for huge files, so it must not read the whole existing file): it calls
/// [`note_append`] instead, which describes the same shift from a line
/// count. A write outside the file tools (a shell command, `cargo fmt`) is
/// not recorded, so a cite over rewritten lines still fails its hash check.
pub fn note_write(path: &Path, old: &str, new: &str) {
    if old == new {
        return;
    }
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let prefix = common_prefix_lines(&old_lines, &new_lines);
    let suffix = common_suffix_lines(&old_lines, &new_lines, prefix);
    note_shift(
        path,
        LineShift {
            start: prefix + 1,
            old_end: old_lines.len() - suffix,
            new_len: new_lines.len() - prefix - suffix,
        },
    );
}

/// [`note_write`] for an append, which cannot afford to read the existing
/// file (plan f89fd63d step 2): `old_line_count` is the file's line count
/// before the write, `old_ends_with_newline` whether it was terminated by a
/// newline, and `appended` the text written at its end. An append either
/// INSERTS lines at the end (a terminated file) or rewrites its last line
/// and then adds lines (an unterminated one).
pub fn note_append(
    path: &Path,
    old_line_count: usize,
    old_ends_with_newline: bool,
    appended: &str,
) {
    if appended.is_empty() {
        return;
    }
    let appended_lines = appended.lines().count();
    if old_ends_with_newline || old_line_count == 0 {
        note_shift(
            path,
            LineShift {
                start: old_line_count + 1,
                old_end: old_line_count,
                new_len: appended_lines,
            },
        );
    } else {
        note_shift(
            path,
            LineShift {
                start: old_line_count,
                old_end: old_line_count,
                new_len: appended_lines,
            },
        );
    }
}

/// Push one shift onto every live receipt of `path`, dropping the receipts
/// that hit [`MAX_SHIFTS`] (a later cite then gets the unknown-receipt
/// drift error and re-reads). A shift that replaces zero lines with zero
/// lines — an edit that only rewrote line endings in place, e.g.
/// `convert_line_endings` — records nothing.
fn note_shift(path: &Path, shift: LineShift) {
    if shift.old_end + 1 == shift.start && shift.new_len == 0 {
        return;
    }
    let mut reg = REGISTRY.lock().expect("read registry lock poisoned");
    let mut dropped: Vec<String> = Vec::new();
    for (id, receipt) in reg.receipts.iter_mut() {
        if receipt.path != path {
            continue;
        }
        if receipt.shifts.len() >= MAX_SHIFTS {
            dropped.push(id.clone());
            continue;
        }
        receipt.shifts.push(shift.clone());
    }
    for id in dropped {
        reg.receipts.remove(&id);
    }
}

/// Common prefix length of two line arrays, comparing EOL-agnostically.
fn common_prefix_lines(old: &[&str], new: &[&str]) -> usize {
    let mut prefix = 0;
    while prefix < old.len() && prefix < new.len() && same_line(old[prefix], new[prefix]) {
        prefix += 1;
    }
    prefix
}

/// Common suffix length of two line arrays, comparing EOL-agnostically and
/// capped so the suffix never overlaps the known common prefix.
fn common_suffix_lines(old: &[&str], new: &[&str], prefix: usize) -> usize {
    let cap = old.len().min(new.len()) - prefix;
    let mut suffix = 0;
    while suffix < cap && same_line(old[old.len() - 1 - suffix], new[new.len() - 1 - suffix]) {
        suffix += 1;
    }
    suffix
}

/// Compare two lines ignoring a trailing carriage return: `str::lines`
/// already strips the `\r` of a `\r\n` terminator, and stripping a lone one
/// keeps a CRLF↔LF conversion from looking like a whole-file rewrite.
fn same_line(a: &str, b: &str) -> bool {
    strip_trailing_cr(a) == strip_trailing_cr(b)
}

/// `line` without one trailing carriage return, if it has one.
fn strip_trailing_cr(line: &str) -> &str {
    line.strip_suffix('\r').unwrap_or(line)
}

/// Drop the oldest receipts until the cap holds (prune-oldest by the
/// id-embedded sequence).
fn prune_oldest(reg: &mut Registry) {
    while reg.receipts.len() > MAX_RECEIPTS {
        let Some(seq) = reg.receipts.values().map(|r| r.seq).min() else {
            return;
        };
        let Some(id) = reg
            .receipts
            .iter()
            .find(|(_, r)| r.seq == seq)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        reg.receipts.remove(&id);
    }
}

/// Hash one line's bytes (terminator-stripped — the read served it that
/// way, so CRLF vs LF never mismatches).
fn hash_line(line: &str) -> u64 {
    let mut h = DefaultHasher::new();
    line.hash(&mut h);
    h.finish()
}

/// Base-36 (0-9 then a-z) — short, monotonic, collision-free ids.
fn to_base36(mut n: u64) -> String {
    if n == 0 {
        return "0".into();
    }
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(ALPHABET[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 is ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_resolve_roundtrip_drift_and_steering() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let id = mint(path.clone(), 1, &["a", "b", "c"]);
        assert!(id.starts_with('r') && id.len() > 1, "{id}");
        // Unchanged content resolves — the whole range or a subset.
        assert!(resolve(&id, &path, 1, 3, "a\nb\nc\n").is_ok());
        assert!(resolve(&id, &path, 2, 2, "a\nb\nc\n").is_ok());
        // Drift hard-fails with the fresh-read steering, never fuzzy.
        let err = resolve(&id, &path, 2, 2, "a\nZZ\nc\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(crate::agent::steering_stats::EDIT_STALE_READ_MARK),
            "{msg}"
        );
        // A cite outside the covered range is a usage error steering to
        // read the region first.
        let err = resolve(&id, &path, 4, 5, "a\nb\nc\nd\ne\n").unwrap_err();
        assert!(err.to_string().contains("read the region first"), "{err}");
        // A reversed range is rejected before any lookup.
        let err = resolve(&id, &path, 3, 1, "a\nb\nc\n").unwrap_err();
        assert!(err.to_string().contains("reversed range"), "{err}");
        // An unknown id is named and steers to a fresh read.
        let err = resolve("rnope", &path, 1, 1, "a\nb\nc\n").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("rnope")
                && msg.contains(crate::agent::steering_stats::EDIT_STALE_READ_MARK),
            "{msg}"
        );
        // A receipt does not transfer to another path.
        let other = dir.path().join("b.txt");
        let err = resolve(&id, &other, 1, 1, "a\nb\nc\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("b.txt") && msg.contains("a.txt"), "{msg}");
    }

    #[test]
    fn revoke_makes_the_receipt_unresolvable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.txt");
        let id = mint(path.clone(), 1, &["one"]);
        assert!(resolve(&id, &path, 1, 1, "one\n").is_ok());
        revoke(&id);
        assert!(resolve(&id, &path, 1, 1, "one\n").is_err());
    }

    #[test]
    fn the_cap_evicts_the_oldest_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let oldest = mint(dir.path().join("f.txt"), 1, &["l"]);
        for i in 0..MAX_RECEIPTS {
            mint(dir.path().join(format!("g{i}.txt")), 1, &["l"]);
        }
        // MAX_RECEIPTS newer mints pushed `oldest` past the cap.
        assert!(
            resolve(&oldest, &dir.path().join("f.txt"), 1, 1, "l\n").is_err(),
            "the oldest receipt must be evicted under the cap"
        );
        let fresh = mint(dir.path().join("n.txt"), 1, &["l"]);
        assert!(resolve(&fresh, &dir.path().join("n.txt"), 1, 1, "l\n").is_ok());
    }

    #[test]
    fn cite_maps_through_two_successive_edits() {
        // Plan f89fd63d step 2: shifts are recorded in the coordinates of
        // the file JUST BEFORE each write, so a cite must walk the chain in
        // order — the second write's coordinates already include the first
        // write's lines.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let original = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";
        let served = ["l1", "l2", "l3", "l4", "l5", "l6", "l7", "l8", "l9", "l10"];
        let id = mint(path.clone(), 1, &served);
        // Write 1 inserts two lines above the read's line 3.
        let after_first = "l1\nl2\nX1\nX2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\n";
        note_write(&path, original, after_first);
        // Write 2 inserts three lines above the read's line 6 — line 8 in
        // this write's (post-write-1) coordinates.
        let after_second = "l1\nl2\nX1\nX2\nl3\nl4\nl5\nY1\nY2\nY3\nl6\nl7\nl8\nl9\nl10\n";
        note_write(&path, after_first, after_second);
        // The read served 'l8' as line 8; it sits at line 13 now.
        assert_eq!(resolve(&id, &path, 8, 8, after_second).unwrap(), (13, 13));
        // 'l4' was line 4: +2 from the first write, untouched by the second.
        assert_eq!(resolve(&id, &path, 4, 4, after_second).unwrap(), (6, 6));
        // The translation is bookkeeping only — the hash check still
        // refuses a cite over a line that is not what the read served.
        let tampered = after_second.replace("l9", "zz");
        let err = resolve(&id, &path, 9, 9, &tampered).unwrap_err();
        assert!(
            err.to_string().contains("line 14 changed since read"),
            "{err}"
        );
    }

    #[test]
    fn insertion_at_top_and_append_at_end_shift_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let id = mint(path.clone(), 1, &["l1", "l2", "l3"]);
        // A new first line moves every cited line down by one.
        let after_top = "TOP\nl1\nl2\nl3\n";
        note_write(&path, "l1\nl2\nl3\n", after_top);
        assert_eq!(resolve(&id, &path, 1, 1, after_top).unwrap(), (2, 2));
        assert_eq!(resolve(&id, &path, 3, 3, after_top).unwrap(), (4, 4));
        // An append at the end leaves every cited line where it is.
        let after_bottom = "TOP\nl1\nl2\nl3\nEND\n";
        note_write(&path, after_top, after_bottom);
        assert_eq!(resolve(&id, &path, 1, 2, after_bottom).unwrap(), (2, 3));
        assert_eq!(resolve(&id, &path, 3, 3, after_bottom).unwrap(), (4, 4));
    }

    #[test]
    fn shift_cap_drops_the_receipt() {
        // 64 writes keep translating; the 65th drops the receipt, so a cite
        // gets the unknown-receipt steering (a fresh read) rather than a
        // translation through a chain that is no longer trustworthy.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        let id = mint(path.clone(), 1, &["l1", "l2"]);
        let mut body = String::from("l1\nl2\n");
        for i in 0..MAX_SHIFTS {
            let next = format!("l1\nl2\n{i}\n");
            note_write(&path, &body, &next);
            body = next;
        }
        assert!(resolve(&id, &path, 1, 1, &body).is_ok());
        note_write(&path, &body, "l1\nl2\nlast\n");
        let err = resolve(&id, &path, 1, 1, "l1\nl2\nlast\n").unwrap_err();
        assert!(err.to_string().contains("unknown read receipt"), "{err}");
    }
}