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

/// One minted read receipt: the sandbox-validated path, the 1-indexed
/// first line served, and a hash per served line (terminator-stripped —
/// EOL-agnostic, mirroring the anchor path's EOL-agnostic matching).
#[derive(Clone)]
struct ReadReceipt {
    path: PathBuf,
    first_line: usize,
    line_hashes: Vec<u64>,
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
            seq,
        },
    );
    if reg.receipts.len() > MAX_RECEIPTS {
        prune_oldest(&mut reg);
    }
    id
}

/// Revoke a receipt. The delta-mode reduced serves (skeleton/diff
/// re-renders) call this with the just-minted id: the model never saw
/// those lines verbatim, so the full-serve receipt must not stay
/// anchorable. Revoking an unknown or empty id is a no-op.
pub fn revoke(id: &str) {
    if id.is_empty() {
        return;
    }
    REGISTRY
        .lock()
        .expect("read registry lock poisoned")
        .receipts
        .remove(id);
}/// Resolve a from_read anchor against the CURRENT file bytes — the
/// server-side validation that replaces the verbatim old_string.
///
/// Checks, in order: the id exists (drift class — the fresh-read
/// steer); the receipt's path is the edit's path; the range is not
/// reversed and `[start, end]` lies within the lines the read actually
/// served (a usage error steering to read the region); and every
/// current line in the range still hashes to what the read served
/// (drift class — NEVER a fuzzy apply).
pub fn resolve(
    id: &str,
    path: &Path,
    start: usize,
    end: usize,
    content: &str,
) -> crate::error::Result<()> {
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
    let lines: Vec<&str> = content.lines().collect();
    if end > lines.len() {
        return Err(with_fresh_read_nudge(format!(
            "lines [{start}, {end}] reach past the file's current end ({} lines) — it changed \
             since read '{id}'",
            lines.len()
        )));
    }
    for (idx, current) in lines.iter().enumerate().take(end).skip(start - 1) {
        let receipt_idx = idx + 1 - receipt.first_line;
        if hash_line(current) != receipt.line_hashes[receipt_idx] {
            return Err(with_fresh_read_nudge(format!(
                "line {} changed since read '{id}' — the file was modified after the read that \
                 minted the receipt",
                idx + 1
            )));
        }
    }
    Ok(())
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
}