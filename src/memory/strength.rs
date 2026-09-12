// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Strength scoring — recency + access frequency + reinforcement, with decay.
//!
//! Strong memories surface first; weak ones (old, rarely accessed, unconfirmed)
//! decay and become eviction candidates. This mirrors the Ebbinghaus forgetting
//! curve.

/// Compute the strength score for a memory.
///
/// - `recency`: seconds since last access (smaller = stronger)
/// - `access_count`: how many times this memory was retrieved
/// - `reinforcement`: how many sessions contributed (for semantic/procedural)
/// - `created_age`: seconds since creation (for decay baseline)
///
/// Returns a score in [0, 1+]. Higher = stronger.
pub fn strength(
    recency_secs: i64,
    access_count: u32,
    reinforcement: u32,
    created_age_secs: i64,
) -> f64 {
    // Recency component: exponential decay (half-life ~7 days).
    // The more recently accessed, the higher this component.
    let half_life_secs: f64 = 7.0 * 24.0 * 60.0 * 60.0; // 7 days
    let recency_score = 0.5_f64.powf(recency_secs as f64 / half_life_secs);

    // Access frequency: log-scaled so the first few accesses matter most.
    let access_score = (1.0 + access_count as f64).ln();

    // Reinforcement: linear, capped.
    let reinforcement_score = (reinforcement as f64).min(5.0) / 5.0;

    // Age decay: very old memories that were never accessed decay further.
    let age_half_life_secs: f64 = 30.0 * 24.0 * 60.0 * 60.0; // 30 days
    let age_decay = 0.5_f64.powf(created_age_secs as f64 / age_half_life_secs);

    // Weighted combination. Recency dominates (coding context is recent-heavy),
    // access and reinforcement break ties.
    let raw = 0.5 * recency_score
        + 0.3 * (access_score / (1.0 + access_score))
        + 0.2 * reinforcement_score;
    raw * (0.5 + 0.5 * age_decay) // age decay scales the whole thing down
}

/// Decay an existing strength score by a time delta (seconds since last computed).
/// This is the Ebbinghaus forgetting curve: S(t) = S0 * e^(-t/tau).
pub fn decay(strength: f64, elapsed_secs: i64) -> f64 {
    let tau: f64 = 7.0 * 24.0 * 60.0 * 60.0; // 7-day time constant
    decay_with_tau(strength, elapsed_secs, tau)
}

/// Decay with an explicit time constant (seconds) — the configurable form
/// used when the caller carries the live `[memory]` search config
/// (`decay_half_life_days`). `decay` is this with the historical 7-day tau.
pub fn decay_with_tau(strength: f64, elapsed_secs: i64, tau_secs: f64) -> f64 {
    strength * (-(elapsed_secs as f64) / tau_secs).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_memory_is_stronger_than_old() {
        let recent = strength(60, 1, 1, 60); // 1 min ago
        let old = strength(30 * 24 * 60 * 60, 1, 1, 30 * 24 * 60 * 60); // 30 days ago
        assert!(recent > old, "recent ({recent}) should be > old ({old})");
    }

    #[test]
    fn frequently_accessed_is_stronger() {
        let rare = strength(60, 0, 1, 60);
        let frequent = strength(60, 10, 1, 60);
        assert!(frequent > rare);
    }

    #[test]
    fn reinforced_is_stronger() {
        let once = strength(60, 1, 1, 60);
        let thrice = strength(60, 1, 3, 60);
        assert!(thrice > once);
    }

    #[test]
    fn decay_reduces_strength() {
        let s0 = 1.0;
        let s1 = decay(s0, 7 * 24 * 60 * 60); // 7 days
        assert!(s1 < s0);
        assert!((s1 - s0 * std::f64::consts::E.powi(-1)).abs() < 0.01); // ~1/e
    }

    #[test]
    fn strength_is_nonnegative() {
        let s = strength(365 * 24 * 60 * 60, 0, 0, 365 * 24 * 60 * 60);
        assert!(s >= 0.0);
    }
}
