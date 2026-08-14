//! Deriving participants for calls that are already in the corpus.

use ca_core::error::{CaError, Result};
use ca_core::identity::AttributionSource;
use ca_core::ids::CallId;
use ca_identity::{attribute, Attribution, Pattern};

use crate::Store;

/// How a backfill run should behave.
#[derive(Debug, Clone, Copy, Default)]
pub struct AttributeOptions {
    /// Report what would change without writing anything.
    pub dry_run: bool,
    /// Overwrite hand-made assignments too. Off by default, deliberately.
    pub force: bool,
}

/// What a backfill run did.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttributeReport {
    pub scanned: usize,
    pub attributed: usize,
    pub skipped_manual: usize,
    /// Matched no pattern. These stay fully usable; they simply do not roll up
    /// under anyone, and the count is reported rather than quietly dropped.
    pub unmatched: usize,
}

impl Store {
    /// Re-derive participants for every call from its filename.
    ///
    /// Idempotent: identity is `(kind, handle)`, so a second run finds the same
    /// people rather than creating a second copy of all 771 of them.
    ///
    /// Rows attributed by hand are left alone unless `force` is set. Without
    /// that rule the second run silently discards every correction the user
    /// made, which is the most likely way this feature loses data.
    pub fn attribute_calls(
        &self,
        patterns: &[Pattern],
        opts: AttributeOptions,
    ) -> Result<AttributeReport> {
        // Read the whole list first and drop the lock: `upsert_person` and
        // `set_call_participants` each take it themselves, and holding it
        // across the loop would deadlock on the first write.
        let rows: Vec<(String, String, String, String)> = {
            let conn = self.lock()?;
            let mut stmt = conn
                .prepare("SELECT id, display_name, source_key, attribution FROM calls")
                .map_err(|e| CaError::Storage(format!("prepare attribute: {e}")))?;
            let mapped = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|e| CaError::Storage(format!("query attribute: {e}")))?;
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| CaError::Storage(format!("read attribute: {e}")))?
        };

        let mut report = AttributeReport::default();
        for (id, display_name, source_key, current) in rows {
            report.scanned += 1;
            let current = AttributionSource::parse(&current).unwrap_or_default();
            if current.is_protected() && !opts.force {
                report.skipped_manual += 1;
                continue;
            }

            let found = attribute(&display_name, &source_key, patterns);
            if found.is_empty() {
                report.unmatched += 1;
                continue;
            }
            report.attributed += 1;
            if opts.dry_run {
                continue;
            }
            self.write_attribution(&CallId::from(id), found)?;
        }
        Ok(report)
    }

    /// Attribute one call, as it is registered.
    ///
    /// Ingest calls this so a freshly scanned folder never needs a separate
    /// backfill pass. Returns whether anything was learned.
    pub fn attribute_call(
        &self,
        call_id: &CallId,
        display_name: &str,
        source_key: &str,
        patterns: &[Pattern],
    ) -> Result<bool> {
        let found = attribute(display_name, source_key, patterns);
        if found.is_empty() {
            return Ok(false);
        }
        self.write_attribution(call_id, found)?;
        Ok(true)
    }

    fn write_attribution(&self, call_id: &CallId, found: Attribution) -> Result<()> {
        let agent = found
            .agent
            .map(|h| self.upsert_person(h.kind, &h.display_name, &h.handle))
            .transpose()?;
        let contact = found
            .contact
            .map(|h| self.upsert_person(h.kind, &h.display_name, &h.handle))
            .transpose()?;

        self.set_call_participants(
            call_id,
            agent.as_ref(),
            contact.as_ref(),
            found.occurred_at,
            AttributionSource::Rule,
        )
    }
}
