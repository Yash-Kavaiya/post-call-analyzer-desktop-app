use crate::audio::{AcousticFeatures, AudioMeta};
use crate::ids::CallId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Where a call is in the pipeline. Persisted after every stage so a batch that
/// takes days can be killed and resumed without redoing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStage {
    Discovered,
    Decoded,
    Transcribed,
    Diarized,
    Redacted,
    Analyzed,
    Indexed,
    Failed,
}

impl CallStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallStage::Discovered => "discovered",
            CallStage::Decoded => "decoded",
            CallStage::Transcribed => "transcribed",
            CallStage::Diarized => "diarized",
            CallStage::Redacted => "redacted",
            CallStage::Analyzed => "analyzed",
            CallStage::Indexed => "indexed",
            CallStage::Failed => "failed",
        }
    }

    /// Ordering used by the resume logic: a call resumes at the first stage it
    /// has not yet reached.
    pub fn rank(&self) -> u8 {
        match self {
            CallStage::Discovered => 0,
            CallStage::Decoded => 1,
            CallStage::Transcribed => 2,
            CallStage::Diarized => 3,
            CallStage::Redacted => 4,
            CallStage::Analyzed => 5,
            CallStage::Indexed => 6,
            CallStage::Failed => 255,
        }
    }

    /// The stage immediately before this one.
    ///
    /// Used to express "redo the work from here onward": a re-run that starts
    /// at `Analyzed` must still trust the transcription, or asking for a
    /// re-score would silently re-transcribe 133 hours of audio.
    pub fn previous(&self) -> CallStage {
        match self {
            CallStage::Discovered | CallStage::Failed => CallStage::Discovered,
            CallStage::Decoded => CallStage::Discovered,
            CallStage::Transcribed => CallStage::Decoded,
            CallStage::Diarized => CallStage::Transcribed,
            CallStage::Redacted => CallStage::Diarized,
            CallStage::Analyzed => CallStage::Redacted,
            CallStage::Indexed => CallStage::Analyzed,
        }
    }

    pub fn is_at_least(&self, other: CallStage) -> bool {
        *self != CallStage::Failed && self.rank() >= other.rank()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub id: CallId,
    /// Stable identity from the source, used to avoid re-ingesting.
    pub source_key: String,
    pub source_name: String,
    pub display_name: String,
    /// Absolute path to the locally materialised media, if any.
    pub media_path: Option<String>,
    pub stage: CallStage,
    pub meta: Option<AudioMeta>,
    pub features: Option<AcousticFeatures>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Call {
    pub fn new(source_key: impl Into<String>, source_name: impl Into<String>, display_name: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: CallId::new(),
            source_key: source_key.into(),
            source_name: source_name.into(),
            display_name: display_name.into(),
            media_path: None,
            stage: CallStage::Discovered,
            meta: None,
            features: None,
            error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_ordering_supports_resume() {
        assert!(CallStage::Analyzed.is_at_least(CallStage::Transcribed));
        assert!(!CallStage::Decoded.is_at_least(CallStage::Analyzed));
        assert!(CallStage::Decoded.is_at_least(CallStage::Decoded));
    }

    #[test]
    fn failed_never_counts_as_progress() {
        // A failed call must re-run from the start, not be treated as complete.
        assert!(!CallStage::Failed.is_at_least(CallStage::Discovered));
    }
}
