// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The scaffold RE-EXPORTS (job 4 D8, the 2b pattern): the core moved to
//! [`crate::project::scaffold`] so the kernel's `Setup::init_project`
//! needs no `mnemo::app` import — the app's `Project::init` (and its
//! tests) keep their `crate::app::scaffold::…` paths unchanged.

pub use crate::project::scaffold::{
    ensure_managed_entries, seed_git_files, MANAGED_GITATTRIBUTES_ENTRIES,
    MANAGED_GITIGNORE_ENTRIES,
};
