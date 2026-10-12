// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The app-side tool modules (job 2b): the workflow tooling relocated out of
//! the kernel's `crate::tool` tree. The kernel keeps `crate::tool::workflow`
//! resolving via a re-export alias until 2b.3 severs it.

pub mod workflow;