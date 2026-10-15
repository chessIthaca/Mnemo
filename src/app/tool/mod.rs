// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The app-side tool modules (job 2b): the workflow tooling relocated out of
//! the kernel's `crate::tool` tree. 2b.3 severed the kernel-side re-export —
//! `crate::tool::workflow` no longer resolves; kernel consumers reference the
//! app path directly (tests) or route through the handle trait (production).

pub mod workflow;