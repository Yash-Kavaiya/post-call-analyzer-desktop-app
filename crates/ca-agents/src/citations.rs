//! Shared citation validation.
//!
//! Models invent segment ids. Anything that survives into the database must
//! point at a real span with timings taken from the transcript, not the model.

use ca_core::analysis::Citation;
use ca_core::ids::SegmentId;
use ca_core::transcript::Transcript;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ModelCitation {
    #[serde(default)]
    pub segment_id: String,
    #[serde(default)]
    pub quote: Option<String>,
}

/// Keep only citations whose segment_id exists; stamp timings from the transcript.
pub fn validate_citations(
    raw: &[ModelCitation],
    transcript: &Transcript,
) -> (Vec<Citation>, usize) {
    let by_id: HashMap<&str, &ca_core::transcript::Segment> = transcript
        .segments
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect();

    let mut out = Vec::new();
    let mut dropped = 0usize;
    for c in raw {
        match by_id.get(c.segment_id.as_str()) {
            Some(seg) => out.push(Citation {
                segment_id: SegmentId::from(c.segment_id.clone()),
                start: seg.start,
                end: seg.end,
                quote: c.quote.clone(),
            }),
            None => {
                dropped += 1;
                tracing::warn!(segment_id = %c.segment_id, "dropping fabricated citation");
            }
        }
    }
    (out, dropped)
}

pub fn transcript_block(transcript: &Transcript) -> String {
    let mut s = String::from("TRANSCRIPT (format: [mm:ss] (segment_id) Speaker: text):\n");
    s.push_str(&transcript.to_prompt_format());
    s
}
