//! Reading the findings ledger back out.
//!
//! `save_findings` has written to this table since Phase 1 and nothing has ever
//! read it, which made the compliance ledger write-only: a critical finding
//! could be stored, counted against the call's pass/fail, and never be visible
//! anywhere. The rollup here is what turns "this call failed" into "this is the
//! rule we keep breaking, and these are the calls to listen to".
//!
//! Rows are read defensively. A severity written by an older build, or a
//! citations blob that no longer parses, costs that one field rather than the
//! whole query — a risk register that refuses to load is worse than one with a
//! gap in it.

use crate::Store;
use ca_core::analysis::{AgentKind, Citation, Severity};
use ca_core::error::{CaError, Result};
use ca_core::ids::CallId;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// One stored finding, as the UI reads it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredFinding {
    pub id: String,
    pub call_id: String,
    /// The agent that raised it, e.g. `compliance`.
    pub agent: String,
    /// That agent's display name, so the UI needs no mapping table.
    pub label: String,
    pub code: String,
    pub title: String,
    pub detail: String,
    pub severity: Severity,
    /// Whether this severity alone fails the call. Computed here so the UI does
    /// not re-implement the threshold.
    pub blocking: bool,
    pub citations: Vec<Citation>,
}

/// One rule, and how much of the corpus breaks it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FindingCount {
    pub code: String,
    /// The most recent title seen for this code, as a human-readable label.
    pub title: String,
    /// The worst severity this code has been raised at.
    pub severity: Severity,
    /// Distinct calls, not rows: one call raising the same code twice is one
    /// call's worth of evidence about the corpus.
    pub calls: usize,
    pub blocking: bool,
}

impl Store {
    /// Every finding against one call, worst first.
    ///
    /// Severity order rather than insertion order: the reason to open this list
    /// is to find out whether anything on the call is serious.
    pub fn findings(&self, call_id: &CallId) -> Result<Vec<StoredFinding>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, call_id, agent, code, title, detail, severity, citations_json
                 FROM findings WHERE call_id = ?1",
            )
            .map_err(|e| CaError::Storage(format!("prepare findings: {e}")))?;

        let rows = stmt
            .query_map(params![call_id.as_str()], |r| {
                let agent: String = r.get(2)?;
                let severity_raw: String = r.get(6)?;
                let citations_raw: String = r.get(7)?;
                // An unreadable severity reads as `info` rather than being
                // promoted: inventing a critical from a bad row would raise a
                // false alarm, and the row is still listed either way.
                let severity = Severity::parse(&severity_raw).unwrap_or(Severity::Info);
                Ok(StoredFinding {
                    id: r.get(0)?,
                    call_id: r.get(1)?,
                    label: AgentKind::parse(&agent)
                        .map(|k| k.label().to_string())
                        .unwrap_or_else(|| agent.clone()),
                    agent,
                    code: r.get(3)?,
                    title: r.get(4)?,
                    detail: r.get(5)?,
                    severity,
                    blocking: severity.is_blocking(),
                    citations: serde_json::from_str(&citations_raw).unwrap_or_default(),
                })
            })
            .map_err(|e| CaError::Storage(format!("query findings: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CaError::Storage(format!("finding row: {e}")))?);
        }
        // Worst first, then by code so two reads of an unchanged call match.
        out.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then_with(|| a.code.cmp(&b.code))
                .then_with(|| a.title.cmp(&b.title))
        });
        Ok(out)
    }

    /// Which rules the corpus breaks, by how many calls break them.
    ///
    /// Ordered so the list reads as a work queue: blocking codes first, then by
    /// reach. A critical raised on four calls outranks an advisory on forty.
    pub fn findings_summary(&self, limit: usize) -> Result<Vec<FindingCount>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT code, count(DISTINCT call_id), group_concat(severity), title
                 FROM findings GROUP BY code",
            )
            .map_err(|e| CaError::Storage(format!("prepare findings summary: {e}")))?;

        let rows = stmt
            .query_map([], |r| {
                let severities: Option<String> = r.get(2)?;
                // The worst severity the code has ever been raised at. A rule
                // shown at its mildest occurrence would understate the risk.
                let severity = severities
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(Severity::parse)
                    .max()
                    .unwrap_or(Severity::Info);
                Ok(FindingCount {
                    code: r.get(0)?,
                    calls: r.get::<_, i64>(1)?.max(0) as usize,
                    title: r.get(3)?,
                    severity,
                    blocking: severity.is_blocking(),
                })
            })
            .map_err(|e| CaError::Storage(format!("query findings summary: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CaError::Storage(format!("findings summary row: {e}")))?);
        }
        out.sort_by(|a, b| {
            b.severity
                .cmp(&a.severity)
                .then_with(|| b.calls.cmp(&a.calls))
                .then_with(|| a.code.cmp(&b.code))
        });
        out.truncate(limit);
        Ok(out)
    }

    /// How many calls have at least one finding that fails them.
    ///
    /// Counted from the ledger rather than from `ComplianceReport.passed`, so it
    /// stays correct for any agent that files findings, not only compliance.
    pub fn failing_call_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT count(DISTINCT call_id) FROM findings
             WHERE severity IN ('high', 'critical')",
            params![],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .map_err(|e| CaError::Storage(format!("failing call count: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::analysis::Finding;
    use ca_core::call::Call;
    use ca_core::ids::{FindingId, SegmentId};

    fn store_with_call(key: &str) -> (Store, CallId) {
        let s = Store::in_memory().unwrap();
        let id = s.upsert_call(&Call::new(key, "test", key)).unwrap();
        (s, id)
    }

    fn finding(call: &CallId, code: &str, severity: Severity) -> Finding {
        Finding {
            id: FindingId::new(),
            call_id: call.clone(),
            agent: AgentKind::Compliance,
            code: code.into(),
            title: format!("{code} title"),
            detail: "detail".into(),
            severity,
            citations: vec![Citation {
                segment_id: SegmentId::from("seg_1"),
                start: 12.5,
                end: 14.0,
                quote: Some("card number".into()),
            }],
        }
    }

    #[test]
    fn a_stored_finding_survives_the_round_trip_intact() {
        // The whole point of the ledger: what the agent filed is what a
        // supervisor later reads, citations included.
        let (s, id) = store_with_call("a");
        s.save_findings(&id, &[finding(&id, "PCI_PAN", Severity::Critical)])
            .unwrap();

        let out = s.findings(&id).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "PCI_PAN");
        assert_eq!(out[0].severity, Severity::Critical);
        assert!(out[0].blocking);
        assert_eq!(out[0].label, "Compliance & risk");
        assert_eq!(out[0].citations.len(), 1);
        assert!((out[0].citations[0].start - 12.5).abs() < 1e-9);
    }

    #[test]
    fn findings_are_listed_worst_first() {
        let (s, id) = store_with_call("a");
        s.save_findings(
            &id,
            &[
                finding(&id, "AAA_ADVISORY", Severity::Info),
                finding(&id, "ZZZ_SERIOUS", Severity::High),
                finding(&id, "MMM_MINOR", Severity::Low),
            ],
        )
        .unwrap();

        let codes: Vec<String> = s
            .findings(&id)
            .unwrap()
            .into_iter()
            .map(|f| f.code)
            .collect();
        assert_eq!(codes, vec!["ZZZ_SERIOUS", "MMM_MINOR", "AAA_ADVISORY"]);
    }

    #[test]
    fn a_call_with_nothing_against_it_reads_as_an_empty_list() {
        let (s, id) = store_with_call("clean");
        assert!(s.findings(&id).unwrap().is_empty());
        assert_eq!(s.failing_call_count().unwrap(), 0);
    }

    #[test]
    fn an_unparseable_severity_is_still_listed_rather_than_dropped() {
        // A row written by another build must not disappear from the register,
        // and must not be promoted to critical either.
        let (s, id) = store_with_call("a");
        s.save_findings(&id, &[finding(&id, "ODD", Severity::Medium)])
            .unwrap();
        {
            let conn = s.lock().unwrap();
            conn.execute("UPDATE findings SET severity = 'catastrophic'", [])
                .unwrap();
        }

        let out = s.findings(&id).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Info);
        assert!(!out[0].blocking);
    }

    #[test]
    fn unreadable_citations_cost_the_citations_not_the_query() {
        let (s, id) = store_with_call("a");
        s.save_findings(&id, &[finding(&id, "ODD", Severity::Low)])
            .unwrap();
        {
            let conn = s.lock().unwrap();
            conn.execute("UPDATE findings SET citations_json = 'not json'", [])
                .unwrap();
        }

        let out = s.findings(&id).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].citations.is_empty());
    }

    #[test]
    fn the_register_counts_calls_rather_than_rows() {
        // Two hits of one rule on one call is one call's worth of evidence.
        let (s, id) = store_with_call("a");
        let mut twice = finding(&id, "NO_DISCLOSURE", Severity::Medium);
        twice.title = "Missing disclosure".into();
        let mut again = finding(&id, "NO_DISCLOSURE", Severity::Medium);
        again.title = "Missing disclosure".into();
        s.save_findings(&id, &[twice, again]).unwrap();

        let register = s.findings_summary(10).unwrap();
        assert_eq!(register.len(), 1);
        assert_eq!(register[0].calls, 1);
        assert_eq!(register[0].title, "Missing disclosure");
    }

    #[test]
    fn a_rule_is_reported_at_its_worst_severity() {
        // Shown at its mildest occurrence, a rule that was once critical would
        // read as advisory.
        let (s, a) = store_with_call("a");
        s.save_findings(&a, &[finding(&a, "PCI_PAN", Severity::Low)])
            .unwrap();
        let b = s.upsert_call(&Call::new("b", "test", "b.m4a")).unwrap();
        s.save_findings(&b, &[finding(&b, "PCI_PAN", Severity::Critical)])
            .unwrap();

        let register = s.findings_summary(10).unwrap();
        assert_eq!(register[0].severity, Severity::Critical);
        assert_eq!(register[0].calls, 2);
        assert_eq!(s.failing_call_count().unwrap(), 1);
    }

    #[test]
    fn blocking_rules_outrank_widespread_advisories() {
        // The register is a work queue: a critical on one call matters more
        // than an advisory on three.
        let (s, a) = store_with_call("a");
        s.save_findings(&a, &[finding(&a, "SERIOUS", Severity::Critical)])
            .unwrap();
        for key in ["b", "c", "d"] {
            let id = s
                .upsert_call(&Call::new(key, "test", format!("{key}.m4a")))
                .unwrap();
            s.save_findings(&id, &[finding(&id, "CHATTY", Severity::Info)])
                .unwrap();
        }

        let register = s.findings_summary(10).unwrap();
        assert_eq!(register[0].code, "SERIOUS");
        assert_eq!(register[1].code, "CHATTY");
        assert_eq!(register[1].calls, 3);
    }

    #[test]
    fn re_running_an_agent_replaces_a_calls_findings_rather_than_adding_to_them() {
        // Otherwise a re-scored call accumulates every historical finding and
        // the register inflates with each run.
        let (s, id) = store_with_call("a");
        s.save_findings(&id, &[finding(&id, "OLD", Severity::High)])
            .unwrap();
        s.save_findings(&id, &[finding(&id, "NEW", Severity::Low)])
            .unwrap();

        let out = s.findings(&id).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].code, "NEW");
        assert_eq!(s.failing_call_count().unwrap(), 0);
    }

    #[test]
    fn saving_one_agents_findings_leaves_anothers_alone() {
        // The live rules file findings during the call; the compliance agent
        // files at hang-up. A delete scoped to the call rather than the agent
        // meant the battery silently erased every live alert the moment it ran.
        let (s, id) = store_with_call("a");
        let mut live = finding(&id, "LIVE_HOLD", Severity::High);
        live.agent = AgentKind::Rules;
        s.save_findings(&id, &[live]).unwrap();
        s.save_findings(&id, &[finding(&id, "COMP", Severity::Low)])
            .unwrap();

        let codes: Vec<String> = s
            .findings(&id)
            .unwrap()
            .into_iter()
            .map(|f| f.code)
            .collect();
        assert!(
            codes.contains(&"LIVE_HOLD".to_string()),
            "the rules findings were wiped: {codes:?}"
        );
        assert!(codes.contains(&"COMP".to_string()));
    }

    #[test]
    fn re_running_one_agent_still_replaces_only_its_own() {
        let (s, id) = store_with_call("a");
        let mut first = finding(&id, "OLD", Severity::High);
        first.agent = AgentKind::Rules;
        s.save_findings(&id, &[first]).unwrap();
        s.save_findings(&id, &[finding(&id, "COMP", Severity::Low)])
            .unwrap();

        let mut second = finding(&id, "NEW", Severity::Low);
        second.agent = AgentKind::Rules;
        s.save_findings(&id, &[second]).unwrap();

        let codes: Vec<String> = s
            .findings(&id)
            .unwrap()
            .into_iter()
            .map(|f| f.code)
            .collect();
        assert!(!codes.contains(&"OLD".to_string()), "stale rule survived");
        assert!(codes.contains(&"NEW".to_string()));
        assert!(codes.contains(&"COMP".to_string()), "other agent was wiped");
    }

    #[test]
    fn clearing_an_agent_is_something_you_have_to_ask_for() {
        // `save_findings(&[])` cannot know which agent it was meant to clear,
        // so a re-run that legitimately finds nothing needs its own call.
        let (s, id) = store_with_call("a");
        let mut live = finding(&id, "LIVE_HOLD", Severity::High);
        live.agent = AgentKind::Rules;
        s.save_findings(&id, &[live]).unwrap();
        s.save_findings(&id, &[finding(&id, "COMP", Severity::Low)])
            .unwrap();

        s.save_findings(&id, &[]).unwrap();
        assert_eq!(s.findings(&id).unwrap().len(), 2, "an empty list deleted");

        s.clear_findings(&id, AgentKind::Rules).unwrap();
        let codes: Vec<String> = s
            .findings(&id)
            .unwrap()
            .into_iter()
            .map(|f| f.code)
            .collect();
        assert_eq!(codes, vec!["COMP"]);
    }

    #[test]
    fn findings_cross_the_wire_as_camel_case() {
        let (s, id) = store_with_call("a");
        s.save_findings(&id, &[finding(&id, "PCI_PAN", Severity::Critical)])
            .unwrap();
        let v = serde_json::to_value(&s.findings(&id).unwrap()[0]).unwrap();
        assert!(v.get("callId").is_some(), "got {v}");
        assert_eq!(v["severity"], "critical");
    }
}
