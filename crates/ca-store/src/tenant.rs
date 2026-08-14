//! Keeping one customer's calls away from another's.
//!
//! **One database file per tenant**, not a `tenant_id` column.
//!
//! A shared table with a tenant column is only as isolated as the least
//! careful `WHERE` clause in the codebase: every query, now and forever, has
//! to remember it, and the one that forgets returns someone else's calls
//! rather than failing. A separate file makes the leak unrepresentable — there
//! is no query you can write against tenant A's connection that returns
//! tenant B's rows, because those rows are not in the database being queried.
//!
//! For transcripts of other people's phone calls that trade is worth making.
//! It costs a connection per active tenant and rules out cross-tenant
//! aggregate queries, neither of which this product needs.
//!
//! The default tenant maps to the existing database path, so a single-user
//! install never encounters any of this.

use ca_core::error::{CaError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Store;

/// The tenant a lone desktop install uses without ever being told about it.
pub const DEFAULT_TENANT: &str = "local";

/// Longest accepted tenant slug. Long enough for a real organisation name,
/// short enough to stay well inside any filesystem's path limit.
const MAX_SLUG: usize = 48;

/// A tenant and the corpus behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantInfo {
    pub slug: String,
    /// Absolute path to this tenant's database.
    pub path: String,
    /// Bytes on disk, or `None` if the file has not been created yet.
    pub bytes: Option<u64>,
}

/// Check a tenant slug before it is ever used to build a path.
///
/// This is the security boundary of the whole scheme. A slug goes straight
/// into a filename, so `../../etc/passwd` or an absolute path would read and
/// write outside the tenant directory — reaching another tenant's corpus, or
/// anything else the process can open. Allowing only lowercase letters,
/// digits, `-` and `_` makes traversal unrepresentable rather than merely
/// filtered: there is no encoding of `..` or a separator that survives.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > MAX_SLUG {
        return Err(CaError::Config(format!(
            "tenant name must be 1..={MAX_SLUG} characters"
        )));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(CaError::Config(
            "tenant name may use only lowercase letters, digits, - and _".into(),
        ));
    }
    Ok(())
}

/// Where a tenant's corpus lives.
///
/// The default tenant keeps the historic path so an existing install is not
/// silently migrated to a new file and left looking empty.
pub fn tenant_db_path(base: &Path, slug: &str) -> Result<PathBuf> {
    validate_slug(slug)?;
    if slug == DEFAULT_TENANT {
        return Ok(base.join("call-analyzer.db"));
    }
    Ok(base.join("tenants").join(format!("{slug}.db")))
}

/// Open corpora, one per tenant.
///
/// Handles are cached because opening a SQLite file runs the migrations, and a
/// server answering per-request would otherwise do that on every call.
pub struct TenantStores {
    base: PathBuf,
    open: Mutex<HashMap<String, Arc<Store>>>,
}

impl TenantStores {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self {
            base: base.into(),
            open: Mutex::new(HashMap::new()),
        }
    }

    /// The store for one tenant, opening it if this is the first request.
    ///
    /// Creates the file when it does not exist: a tenant with no calls yet is
    /// a normal state, not an error.
    pub fn get(&self, slug: &str) -> Result<Arc<Store>> {
        validate_slug(slug)?;

        let mut open = self
            .open
            .lock()
            .map_err(|_| CaError::Storage("tenant map poisoned".into()))?;
        if let Some(store) = open.get(slug) {
            return Ok(store.clone());
        }

        let path = tenant_db_path(&self.base, slug)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CaError::Storage(format!("create {}: {e}", parent.display())))?;
        }
        let store = Arc::new(Store::open(&path)?);
        open.insert(slug.to_string(), store.clone());
        Ok(store)
    }

    /// Every tenant that has a corpus on disk.
    pub fn list(&self) -> Result<Vec<TenantInfo>> {
        let mut out = Vec::new();

        let default_path = tenant_db_path(&self.base, DEFAULT_TENANT)?;
        if default_path.exists() {
            out.push(TenantInfo {
                slug: DEFAULT_TENANT.to_string(),
                bytes: std::fs::metadata(&default_path).ok().map(|m| m.len()),
                path: default_path.display().to_string(),
            });
        }

        let dir = self.base.join("tenants");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("db") {
                    continue;
                }
                let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                // A file whose name is not a valid slug was not created here
                // and is not addressable, so it is not listed as a tenant.
                if validate_slug(slug).is_err() {
                    continue;
                }
                out.push(TenantInfo {
                    slug: slug.to_string(),
                    bytes: entry.metadata().ok().map(|m| m.len()),
                    path: path.display().to_string(),
                });
            }
        }

        out.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(out)
    }

    /// Create a tenant's corpus now rather than on first use.
    pub fn create(&self, slug: &str) -> Result<TenantInfo> {
        let store = self.get(slug)?;
        let path = store
            .path()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        Ok(TenantInfo {
            slug: slug.to_string(),
            bytes: std::fs::metadata(&path).ok().map(|m| m.len()),
            path,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::call::Call;
    use ca_core::identity::{PersonKind, ViewScope};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ca-tenants-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The property the whole design exists for.
    #[test]
    fn one_tenants_calls_are_invisible_to_another() {
        let dir = scratch("isolation");
        let stores = TenantStores::new(&dir);

        let acme = stores.get("acme").unwrap();
        let globex = stores.get("globex").unwrap();

        acme.upsert_call(&Call::new("k1", "local", "acme call.m4a"))
            .unwrap();
        globex
            .upsert_call(&Call::new("k1", "local", "globex call.m4a"))
            .unwrap();

        // Same source key in both, and neither sees the other's row — not
        // because a filter excluded it, but because it is in another file.
        let acme_calls = acme.list_calls(0, 50, None, &ViewScope::all()).unwrap();
        let globex_calls = globex.list_calls(0, 50, None, &ViewScope::all()).unwrap();
        assert_eq!(acme_calls.len(), 1);
        assert_eq!(globex_calls.len(), 1);
        assert_eq!(acme_calls[0].display_name, "acme call.m4a");
        assert_eq!(globex_calls[0].display_name, "globex call.m4a");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same phone number is a different person for each customer, and a
    /// shared unique index would have collided on the second insert.
    #[test]
    fn identity_is_per_tenant() {
        let dir = scratch("identity");
        let stores = TenantStores::new(&dir);

        let a = stores.get("acme").unwrap();
        let b = stores.get("globex").unwrap();
        let pa = a
            .upsert_person(PersonKind::Contact, "+918485823280", "+918485823280")
            .unwrap();
        let pb = b
            .upsert_person(PersonKind::Contact, "+918485823280", "+918485823280")
            .unwrap();

        assert_ne!(pa, pb, "same handle, different tenant, different person");
        assert_eq!(a.list_people(None, None).unwrap().len(), 1);
        assert_eq!(b.list_people(None, None).unwrap().len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A slug reaches the filesystem, so traversal has to be impossible rather
    /// than merely unlikely.
    #[test]
    fn a_slug_cannot_escape_the_tenant_directory() {
        for bad in [
            "../escape",
            "..",
            "a/b",
            "a\\b",
            "C:/windows",
            "/etc/passwd",
            "UPPER",
            "with space",
            "dot.dot",
            "",
        ] {
            assert!(
                validate_slug(bad).is_err(),
                "`{bad}` must be rejected as a tenant name"
            );
        }
        for good in ["acme", "acme-north", "acme_2", "t1"] {
            assert!(validate_slug(good).is_ok(), "`{good}` should be allowed");
        }
    }

    #[test]
    fn an_over_long_slug_is_rejected() {
        assert!(validate_slug(&"a".repeat(MAX_SLUG)).is_ok());
        assert!(validate_slug(&"a".repeat(MAX_SLUG + 1)).is_err());
    }

    /// An existing single-user install must not wake up pointing at a new
    /// empty file.
    #[test]
    fn the_default_tenant_keeps_the_historic_path() {
        let dir = scratch("default-path");
        let path = tenant_db_path(&dir, DEFAULT_TENANT).unwrap();
        assert_eq!(path, dir.join("call-analyzer.db"));

        let other = tenant_db_path(&dir, "acme").unwrap();
        assert_eq!(other, dir.join("tenants").join("acme.db"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_tenant_reuses_one_open_store() {
        let dir = scratch("cache");
        let stores = TenantStores::new(&dir);
        let a = stores.get("acme").unwrap();
        let b = stores.get("acme").unwrap();
        assert!(Arc::ptr_eq(&a, &b), "opening twice runs migrations twice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn listing_reports_every_tenant_with_a_corpus() {
        let dir = scratch("listing");
        let stores = TenantStores::new(&dir);
        stores.get(DEFAULT_TENANT).unwrap();
        stores.get("acme").unwrap();
        stores.get("globex").unwrap();

        let listed = stores.list().unwrap();
        let slugs: Vec<_> = listed.iter().map(|t| t.slug.as_str()).collect();
        assert_eq!(slugs, vec!["acme", "globex", "local"]);
        assert!(listed.iter().all(|t| t.bytes.unwrap_or(0) > 0));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
