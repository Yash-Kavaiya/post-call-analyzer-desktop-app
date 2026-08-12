use crate::ids::{CallId, SegmentId};
use serde::{Deserialize, Serialize};

/// Which side of the conversation a diarized speaker turned out to be.
///
/// Diarization gives us anonymous clusters (`Speaker 0`, `Speaker 1`); role
/// assignment is a separate, fallible inference step, so `Unknown` is a real
/// and common state rather than an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerRole {
    Agent,
    Customer,
    Unknown,
}

impl SpeakerRole {
    pub fn label(&self) -> &'static str {
        match self {
            SpeakerRole::Agent => "Agent",
            SpeakerRole::Customer => "Customer",
            SpeakerRole::Unknown => "Speaker",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Speaker {
    /// Diarization cluster index, e.g. 0 or 1.
    pub index: u8,
    pub role: SpeakerRole,
    /// Confidence in the *role* assignment, not the diarization.
    #[serde(alias = "role_confidence")]
    pub role_confidence: Option<f32>,
    #[serde(alias = "total_speech_secs")]
    pub total_speech_secs: f64,
    #[serde(alias = "turn_count")]
    pub turn_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub confidence: Option<f32>,
}

/// One speaker turn: a contiguous run of speech from a single speaker.
///
/// Segments are the atomic unit agents cite. A citation that does not resolve
/// to a real `SegmentId` is rejected rather than shown to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: SegmentId,
    pub speaker_index: u8,
    pub role: SpeakerRole,
    pub start: f64,
    pub end: f64,
    /// Text as spoken, with PII still present. Never sent to a cloud provider.
    pub text: String,
    /// Text with PII replaced by stable tokens. This is what leaves the machine.
    pub redacted_text: Option<String>,
    pub words: Vec<Word>,
    pub confidence: Option<f32>,
    /// Detected language for this turn; calls here are frequently code-switched.
    pub language: Option<String>,
}

impl Segment {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    /// The text safe to send off-machine, falling back to raw only when
    /// redaction has not run. Callers that must guarantee redaction should
    /// check `redacted_text` directly.
    pub fn outbound_text(&self) -> &str {
        self.redacted_text.as_deref().unwrap_or(&self.text)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub call_id: CallId,
    pub segments: Vec<Segment>,
    pub speakers: Vec<Speaker>,
    /// Dominant language across the call.
    pub primary_language: Option<String>,
    /// Every language observed, for code-switched calls.
    pub languages: Vec<String>,
    pub engine: String,
    pub model: String,
    pub duration_secs: f64,
}

impl Transcript {
    pub fn word_count(&self) -> usize {
        self.segments.iter().map(|s| s.words.len()).sum()
    }

    pub fn find_segment(&self, id: &SegmentId) -> Option<&Segment> {
        self.segments.iter().find(|s| &s.id == id)
    }

    /// Render as `[mm:ss] Role: text` lines — the exact shape agents receive.
    /// Uses redacted text so prompt construction cannot leak PII by accident.
    pub fn to_prompt_format(&self) -> String {
        self.segments
            .iter()
            .map(|s| {
                format!(
                    "[{}] ({}) {}: {}",
                    fmt_ts(s.start),
                    s.id,
                    s.role.label(),
                    s.outbound_text()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn fmt_ts(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!("{:02}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: &str, text: &str, redacted: Option<&str>) -> Segment {
        Segment {
            id: SegmentId::from(id),
            speaker_index: 0,
            role: SpeakerRole::Agent,
            start: 65.0,
            end: 70.0,
            text: text.to_string(),
            redacted_text: redacted.map(|s| s.to_string()),
            words: vec![],
            confidence: None,
            language: None,
        }
    }

    #[test]
    fn timestamps_format_as_mm_ss() {
        assert_eq!(fmt_ts(0.0), "00:00");
        assert_eq!(fmt_ts(65.4), "01:05");
        assert_eq!(fmt_ts(3599.0), "59:59");
    }

    #[test]
    fn prompt_format_prefers_redacted_text() {
        let t = Transcript {
            call_id: crate::ids::CallId::from("call_x"),
            segments: vec![seg("seg_1", "call me on 9876543210", Some("call me on [PHONE_1]"))],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "test".into(),
            model: "test".into(),
            duration_secs: 70.0,
        };
        let out = t.to_prompt_format();
        assert!(out.contains("[PHONE_1]"), "redacted text must be used: {out}");
        assert!(!out.contains("9876543210"), "raw PII leaked into prompt: {out}");
        assert!(out.contains("01:05"), "timestamp missing: {out}");
        assert!(out.contains("seg_1"), "segment id missing: {out}");
    }

    #[test]
    fn outbound_falls_back_to_raw_when_not_redacted() {
        let s = seg("seg_2", "hello", None);
        assert_eq!(s.outbound_text(), "hello");
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;

    #[test]
    fn speakers_cross_the_wire_as_camel_case() {
        let s = Speaker {
            index: 0,
            role: SpeakerRole::Agent,
            role_confidence: Some(0.9),
            total_speech_secs: 61.0,
            turn_count: 10,
        };
        let v = serde_json::to_value(&s).unwrap();
        for k in v.as_object().unwrap().keys() {
            assert!(!k.contains('_'), "Speaker serialises `{k}`");
        }
        assert!(v.get("totalSpeechSecs").is_some());
        assert!(v.get("roleConfidence").is_some());
    }

    #[test]
    fn speakers_already_written_in_snake_case_still_load() {
        let old = r#"{"index":1,"role":"customer","role_confidence":0.8,
                      "total_speech_secs":27.9,"turn_count":6}"#;
        let s: Speaker = serde_json::from_str(old).unwrap();
        assert_eq!(s.turn_count, 6);
        assert_eq!(s.total_speech_secs, 27.9);
    }

    #[test]
    fn speaker_roles_stay_snake_case_because_they_are_values_not_keys() {
        // The rename applies to field names; the enum's own spelling is data
        // the UI matches on, and changing it would break every comparison.
        let v = serde_json::to_value(SpeakerRole::Customer).unwrap();
        assert_eq!(v, serde_json::json!("customer"));
    }
}
