// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel's project pieces (2c.3/G3): the constitution loader
//! ([`agent_md`]) and the git-ops helpers ([`git_ops`], consumed by the
//! kernel's review scope, the git tool, and the memory finish-capture). The
//! APP's project model (the `Project` type + the discovery + the registry +
//! the init + the scaffold) moved to [`crate::app::project`], with the
//! scaffold and the run-all worktree helpers app-side next to it; the
//! kernel's state-dir primitive is [`crate::workspace::Workspace`] (§5.9.3).
//! The kernel never scaffolds and never writes `.gitignore` /
//! `.gitattributes` (row 27's letter). The shipped-skill catalog stays
//! kernel-owned (job 2b decision 5): the app's init seeds it, app→kernel
//! (the legal direction).

pub mod agent_md;
pub mod git_ops;
pub mod skills_seed;

pub use agent_md::{Constitution, ConstitutionSource};