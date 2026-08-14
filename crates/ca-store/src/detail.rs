//! Everything the call screen draws, assembled in one place.
//!
//! This lived in the desktop command layer, which meant the dev bridge could
//! not serve it and the browser had to fall back to an invented transcript.
//! Composing it here keeps the command layer thin — as its own module doc
//! claims — and means both frontends read the same call.

use ca_core::error::{CaError, Result};
use ca_core::identity::ViewScope;
use ca_core::ids::CallId;
use serde::{Deserialize, Serialize};

use crate::Store;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallDetail {
    pub id: String,
    pub display_name: String,
    pub duration_secs: Option<f64>,
    pub features: Option<ca_core::audio::AcousticFeatures>,
    pub segments: Vec<SegmentDto>,
    pub speakers: Vec<ca_core::transcript::Speaker>,
    pub engine: Option<String>,
    pub model: Option<String>,
    /// Absolute path, so the UI can play the original audio.
    pub media_path: Option<String>,
    /// Talk ratio, interruptions, silences, keywords and the rest.
    ///
    /// Computed on read from the transcript rather than stored: it is
    /// arithmetic over timings, so recomputing costs microseconds and can never
    /// go stale against a transcript that was re-run.
    pub metrics: Option<ca_metrics::ConversationMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentDto {
    pub id: String,
    pub speaker_index: u8,
    pub role: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub redacted_text: Option<String>,
    pub words: Vec<ca_core::transcript::Word>,
    pub confidence: Option<f32>,
    pub language: Option<String>,
}

impl Store {
    /// One call, with its transcript, metrics and media path.
    ///
    /// Scoped: a viewer who may not see this call gets the same `not found`
    /// as one asking for an id that does not exist. Distinguishing the two
    /// would confirm the call exists, which is the thing being withheld.
    pub fn call_detail(&self, id: &CallId, scope: &ViewScope) -> Result<CallDetail> {
        let transcript = self.load_transcript(id)?;
        let features = self.features(id)?;
        let media_path = self.media_path(id)?;

        let summary = self
            .list_calls(0, 1_000_000, None, scope)?
            .into_iter()
            .find(|c| c.id == id.as_str())
            .ok_or_else(|| CaError::NotFound(format!("call {id} not found")))?;

        let metrics = transcript.as_ref().map(ca_metrics::analyse);

        let (segments, speakers, engine, model) = match transcript {
            Some(t) => (
                t.segments
                    .into_iter()
                    .map(|s| SegmentDto {
                        id: s.id.as_str().to_string(),
                        speaker_index: s.speaker_index,
                        role: match s.role {
                            ca_core::transcript::SpeakerRole::Agent => "agent",
                            ca_core::transcript::SpeakerRole::Customer => "customer",
                            ca_core::transcript::SpeakerRole::Unknown => "unknown",
                        }
                        .to_string(),
                        start: s.start,
                        end: s.end,
                        text: s.text,
                        redacted_text: s.redacted_text,
                        words: s.words,
                        confidence: s.confidence,
                        language: s.language,
                    })
                    .collect(),
                t.speakers,
                Some(t.engine),
                Some(t.model),
            ),
            None => (vec![], vec![], None, None),
        };

        Ok(CallDetail {
            id: summary.id,
            display_name: summary.display_name,
            duration_secs: summary.duration_secs,
            features,
            segments,
            speakers,
            engine,
            model,
            media_path,
            metrics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::call::Call;

    #[test]
    fn a_call_with_no_transcript_still_has_a_detail() {
        let s = Store::in_memory().unwrap();
        let id = s
            .upsert_call(&Call::new("k", "local", "untranscribed.m4a"))
            .unwrap();

        let d = s.call_detail(&id, &ViewScope::all()).unwrap();
        assert_eq!(d.display_name, "untranscribed.m4a");
        assert!(d.segments.is_empty());
        assert!(d.metrics.is_none(), "no transcript means no metrics");
        assert!(d.engine.is_none());
    }

    #[test]
    fn an_unknown_call_is_reported_as_missing_rather_than_returned_empty() {
        let s = Store::in_memory().unwrap();
        let err = s
            .call_detail(&CallId::from("call_nope"), &ViewScope::all())
            .unwrap_err();
        assert!(format!("{err}").contains("not found"), "got {err}");
    }
}
