//! What each agent did, cost, and whether it worked.
//!
//! The `analyses` table already carries provider, model, tokens, cost, latency
//! and error per agent per call — it just had no reader. Without one, a batch
//! that silently degraded (one agent failing on every call, or a router quietly
//! substituting a cheaper model) looks identical to a healthy one, because the
//! calls still reach `analyzed`.
//!
//! Two views: per call, for the detail page; and aggregated per agent, for
//! answering "is anything broken, and what is this costing".

use crate::Store;
use ca_core::error::{CaError, Result};
use ca_core::ids::CallId;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// One agent's run against one call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    pub agent: String,
    /// Display name, so the UI needs no mapping table of its own.
    pub label: String,
    /// The agent's output. `None` when the run failed.
    pub payload: Option<serde_json::Value>,
    pub provider: String,
    /// The model that actually answered, which is not always the one asked for.
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: Option<f64>,
    pub latency_ms: u64,
    pub error: Option<String>,
    pub created_at: String,
}

/// Health and spend for one agent across every call it has run on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHealth {
    pub agent: String,
    pub label: String,
    pub runs: u32,
    pub failures: u32,
    /// 0..=1. The number worth alerting on.
    pub failure_rate: f32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub mean_latency_ms: u64,
    /// Distinct models that have answered for this agent. More than one means
    /// a router substituted, and results are not comparable across them.
    pub models: Vec<String>,
    pub last_error: Option<String>,
}

impl Store {
    /// Every agent's run for one call, newest first.
    pub fn agent_runs(&self, call_id: &CallId) -> Result<Vec<AgentRun>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT agent, payload_json, provider, model, input_tokens, output_tokens,
                        cost_usd, latency_ms, error, created_at
                 FROM analyses WHERE call_id = ?1 ORDER BY created_at DESC, agent",
            )
            .map_err(|e| CaError::Storage(format!("prepare agent runs: {e}")))?;

        let rows = stmt
            .query_map(params![call_id.as_str()], |r| {
                let agent: String = r.get(0)?;
                let payload_raw: String = r.get(1)?;
                let error: Option<String> = r.get(8)?;
                Ok(AgentRun {
                    label: ca_core::analysis::AgentKind::parse(&agent)
                        .map(|k| k.label().to_string())
                        .unwrap_or_else(|| agent.clone()),
                    agent,
                    // A failed run stores no usable payload; `null` would render
                    // as an empty panel rather than as the failure it was.
                    payload: if error.is_some() {
                        None
                    } else {
                        serde_json::from_str(&payload_raw).ok()
                    },
                    provider: r.get(2)?,
                    model: r.get(3)?,
                    input_tokens: r.get(4)?,
                    output_tokens: r.get(5)?,
                    cost_usd: r.get(6)?,
                    latency_ms: r.get::<_, i64>(7)?.max(0) as u64,
                    error,
                    created_at: r.get(9)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query agent runs: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CaError::Storage(format!("agent run row: {e}")))?);
        }
        Ok(out)
    }

    /// Health and spend per agent across the whole corpus.
    pub fn agent_health(&self) -> Result<Vec<AgentHealth>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare(
                "SELECT agent,
                        count(*),
                        COALESCE(sum(error IS NOT NULL), 0),
                        COALESCE(sum(input_tokens), 0),
                        COALESCE(sum(output_tokens), 0),
                        COALESCE(sum(cost_usd), 0),
                        COALESCE(avg(latency_ms), 0),
                        group_concat(DISTINCT model),
                        max(error)
                 FROM analyses GROUP BY agent ORDER BY agent",
            )
            .map_err(|e| CaError::Storage(format!("prepare agent health: {e}")))?;

        let rows = stmt
            .query_map([], |r| {
                let agent: String = r.get(0)?;
                let runs: u32 = r.get(1)?;
                let failures: u32 = r.get(2)?;
                let models: Option<String> = r.get(7)?;
                Ok(AgentHealth {
                    label: ca_core::analysis::AgentKind::parse(&agent)
                        .map(|k| k.label().to_string())
                        .unwrap_or_else(|| agent.clone()),
                    agent,
                    runs,
                    failures,
                    failure_rate: if runs == 0 {
                        0.0
                    } else {
                        failures as f32 / runs as f32
                    },
                    // SQLite integers are signed; `u64` has no `FromSql`.
                    input_tokens: r.get::<_, i64>(3)?.max(0) as u64,
                    output_tokens: r.get::<_, i64>(4)?.max(0) as u64,
                    cost_usd: r.get(5)?,
                    mean_latency_ms: r.get::<_, f64>(6)?.max(0.0) as u64,
                    models: models
                        .map(|m| m.split(',').map(|s| s.trim().to_string()).collect())
                        .unwrap_or_default(),
                    last_error: r.get(8)?,
                })
            })
            .map_err(|e| CaError::Storage(format!("query agent health: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| CaError::Storage(format!("agent health row: {e}")))?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::analysis::{AgentKind, Analysis, Usage};
    use ca_core::call::Call;
    use ca_core::ids::AnalysisId;

    fn store_with_call() -> (Store, CallId) {
        let s = Store::in_memory().unwrap();
        let call = Call::new("k", "test", "a.m4a");
        let id = s.upsert_call(&call).unwrap();
        (s, id)
    }

    fn analysis(call: &CallId, agent: AgentKind, error: Option<&str>) -> Analysis {
        Analysis {
            id: AnalysisId::new(),
            call_id: call.clone(),
            agent,
            payload: serde_json::json!({"ok": true}),
            provider: "stub".into(),
            model: "stub-1".into(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                cost_usd: Some(0.25),
                latency_ms: 400,
            },
            created_at: chrono::Utc::now(),
            error: error.map(|e| e.to_string()),
        }
    }

    #[test]
    fn a_successful_run_carries_its_payload_and_accounting() {
        let (s, id) = store_with_call();
        s.save_analysis(&analysis(&id, AgentKind::QaScorecard, None))
            .unwrap();

        let runs = s.agent_runs(&id).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].agent, "qa_scorecard");
        assert_eq!(runs[0].label, "QA scorecard");
        assert!(runs[0].payload.is_some());
        assert_eq!(runs[0].cost_usd, Some(0.25));
        assert_eq!(runs[0].latency_ms, 400);
        assert!(runs[0].error.is_none());
    }

    #[test]
    fn a_failed_run_reports_no_payload_rather_than_an_empty_one() {
        // `null` would render as a blank panel, which reads as "this agent found
        // nothing" instead of "this agent did not run".
        let (s, id) = store_with_call();
        s.save_analysis(&analysis(&id, AgentKind::Coaching, Some("rate limited")))
            .unwrap();

        let runs = s.agent_runs(&id).unwrap();
        assert!(runs[0].payload.is_none());
        assert_eq!(runs[0].error.as_deref(), Some("rate limited"));
    }

    #[test]
    fn health_aggregates_runs_failures_and_spend_per_agent() {
        let (s, id) = store_with_call();
        s.save_analysis(&analysis(&id, AgentKind::QaScorecard, None))
            .unwrap();

        let call2 = Call::new("k2", "test", "b.m4a");
        let id2 = s.upsert_call(&call2).unwrap();
        s.save_analysis(&analysis(&id2, AgentKind::QaScorecard, Some("boom")))
            .unwrap();

        let health = s.agent_health().unwrap();
        let qa = health.iter().find(|h| h.agent == "qa_scorecard").unwrap();
        assert_eq!(qa.runs, 2);
        assert_eq!(qa.failures, 1);
        assert!((qa.failure_rate - 0.5).abs() < 1e-6);
        assert!((qa.cost_usd - 0.5).abs() < 1e-6);
        assert_eq!(qa.mean_latency_ms, 400);
        assert_eq!(qa.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn more_than_one_model_answering_is_visible() {
        // A router substituting a cheaper model changes what the scores mean,
        // and nothing else in the app would show it.
        let (s, id) = store_with_call();
        s.save_analysis(&analysis(&id, AgentKind::Sentiment, None))
            .unwrap();

        let call2 = Call::new("k2", "test", "b.m4a");
        let id2 = s.upsert_call(&call2).unwrap();
        let mut other = analysis(&id2, AgentKind::Sentiment, None);
        other.model = "substituted-8b".into();
        s.save_analysis(&other).unwrap();

        let health = s.agent_health().unwrap();
        let sentiment = health.iter().find(|h| h.agent == "sentiment").unwrap();
        assert_eq!(sentiment.models.len(), 2, "got {:?}", sentiment.models);
    }

    #[test]
    fn an_agent_that_never_ran_is_absent_rather_than_zeroed() {
        // Absent and "ran 0 times" are different states; inventing a zero row
        // would claim an agent exists in this build's history when it does not.
        let (s, id) = store_with_call();
        s.save_analysis(&analysis(&id, AgentKind::QaScorecard, None))
            .unwrap();
        let health = s.agent_health().unwrap();
        assert_eq!(health.len(), 1);
    }

    #[test]
    fn a_call_with_no_analyses_reads_as_an_empty_list() {
        let (s, id) = store_with_call();
        assert!(s.agent_runs(&id).unwrap().is_empty());
    }
}
