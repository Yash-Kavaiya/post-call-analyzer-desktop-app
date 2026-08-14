//! What the collected metrics add up to.
//!
//! The collector was receiving records that nothing read: data went in and
//! nothing came out, which is not a dashboard, it is a landfill. This turns
//! the stored records into the numbers a supervisor actually asks for.
//!
//! Two properties carry over from the rest of the product and matter more here
//! than anywhere else, because this is the view furthest from the raw call:
//!
//! - **Absent is not zero.** Every mean is an `Option`. A collector holding a
//!   thousand calls that nobody has scored must not report an average QA of 0,
//!   which on a dashboard is indistinguishable from a genuinely terrible team.
//! - **People are ids.** The collector has no `people` table and never will,
//!   so a rollup names `person_a1b2c3`. A server operator can see that one
//!   agent is struggling without learning who they are; putting a name here
//!   would require syncing one, which is the thing the whole design refuses.

use ca_core::error::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::record::SyncRecord;

/// One agent's totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRollup {
    /// Opaque id. The collector cannot resolve this to a name.
    pub agent_id: String,
    pub calls: usize,
    /// `None` when none of their calls carry the measure.
    pub mean_qa: Option<f32>,
    pub mean_sentiment: Option<f32>,
    pub compliance_pass_rate: Option<f32>,
    /// Calls carrying at least one high or critical finding.
    pub failing_calls: usize,
}

/// How often one finding code was raised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRollup {
    pub code: String,
    pub severity: String,
    /// Calls that raised it, not raw occurrences: a call where the agent held
    /// four times is one call with a hold problem.
    pub calls: usize,
}

/// One month of the corpus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthRollup {
    /// `YYYY-MM`.
    pub month: String,
    pub calls: usize,
    pub mean_qa: Option<f32>,
}

/// Everything the collector can say about what it holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rollup {
    pub calls: usize,
    /// Calls with no `occurredAt`, which cannot appear in the monthly view.
    /// Reported rather than dropped, so the totals reconcile.
    pub calls_without_a_time: usize,
    pub mean_qa: Option<f32>,
    pub mean_sentiment: Option<f32>,
    pub compliance_pass_rate: Option<f32>,
    /// How many calls carry a QA score at all. A mean over 7 of 1,972 calls
    /// is a different claim from a mean over all of them, and the dashboard
    /// has no way to know that without this.
    pub scored_calls: usize,
    pub agents: Vec<AgentRollup>,
    pub codes: Vec<CodeRollup>,
    pub months: Vec<MonthRollup>,
}

fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f32)
}

/// Severities that fail a call, matching the built-in threshold.
fn is_blocking(severity: &str) -> bool {
    matches!(severity, "high" | "critical")
}

/// Summarise a set of records.
pub fn summarise(records: &[SyncRecord]) -> Rollup {
    let mut by_agent: BTreeMap<&str, Vec<&SyncRecord>> = BTreeMap::new();
    let mut by_month: BTreeMap<String, Vec<&SyncRecord>> = BTreeMap::new();
    // Counted per call, so a code raised four times on one call counts once.
    let mut by_code: BTreeMap<(&str, &str), usize> = BTreeMap::new();

    for r in records {
        if let Some(agent) = r.agent_id.as_deref() {
            by_agent.entry(agent).or_default().push(r);
        }
        if let Some(when) = r.occurred_at {
            by_month
                .entry(when.format("%Y-%m").to_string())
                .or_default()
                .push(r);
        }
        let mut seen = std::collections::HashSet::new();
        for f in &r.findings {
            if seen.insert((f.code.as_str(), f.severity.as_str())) {
                *by_code
                    .entry((f.code.as_str(), f.severity.as_str()))
                    .or_default() += 1;
            }
        }
    }

    let mut codes: Vec<CodeRollup> = by_code
        .into_iter()
        .map(|((code, severity), calls)| CodeRollup {
            code: code.to_string(),
            severity: severity.to_string(),
            calls,
        })
        .collect();
    // Blocking first, then by reach — the same order the local risk register
    // uses, so the two views rank the same problems the same way.
    codes.sort_by(|a, b| {
        is_blocking(&b.severity)
            .cmp(&is_blocking(&a.severity))
            .then(b.calls.cmp(&a.calls))
            .then(a.code.cmp(&b.code))
    });

    Rollup {
        calls: records.len(),
        calls_without_a_time: records.iter().filter(|r| r.occurred_at.is_none()).count(),
        mean_qa: mean(records.iter().filter_map(|r| r.qa_overall)),
        mean_sentiment: mean(records.iter().filter_map(|r| r.sentiment)),
        compliance_pass_rate: mean(records.iter().filter_map(|r| r.compliance_passed).map(|p| {
            if p {
                1.0
            } else {
                0.0
            }
        })),
        scored_calls: records.iter().filter(|r| r.qa_overall.is_some()).count(),
        agents: by_agent
            .into_iter()
            .map(|(agent_id, rs)| AgentRollup {
                agent_id: agent_id.to_string(),
                calls: rs.len(),
                mean_qa: mean(rs.iter().filter_map(|r| r.qa_overall)),
                mean_sentiment: mean(rs.iter().filter_map(|r| r.sentiment)),
                compliance_pass_rate: mean(
                    rs.iter()
                        .filter_map(|r| r.compliance_passed)
                        .map(|p| if p { 1.0 } else { 0.0 }),
                ),
                failing_calls: rs
                    .iter()
                    .filter(|r| r.findings.iter().any(|f| is_blocking(&f.severity)))
                    .count(),
            })
            .collect(),
        codes,
        months: by_month
            .into_iter()
            .map(|(month, rs)| MonthRollup {
                month,
                calls: rs.len(),
                mean_qa: mean(rs.iter().filter_map(|r| r.qa_overall)),
            })
            .collect(),
    }
}

/// Summarise everything a tenant's collector holds.
pub fn summarise_store(store: &ca_store::Store, limit: usize) -> Result<Rollup> {
    let rows = store.sync_records_after(0, limit)?;
    let records: Vec<SyncRecord> = rows
        .into_iter()
        .filter_map(|(_, json)| serde_json::from_str(&json).ok())
        .collect();
    Ok(summarise(&records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::FindingMetric;
    use chrono::TimeZone;

    fn record(id: &str, agent: Option<&str>, qa: Option<f32>, month: Option<u32>) -> SyncRecord {
        SyncRecord {
            call_id: id.into(),
            occurred_at: month.map(|m| chrono::Utc.with_ymd_and_hms(2025, m, 5, 12, 0, 0).unwrap()),
            duration_secs: Some(60.0),
            stage: "analyzed".into(),
            agent_id: agent.map(str::to_string),
            contact_id: None,
            qa_overall: qa,
            sentiment: None,
            compliance_passed: None,
            findings: vec![],
            updated_at: chrono::Utc::now(),
        }
    }

    /// The failure this guards is a dashboard reporting 0 for a corpus nobody
    /// has scored, which reads identically to a genuinely terrible team.
    #[test]
    fn an_unscored_corpus_reports_no_mean_rather_than_zero() {
        let records = vec![
            record("a", Some("person_1"), None, Some(1)),
            record("b", Some("person_1"), None, Some(2)),
        ];
        let r = summarise(&records);

        assert_eq!(r.calls, 2, "the calls are real and counted");
        assert_eq!(r.scored_calls, 0);
        assert_eq!(r.mean_qa, None);
        assert_eq!(r.agents[0].mean_qa, None);
    }

    /// A mean over 1 of 100 calls is a different claim from a mean over 100.
    #[test]
    fn the_rollup_says_how_many_calls_the_mean_covers() {
        let records = vec![
            record("a", Some("person_1"), Some(80.0), Some(1)),
            record("b", Some("person_1"), None, Some(1)),
            record("c", Some("person_1"), None, Some(1)),
        ];
        let r = summarise(&records);

        assert_eq!(r.calls, 3);
        assert_eq!(r.scored_calls, 1);
        assert_eq!(r.mean_qa, Some(80.0), "averaged over the one that has it");
    }

    #[test]
    fn agents_are_grouped_by_their_opaque_id() {
        let records = vec![
            record("a", Some("person_1"), Some(60.0), Some(1)),
            record("b", Some("person_1"), Some(80.0), Some(1)),
            record("c", Some("person_2"), Some(90.0), Some(1)),
        ];
        let r = summarise(&records);

        assert_eq!(r.agents.len(), 2);
        assert_eq!(r.agents[0].agent_id, "person_1");
        assert_eq!(r.agents[0].calls, 2);
        assert_eq!(r.agents[0].mean_qa, Some(70.0));
        assert_eq!(r.agents[1].mean_qa, Some(90.0));
    }

    /// Calls with no time cannot appear in a monthly view; the total has to
    /// reconcile anyway, so they are reported rather than dropped.
    #[test]
    fn calls_without_a_time_are_counted_not_hidden() {
        let records = vec![
            record("a", Some("person_1"), Some(70.0), Some(3)),
            record("b", Some("person_1"), Some(70.0), None),
        ];
        let r = summarise(&records);

        assert_eq!(r.calls, 2);
        assert_eq!(r.calls_without_a_time, 1);
        assert_eq!(r.months.len(), 1, "only the dated one can be plotted");
        assert_eq!(r.months[0].calls, 1);
    }

    /// A call where the agent held four times is one call with a hold problem,
    /// not four.
    #[test]
    fn a_code_counts_calls_rather_than_occurrences() {
        let mut a = record("a", Some("person_1"), None, Some(1));
        a.findings = vec![
            FindingMetric {
                code: "HOLD_NOT_TIMEBOXED".into(),
                severity: "high".into(),
                count: 4,
            },
            FindingMetric {
                code: "HOLD_NOT_TIMEBOXED".into(),
                severity: "high".into(),
                count: 1,
            },
        ];
        let mut b = record("b", Some("person_1"), None, Some(1));
        b.findings = vec![FindingMetric {
            code: "HOLD_NOT_TIMEBOXED".into(),
            severity: "high".into(),
            count: 1,
        }];

        let r = summarise(&[a, b]);
        assert_eq!(r.codes.len(), 1);
        assert_eq!(r.codes[0].calls, 2, "two calls, not five occurrences");
    }

    /// Same ordering as the local risk register: blocking first, then reach.
    #[test]
    fn blocking_codes_outrank_widespread_advisories() {
        let mut common = record("a", None, None, None);
        common.findings = vec![FindingMetric {
            code: "TALK_OVER".into(),
            severity: "low".into(),
            count: 1,
        }];
        let mut common2 = record("b", None, None, None);
        common2.findings = common.findings.clone();
        let mut rare = record("c", None, None, None);
        rare.findings = vec![FindingMetric {
            code: "PCI_PAN_SPOKEN".into(),
            severity: "critical".into(),
            count: 1,
        }];

        let r = summarise(&[common, common2, rare]);
        assert_eq!(
            r.codes[0].code, "PCI_PAN_SPOKEN",
            "one critical outranks two advisories"
        );
    }

    #[test]
    fn failing_calls_use_the_same_threshold_as_the_local_register() {
        let mut blocking = record("a", Some("person_1"), None, Some(1));
        blocking.findings = vec![FindingMetric {
            code: "X".into(),
            severity: "high".into(),
            count: 1,
        }];
        let mut advisory = record("b", Some("person_1"), None, Some(1));
        advisory.findings = vec![FindingMetric {
            code: "Y".into(),
            severity: "low".into(),
            count: 1,
        }];

        let r = summarise(&[blocking, advisory]);
        assert_eq!(r.agents[0].failing_calls, 1, "low does not fail a call");
    }

    #[test]
    fn an_empty_collector_summarises_to_nothing_rather_than_failing() {
        let r = summarise(&[]);
        assert_eq!(r.calls, 0);
        assert_eq!(r.mean_qa, None);
        assert!(r.agents.is_empty() && r.codes.is_empty() && r.months.is_empty());
    }
}
