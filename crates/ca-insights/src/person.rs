//! Rolling calls up per person.
//!
//! Every aggregate is an `Option`. With almost nothing in a corpus analysed, a
//! mean rendered as `0` is a fabricated number wearing the same clothes as a
//! real one — and a person whose calls have never been scored looks identical
//! to the worst agent on the floor. Absent has to stay visibly absent.

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

/// The minimum a call needs to expose for a person rollup.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRef {
    pub call_id: String,
    pub contact_id: Option<String>,
    /// The real call time. `None` when the call was never attributed one, in
    /// which case it cannot contribute to a trend or a repeat calculation.
    pub occurred_at: Option<DateTime<Utc>>,
    pub qa_overall: Option<f32>,
    pub sentiment: Option<f32>,
    pub compliance_passed: Option<bool>,
}

/// What one person's calls add up to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonFacts {
    pub person_id: String,
    pub calls: usize,
    pub mean_qa: Option<f32>,
    pub mean_sentiment: Option<f32>,
    pub compliance_pass_rate: Option<f32>,
    pub first_call: Option<DateTime<Utc>>,
    pub last_call: Option<DateTime<Utc>>,
}

/// One month of a trend line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonthPoint {
    /// `YYYY-MM`.
    pub month: String,
    pub value: f32,
    pub calls: usize,
}

fn mean(values: impl Iterator<Item = f32>) -> Option<f32> {
    let (sum, n) = values.fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f32)
}

/// Summarise one person's calls.
///
/// `calls` counts every call; the measures count only the calls that carry
/// them. A person with twenty calls and two scorecards has `calls: 20` and a
/// mean over the two, never a mean over twenty with eighteen zeroes.
pub fn person_facts(person_id: &str, calls: &[CallRef]) -> PersonFacts {
    PersonFacts {
        person_id: person_id.to_string(),
        calls: calls.len(),
        mean_qa: mean(calls.iter().filter_map(|c| c.qa_overall)),
        mean_sentiment: mean(calls.iter().filter_map(|c| c.sentiment)),
        compliance_pass_rate: mean(calls.iter().filter_map(|c| c.compliance_passed).map(|p| {
            if p {
                1.0
            } else {
                0.0
            }
        })),
        first_call: calls.iter().filter_map(|c| c.occurred_at).min(),
        last_call: calls.iter().filter_map(|c| c.occurred_at).max(),
    }
}

/// Share of calls whose contact also called within the preceding `window_days`.
///
/// Both bounds are explicit on purpose. "Repeat" is meaningless without a
/// window, and a lifetime definition climbs forever — it would read as a
/// steadily worsening trend when nothing had actually changed.
///
/// Returns `None` when no call carries both a contact and a time, because the
/// question is then unanswerable rather than zero.
pub fn repeat_contact_rate(calls: &[CallRef], window_days: i64) -> Option<f32> {
    let mut timed: Vec<(&str, DateTime<Utc>)> = calls
        .iter()
        .filter_map(|c| Some((c.contact_id.as_deref()?, c.occurred_at?)))
        .collect();
    if timed.is_empty() {
        return None;
    }
    timed.sort_by_key(|(_, t)| *t);

    let window = chrono::Duration::days(window_days);
    let repeats = timed
        .iter()
        .enumerate()
        .filter(|(i, (who, when))| {
            timed[..*i]
                .iter()
                .any(|(prev, prev_t)| prev == who && *when - *prev_t <= window)
        })
        .count();
    Some(repeats as f32 / timed.len() as f32)
}

/// Monthly means of one measure.
///
/// Months in which no call carried the measure are omitted rather than plotted
/// as zero — a gap in a trend line is honest, a dip to zero is not.
pub fn monthly_trend(calls: &[CallRef], measure: fn(&CallRef) -> Option<f32>) -> Vec<MonthPoint> {
    let mut buckets: std::collections::BTreeMap<String, Vec<f32>> = Default::default();
    for c in calls {
        let (Some(when), Some(v)) = (c.occurred_at, measure(c)) else {
            continue;
        };
        buckets
            .entry(format!("{:04}-{:02}", when.year(), when.month()))
            .or_default()
            .push(v);
    }
    buckets
        .into_iter()
        .filter_map(|(month, vs)| {
            let calls = vs.len();
            mean(vs.into_iter()).map(|value| MonthPoint {
                month,
                value,
                calls,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap()
    }

    fn call(person: &str, when: DateTime<Utc>, qa: Option<f32>) -> CallRef {
        CallRef {
            call_id: format!("c{}", when.timestamp()),
            contact_id: Some(person.into()),
            occurred_at: Some(when),
            qa_overall: qa,
            sentiment: None,
            compliance_passed: None,
        }
    }

    /// The failure this guards is a fabricated number: with nothing analysed, a
    /// mean of 0 looks exactly like a genuinely terrible agent.
    #[test]
    fn a_person_with_no_analysed_calls_has_no_scores_rather_than_zeroes() {
        let calls = vec![
            call("p1", at(2025, 1, 5), None),
            call("p1", at(2025, 2, 5), None),
        ];
        let f = person_facts("p1", &calls);
        assert_eq!(f.calls, 2, "the calls themselves are real and counted");
        assert_eq!(f.mean_qa, None);
        assert_eq!(f.mean_sentiment, None);
        assert_eq!(f.compliance_pass_rate, None);
    }

    #[test]
    fn means_ignore_unanalysed_calls_instead_of_counting_them_as_zero() {
        let calls = vec![
            call("p1", at(2025, 1, 5), Some(80.0)),
            call("p1", at(2025, 1, 6), None),
            call("p1", at(2025, 1, 7), Some(60.0)),
        ];
        let f = person_facts("p1", &calls);
        assert_eq!(f.calls, 3);
        assert_eq!(f.mean_qa, Some(70.0));
    }

    #[test]
    fn first_and_last_call_come_from_the_real_call_time() {
        let calls = vec![
            call("p1", at(2025, 3, 9), None),
            call("p1", at(2025, 1, 2), None),
        ];
        let f = person_facts("p1", &calls);
        assert_eq!(f.first_call, Some(at(2025, 1, 2)));
        assert_eq!(f.last_call, Some(at(2025, 3, 9)));
    }

    #[test]
    fn a_contact_calling_twice_inside_the_window_counts_as_a_repeat() {
        let calls = vec![
            call("p1", at(2025, 5, 1), None),
            call("p1", at(2025, 5, 10), None),
        ];
        assert_eq!(repeat_contact_rate(&calls, 30), Some(0.5));
    }

    /// Without a window "repeat" drifts upward forever and reads as a worsening
    /// trend when nothing has changed.
    #[test]
    fn a_call_outside_the_window_is_not_a_repeat() {
        let calls = vec![
            call("p1", at(2025, 1, 1), None),
            call("p1", at(2025, 6, 1), None),
        ];
        assert_eq!(repeat_contact_rate(&calls, 30), Some(0.0));
    }

    #[test]
    fn calls_without_a_time_cannot_answer_the_repeat_question() {
        let mut c = call("p1", at(2025, 1, 1), None);
        c.occurred_at = None;
        assert_eq!(repeat_contact_rate(&[c], 30), None);
    }

    #[test]
    fn the_trend_buckets_by_month_and_skips_months_with_no_measure() {
        let calls = vec![
            call("p1", at(2025, 1, 5), Some(50.0)),
            call("p1", at(2025, 1, 20), Some(70.0)),
            call("p1", at(2025, 2, 5), None),
            call("p1", at(2025, 3, 5), Some(90.0)),
        ];
        let t = monthly_trend(&calls, |c| c.qa_overall);
        assert_eq!(t.len(), 2, "february has no measure and is omitted");
        assert_eq!(
            t[0],
            MonthPoint {
                month: "2025-01".into(),
                value: 60.0,
                calls: 2
            }
        );
        assert_eq!(
            t[1],
            MonthPoint {
                month: "2025-03".into(),
                value: 90.0,
                calls: 1
            }
        );
    }
}
