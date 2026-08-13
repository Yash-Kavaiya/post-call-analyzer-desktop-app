//! Domain types and provider traits shared by every crate in the workspace.
//!
//! This crate performs no I/O and depends on no vendor SDK. Everything that
//! touches the network, the filesystem, or a model lives behind one of the
//! traits in [`provider`], which is what lets the desktop app, the CLI, and a
//! future server share one implementation.

pub mod analysis;
pub mod audio;
pub mod call;
pub mod error;
pub mod ids;
pub mod provider;
pub mod transcript;

pub use analysis::{
    ActionItem, ActionItemsReport, AgentKind, Analysis, Citation, ComplianceReport, CriterionScore,
    Finding, LiveMonitorReport, RootCauseReport, Scorecard, SentimentPoint, SentimentReport,
    Severity, Usage,
};
pub use audio::{
    AcousticFeatures, AudioBuffer, AudioMeta, CrosstalkRegion, PauseRegion, PeakLevel,
    WaveformPeaks, TARGET_LUFS, TARGET_SAMPLE_RATE,
};
pub use call::{Call, CallStage};
pub use error::{CaError, Result};
pub use ids::{AnalysisId, CallId, FindingId, JobId, SegmentId};
pub use provider::{
    AgentContext, Caps, ChatRequest, ChatResponse, Diarizer, LlmProvider, Message, Role, Source,
    SourceItem, SourceKind, SpeakerTurn, TranscribeOptions, Transcriber,
};
pub use transcript::{
    ChannelTimeline, Segment, Speaker, SpeakerRole, SpeechSpan, Transcript, Word,
};
