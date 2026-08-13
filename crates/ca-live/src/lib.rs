//! Live call capture and analysis.
//!
//! Phase 1 built a corpus, Phase 2 made every agent's output visible. This runs
//! the same idea against a call that has not finished yet: capture both sides,
//! transcribe as it happens, and surface breaches while the agent can still act
//! on them.
//!
//! # Two tiers, twice
//!
//! **Transcription.** The live tier is a warm `whisper-server` on `base.en`,
//! fast and approximate, and it never reaches storage. The authoritative tier is
//! the existing batch pipeline on `small.en`, which re-transcribes the recorded
//! audio at hang-up. That separation is what makes the live tier's accuracy
//! acceptable — nothing it gets wrong reaches the corpus, the scorecard or the
//! risk register.
//!
//! **Analysis.** [`ca_rules`] runs on every chunk for nothing, offline, and
//! catches the things where being fifteen seconds late is the difference
//! between stopping a breach and logging one. The `LiveMonitorAgent` runs on a
//! throttle for judgement — sentiment, risk, what to say next.
//!
//! # Everything effectful is behind a trait
//!
//! [`AudioSource`] and [`ChunkTranscriber`] each have a real implementation and
//! a test one. Real capture needs two devices and a person talking, so it can
//! never run in CI; [`ReplaySource`] plus [`StubTranscriber`] drive the identical
//! path deterministically. The untestable surface is one file.

pub mod advisor;
pub mod asr;
pub mod chunker;
pub mod event;
pub mod session;
pub mod source;
pub mod wav;

#[cfg(windows)]
pub mod capture;

pub use advisor::ProviderAdvisor;
pub use asr::{ChunkTranscriber, ServerHandle, StubTranscriber, WhisperServer};
pub use chunker::{Chunk, Chunker};
pub use event::LiveEvent;
pub use session::{LiveAdvisor, LiveConfig, LiveSession, SessionOutcome};
pub use source::{AudioSource, Frame, ReplaySource};

use ca_core::transcript::SpeakerRole;
use serde::{Deserialize, Serialize};

/// Which capture device an utterance arrived on.
///
/// This is the whole reason live calls get exact speaker labels. The agent is
/// on the microphone and the customer is on the system loopback, so the
/// question the acoustic diarizer spends effort guessing at is already answered
/// by which stream the samples came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Channel {
    Agent,
    Customer,
}

impl Channel {
    /// Index used in the stored timeline and as `Segment::speaker_index`.
    pub fn index(&self) -> u8 {
        match self {
            Channel::Agent => 0,
            Channel::Customer => 1,
        }
    }

    pub fn role(&self) -> SpeakerRole {
        match self {
            Channel::Agent => SpeakerRole::Agent,
            Channel::Customer => SpeakerRole::Customer,
        }
    }

    pub fn all() -> [Channel; 2] {
        [Channel::Agent, Channel::Customer]
    }
}

/// Sample rate everything downstream assumes. whisper wants 16 kHz mono, and
/// resampling once at capture is cheaper than doing it per chunk.
pub const SAMPLE_RATE: u32 = 16_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_index_matches_the_role_it_stands_for() {
        // The index is persisted in the channel timeline and read back by
        // `roles_from_timeline`, which maps 0 to agent. If these disagree,
        // every live call comes back with its speakers swapped.
        assert_eq!(Channel::Agent.index(), 0);
        assert_eq!(Channel::Agent.role(), SpeakerRole::Agent);
        assert_eq!(Channel::Customer.index(), 1);
        assert_eq!(Channel::Customer.role(), SpeakerRole::Customer);
    }

    #[test]
    fn channels_cross_the_wire_as_the_ui_spells_them() {
        assert_eq!(
            serde_json::to_value(Channel::Customer).unwrap(),
            serde_json::json!("customer")
        );
    }
}
