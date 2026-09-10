//! API keys, kept out of the project.
//!
//! A key typed into a settings box has to live somewhere. The two obvious
//! places are both wrong: a config file gets committed, and a plain row in the
//! app database travels with any copy of it. So keys go to the OS credential
//! store — Windows Credential Manager here — which is encrypted per user
//! account and which the rest of the app can read without ever holding the
//! value long enough to log it.
//!
//! Resolution order when a provider needs a key:
//!
//! 1. **The credential store**, which is what the settings page writes to.
//! 2. **The vendor's environment variable**, so a machine that already has the
//!    vendor's own CLI working needs no extra setup, and so CI has a route in.
//!
//! Nothing here ever returns a key to the UI. The settings page is told only
//! whether one is present.

use crate::registry::ProviderKind;
use ca_core::error::{CaError, Result};

/// Credential Manager groups entries by service; one per vendor keeps them
/// individually revocable from the OS UI.
const ACCOUNT: &str = "api-key";

fn entry(kind: ProviderKind) -> Result<keyring::Entry> {
    named_entry(kind.as_str(), ACCOUNT)
}

/// A credential that is not a vendor API key.
///
/// The sync token is the other secret this product holds, and it belongs in
/// the same place for the same reasons: a config file gets committed, and a
/// row in the database travels with any copy of it.
fn named_entry(name: &str, account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(&format!("call-analyzer:{name}"), account)
        .map_err(|e| CaError::Config(format!("credential store unavailable: {e}")))
}

/// Save a secret under a name. An empty string deletes rather than storing a
/// blank, which would otherwise read as "configured" while being useless.
pub fn store_named(name: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() {
        return delete_named(name);
    }
    named_entry(name, ACCOUNT)?
        .set_password(value)
        .map_err(|e| CaError::Config(format!("could not save `{name}`: {e}")))
}

/// Read a secret. Used by the backend only — never returned to the UI.
pub fn read_named(name: &str) -> Option<String> {
    named_entry(name, ACCOUNT)
        .ok()
        .and_then(|e| e.get_password().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn delete_named(name: &str) -> Result<()> {
    match named_entry(name, ACCOUNT)?.delete_credential() {
        Ok(()) => Ok(()),
        // Deleting something that was never there is the desired end state.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CaError::Config(format!("could not clear `{name}`: {e}"))),
    }
}

/// Save a key. An empty string deletes rather than storing a blank.
pub fn store_key(kind: ProviderKind, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return delete_key(kind);
    }
    entry(kind)?
        .set_password(key)
        .map_err(|e| CaError::Config(format!("could not save the key: {e}")))
}

pub fn delete_key(kind: ProviderKind) -> Result<()> {
    match entry(kind)?.delete_credential() {
        Ok(()) => Ok(()),
        // Deleting something that was never there is the desired end state.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CaError::Config(format!("could not remove the key: {e}"))),
    }
}

/// The credential store only. Use [`resolve_key`] for the full order.
pub fn load_key(kind: ProviderKind) -> Option<String> {
    entry(kind)
        .ok()?
        .get_password()
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Where a key would come from, for showing the user why it works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeySource {
    CredentialStore,
    Environment,
    /// Local providers need none.
    NotRequired,
    Missing,
}

/// The key this provider will actually use, and where it came from.
pub fn resolve_key(kind: ProviderKind) -> (Option<String>, KeySource) {
    if !kind.requires_api_key() {
        return (None, KeySource::NotRequired);
    }
    if let Some(k) = load_key(kind) {
        return (Some(k), KeySource::CredentialStore);
    }
    if let Some(k) = kind.api_key_from_env() {
        return (Some(k), KeySource::Environment);
    }
    (None, KeySource::Missing)
}

/// Whether this provider is ready to be used, without revealing the key.
pub fn key_source(kind: ProviderKind) -> KeySource {
    resolve_key(kind).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_providers_never_need_a_key() {
        for kind in [ProviderKind::Ollama, ProviderKind::LmStudio] {
            assert_eq!(key_source(kind), KeySource::NotRequired);
        }
    }

    #[test]
    fn each_vendor_gets_its_own_credential_entry() {
        // Shared entries cannot be revoked one at a time, and one vendor's key
        // would silently authenticate calls to another.
        let a = format!("call-analyzer:{}", ProviderKind::OpenAi.as_str());
        let b = format!("call-analyzer:{}", ProviderKind::Anthropic.as_str());
        assert_ne!(a, b);
        assert!(a.starts_with("call-analyzer:"));
    }

    #[test]
    fn the_environment_is_consulted_when_the_store_is_empty() {
        // Uses a vendor unlikely to have a real stored credential on a dev box.
        let kind = ProviderKind::Groq;
        if load_key(kind).is_some() {
            return; // A real key is present; nothing to assert safely.
        }
        // SAFETY: single-threaded test, variable is vendor-specific.
        unsafe { std::env::set_var("GROQ_API_KEY", "env-provided") };
        let (key, source) = resolve_key(kind);
        assert_eq!(source, KeySource::Environment);
        assert_eq!(key.as_deref(), Some("env-provided"));
        unsafe { std::env::remove_var("GROQ_API_KEY") };

        assert_eq!(key_source(kind), KeySource::Missing);
    }
}
