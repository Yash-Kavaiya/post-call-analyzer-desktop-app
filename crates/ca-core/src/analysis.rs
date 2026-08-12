use crate::ids::{AnalysisId, CallId, FindingId, SegmentId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which agent produced a result. Adding an agent means adding a variant here
/// and an impl in `ca-agents` — the pipeline itself stays unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    QaScorecard,
    Compliance,
    Sentiment,
    RootCause,
    ActionItems,
    Coaching,
    CustomerExperience,
    ExecutiveSummary,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::QaScorecard => "qa_scorecard",
            AgentKind::Compliance => "compliance",
            AgentKind::Sentiment => "sentiment",
            AgentKind::RootCause => "root_cause",
            AgentKind::ActionItems => "action_items",
            AgentKind::Coaching => "coaching",
            AgentKind::CustomerExperience => "customer_experience",
            AgentKind::ExecutiveSummary => "executive_summary",
        }
    }

    /// Display name, for the UI and for run labels.
    pub fn label(&self) -> &'static str {
        match self {
            AgentKind::QaScorecard => "QA scorecard",
            AgentKind::Compliance => "Compliance & risk",
            AgentKind::Sentiment => "Sentiment",
            AgentKind::RootCause => "Root cause & intent",
            AgentKind::ActionItems => "Action items",
            AgentKind::Coaching => "Coaching",
            AgentKind::CustomerExperience => "Customer experience",
            AgentKind::ExecutiveSummary => "Executive summary",
        }
    }

    pub fn all() -> &'static [AgentKind] {
        &[
            AgentKind::QaScorecard,
            AgentKind::Compliance,
            AgentKind::Sentiment,
            AgentKind::RootCause,
            AgentKind::ActionItems,
            AgentKind::Coaching,
            AgentKind::CustomerExperience,
            AgentKind::ExecutiveSummary,
        ]
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        AgentKind::all().iter().copied().find(|k| k.as_str() == s)
    }
}

/// A pointer from a model claim back into the transcript.
///
/// Citations are *validated* against real segments before persistence; a model
/// that invents a segment id or a timestamp outside the call gets its finding
/// rejected. This is the main defence against confident fabrication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub segment_id: SegmentId,
    pub start: f64,
    pub end: f64,
    /// The quoted span the model claims supports its point.
    pub quote: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// A single scored criterion on the QA scorecard.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CriterionScore {
    pub key: String,
    pub label: String,
    /// 0.0..=1.0 before weighting.
    pub score: f32,
    pub weight: f32,
    pub reasoning: String,
    pub citations: Vec<Citation>,
    /// True when the model declined to score for lack of evidence.
    pub not_applicable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scorecard {
    pub call_id: CallId,
    /// Weighted total, 0..=100.
    pub overall: f32,
    pub criteria: Vec<CriterionScore>,
    pub summary: String,
    pub rubric_id: String,
    pub rubric_version: String,
}

impl Scorecard {
    /// Recomputes `overall` from the criteria rather than trusting the model's
    /// arithmetic. N/A criteria are excluded and the remaining weights renormalised.
    pub fn compute_overall(criteria: &[CriterionScore]) -> f32 {
        let scored: Vec<&CriterionScore> = criteria.iter().filter(|c| !c.not_applicable).collect();
        let total_weight: f32 = scored.iter().map(|c| c.weight).sum();
        if total_weight <= 0.0 {
            return 0.0;
        }
        let weighted: f32 = scored
            .iter()
            .map(|c| c.score.clamp(0.0, 1.0) * c.weight)
            .sum();
        (weighted / total_weight * 100.0).clamp(0.0, 100.0)
    }
}

/// A compliance, risk, or friction observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: FindingId,
    pub call_id: CallId,
    pub agent: AgentKind,
    pub code: String,
    pub title: String,
    pub detail: String,
    pub severity: Severity,
    pub citations: Vec<Citation>,
}

/// One labelled moment on the call's sentiment trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentimentPoint {
    /// Seconds from call start (taken from a cited segment when possible).
    pub t: f64,
    /// −1.0 (very negative) ..= 1.0 (very positive).
    pub score: f32,
    pub label: String,
    /// "agent" | "customer" | "overall" — free text from the model, clamped later.
    pub speaker: String,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentimentReport {
    pub call_id: CallId,
    /// Whole-call customer sentiment, −1..=1.
    pub overall: f32,
    pub trajectory: Vec<SentimentPoint>,
    pub summary: String,
}

/// Why the customer called and what should change next.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootCauseReport {
    pub call_id: CallId,
    pub primary_cause: String,
    /// Coarse bucket for dashboards (billing, product, process, agent, other).
    pub category: String,
    pub confidence: f32,
    pub contributing_factors: Vec<String>,
    pub recommended_fix: String,
    pub citations: Vec<Citation>,
    pub summary: String,
}

/// Policy / script adherence rollup. Individual items also land in `findings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceReport {
    pub call_id: CallId,
    pub passed: bool,
    pub findings: Vec<Finding>,
    pub summary: String,
}

/// One follow-up the business or agent owes after the call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub title: String,
    pub detail: String,
    /// Who should act: agent | supervisor | backoffice | customer | system.
    pub owner: String,
    /// low | medium | high
    pub priority: String,
    /// Optional due hint from the conversation (free text, may be empty).
    pub due: String,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItemsReport {
    pub call_id: CallId,
    pub items: Vec<ActionItem>,
    pub summary: String,
}

/// One coachable moment.
///
/// "Improve empathy" is not coaching. A usable note names what happened, points
/// at where it happened, and says what to do instead — the three parts a
/// supervisor needs to actually run the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachingNote {
    /// Short handle, e.g. "interrupted the customer".
    pub behaviour: String,
    /// What the agent actually did, in a sentence or two.
    pub observation: String,
    /// What to do instead.
    pub recommendation: String,
    /// Wording the agent could use verbatim. Not every note warrants one.
    pub suggested_phrase: Option<String>,
    pub severity: Severity,
    pub citations: Vec<Citation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoachingReport {
    pub call_id: CallId,
    /// What went well. Coaching that is only criticism gets ignored, and the
    /// behaviours worth repeating are the ones a supervisor wants to name.
    pub strengths: Vec<CoachingNote>,
    pub improvements: Vec<CoachingNote>,
    pub summary: String,
}

/// How the call felt from the customer's side.
///
/// Every figure is 0..=1 and every one is a model estimate rather than a
/// measurement — unlike the conversation metrics, which are arithmetic. The UI
/// is expected to present them as judgements.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomerExperienceReport {
    pub call_id: CallId,
    /// How hard the customer had to work to get an outcome.
    pub effort: f32,
    pub frustration: f32,
    /// Confidence that the issue was actually resolved, not just closed.
    pub resolution_confidence: f32,
    pub escalation_risk: f32,
    pub churn_risk: f32,
    pub summary: String,
    pub citations: Vec<Citation>,
}

impl CustomerExperienceReport {
    /// Force every score into range.
    ///
    /// A model that returns 1.4 for frustration renders as a bar overflowing
    /// its track, and one that returns -0.2 renders as nothing at all.
    pub fn clamped(mut self) -> Self {
        for v in [
            &mut self.effort,
            &mut self.frustration,
            &mut self.resolution_confidence,
            &mut self.escalation_risk,
            &mut self.churn_risk,
        ] {
            *v = v.clamp(0.0, 1.0);
        }
        self
    }
}

/// The paragraph a supervisor reads instead of listening to the call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutiveSummary {
    pub call_id: CallId,
    pub summary: String,
    pub primary_issue: String,
    pub resolution: String,
    /// positive | neutral | negative
    pub customer_outcome: String,
    pub risk: Severity,
    pub citations: Vec<Citation>,
}

/// Token and cost accounting for one provider call. Tracked per analysis so a
/// 1,968-call batch can be costed before and after it runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: Option<f64>,
    pub latency_ms: u64,
}

/// The persisted envelope around whatever an agent produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub id: AnalysisId,
    pub call_id: CallId,
    pub agent: AgentKind,
    /// Agent-specific payload (a `Scorecard`, a sentiment series, etc.).
    pub payload: serde_json::Value,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub created_at: DateTime<Utc>,
    /// Set when the agent ran but its output failed validation.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(score: f32, weight: f32, na: bool) -> CriterionScore {
        CriterionScore {
            key: "k".into(),
            label: "K".into(),
            score,
            weight,
            reasoning: String::new(),
            citations: vec![],
            not_applicable: na,
        }
    }

    #[test]
    fn overall_is_weighted_mean_scaled_to_100() {
        let s = Scorecard::compute_overall(&[c(1.0, 1.0, false), c(0.0, 1.0, false)]);
        assert!((s - 50.0).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn weights_are_respected() {
        let s = Scorecard::compute_overall(&[c(1.0, 3.0, false), c(0.0, 1.0, false)]);
        assert!((s - 75.0).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn not_applicable_criteria_are_excluded_and_weights_renormalise() {
        // The N/A criterion must not drag the score down as if it scored zero.
        let s = Scorecard::compute_overall(&[c(1.0, 1.0, false), c(0.0, 9.0, true)]);
        assert!((s - 100.0).abs() < 1e-4, "got {s}");
    }

    #[test]
    fn all_na_yields_zero_rather_than_nan() {
        let s = Scorecard::compute_overall(&[c(1.0, 1.0, true)]);
        assert_eq!(s, 0.0);
    }

    #[test]
    fn out_of_range_model_scores_are_clamped() {
        let s = Scorecard::compute_overall(&[c(5.0, 1.0, false)]);
        assert!((s - 100.0).abs() < 1e-4, "got {s}");
    }
}
