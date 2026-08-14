//! Cross-call aggregation and pattern discovery.
//!
//! What a supervisor wants from 18,000 calls is not 18,000 summaries. It is:
//! what keeps happening, what is getting worse, and where the risk is
//! concentrated. That is arithmetic over facts already extracted per call, so
//! it is done here rather than by asking a model to read summaries of summaries
//! — which costs money per call and produces a different answer each run.
//!
//! **On patterns.** This finds *associations*, and says so. "Calls mentioning
//! refund escalate 2.7× more often" is a fact about the corpus; it is not
//! evidence that refunds cause escalation, and the report never phrases it that
//! way. Two guards keep the output from being misleading:
//!
//! - A minimum sample on **both sides** of every comparison. A term appearing
//!   in three calls will produce a spectacular ratio and mean nothing.
//! - A minimum effect size, so the list is not padded with noise that happens
//!   to be non-zero.

pub mod person;

pub use person::{
    monthly_trend, person_facts, repeat_contact_rate, CallRef, MonthPoint, PersonFacts,
};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Calls needed on each side of a comparison before it is reported.
///
/// Below this the ratio is dominated by whichever handful of calls happened to
/// land in the smaller group.
const MIN_SAMPLE: usize = 8;

/// Smallest relative difference worth showing, as a ratio away from 1.0.
const MIN_LIFT: f64 = 0.25;

const MAX_PATTERNS: usize = 12;
const MAX_TERMS_CONSIDERED: usize = 60;

/// Below this, a call counts as one where the customer left unhappy.
///
/// Not zero: a score of −0.05 is a neutral call with rounding on it, and
/// counting those would report half a healthy corpus as negative.
const NEGATIVE_SENTIMENT: f32 = -0.2;

/// What one call contributes to the corpus view.
///
/// Assembled by the caller from stored analyses and transcripts, so this crate
/// stays free of storage and can be tested on literals.
#[derive(Debug, Clone, Default)]
pub struct CallFacts {
    pub call_id: String,
    /// QA scorecard total, 0..=100.
    pub qa_overall: Option<f32>,
    /// Customer-experience estimates, 0..=1.
    pub escalation_risk: Option<f32>,
    pub churn_risk: Option<f32>,
    pub frustration: Option<f32>,
    /// Whole-call customer sentiment, −1..=1.
    pub sentiment: Option<f32>,
    /// Terms said more than once on this call.
    pub keywords: Vec<String>,
    /// Compliance finding codes raised against this call.
    pub compliance_codes: Vec<String>,
    /// The compliance agent's verdict. `None` when it did not run — which is
    /// not the same as passing, and must not be counted as either.
    pub compliance_passed: Option<bool>,
    /// Root-cause label, when the agent produced one.
    pub primary_issue: Option<String>,
    /// The root-cause agent's coarse bucket: billing, product, process, agent
    /// or other. Free-text causes do not aggregate; these do.
    pub root_cause_category: Option<String>,
    /// Who handled the call, when it has been attributed. Lets a corpus report
    /// be scoped to one person or team without a second query.
    pub agent_id: Option<String>,
    /// The other party, when attributed.
    pub contact_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Counted {
    pub label: String,
    pub calls: usize,
    /// Share of analysed calls, 0..=1.
    pub share: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScoreBucket {
    /// Inclusive lower bound, e.g. 80 for the 80–89 band.
    pub floor: u32,
    pub calls: usize,
}

/// An association between a term and an outcome.
///
/// Deliberately not called a "cause". The fields are chosen so the reader can
/// judge it themselves: both group sizes, both means, and the ratio.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pattern {
    pub term: String,
    /// What was measured: "escalation risk", "QA score", …
    pub outcome: String,
    pub with_calls: usize,
    pub without_calls: usize,
    pub with_mean: f32,
    pub without_mean: f32,
    /// `with_mean / without_mean`. Above 1 means the term goes with a higher
    /// value; below 1, lower.
    pub lift: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CorpusReport {
    pub calls_analysed: usize,
    pub mean_qa: Option<f32>,
    pub qa_distribution: Vec<ScoreBucket>,
    pub mean_escalation_risk: Option<f32>,
    pub mean_churn_risk: Option<f32>,
    /// Mean whole-call customer sentiment, −1..=1.
    pub mean_sentiment: Option<f32>,
    /// Share of the calls that have a sentiment score where the customer left
    /// unhappy, 0..=1. Denominator is those calls, not the whole corpus: a
    /// batch half-way through the sentiment agent would otherwise report a
    /// falling share as sentiment improving.
    pub negative_share: Option<f32>,
    /// Share of the calls the compliance agent judged that it passed, 0..=1.
    pub compliance_pass_rate: Option<f32>,
    /// Calls with at least one finding severe enough to fail them.
    pub failing_calls: usize,
    /// Most frequent topics across the corpus.
    pub top_topics: Vec<Counted>,
    /// Compliance codes by how many calls raised them.
    pub compliance_issues: Vec<Counted>,
    /// Root-cause labels by frequency.
    pub top_issues: Vec<Counted>,
    /// Root causes rolled up into their five buckets — the view that answers
    /// "is this a billing problem or a process problem".
    pub root_cause_categories: Vec<Counted>,
    pub patterns: Vec<Pattern>,
}

fn mean(values: &[f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f32>() / values.len() as f32)
}

fn counted(counts: HashMap<String, usize>, total: usize, limit: usize) -> Vec<Counted> {
    let mut out: Vec<Counted> = counts
        .into_iter()
        .map(|(label, calls)| Counted {
            label,
            calls,
            share: if total == 0 {
                0.0
            } else {
                calls as f32 / total as f32
            },
        })
        .collect();
    // Frequency first, then alphabetical, so the order is stable across runs
    // rather than following HashMap iteration.
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.label.cmp(&b.label)));
    out.truncate(limit);
    out
}

/// Build the corpus view.
pub fn analyse_corpus(calls: &[CallFacts]) -> CorpusReport {
    let total = calls.len();

    let qa: Vec<f32> = calls.iter().filter_map(|c| c.qa_overall).collect();
    let escalation: Vec<f32> = calls.iter().filter_map(|c| c.escalation_risk).collect();
    let churn: Vec<f32> = calls.iter().filter_map(|c| c.churn_risk).collect();
    let sentiment: Vec<f32> = calls.iter().filter_map(|c| c.sentiment).collect();

    // Both rates are shares of the calls that actually have the measure, never
    // of the corpus. A partly-analysed batch would otherwise read as improving
    // purely because the agent had not caught up.
    let negative_share = if sentiment.is_empty() {
        None
    } else {
        let bad = sentiment
            .iter()
            .filter(|s| **s < NEGATIVE_SENTIMENT)
            .count();
        Some(bad as f32 / sentiment.len() as f32)
    };

    let judged: Vec<bool> = calls.iter().filter_map(|c| c.compliance_passed).collect();
    let compliance_pass_rate = if judged.is_empty() {
        None
    } else {
        Some(judged.iter().filter(|p| **p).count() as f32 / judged.len() as f32)
    };
    let failing_calls = judged.iter().filter(|p| !**p).count();

    // Ten-point bands. Finer buckets on a corpus of a few hundred calls produce
    // a comb rather than a distribution.
    let mut buckets: HashMap<u32, usize> = HashMap::new();
    for score in &qa {
        let floor = ((score.clamp(0.0, 100.0) as u32) / 10) * 10;
        *buckets.entry(floor.min(90)).or_insert(0) += 1;
    }
    let mut qa_distribution: Vec<ScoreBucket> = buckets
        .into_iter()
        .map(|(floor, calls)| ScoreBucket { floor, calls })
        .collect();
    qa_distribution.sort_by_key(|b| b.floor);

    let mut topics: HashMap<String, usize> = HashMap::new();
    let mut codes: HashMap<String, usize> = HashMap::new();
    let mut issues: HashMap<String, usize> = HashMap::new();
    let mut categories: HashMap<String, usize> = HashMap::new();

    for c in calls {
        // Per call, not per mention: a term repeated twenty times on one call
        // is one call's worth of evidence about the corpus.
        let mut seen: Vec<&str> = c.keywords.iter().map(|k| k.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        for term in seen {
            *topics.entry(term.to_string()).or_insert(0) += 1;
        }

        let mut seen_codes: Vec<&str> = c.compliance_codes.iter().map(|s| s.as_str()).collect();
        seen_codes.sort_unstable();
        seen_codes.dedup();
        for code in seen_codes {
            *codes.entry(code.to_string()).or_insert(0) += 1;
        }

        if let Some(issue) = &c.primary_issue {
            let issue = issue.trim();
            if !issue.is_empty() {
                *issues.entry(issue.to_string()).or_insert(0) += 1;
            }
        }

        if let Some(category) = &c.root_cause_category {
            let category = category.trim();
            if !category.is_empty() {
                *categories.entry(category.to_string()).or_insert(0) += 1;
            }
        }
    }

    let patterns = discover_patterns(calls, &topics);

    CorpusReport {
        calls_analysed: total,
        mean_qa: mean(&qa),
        qa_distribution,
        mean_escalation_risk: mean(&escalation),
        mean_churn_risk: mean(&churn),
        mean_sentiment: mean(&sentiment),
        negative_share,
        compliance_pass_rate,
        failing_calls,
        top_topics: counted(topics, total, 12),
        compliance_issues: counted(codes, total, 12),
        top_issues: counted(issues, total, 12),
        // Five buckets at most, so no truncation is possible — but the limit is
        // passed anyway rather than assuming the agent's vocabulary is fixed.
        root_cause_categories: counted(categories, total, 8),
        patterns,
    }
}

/// A named outcome and how to read it off one call, for the pattern finder.
type Measure = (&'static str, fn(&CallFacts) -> Option<f32>);

/// Compare each frequent term's outcome against calls without it.
fn discover_patterns(calls: &[CallFacts], topics: &HashMap<String, usize>) -> Vec<Pattern> {
    // Only terms common enough to have a comparison group worth the name.
    let mut candidates: Vec<(&String, &usize)> = topics
        .iter()
        .filter(|(_, n)| **n >= MIN_SAMPLE && calls.len() - **n >= MIN_SAMPLE)
        .collect();
    candidates.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    candidates.truncate(MAX_TERMS_CONSIDERED);

    // Every measure here must be non-negative, because the effect is reported
    // as a ratio. Sentiment is deliberately absent: it runs −1..1, so −0.4
    // against 0.2 would be published as "2× lower sentiment" when the two
    // groups are on opposite sides of neutral. Sentiment is reported as a mean
    // and a negative share instead, where the sign survives.
    let measures: [Measure; 3] = [
        ("escalation risk", |c| c.escalation_risk),
        ("churn risk", |c| c.churn_risk),
        ("QA score", |c| c.qa_overall),
    ];

    let mut out = Vec::new();
    for (term, _) in candidates {
        for (outcome, get) in measures {
            let (mut with, mut without) = (Vec::new(), Vec::new());
            for c in calls {
                let Some(v) = get(c) else { continue };
                if c.keywords.iter().any(|k| k == term) {
                    with.push(v);
                } else {
                    without.push(v);
                }
            }
            if with.len() < MIN_SAMPLE || without.len() < MIN_SAMPLE {
                continue;
            }
            let (Some(wm), Some(om)) = (mean(&with), mean(&without)) else {
                continue;
            };
            // A zero baseline makes the ratio meaningless rather than infinite.
            if om.abs() < f32::EPSILON {
                continue;
            }
            let lift = wm / om;
            if ((lift as f64) - 1.0).abs() < MIN_LIFT {
                continue;
            }
            out.push(Pattern {
                term: term.clone(),
                outcome: outcome.to_string(),
                with_calls: with.len(),
                without_calls: without.len(),
                with_mean: wm,
                without_mean: om,
                lift,
            });
        }
    }

    // Strongest effect first, in either direction.
    out.sort_by(|a, b| {
        let da = ((a.lift as f64) - 1.0).abs();
        let db = ((b.lift as f64) - 1.0).abs();
        db.partial_cmp(&da)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.term.cmp(&b.term))
    });
    out.truncate(MAX_PATTERNS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, qa: f32, escalation: f32, keywords: &[&str]) -> CallFacts {
        CallFacts {
            call_id: id.into(),
            qa_overall: Some(qa),
            escalation_risk: Some(escalation),
            churn_risk: Some(0.1),
            frustration: Some(0.2),
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
            ..Default::default()
        }
    }

    /// `n` calls with a term and high escalation, `m` without and low.
    fn corpus(with: usize, without: usize) -> Vec<CallFacts> {
        let mut v: Vec<CallFacts> = (0..with)
            .map(|i| call(&format!("w{i}"), 70.0, 0.8, &["refund", "payment"]))
            .collect();
        v.extend((0..without).map(|i| call(&format!("o{i}"), 90.0, 0.2, &["payment"])));
        v
    }

    #[test]
    fn an_empty_corpus_reports_nothing_rather_than_panicking() {
        let r = analyse_corpus(&[]);
        assert_eq!(r.calls_analysed, 0);
        assert!(r.mean_qa.is_none());
        assert!(r.patterns.is_empty());
    }

    #[test]
    fn topics_are_counted_per_call_not_per_mention() {
        // A word repeated twenty times on one call is one call's worth of
        // evidence about the corpus.
        let c = CallFacts {
            keywords: vec!["refund".into(), "refund".into(), "refund".into()],
            ..call("a", 80.0, 0.1, &[])
        };
        let r = analyse_corpus(&[c]);
        let refund = r.top_topics.iter().find(|t| t.label == "refund").unwrap();
        assert_eq!(refund.calls, 1);
        assert_eq!(refund.share, 1.0);
    }

    #[test]
    fn a_real_association_is_found_with_both_group_sizes() {
        let r = analyse_corpus(&corpus(20, 20));
        let p = r
            .patterns
            .iter()
            .find(|p| p.term == "refund" && p.outcome == "escalation risk")
            .expect("association not found");
        assert_eq!(p.with_calls, 20);
        assert_eq!(p.without_calls, 20);
        assert!((p.lift - 4.0).abs() < 0.01, "got {}", p.lift);
    }

    #[test]
    fn a_term_appearing_on_too_few_calls_is_not_reported() {
        // Three calls will produce a spectacular ratio and mean nothing.
        let r = analyse_corpus(&corpus(3, 30));
        assert!(
            r.patterns.iter().all(|p| p.term != "refund"),
            "reported a pattern from a sample of 3"
        );
    }

    #[test]
    fn a_term_on_almost_every_call_is_not_reported_either() {
        // "payment" is on all of them, so there is no comparison group.
        let r = analyse_corpus(&corpus(20, 20));
        assert!(r.patterns.iter().all(|p| p.term != "payment"));
    }

    #[test]
    fn a_negligible_difference_is_not_dressed_up_as_a_finding() {
        let mut v: Vec<CallFacts> = (0..20)
            .map(|i| call(&format!("w{i}"), 80.0, 0.50, &["refund", "payment"]))
            .collect();
        v.extend((0..20).map(|i| call(&format!("o{i}"), 80.0, 0.52, &["payment"])));
        let r = analyse_corpus(&v);
        assert!(r.patterns.is_empty(), "got {:?}", r.patterns);
    }

    #[test]
    fn associations_in_the_negative_direction_are_kept() {
        // A term that goes with *lower* escalation is as interesting as one
        // that goes with higher, and sorting by |lift - 1| must not drop it.
        let mut v: Vec<CallFacts> = (0..20)
            .map(|i| call(&format!("w{i}"), 95.0, 0.1, &["resolved", "payment"]))
            .collect();
        v.extend((0..20).map(|i| call(&format!("o{i}"), 60.0, 0.8, &["payment"])));
        let r = analyse_corpus(&v);
        let p = r
            .patterns
            .iter()
            .find(|p| p.term == "resolved" && p.outcome == "escalation risk")
            .expect("negative association dropped");
        assert!(p.lift < 1.0, "got {}", p.lift);
    }

    #[test]
    fn a_zero_baseline_does_not_produce_an_infinite_lift() {
        let mut v: Vec<CallFacts> = (0..20)
            .map(|i| call(&format!("w{i}"), 80.0, 0.5, &["refund", "payment"]))
            .collect();
        v.extend((0..20).map(|i| call(&format!("o{i}"), 80.0, 0.0, &["payment"])));
        let r = analyse_corpus(&v);
        assert!(
            r.patterns
                .iter()
                .all(|p| p.lift.is_finite() && p.outcome != "escalation risk"
                    || p.term != "refund"),
            "an infinite lift was reported"
        );
    }

    #[test]
    fn scores_are_bucketed_into_ten_point_bands_with_100_in_the_top_one() {
        let calls = vec![
            call("a", 100.0, 0.1, &[]),
            call("b", 95.0, 0.1, &[]),
            call("c", 82.0, 0.1, &[]),
            call("d", 41.0, 0.1, &[]),
        ];
        let r = analyse_corpus(&calls);
        let top = r.qa_distribution.iter().find(|b| b.floor == 90).unwrap();
        assert_eq!(top.calls, 2, "100 should land in the 90s band, not its own");
        assert!(r
            .qa_distribution
            .iter()
            .any(|b| b.floor == 80 && b.calls == 1));
        assert!(r
            .qa_distribution
            .iter()
            .any(|b| b.floor == 40 && b.calls == 1));
    }

    #[test]
    fn calls_missing_a_measure_are_excluded_rather_than_counted_as_zero() {
        // Treating an absent score as 0 would drag every mean toward nothing.
        let calls = vec![
            call("a", 90.0, 0.2, &[]),
            CallFacts {
                qa_overall: None,
                ..call("b", 0.0, 0.2, &[])
            },
        ];
        let r = analyse_corpus(&calls);
        assert_eq!(r.mean_qa, Some(90.0));
    }

    #[test]
    fn compliance_codes_and_issues_are_ranked_by_how_many_calls_raised_them() {
        let calls = vec![
            CallFacts {
                compliance_codes: vec!["missing_disclosure".into()],
                primary_issue: Some("Payment failure".into()),
                ..call("a", 80.0, 0.1, &[])
            },
            CallFacts {
                compliance_codes: vec!["missing_disclosure".into(), "pii_exposure".into()],
                primary_issue: Some("Payment failure".into()),
                ..call("b", 80.0, 0.1, &[])
            },
        ];
        let r = analyse_corpus(&calls);
        assert_eq!(r.compliance_issues[0].label, "missing_disclosure");
        assert_eq!(r.compliance_issues[0].calls, 2);
        assert_eq!(r.top_issues[0].label, "Payment failure");
        assert_eq!(r.top_issues[0].calls, 2);
    }

    #[test]
    fn sentiment_is_averaged_with_its_sign_intact() {
        // Averaging magnitudes would report a corpus split between delighted
        // and furious customers as uniformly neutral-positive.
        let calls = vec![
            CallFacts {
                sentiment: Some(0.6),
                ..call("a", 80.0, 0.1, &[])
            },
            CallFacts {
                sentiment: Some(-0.8),
                ..call("b", 80.0, 0.1, &[])
            },
        ];
        let r = analyse_corpus(&calls);
        assert!(
            (r.mean_sentiment.unwrap() - -0.1).abs() < 1e-6,
            "got {:?}",
            r.mean_sentiment
        );
    }

    #[test]
    fn the_negative_share_ignores_calls_that_are_merely_neutral() {
        // −0.05 is a neutral call with rounding on it. Counting it would report
        // half a healthy corpus as unhappy customers.
        let calls = vec![
            CallFacts {
                sentiment: Some(-0.05),
                ..call("neutral", 80.0, 0.1, &[])
            },
            CallFacts {
                sentiment: Some(-0.9),
                ..call("angry", 80.0, 0.1, &[])
            },
        ];
        let r = analyse_corpus(&calls);
        assert_eq!(r.negative_share, Some(0.5));
    }

    #[test]
    fn rates_are_shares_of_the_calls_that_have_the_measure() {
        // Half this corpus has not been through either agent. Dividing by the
        // whole corpus would report the pass rate falling as coverage grew.
        let calls = vec![
            CallFacts {
                sentiment: Some(-0.9),
                compliance_passed: Some(false),
                ..call("judged", 80.0, 0.1, &[])
            },
            call("not-yet", 80.0, 0.1, &[]),
        ];
        let r = analyse_corpus(&calls);
        assert_eq!(r.negative_share, Some(1.0));
        assert_eq!(r.compliance_pass_rate, Some(0.0));
        assert_eq!(r.failing_calls, 1);
    }

    #[test]
    fn a_call_the_compliance_agent_never_judged_is_neither_a_pass_nor_a_failure() {
        let r = analyse_corpus(&[call("a", 80.0, 0.1, &[])]);
        assert!(r.compliance_pass_rate.is_none());
        assert_eq!(r.failing_calls, 0);
        assert!(r.mean_sentiment.is_none());
        assert!(r.negative_share.is_none());
    }

    #[test]
    fn root_causes_roll_up_into_their_buckets() {
        // The free-text cause differs on every call; the bucket is what
        // aggregates into "this is a billing problem".
        let calls = vec![
            CallFacts {
                primary_issue: Some("Double charge on renewal".into()),
                root_cause_category: Some("billing".into()),
                ..call("a", 80.0, 0.1, &[])
            },
            CallFacts {
                primary_issue: Some("Card declined twice".into()),
                root_cause_category: Some("billing".into()),
                ..call("b", 80.0, 0.1, &[])
            },
            CallFacts {
                primary_issue: Some("App crashes on upload".into()),
                root_cause_category: Some("product".into()),
                ..call("c", 80.0, 0.1, &[])
            },
        ];
        let r = analyse_corpus(&calls);
        assert_eq!(r.root_cause_categories[0].label, "billing");
        assert_eq!(r.root_cause_categories[0].calls, 2);
        assert_eq!(r.top_issues.len(), 3, "distinct causes were merged");
    }

    #[test]
    fn sentiment_is_never_reported_as_a_ratio() {
        // A term whose group averages −0.4 against +0.2 is not "2× lower
        // sentiment" — the groups are on opposite sides of neutral, and a lift
        // cannot say so. Sentiment is excluded from patterns for that reason.
        let mut v: Vec<CallFacts> = (0..20)
            .map(|i| CallFacts {
                sentiment: Some(-0.4),
                ..call(&format!("w{i}"), 80.0, 0.5, &["refund", "payment"])
            })
            .collect();
        v.extend((0..20).map(|i| CallFacts {
            sentiment: Some(0.2),
            ..call(&format!("o{i}"), 80.0, 0.5, &["payment"])
        }));
        let r = analyse_corpus(&v);
        assert!(
            r.patterns.iter().all(|p| !p.outcome.contains("sentiment")),
            "got {:?}",
            r.patterns
        );
    }

    #[test]
    fn the_report_crosses_the_wire_as_camel_case() {
        let r = analyse_corpus(&corpus(20, 20));
        let v = serde_json::to_value(&r).unwrap();
        for k in v.as_object().unwrap().keys() {
            assert!(!k.contains('_'), "CorpusReport serialises `{k}`");
        }
        assert!(v.get("callsAnalysed").is_some());
        assert!(v["patterns"][0].get("withCalls").is_some());
    }

    #[test]
    fn output_is_identical_across_runs() {
        // HashMap iteration order would otherwise reshuffle the lists, making
        // two reports on the same corpus look like a change.
        let calls = corpus(20, 20);
        assert_eq!(analyse_corpus(&calls), analyse_corpus(&calls));
    }
}
