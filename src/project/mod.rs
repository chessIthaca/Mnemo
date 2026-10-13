// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel's project pieces (2c.3/G3): the constitution loader
//! ([`agent_md`]) and the git-ops helpers ([`git_ops`], consumed by the
//! kernel's review scope, the git tool, and the memory finish-capture). The
//! APP's project model (the `Project` type + the discovery + the registry
//! + the init + the scaffold) moved to [`crate::app::project`], with the
//! run-all worktree helpers app-side next to it; the
//! kernel's state-dir primitive is [`crate::workspace::Workspace`] (§5.9.3).
//! The shipped-skill catalog stays kernel-owned (job 2b decision 5): the
//! app's init seeds it, app→kernel (the legal direction).
//!
//! Job 4 D8 (§5.8 :556) amended the old "the kernel never scaffolds"
//! letter: `Setup::init_project` is a KERNEL surface, so the scaffold CORE
//! (the git-file seeding) moved here ([`scaffold`]) — the app's
//! `src/app/scaffold.rs` keeps re-exports and its `Project::init` consumes
//! the kernel core (app→kernel, the legal direction). The `Project`
//! model + discovery + `seed_stores` stay app-side.

pub mod agent_md;
pub mod git_ops;
pub mod scaffold;
pub mod skills_seed;

pub use agent_md::{Constitution, ConstitutionSource};