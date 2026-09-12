// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! API-key Tauri command.
//!
//! [`get_api_keys`] returns the stored API key for every endpoint, keyed by
//! endpoint name. It is a *separate* command from
//! [`get_config`](super::settings::get_config) (which never includes secrets)
//! so the no-secrets guarantee of the normal config payload is preserved —
//! keys are only fetched when the Settings dialog is open.

use tauri::State;

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// Return the API keys for every endpoint, keyed by endpoint name.
///
/// The Endpoints tab populates its password fields from this map. It is a
/// *separate* command from [`get_config`](super::settings::get_config) (which
/// never includes secrets) so the no-secrets guarantee of the normal config
/// payload is preserved — keys are only fetched when the Settings dialog is
/// open.
///
/// Endpoints with no stored key are omitted from the map (the UI treats an
/// absent key as an empty password field).
#[tauri::command]
pub async fn get_api_keys(
    state: State<'_, IpcState>,
) -> Result<std::collections::HashMap<String, String>, IpcError> {
    let config = state.project.config.lock().await;
    let mut keys = std::collections::HashMap::new();
    for ep in &config.endpoints {
        if let Some(k) = config.key_for(&ep.name) {
            keys.insert(ep.name.clone(), k.to_string());
        }
    }
    Ok(keys)
}
