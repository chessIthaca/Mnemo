// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! OpenSpec current-state specifications, change deltas, and capability paths.
//!
//! This is Mnemo's single OpenSpec format layer. App-side workflow code uses
//! the same API through `crate::app::openspec`; the memory indexer uses this
//! kernel module directly.

mod capability;
mod delta;
mod spec;

pub use capability::{Capability, CapabilityError};
pub use delta::{
    apply, Delta, DeltaConflict, DeltaOperation, RemovedRequirement, RenamedRequirement,
};
pub use spec::{Requirement, Scenario, Spec, SpecError};
