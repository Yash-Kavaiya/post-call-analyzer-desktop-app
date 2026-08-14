//! Reading and writing the metrics that cross a sync boundary.
//!
//! The projection lives here rather than in `ca-sync` so it is written once
//! against the schema that owns the data. `ca-sync` decides what the protocol
//! looks like; this decides which columns are allowed to fill it.

use ca_core::error::{CaError, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::Store;

/// One call's metrics, exactly as they leave the database.
///
/// Deliberately mirrors `ca_sync::SyncRecord` without depending on it: this
/// crate is the lower layer, and a dependency the other way would make the
/// protocol crate impossible to test without a database.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallMetrics {
    pub call_id: String,
    pub occurred_at: Option<String>,
    pub duration_secs: Option<f64>,
    pub stage: String,
    pub agent_id: Option<String>,
    pub contact_id: Option<String>,
    pub qa_overall: Option<f32>,
    pub sentiment: Option<f32>,
    pub compliance_passed: Option<bool>,
    /// `(code, severity, count)` — never the finding's prose.
    pub findings: Vec<(String, String, u32)>,
    pub updated_at: String,
}

impl Store {
    /// Calls changed since a watermark, oldest change first.
    ///
    /// The watermark is `calls.updated_at` rather than a local sequence, so a
    /// corpus that was restored from a backup re-pushes what the peer may have
    /// missed instead of silently resuming past it.
    pub fn call_metrics_since(
        &self,
        since: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CallMetrics>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.occurred_at, c.duration_secs, c.stage,
                        c.agent_id, c.contact_id, c.updated_at,
                        (SELECT json_extract(a.payload_json, '$.overall')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'qa_scorecard'
                            AND a.error IS NULL),
                        (SELECT json_extract(a.payload_json, '$.overall')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'sentiment'
                            AND a.error IS NULL),
                        (SELECT json_extract(a.payload_json, '$.passed')
                           FROM analyses a
                          WHERE a.call_id = c.id AND a.agent = 'compliance'
                            AND a.error IS NULL)
                   FROM calls c
                  WHERE (?1 IS NULL OR c.updated_at > ?1)
                  ORDER BY c.updated_at ASC
                  LIMIT ?2",
            )
            .map_err(|e| CaError::Storage(format!("prepare metrics: {e}")))?;

        let rows = stmt
            .query_map(params![since, limit as i64], |r| {
                let passed: Option<i64> = r.get(9)?;
                Ok(CallMetrics {
                    call_id: r.get(0)?,
                    occurred_at: r.get(1)?,
                    duration_secs: r.get(2)?,
                    stage: r.get(3)?,
                    agent_id: r.get(4)?,
                    contact_id: r.get(5)?,
                    updated_at: r.get(6)?,
                    qa_overall: r.get::<_, Option<f64>>(7)?.map(|v| v as f32),
                    sentiment: r.get::<_, Option<f64>>(8)?.map(|v| v as f32),
                    compliance_passed: passed.map(|v| v != 0),
                    findings: vec![],
                })
            })
            .map_err(|e| CaError::Storage(format!("query metrics: {e}")))?;

        let mut out = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read metrics: {e}")))?;

        // Findings in a second pass, selecting only code, severity and count.
        // `detail` and `title` are prose about a specific moment in the call
        // and are deliberately not in this query.
        let mut find = conn
            .prepare(
                "SELECT code, severity, count(*) FROM findings
                  WHERE call_id = ?1 GROUP BY code, severity",
            )
            .map_err(|e| CaError::Storage(format!("prepare finding metrics: {e}")))?;
        for m in &mut out {
            let rows = find
                .query_map([m.call_id.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?.max(0) as u32,
                    ))
                })
                .map_err(|e| CaError::Storage(format!("query finding metrics: {e}")))?;
            m.findings = rows
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| CaError::Storage(format!("read finding metrics: {e}")))?;
        }

        Ok(out)
    }

    /// Store a record received from a peer, replacing any earlier one.
    ///
    /// Returns the sequence number assigned. Replacing rather than appending is
    /// what makes a client retry safe: a call cannot appear twice in a rollup
    /// because a push timed out and was sent again.
    pub fn put_sync_record(&self, call_id: &str, payload_json: &str) -> Result<i64> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sync_records (call_id, payload_json, received_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(call_id) DO UPDATE SET
                 payload_json = excluded.payload_json,
                 received_at  = excluded.received_at,
                 -- A refreshed record moves to the end of the sequence so a
                 -- peer that already pulled it sees the update.
                 seq = (SELECT COALESCE(max(seq), 0) + 1 FROM sync_records)",
            params![call_id, payload_json, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| CaError::Storage(format!("put sync record: {e}")))?;

        conn.query_row(
            "SELECT seq FROM sync_records WHERE call_id = ?1",
            [call_id],
            |r| r.get(0),
        )
        .map_err(|e| CaError::Storage(format!("read sync seq: {e}")))
    }

    /// Records after a cursor, in sequence order.
    pub fn sync_records_after(&self, after: i64, limit: usize) -> Result<Vec<(i64, String)>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT seq, payload_json FROM sync_records
                  WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
            )
            .map_err(|e| CaError::Storage(format!("prepare sync pull: {e}")))?;
        let rows = stmt
            .query_map(params![after, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| CaError::Storage(format!("query sync pull: {e}")))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| CaError::Storage(format!("read sync pull: {e}")))
    }

    /// The highest sequence number held.
    pub fn sync_cursor(&self) -> Result<i64> {
        let conn = self.lock()?;
        conn.query_row("SELECT COALESCE(max(seq), 0) FROM sync_records", [], |r| {
            r.get(0)
        })
        .map_err(|e| CaError::Storage(format!("sync cursor: {e}")))
    }

    /// How far this machine has got with one peer.
    pub fn sync_state(&self, peer: &str) -> Result<(Option<String>, i64)> {
        let conn = self.lock()?;
        let row: Option<(Option<String>, i64)> = conn
            .query_row(
                "SELECT pushed_through, pulled_through FROM sync_state WHERE peer = ?1",
                [peer],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| CaError::Storage(format!("sync state: {e}")))?;
        Ok(row.unwrap_or((None, 0)))
    }

    /// Record progress with a peer.
    pub fn set_sync_state(
        &self,
        peer: &str,
        pushed_through: Option<&str>,
        pulled_through: i64,
    ) -> Result<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO sync_state (peer, pushed_through, pulled_through, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(peer) DO UPDATE SET
                 pushed_through = excluded.pushed_through,
                 pulled_through = excluded.pulled_through,
                 updated_at     = excluded.updated_at",
            params![
                peer,
                pushed_through,
                pulled_through,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| CaError::Storage(format!("set sync state: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::analysis::{Finding, Severity};
    use ca_core::call::Call;

    fn seeded() -> (Store, String) {
        let s = Store::in_memory().unwrap();
        let id = s
            .upsert_call(&Call::new(
                "C:/calls/Call recording +918485823280_251112_142326.m4a",
                "local",
                "Call recording +918485823280_251112_142326.m4a",
            ))
            .unwrap();
        s.attribute_calls(&ca_identity::builtin_patterns(), Default::default())
            .unwrap();
        s.save_findings(
            &id,
            &[Finding {
                id: ca_core::ids::FindingId::new(),
                call_id: id.clone(),
                agent: ca_core::AgentKind::Compliance,
                code: "HOLD_NOT_TIMEBOXED".into(),
                title: "Hold placed without an expected duration".into(),
                detail: "The agent said 'give me the card number' at 1:01.".into(),
                severity: Severity::High,
                citations: vec![],
            }],
        )
        .unwrap();
        (s, id.as_str().to_string())
    }

    /// The projection must not be able to carry the finding's prose, even
    /// though the row it comes from has it.
    #[test]
    fn metrics_carry_the_finding_code_but_never_its_detail() {
        let (s, _) = seeded();
        let metrics = s.call_metrics_since(None, 100).unwrap();
        assert_eq!(metrics.len(), 1);

        let json = serde_json::to_string(&metrics[0]).unwrap();
        assert!(
            json.contains("HOLD_NOT_TIMEBOXED"),
            "the code should travel"
        );
        assert!(
            !json.contains("card number"),
            "the finding detail must not travel"
        );
        assert!(
            !json.contains("Hold placed without"),
            "the finding title must not travel"
        );
    }

    /// A contact's number is in `people`, and must not reach the projection
    /// even though the call is attributed to them.
    #[test]
    fn metrics_carry_an_opaque_participant_id_never_a_number() {
        let (s, _) = seeded();
        let metrics = s.call_metrics_since(None, 100).unwrap();
        let json = serde_json::to_string(&metrics[0]).unwrap();

        assert!(metrics[0].contact_id.is_some(), "the id should travel");
        assert!(
            !json.contains("918485823280"),
            "the contact's number must not travel"
        );
    }

    #[test]
    fn the_watermark_returns_only_what_changed() {
        let (s, _) = seeded();
        let first = s.call_metrics_since(None, 100).unwrap();
        assert_eq!(first.len(), 1);

        let after = first[0].updated_at.clone();
        assert!(s.call_metrics_since(Some(&after), 100).unwrap().is_empty());
    }

    /// A retried push must not double-count a call in a rollup.
    #[test]
    fn re_pushing_a_call_replaces_rather_than_appends() {
        let s = Store::in_memory().unwrap();
        let a = s.put_sync_record("call_1", r#"{"qaOverall":50}"#).unwrap();
        let b = s.put_sync_record("call_1", r#"{"qaOverall":70}"#).unwrap();

        assert!(b >= a, "a refreshed record moves forward in the sequence");
        let all = s.sync_records_after(0, 100).unwrap();
        assert_eq!(all.len(), 1, "one row per call");
        assert!(all[0].1.contains("70"), "the newer payload won");
    }

    /// A peer that already pulled a record must see it again when it changes.
    #[test]
    fn an_updated_record_moves_past_a_peers_cursor() {
        let s = Store::in_memory().unwrap();
        s.put_sync_record("call_1", r#"{"v":1}"#).unwrap();
        s.put_sync_record("call_2", r#"{"v":1}"#).unwrap();
        let cursor = s.sync_cursor().unwrap();
        assert!(s.sync_records_after(cursor, 100).unwrap().is_empty());

        s.put_sync_record("call_1", r#"{"v":2}"#).unwrap();
        let fresh = s.sync_records_after(cursor, 100).unwrap();
        assert_eq!(fresh.len(), 1);
        assert!(fresh[0].1.contains("\"v\":2"));
    }

    #[test]
    fn peer_state_starts_at_the_beginning_and_survives_a_write() {
        let s = Store::in_memory().unwrap();
        assert_eq!(s.sync_state("hq").unwrap(), (None, 0));

        s.set_sync_state("hq", Some("2025-11-12T00:00:00Z"), 42)
            .unwrap();
        assert_eq!(
            s.sync_state("hq").unwrap(),
            (Some("2025-11-12T00:00:00Z".into()), 42)
        );
        // Peers are tracked separately; one syncing must not move another.
        assert_eq!(s.sync_state("other").unwrap(), (None, 0));
    }
}
