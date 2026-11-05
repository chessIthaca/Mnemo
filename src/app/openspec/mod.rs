// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! App-side access to Mnemo's single kernel-owned OpenSpec format layer.

pub(crate) mod archive;
mod capability_spec;
pub mod schema;

pub use crate::openspec::*;
pub use capability_spec::read_capability_spec;
