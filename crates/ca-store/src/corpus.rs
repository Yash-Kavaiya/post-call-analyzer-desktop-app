//! Assembling corpus-wide facts from what each agent stored.
//!
//! `ca-insights` does the arithmetic and knows nothing about SQLite; this reads
//! the rows and flattens each call into the handful of fields that aggregation
//! needs. Keeping the two apart is what lets the pattern logic be tested on
//! literals rather than against a database.
//!
//! Payloads are read defensively. An agent's shape can change between versions,
//! and a corpus report that fails entirely because one call has an older
//! scorecard is worse than one that quietly omits it.

use crate::Store;
use ca_core::error::{CaError, Result};
use ca_insights::CallFacts;
use rusqlite::params;
use std::collections::HashMap;

impl Store {
    /// One [`CallFacts`] per call that has at least one analysis.
    ///
    /// Calls with no analysis are excluded rather than included as blanks: the
    /// question this answers is "what do the analysed calls show", and padding
    /// it with unprocessed ones would drag every mean toward nothing.
    pub fn corpus_facts(&self, limit: usize) -> Result<Vec<CallFacts>> {
        let conn = self.lock()?;

        let mut stmt = conn
            .prepare(
                "SELECT call_id, agent, payload_json FROM analyses
                 WHERE error IS NULL
                 ORDER BY call_id",
            )
            .map_err(|e| CaError::Storage(format!("prepare corpus facts: {e}")))?;

        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| CaError::Storage(format!("query corpus facts: {e}")))?;

        let mut by_call: HashMap<String, CallFacts> = HashMap::new();

        for row in rows {
            let (call_id, agent, payload) =
                row.map_err(|e| CaError::Storage(format!("corpus row: {e}")))?;
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) else {
                continue;
            };
            // Rows predating the camelCase rename are on disk in snake_case.
            // Normalising here means each extraction below names one key rather
            // than two, and cannot be updated for one spelling and not the other.
            let v = crate::payload::normalise(v);

            let facts = by_call.entry(call_id.clone()).or_insert_with(|| CallFacts {
                call_id: call_id.clone(),
                ..Default::default()
            });

            match agent.as_str() {
                "qa_scorecard" => {
                    facts.qa_overall = v.get("overall").and_then(|x| x.as_f64()).map(|x| x as f32);
                }
                "customer_experience" => {
                    facts.escalation_risk = v
                        .get("escalationRisk")
                        .and_then(|x| x.as_f64())
                        .map(|x| x as f32);
                    facts.churn_risk = v
                        .get("churnRisk")
                        .and_then(|x| x.as_f64())
                        .map(|x| x as f32);
                    facts.frustration = v
                        .get("frustration")
                        .and_then(|x| x.as_f64())
                        .map(|x| x as f32);
                }
                "sentiment" => {
                    facts.sentiment = v.get("overall").and_then(|x| x.as_f64()).map(|x| x as f32);
                }
                "compliance" => {
                    if let Some(findings) = v.get("findings").and_then(|x| x.as_array()) {
                        for f in findings {
                            if let Some(code) = f.get("code").and_then(|x| x.as_str()) {
                                facts.compliance_codes.push(code.to_string());
                            }
                        }
                    }
                    // Absent rather than false when the field cannot be read: a
                    // call the agent never judged must not be counted as a
                    // failure in the pass rate.
                    facts.compliance_passed = v.get("passed").and_then(|x| x.as_bool());
                }
                "root_cause" => {
                    // `primaryCause` is what `RootCauseReport` serialises. This
                    // read looked only for `primaryIssue`, so every root-cause
                    // row missed and the corpus panel was quietly being filled
                    // by the executive summary instead. `rootCause` is kept as a
                    // fallback for any row written under that earlier name.
                    facts.primary_issue = v
                        .get("primaryCause")
                        .or_else(|| v.get("primaryIssue"))
                        .or_else(|| v.get("rootCause"))
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                    facts.root_cause_category = v
                        .get("category")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                }
                // Only if root cause did not already provide one — that
                // agent is the more specific source for this field.
                "executive_summary" if facts.primary_issue.is_none() => {
                    facts.primary_issue = v
                        .get("primaryIssue")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string());
                }
                _ => {}
            }
        }

        // Keywords come from the transcript, not from an agent, so they are
        // available for every transcribed call and cost nothing.
        let mut kw = conn
            .prepare(
                "SELECT call_id, COALESCE(redacted_text, text) FROM segments
                 ORDER BY call_id, idx",
            )
            .map_err(|e| CaError::Storage(format!("prepare keywords: {e}")))?;

        let mut texts: HashMap<String, String> = HashMap::new();
        let rows = kw
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| CaError::Storage(format!("query keywords: {e}")))?;
        for row in rows {
            let (call_id, text) = row.map_err(|e| CaError::Storage(format!("kw row: {e}")))?;
            if by_call.contains_key(&call_id) {
                texts
                    .entry(call_id)
                    .or_default()
                    .push_str(&format!(" {text}"));
            }
        }

        for (call_id, text) in texts {
            if let Some(facts) = by_call.get_mut(&call_id) {
                facts.keywords = ca_metrics::keywords_in(&text);
            }
        }

        // Participants live on `calls`, not in a payload, so they are filled in
        // with one pass rather than joined per analysis row.
        let mut who = conn
            .prepare("SELECT id, agent_id, contact_id FROM calls WHERE agent_id IS NOT NULL OR contact_id IS NOT NULL")
            .map_err(|e| CaError::Storage(format!("prepare participants: {e}")))?;
        let rows = who
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| CaError::Storage(format!("query participants: {e}")))?;
        for row in rows {
            let (call_id, agent_id, contact_id) =
                row.map_err(|e| CaError::Storage(format!("participant row: {e}")))?;
            if let Some(facts) = by_call.get_mut(&call_id) {
                facts.agent_id = agent_id;
                facts.contact_id = contact_id;
            }
        }

        let mut out: Vec<CallFacts> = by_call.into_values().collect();
        // Stable order so two reports on an unchanged corpus are identical.
        out.sort_by(|a, b| a.call_id.cmp(&b.call_id));
        out.truncate(limit);
        Ok(out)
    }

    /// Which calls the corpus report is drawn from, for the "n analysed" line.
    pub fn analysed_call_count(&self) -> Result<usize> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT count(DISTINCT call_id) FROM analyses WHERE error IS NULL",
            params![],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as usize)
        .map_err(|e| CaError::Storage(format!("analysed count: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::analysis::{AgentKind, Analysis, Usage};
    use ca_core::call::Call;
    use ca_core::ids::{AnalysisId, CallId};

    fn store() -> Store {
        Store::in_memory().unwrap()
    }

    fn add_call(s: &Store, key: &str) -> CallId {
        s.upsert_call(&Call::new(key, "test", key)).unwrap()
    }

    fn add_analysis(s: &Store, call: &CallId, agent: AgentKind, payload: serde_json::Value) {
        s.save_analysis(&Analysis {
            id: AnalysisId::new(),
            call_id: call.clone(),
            agent,
            payload,
            provider: "stub".into(),
            model: "stub-1".into(),
            usage: Usage::default(),
            created_at: chrono::Utc::now(),
            error: None,
        })
        .unwrap();
    }

    #[test]
    fn a_calls_scores_are_gathered_from_the_agents_that_produced_them() {
        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::QaScorecard,
            serde_json::json!({"overall": 82.5}),
        );
        add_analysis(
            &s,
            &id,
            AgentKind::CustomerExperience,
            serde_json::json!({"escalationRisk": 0.07, "churnRisk": 0.04, "frustration": 0.23}),
        );

        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].qa_overall, Some(82.5));
        assert_eq!(facts[0].escalation_risk, Some(0.07));
        assert_eq!(facts[0].churn_risk, Some(0.04));
    }

    #[test]
    fn a_call_with_no_analysis_is_absent_rather_than_blank() {
        // Padding the corpus with unprocessed calls would drag every mean
        // toward nothing and misreport how much has actually been assessed.
        let s = store();
        add_call(&s, "never-analysed");
        assert!(s.corpus_facts(100).unwrap().is_empty());
        assert_eq!(s.analysed_call_count().unwrap(), 0);
    }

    #[test]
    fn a_failed_analysis_does_not_contribute_facts() {
        let s = store();
        let id = add_call(&s, "a");
        s.save_analysis(&Analysis {
            id: AnalysisId::new(),
            call_id: id.clone(),
            agent: AgentKind::QaScorecard,
            payload: serde_json::json!(null),
            provider: "stub".into(),
            model: "stub-1".into(),
            usage: Usage::default(),
            created_at: chrono::Utc::now(),
            error: Some("rate limited".into()),
        })
        .unwrap();

        assert!(s.corpus_facts(100).unwrap().is_empty());
    }

    #[test]
    fn an_unreadable_payload_is_skipped_rather_than_failing_the_report() {
        // One call with an older shape must not take the whole corpus view down.
        let s = store();
        let good = add_call(&s, "good");
        add_analysis(
            &s,
            &good,
            AgentKind::QaScorecard,
            serde_json::json!({"overall": 90.0}),
        );

        let bad = add_call(&s, "bad");
        add_analysis(
            &s,
            &bad,
            AgentKind::QaScorecard,
            serde_json::json!({"overall": "ninety"}),
        );

        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(facts.len(), 2);
        let bad_facts = facts.iter().find(|f| f.call_id == bad.as_str()).unwrap();
        assert!(
            bad_facts.qa_overall.is_none(),
            "a string score was accepted"
        );
    }

    #[test]
    fn compliance_codes_are_collected_from_the_findings_list() {
        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::Compliance,
            serde_json::json!({"findings": [
                {"code": "missing_disclosure"},
                {"code": "pii_exposure"}
            ]}),
        );
        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(facts[0].compliance_codes.len(), 2);
    }

    #[test]
    fn root_cause_wins_over_the_executive_summary_for_the_primary_issue() {
        // Both agents name an issue; the specialised one is the better source.
        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::ExecutiveSummary,
            serde_json::json!({"primaryIssue": "Something vague"}),
        );
        add_analysis(
            &s,
            &id,
            AgentKind::RootCause,
            serde_json::json!({"primaryIssue": "Payment gateway timeout"}),
        );
        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(
            facts[0].primary_issue.as_deref(),
            Some("Payment gateway timeout")
        );
    }

    #[test]
    fn the_root_cause_agents_own_field_name_is_the_one_that_is_read() {
        // `RootCauseReport` serialises `primaryCause`. This read looked only for
        // `primaryIssue`, so every root-cause row missed and the corpus panel
        // was filled by the executive summary — which is a different question.
        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::ExecutiveSummary,
            serde_json::json!({"primaryIssue": "Customer was unhappy"}),
        );
        add_analysis(
            &s,
            &id,
            AgentKind::RootCause,
            serde_json::json!({
                "primaryCause": "Renewal charged twice",
                "category": "billing",
            }),
        );

        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(
            facts[0].primary_issue.as_deref(),
            Some("Renewal charged twice")
        );
        assert_eq!(facts[0].root_cause_category.as_deref(), Some("billing"));
    }

    #[test]
    fn the_shape_the_agents_actually_write_is_the_shape_that_is_read() {
        // Guards every extraction against a field rename in `ca-core`: the
        // payloads here are serialised from the real report types rather than
        // hand-written JSON, so a rename fails this test instead of silently
        // emptying the corpus report.
        use ca_core::analysis::{ComplianceReport, RootCauseReport, SentimentReport};

        let s = store();
        let id = add_call(&s, "a");

        add_analysis(
            &s,
            &id,
            AgentKind::Sentiment,
            serde_json::to_value(SentimentReport {
                call_id: id.clone(),
                overall: -0.62,
                trajectory: vec![],
                summary: "Unhappy.".into(),
            })
            .unwrap(),
        );
        add_analysis(
            &s,
            &id,
            AgentKind::Compliance,
            serde_json::to_value(ComplianceReport {
                call_id: id.clone(),
                passed: false,
                findings: vec![],
                summary: "Failed.".into(),
            })
            .unwrap(),
        );
        add_analysis(
            &s,
            &id,
            AgentKind::RootCause,
            serde_json::to_value(RootCauseReport {
                call_id: id.clone(),
                primary_cause: "Renewal charged twice".into(),
                category: "billing".into(),
                confidence: 0.8,
                contributing_factors: vec![],
                recommended_fix: "Reconcile".into(),
                citations: vec![],
                summary: "Billing.".into(),
            })
            .unwrap(),
        );

        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(facts[0].sentiment, Some(-0.62));
        assert_eq!(facts[0].compliance_passed, Some(false));
        assert_eq!(
            facts[0].primary_issue.as_deref(),
            Some("Renewal charged twice")
        );
        assert_eq!(facts[0].root_cause_category.as_deref(), Some("billing"));
    }

    #[test]
    fn a_row_written_before_the_camel_case_rename_still_contributes() {
        // A real corpus holds both shapes: the `analyses` table is keyed
        // (call_id, agent), so an old row only changes when that agent is
        // re-run, which for a paid batch is not something to require.
        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::RootCause,
            serde_json::json!({
                "call_id": "a",
                "primary_cause": "Renewal charged twice",
                "category": "billing",
                "contributing_factors": ["retry"],
            }),
        );

        let facts = s.corpus_facts(100).unwrap();
        assert_eq!(
            facts[0].primary_issue.as_deref(),
            Some("Renewal charged twice")
        );
        assert_eq!(facts[0].root_cause_category.as_deref(), Some("billing"));
    }

    #[test]
    fn a_compliance_agent_that_never_ran_leaves_no_verdict() {
        // `None` and `Some(false)` are different states: one is unassessed, the
        // other is a failed call, and the pass rate must not conflate them.
        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::QaScorecard,
            serde_json::json!({"overall": 80.0}),
        );
        let facts = s.corpus_facts(100).unwrap();
        assert!(facts[0].compliance_passed.is_none());
        assert!(facts[0].sentiment.is_none());
    }

    #[test]
    fn keywords_come_from_the_redacted_transcript() {
        use ca_core::ids::SegmentId;
        use ca_core::transcript::{Segment, SpeakerRole, Transcript};

        let s = store();
        let id = add_call(&s, "a");
        add_analysis(
            &s,
            &id,
            AgentKind::QaScorecard,
            serde_json::json!({"overall": 80.0}),
        );

        let seg = |i: &str, text: &str, redacted: &str| Segment {
            id: SegmentId::from(i),
            speaker_index: 0,
            role: SpeakerRole::Agent,
            start: 0.0,
            end: 5.0,
            text: text.into(),
            redacted_text: Some(redacted.into()),
            words: vec![],
            confidence: None,
            language: None,
        };

        s.save_transcript(&Transcript {
            call_id: id.clone(),
            segments: vec![
                seg("s1", "refund on 9876543210", "refund on [PHONE_1]"),
                seg("s2", "refund again", "refund again"),
            ],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: 10.0,
        })
        .unwrap();

        let facts = s.corpus_facts(100).unwrap();
        assert!(facts[0].keywords.iter().any(|k| k == "refund"));
        assert!(
            !facts[0].keywords.iter().any(|k| k.contains("9876")),
            "raw PII became a corpus keyword"
        );
    }
}
