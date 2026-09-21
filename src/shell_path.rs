// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Adoption of the login shell's `PATH` (Unix) — the pure decision logic.
//!
//! macOS GUI apps launched from Finder/Dock do NOT inherit the user's shell
//! environment (no `.zshrc`/`.zprofile`), so `PATH` stays the bare system
//! default (`/usr/bin:/bin:/usr/sbin:/sbin`) and the agent's `sh -c` tool
//! cannot find `cargo`/`npm`/`node`/Homebrew `git` inside a shipped `.app`.
//! `mnemo-app` therefore runs the user's login shell once at startup and
//! adopts its resolved `PATH`.
//!
//! The *decision* — may this captured value be adopted? — lives here, in
//! platform-neutral library code, so it is unit-testable on every platform.
//! The `#[cfg(unix)]` capture/spawn side lives in `mnemo-app`'s `main.rs`.
//! (This split exists because the predicate was once `#[cfg(unix)]`-only,
//! which hid a whitespace-only regression from the Windows leg entirely —
//! the macOS CI failure of 2026-09-21, run 35575972400.)

/// Whether a captured login-shell `PATH` value is safe to adopt.
///
/// Accepts any non-blank value free of control characters. Spaces are LEGAL:
/// PATH entries like `/Applications/Some App/bin` must not be rejected (review
/// F5) — a blanket whitespace rejection silently kept the bare GUI PATH, the
/// exact failure this exists to fix. A whitespace-only value is not a usable
/// PATH, however, so it is rejected.
///
/// Control characters are rejected because a dotfile that echoes to stdout
/// pollutes the captured value (the rc output shares stdout with the `printf`)
/// — a newline in the value means exactly that.
///
/// ```
/// use mnemo::shell_path::is_adoptable_path;
/// assert!(is_adoptable_path("/Applications/Some App/bin:/usr/bin"));
/// assert!(!is_adoptable_path("   "));
/// assert!(!is_adoptable_path("/usr/bin\n/usr/local/bin"));
/// ```
pub fn is_adoptable_path(value: &str) -> bool {
    !value.trim().is_empty() && !value.chars().any(|c| c.is_control())
}

#[cfg(test)]
mod tests {
    use super::is_adoptable_path;

    /// Regression (review F5): a legal PATH containing spaces (e.g.
    /// `/Applications/Some App/bin`) must be adoptable — the pre-fix check
    /// rejected ANY whitespace and silently kept the bare GUI PATH, the exact
    /// failure `inherit_shell_path` exists to fix. The fix must never
    /// reappear.
    #[test]
    fn adoptable_path_allows_spaces() {
        assert!(is_adoptable_path("/usr/bin:/bin:/Applications/Some App/bin"));
        assert!(is_adoptable_path("/usr/local/bin"));
    }

    /// Empty values and control characters (a rc file echoing into the
    /// captured stdout shows up as newlines/garbage) must still be rejected.
    /// A whitespace-only value is not a usable PATH either — this assertion
    /// was the macOS CI failure of 2026-09-21 (run 35575972400): the predicate
    /// checked only `is_empty()`, so `"   "` slipped through. It lives here,
    /// platform-neutral, so the Windows leg guards it too.
    #[test]
    fn adoptable_path_rejects_empty_and_control_characters() {
        assert!(!is_adoptable_path(""));
        assert!(!is_adoptable_path("   "));
        assert!(!is_adoptable_path("\t"));
        assert!(!is_adoptable_path("\n"));
        assert!(!is_adoptable_path("/usr/bin\n/usr/local/bin"));
        assert!(!is_adoptable_path("/usr/bin\u{7}:/bin"));
        assert!(!is_adoptable_path("/usr/bin\t/bin"));
    }

    /// The trimmed-empty guard must not become a blanket whitespace rejection:
    /// a real PATH whose entries merely CONTAIN spaces stays adoptable.
    #[test]
    fn adoptable_path_keeps_internal_spaces() {
        assert!(is_adoptable_path(" /usr/local/bin"));
        assert!(is_adoptable_path("/Applications/Some App/bin:/usr/bin"));
    }
}
