// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `keys.toml` — secrets (API keys only).
//!
//! Each table is keyed by endpoint name and contains an `api_key` field.
//! The `KeyStore` deliberately does **not** implement `Debug` with the key
//! values, to prevent accidental logging.
//!
//! ## File permissions (Quality M6)
//!
//! `keys.toml` is plaintext on disk, so after every write [`KeyStore::save`]
//! calls [`restrict_permissions`] to lock the file down to the current user:
//! - **Unix**: `chmod 0600` (owner read/write only) via
//!   `std::os::unix::fs::PermissionsExt`.
//! - **Windows**: a user-only DACL (the current user's SID gets `GENERIC_ALL`,
//!   everyone else is denied) via `windows-sys`'s `SetNamedSecurityInfoW`.
//!
//! The permission step is **fail-safe**: if it errors (e.g. the SID lookup
//! fails on an unusual Windows configuration), the failure is logged at
//! `warn` but does **not** block the write — the secrets are already safely
//! on disk via the atomic temp-file + rename. A missing restriction is a
//! hardening gap, not a data-loss risk.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// One entry in `keys.toml`: `[<endpoint-name>]` with an `api_key`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyEntry {
    pub api_key: String,
}

/// The wire-format file: a map of endpoint-name → `KeyEntry`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeysFile {
    #[serde(flatten)]
    pub keys: HashMap<String, KeyEntry>,
}

/// A store of API keys, keyed by endpoint name.
///
/// This type intentionally does NOT print key values in its `Debug` output.
#[derive(Clone, Default)]
pub struct KeyStore {
    keys: HashMap<String, String>,
}

impl KeyStore {
    /// Load from `keys.toml`, or return an empty store if missing.
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let file: KeysFile = toml::from_str(&text)?;
        let keys = file.keys.into_iter().map(|(k, v)| (k, v.api_key)).collect();
        Ok(Self { keys })
    }

    /// Look up the API key for an endpoint by name.
    pub fn get(&self, endpoint_name: &str) -> Option<&str> {
        self.keys.get(endpoint_name).map(|s| s.as_str())
    }

    /// Insert or replace a key for an endpoint.
    pub fn insert(&mut self, endpoint_name: String, key: String) {
        self.keys.insert(endpoint_name, key);
    }

    /// Number of stored keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Iterate over (name, api_key) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.keys.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Serialize the store to a TOML string (sorted by endpoint name for
    /// stable output). Used by [`save`] and by `Config::save_all` staging.
    pub fn to_toml_string(&self) -> Result<String> {
        use std::collections::BTreeMap;
        #[derive(Serialize)]
        struct OrderedKeysFile {
            #[serde(flatten)]
            keys: BTreeMap<String, KeyEntry>,
        }
        let file = OrderedKeysFile {
            keys: self
                .keys
                .iter()
                .map(|(k, v)| (k.clone(), KeyEntry { api_key: v.clone() }))
                .collect(),
        };
        Ok(toml::to_string_pretty(&file)?)
    }

    /// Write the store to `keys.toml`, serializing each entry as a
    /// `[<endpoint-name>]` table with an `api_key` field. Entries are sorted
    /// by endpoint name so the output is stable across writes. Uses a
    /// temp-file and rename for a crash-safe single-file write, then
    /// restricts the file's permissions to the current user only (Quality
    /// M6 — see [`restrict_permissions`]).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_toml_string()?;
        crate::config::write_atomic(path, &text)?;
        // Best-effort permission restriction: never block the write on a
        // hardening failure (the secrets are already safely on disk).
        if let Err(e) = restrict_permissions(path) {
            eprintln!(
                "keys: warning — could not restrict permissions on '{}': {e}",
                path.display()
            );
        }
        Ok(())
    }
}

/// Restrict a file's permissions to the current user only (Quality M6).
///
/// - **Unix**: sets the mode to `0o600` (owner read/write).
/// - **Windows**: replaces the DACL with one granting the current user
///   `GENERIC_ALL` and denying everyone else, via `SetNamedSecurityInfoW`.
///
/// This is best-effort: callers (notably [`KeyStore::save`]) treat an error
/// here as a warning, not a hard failure — the atomic write has already
/// landed the data safely. Returns `Ok(())` on success or when the platform
/// has no restriction to apply.
///
/// Shared with the provider trace log ([`crate::provider::trace`]) so the
/// `.coding/logs/*.jsonl` files — which persist full request/response bodies
/// that may embed secrets — get the same user-only restriction as `keys.toml`.
pub(crate) fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(path, perms)?;
    }

    #[cfg(windows)]
    {
        restrict_permissions_windows(path)?;
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
    }

    Ok(())
}

/// Windows user-only DACL via `windows-sys` (Quality M6).
///
/// Builds a DACL that grants the current user `GENERIC_ALL` (full control)
/// and attaches it via `SetNamedSecurityInfoW` with
/// `DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION`. The
/// `PROTECTED` flag prevents inherited ACEs from re-granting access, so only
/// the current user can read/write the file.
#[cfg(windows)]
fn restrict_permissions_windows(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{LocalFree, GENERIC_ALL};
    use windows_sys::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, SET_ACCESS, SE_FILE_OBJECT,
        TRUSTEE_IS_SID, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PROTECTED_DACL_SECURITY_INFORMATION,
    };

    // 1. Current user's SID (owned bytes; a SID is variable-length).
    let user_sid = current_user_sid()?;

    // 2. One explicit-access entry: the user gets GENERIC_ALL (set, not
    //    merged), no inheritance. TRUSTEE_IS_SID means ptstrName points at
    //    the SID bytes (cast to PWSTR — the union is discriminated by the form).
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: 0,
            ptstrName: user_sid.as_ptr() as *mut _,
        },
    };

    // 3. Build the ACL. SetEntriesInAclW allocates it via LocalAlloc.
    let mut new_acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `ea` is a valid stack struct; `new_acl` receives an ACL the API
    // allocates (freed with LocalFree below). Returns ERROR_SUCCESS (0).
    let rc = unsafe { SetEntriesInAclW(1, &ea, std::ptr::null(), &mut new_acl) };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc as i32));
    }

    // 4. Attach the DACL to the file. SE_FILE_OBJECT = 1.
    let path_w: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path_w.as_ptr() as *const _,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(), // owner — unchanged
            std::ptr::null_mut(), // group — unchanged
            new_acl as *const _,
            std::ptr::null(), // SACL — unchanged
        )
    };
    // Free the ACL the API allocated (LocalAlloc → LocalFree).
    unsafe { LocalFree(new_acl as *mut _) };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc as i32));
    }
    Ok(())
}

/// Obtain the current user's SID as an owned `Vec<u8>` (a `SID` is a
/// variable-length struct; we hold its raw bytes). Used to build the
/// user-only DACL on Windows.
#[cfg(windows)]
fn current_user_sid() -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, IsValidSid, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // Open the current process's token with TOKEN_QUERY.
    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error());
    }

    // First call to learn the required buffer size.
    let mut len: u32 = 0;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
    }
    let mut buf = vec![0u8; len as usize];
    let ok =
        unsafe { GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut _, len, &mut len) };
    if ok == 0 {
        let e = std::io::Error::last_os_error();
        unsafe { CloseHandle(token) };
        return Err(e);
    }
    unsafe { CloseHandle(token) };

    // TOKEN_USER { User: SID_AND_ATTRIBUTES { Sid: PSID, Attributes: u32 } }.
    // The SID pointer aliases into `buf` (which owns the storage).
    let user: &TOKEN_USER = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let sid_ptr: *const u8 = user.User.Sid as *const u8;
    if sid_ptr.is_null() {
        return Err(std::io::Error::other("token user SID is null"));
    }
    if unsafe { IsValidSid(user.User.Sid) } == 0 {
        return Err(std::io::Error::other("invalid user SID"));
    }
    // SID layout: Revision(1) + SubAuthorityCount(1) + IdentifierAuthority(6)
    //             + SubAuthority[SubAuthorityCount](4 each) = 8 + 4*count.
    let sub_auth_count = unsafe { *sid_ptr.add(1) } as usize;
    let sid_len = 8 + 4 * sub_auth_count;
    let sid_bytes = unsafe { std::slice::from_raw_parts(sid_ptr, sid_len) };
    Ok(sid_bytes.to_vec())
}

// Manual Debug impl: never print key values.
impl std::fmt::Debug for KeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyStore")
            .field("len", &self.keys.len())
            .field("keys", &"<redacted>".to_string())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys() {
        let text = r#"
[openai]
api_key = "sk-abc"

[ollama-local]
api_key = "dummy"
"#;
        let file: KeysFile = toml::from_str(text).unwrap();
        assert_eq!(file.keys.len(), 2);
        assert_eq!(file.keys.get("openai").unwrap().api_key, "sk-abc");
        assert_eq!(file.keys.get("ollama-local").unwrap().api_key, "dummy");
    }

    #[test]
    fn store_lookup() {
        let text = r#"
[openai]
api_key = "sk-xyz"
"#;
        let file: KeysFile = toml::from_str(text).unwrap();
        let store = KeyStore {
            keys: file.keys.into_iter().map(|(k, v)| (k, v.api_key)).collect(),
        };
        assert_eq!(store.get("openai"), Some("sk-xyz"));
        assert_eq!(store.get("missing"), None);
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn debug_does_not_leak_keys() {
        let store = KeyStore {
            keys: [("openai".to_string(), "sk-super-secret".to_string())]
                .into_iter()
                .collect(),
        };
        let debug = format!("{:?}", store);
        assert!(
            !debug.contains("sk-super-secret"),
            "key leaked in Debug: {debug}"
        );
        assert!(debug.contains("len"));
    }

    #[test]
    fn save_round_trips_keys() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        let mut store = KeyStore::default();
        store
            .keys
            .insert("openai".to_string(), "sk-abc".to_string());
        store
            .keys
            .insert("ollama-local".to_string(), "dummy".to_string());
        store.save(&path).unwrap();

        // The written file must not be empty and must re-parse to the same map.
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("api_key"));
        let reloaded = KeyStore::load_or_default(&path).unwrap();
        assert_eq!(reloaded.get("openai"), Some("sk-abc"));
        assert_eq!(reloaded.get("ollama-local"), Some("dummy"));
        assert_eq!(reloaded.len(), 2);
    }

    #[test]
    fn save_writes_entries_sorted_by_name() {
        // The on-disk order must be deterministic (sorted by endpoint name) so
        // repeated saves don't churn the file regardless of HashMap iteration
        // order. Insert in non-sorted order and verify the file lists them
        // alphabetically.
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        let mut store = KeyStore::default();
        store.keys.insert("zeta".to_string(), "k-z".to_string());
        store.keys.insert("alpha".to_string(), "k-a".to_string());
        store.keys.insert("mid".to_string(), "k-m".to_string());
        store.save(&path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let alpha = text.find("[alpha]").unwrap();
        let mid = text.find("[mid]").unwrap();
        let zeta = text.find("[zeta]").unwrap();
        assert!(alpha < mid && mid < zeta, "keys not sorted: {text}");
    }

    #[test]
    fn save_empty_store_writes_empty_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        KeyStore::default().save(&path).unwrap();
        let reloaded = KeyStore::load_or_default(&path).unwrap();
        assert!(reloaded.is_empty());
    }

    #[test]
    fn save_restricts_permissions_to_user_only() {
        // Quality M6: after save, the file must be restricted to the current
        // user only (Unix 0600; Windows user-only DACL). The save itself must
        // not panic and the data must still round-trip.
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        let mut store = KeyStore::default();
        store
            .keys
            .insert("openai".to_string(), "sk-secret".to_string());
        store.save(&path).unwrap();

        // The file exists + round-trips.
        assert!(path.exists());
        let reloaded = KeyStore::load_or_default(&path).unwrap();
        assert_eq!(reloaded.get("openai"), Some("sk-secret"));

        // Platform-specific permission assertion.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "keys.toml should be 0600, got {:o}",
                mode
            );
        }
        // On Windows we assert only that save succeeded without panicking —
        // the DACL is applied best-effort and asserting the exact ACE set
        // would couple the test to the OS account layout.
    }

    #[test]
    fn restrict_permissions_is_idempotent() {
        // Calling restrict_permissions twice (e.g. across repeated saves)
        // must not error — the DACL/mode is simply re-applied.
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        fs::write(&path, "[x]\napi_key = \"k\"\n").unwrap();
        restrict_permissions(&path).unwrap();
        restrict_permissions(&path).unwrap();
    }
}
