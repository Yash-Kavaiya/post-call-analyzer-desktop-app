//! User configuration, stored as JSON under named keys.
//!
//! Typed columns would mean a migration every time a checkbox is added, so
//! this is a small key/value table and the *shape* of each value lives with
//! the code that owns it.
//!
//! One rule holds absolutely: **no secrets in here**. API keys go to the OS
//! credential store (see `ca_llm::secrets`). This table is inside the app
//! database, which gets copied, backed up and handed around; a key written
//! here would travel with every one of those copies.

use crate::Store;
use ca_core::error::{CaError, Result};
use rusqlite::{params, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};

impl Store {
    /// Read a setting, or `None` if it has never been written.
    ///
    /// A value that no longer parses — an old shape after an upgrade — reads
    /// as `None` rather than as an error, so a stale row degrades to the
    /// caller's default instead of blocking the settings screen entirely.
    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let conn = self.lock()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("read setting `{key}`: {e}")))?;

        Ok(raw.and_then(|s| match serde_json::from_str(&s) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(key, error = %e, "discarding unreadable setting");
                None
            }
        }))
    }

    pub fn put_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)
            .map_err(|e| CaError::Storage(format!("encode setting `{key}`: {e}")))?;
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET
                value_json = excluded.value_json, updated_at = excluded.updated_at",
            params![key, json, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| CaError::Storage(format!("write setting `{key}`: {e}")))?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
            .map_err(|e| CaError::Storage(format!("delete setting `{key}`: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Cfg {
        provider: String,
        concurrency: u32,
    }

    #[test]
    fn a_setting_round_trips() {
        let s = Store::in_memory().unwrap();
        let cfg = Cfg {
            provider: "openai".into(),
            concurrency: 4,
        };
        s.put_setting("llm", &cfg).unwrap();
        assert_eq!(s.get_setting::<Cfg>("llm").unwrap(), Some(cfg));
    }

    #[test]
    fn an_unset_key_reads_as_none() {
        let s = Store::in_memory().unwrap();
        assert!(s.get_setting::<Cfg>("never-written").unwrap().is_none());
    }

    #[test]
    fn writing_twice_replaces_rather_than_failing() {
        let s = Store::in_memory().unwrap();
        s.put_setting(
            "llm",
            &Cfg {
                provider: "a".into(),
                concurrency: 1,
            },
        )
        .unwrap();
        s.put_setting(
            "llm",
            &Cfg {
                provider: "b".into(),
                concurrency: 2,
            },
        )
        .unwrap();
        let got: Cfg = s.get_setting("llm").unwrap().unwrap();
        assert_eq!(got.provider, "b");
    }

    #[test]
    fn a_value_of_the_wrong_shape_degrades_to_none() {
        // An upgrade that changes a setting's shape must not brick the screen
        // that would let the user fix it.
        let s = Store::in_memory().unwrap();
        s.put_setting("llm", &"not an object").unwrap();
        assert!(s.get_setting::<Cfg>("llm").unwrap().is_none());
    }

    #[test]
    fn deleting_restores_the_default() {
        let s = Store::in_memory().unwrap();
        s.put_setting(
            "llm",
            &Cfg {
                provider: "a".into(),
                concurrency: 1,
            },
        )
        .unwrap();
        s.delete_setting("llm").unwrap();
        assert!(s.get_setting::<Cfg>("llm").unwrap().is_none());
    }
}
