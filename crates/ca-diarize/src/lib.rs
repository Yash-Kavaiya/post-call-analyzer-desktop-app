//! Speaker diarization.
//!
//! # Scope and honesty about it
//!
//! This is an **acoustic-clustering** diarizer, not a neural one. It groups
//! speech regions by voice characteristics (pitch, spectral shape, level) and
//! splits them into N clusters. On two-party phone calls with dissimilar
//! voices — the common agent/customer case — it is reliable. On two speakers
//! with similar voices it degrades, and it does not model overlapping speech.
//!
//! A neural embedding model (sherpa-onnx / pyannote) is the upgrade path and
//! plugs in behind the same [`Diarizer`] trait. Until then, callers should
//! treat `role_confidence` as real and surface low-confidence assignments in
//! the UI rather than presenting them as fact — a QA scorecard built on a
//! wrong speaker mapping is worse than none.

use async_trait::async_trait;
use ca_core::audio::AudioBuffer;
use ca_core::error::Result;
use ca_core::provider::{Diarizer, SpeakerTurn};

mod features;
mod kmeans;

pub use features::SegmentFeatures;

/// Minimum speech needed before clustering is meaningful.
const MIN_SPEECH_SECS: f64 = 2.0;

/// Half-width of the smoothing window, in analysis windows. With 1.5 s windows
/// this makes a turn at least ~4.5 s before it survives — about the shortest a
/// real conversational turn tends to be.
const SMOOTHING_WINDOW: usize = 1;

/// Replace each label with the majority label in its neighbourhood.
///
/// A mode filter rather than a median: labels are categorical, so "middle
/// value" is meaningless, but "what do my neighbours mostly say" is exactly
/// the question worth asking.
fn smooth_labels(labels: &mut [usize], half_width: usize) {
    if labels.len() < 3 || half_width == 0 {
        return;
    }
    let original = labels.to_vec();
    for i in 0..labels.len() {
        let lo = i.saturating_sub(half_width);
        let hi = (i + half_width + 1).min(original.len());

        let mut counts: std::collections::BTreeMap<usize, usize> = Default::default();
        for l in &original[lo..hi] {
            *counts.entry(*l).or_insert(0) += 1;
        }
        // Ties keep the original label, so smoothing never invents a change.
        let best = counts.iter().max_by_key(|(_, c)| **c).map(|(l, _)| *l);
        if let Some(b) = best {
            let best_count = counts[&b];
            let own_count = counts.get(&original[i]).copied().unwrap_or(0);
            if best_count > own_count {
                labels[i] = b;
            }
        }
    }
}

pub struct AcousticDiarizer {
    /// Analysis window; also the finest speaker-change resolution.
    window_secs: f64,
    min_pause_secs: f64,
}

impl Default for AcousticDiarizer {
    fn default() -> Self {
        Self {
            // 1.5 s is long enough for a stable pitch estimate and short
            // enough to catch a quick back-and-forth exchange.
            window_secs: 1.5,
            min_pause_secs: 0.35,
        }
    }
}

impl AcousticDiarizer {
    pub fn new(window_secs: f64, min_pause_secs: f64) -> Self {
        Self {
            window_secs,
            min_pause_secs,
        }
    }
}

#[async_trait]
impl Diarizer for AcousticDiarizer {
    fn name(&self) -> &str {
        "acoustic-kmeans"
    }

    async fn diarize(
        &self,
        audio: &AudioBuffer,
        num_speakers: Option<u8>,
    ) -> Result<Vec<SpeakerTurn>> {
        let k = num_speakers.unwrap_or(2).max(1) as usize;

        let vad = ca_audio::vad::detect(&audio.samples, audio.sample_rate, self.min_pause_secs);
        if vad.speech_secs < MIN_SPEECH_SECS {
            // Too little speech to cluster; attribute everything to one
            // speaker rather than inventing a split.
            return Ok(vad
                .speech
                .iter()
                .map(|r| SpeakerTurn {
                    speaker_index: 0,
                    start: r.start,
                    end: r.end,
                })
                .collect());
        }

        let windows = features::extract_windows(
            &audio.samples,
            audio.sample_rate,
            &vad,
            self.window_secs,
        );

        if windows.len() < k {
            return Ok(windows
                .iter()
                .map(|w| SpeakerTurn {
                    speaker_index: 0,
                    start: w.start,
                    end: w.end,
                })
                .collect());
        }

        let vectors: Vec<Vec<f64>> = windows.iter().map(|w| w.vector()).collect();
        let mut labels = kmeans::cluster(&vectors, k.min(windows.len()));

        // Clustering treats each window independently, so a single atypical
        // window flips the speaker for 1.5 s and flips back. Conversations do
        // not work that way: a turn lasts seconds. Smoothing the label
        // sequence removes those isolated flips, which otherwise fragment the
        // transcript into unreadable one-word segments.
        smooth_labels(&mut labels, SMOOTHING_WINDOW);

        // Merge adjacent windows with the same label into one turn; a caller
        // wants "speaker 1 spoke from 12s to 19s", not fifty 1.5s fragments.
        let mut turns: Vec<SpeakerTurn> = Vec::new();
        for (w, &label) in windows.iter().zip(labels.iter()) {
            match turns.last_mut() {
                Some(last)
                    if last.speaker_index == label as u8
                        && (w.start - last.end) < self.min_pause_secs =>
                {
                    last.end = w.end;
                }
                _ => turns.push(SpeakerTurn {
                    speaker_index: label as u8,
                    start: w.start,
                    end: w.end,
                }),
            }
        }

        Ok(turns)
    }
}

/// Assign a speaker index to each transcript segment by maximum time overlap
/// with the diarized turns.
///
/// Overlap rather than midpoint, because ASR segment boundaries and diarization
/// boundaries never line up exactly and a midpoint can fall in a gap.
///
/// Prefer [`resegment_by_speaker`] where word timings exist: ASR segments
/// routinely span a turn boundary, and forcing one speaker onto such a segment
/// mislabels half of it.
pub fn assign_speakers(
    segments: &mut [ca_core::transcript::Segment],
    turns: &[SpeakerTurn],
) {
    for seg in segments.iter_mut() {
        if let Some(idx) = speaker_at(seg.start, seg.end, turns) {
            seg.speaker_index = idx;
        }
    }
}

/// Which speaker covers `[start, end)`, by greatest overlap.
fn speaker_at(start: f64, end: f64, turns: &[SpeakerTurn]) -> Option<u8> {
    let mut best: Option<(f64, u8)> = None;
    for t in turns {
        let overlap = (end.min(t.end) - start.max(t.start)).max(0.0);
        if overlap > 0.0 && best.map(|(o, _)| overlap > o).unwrap_or(true) {
            best = Some((overlap, t.speaker_index));
        }
    }
    best.map(|(_, idx)| idx)
}

/// Rebuild segments so each one belongs to exactly one speaker.
///
/// ASR segments are chosen for transcription convenience, not conversational
/// structure: a single one often contains a question and its answer from two
/// different people. Since every word carries its own timing, the honest unit
/// is the word — assign a speaker per word, then regroup runs of the same
/// speaker into segments.
///
/// Segments without word timings are passed through with a whole-segment
/// assignment, so a transcript from an engine that omits them still works.
pub fn resegment_by_speaker(
    transcript: &mut ca_core::transcript::Transcript,
    turns: &[SpeakerTurn],
) {
    use ca_core::ids::SegmentId;
    use ca_core::transcript::Segment;

    if turns.is_empty() {
        return;
    }

    let mut out: Vec<Segment> = Vec::with_capacity(transcript.segments.len());

    for seg in transcript.segments.drain(..) {
        if seg.words.is_empty() {
            let mut s = seg;
            if let Some(idx) = speaker_at(s.start, s.end, turns) {
                s.speaker_index = idx;
            }
            out.push(s);
            continue;
        }

        let mut current: Option<Segment> = None;

        for w in seg.words.into_iter() {
            let spk = speaker_at(w.start, w.end, turns)
                .or_else(|| speaker_at(seg.start, seg.end, turns))
                .unwrap_or(seg.speaker_index);

            match current.as_mut() {
                Some(cur) if cur.speaker_index == spk => {
                    cur.text.push(' ');
                    cur.text.push_str(&w.text);
                    cur.end = w.end;
                    cur.words.push(w);
                }
                _ => {
                    if let Some(done) = current.take() {
                        out.push(done);
                    }
                    current = Some(Segment {
                        id: SegmentId::new(),
                        speaker_index: spk,
                        role: seg.role,
                        start: w.start,
                        end: w.end,
                        text: w.text.clone(),
                        redacted_text: None,
                        words: vec![w],
                        confidence: seg.confidence,
                        language: seg.language.clone(),
                    });
                }
            }
        }

        if let Some(done) = current.take() {
            out.push(done);
        }
    }

    // Recompute per-speaker totals so the dashboard reflects the new split.
    let mut speakers: std::collections::BTreeMap<u8, (f64, u32)> = Default::default();
    for s in &out {
        let e = speakers.entry(s.speaker_index).or_insert((0.0, 0));
        e.0 += s.duration();
        e.1 += 1;
    }
    transcript.speakers = speakers
        .into_iter()
        .map(|(index, (secs, turns))| ca_core::transcript::Speaker {
            index,
            role: ca_core::transcript::SpeakerRole::Unknown,
            role_confidence: None,
            total_speech_secs: secs,
            turn_count: turns,
        })
        .collect();

    transcript.segments = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::SegmentId;
    use ca_core::transcript::{Segment, SpeakerRole};

    fn seg(start: f64, end: f64) -> Segment {
        Segment {
            id: SegmentId::new(),
            speaker_index: 0,
            role: SpeakerRole::Unknown,
            start,
            end,
            text: "x".into(),
            redacted_text: None,
            words: vec![],
            confidence: None,
            language: None,
        }
    }

    fn turn(idx: u8, start: f64, end: f64) -> SpeakerTurn {
        SpeakerTurn {
            speaker_index: idx,
            start,
            end,
        }
    }

    #[test]
    fn segments_take_the_speaker_they_overlap_most() {
        let mut segs = vec![seg(0.0, 5.0), seg(5.0, 10.0)];
        let turns = vec![turn(0, 0.0, 5.0), turn(1, 5.0, 10.0)];
        assign_speakers(&mut segs, &turns);
        assert_eq!(segs[0].speaker_index, 0);
        assert_eq!(segs[1].speaker_index, 1);
    }

    #[test]
    fn partial_overlap_picks_the_dominant_speaker() {
        // Boundaries never align exactly; the majority of the segment wins.
        let mut segs = vec![seg(4.0, 10.0)];
        let turns = vec![turn(0, 0.0, 5.0), turn(1, 5.0, 12.0)];
        assign_speakers(&mut segs, &turns);
        assert_eq!(segs[0].speaker_index, 1, "5s with spk1 beats 1s with spk0");
    }

    #[test]
    fn segments_with_no_overlap_keep_their_existing_speaker() {
        let mut segs = vec![seg(50.0, 55.0)];
        let turns = vec![turn(1, 0.0, 5.0)];
        assign_speakers(&mut segs, &turns);
        assert_eq!(segs[0].speaker_index, 0);
    }

    #[tokio::test]
    async fn very_short_audio_yields_a_single_speaker() {
        let sr = 16_000u32;
        let samples: Vec<f32> = (0..sr as usize)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (0.4 * (2.0 * std::f64::consts::PI * 150.0 * t).sin()) as f32
            })
            .collect();
        let audio = AudioBuffer {
            samples,
            sample_rate: sr,
        };
        let turns = AcousticDiarizer::default().diarize(&audio, Some(2)).await.unwrap();
        assert!(turns.iter().all(|t| t.speaker_index == 0));
    }

    #[tokio::test]
    async fn two_distinct_voices_separate_into_two_clusters() {
        // Alternating low and high pitched speech, 3 s each. Distinct enough
        // that clustering must find the boundary.
        let sr = 16_000u32;
        let mut samples = Vec::new();
        for block in 0..4 {
            let freq = if block % 2 == 0 { 120.0 } else { 260.0 };
            for i in 0..(sr as usize * 3) {
                let t = i as f64 / sr as f64;
                samples.push((0.4 * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32);
            }
        }
        let audio = AudioBuffer {
            samples,
            sample_rate: sr,
        };
        let turns = AcousticDiarizer::default().diarize(&audio, Some(2)).await.unwrap();
        let distinct: std::collections::HashSet<u8> =
            turns.iter().map(|t| t.speaker_index).collect();
        assert_eq!(distinct.len(), 2, "turns: {turns:?}");
    }

    fn word(text: &str, start: f64, end: f64) -> ca_core::transcript::Word {
        ca_core::transcript::Word {
            text: text.into(),
            start,
            end,
            confidence: Some(0.9),
        }
    }

    fn transcript_with_words() -> ca_core::transcript::Transcript {
        let mut s = seg(0.0, 4.0);
        // One ASR segment spanning a question and its answer.
        s.words = vec![
            word("Am", 0.0, 0.5),
            word("I", 0.5, 1.0),
            word("speaking?", 1.0, 2.0),
            word("Yes.", 3.0, 4.0),
        ];
        ca_core::transcript::Transcript {
            call_id: ca_core::CallId::from("c"),
            segments: vec![s],
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: 4.0,
        }
    }

    #[test]
    fn resegmentation_splits_a_turn_boundary_inside_one_asr_segment() {
        let mut t = transcript_with_words();
        let turns = vec![turn(0, 0.0, 2.5), turn(1, 2.5, 4.0)];
        resegment_by_speaker(&mut t, &turns);

        assert_eq!(t.segments.len(), 2, "segments: {:?}",
            t.segments.iter().map(|s| (s.speaker_index, s.text.clone())).collect::<Vec<_>>());
        assert_eq!(t.segments[0].speaker_index, 0);
        assert_eq!(t.segments[0].text, "Am I speaking?");
        assert_eq!(t.segments[1].speaker_index, 1);
        assert_eq!(t.segments[1].text, "Yes.");
    }

    #[test]
    fn resegmentation_keeps_one_segment_when_a_single_speaker_talks() {
        let mut t = transcript_with_words();
        let turns = vec![turn(0, 0.0, 4.0)];
        resegment_by_speaker(&mut t, &turns);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].text, "Am I speaking? Yes.");
    }

    #[test]
    fn resegmentation_recomputes_speaker_totals() {
        let mut t = transcript_with_words();
        let turns = vec![turn(0, 0.0, 2.5), turn(1, 2.5, 4.0)];
        resegment_by_speaker(&mut t, &turns);
        assert_eq!(t.speakers.len(), 2);
        let spk0 = t.speakers.iter().find(|s| s.index == 0).unwrap();
        assert!(spk0.total_speech_secs > 0.0);
    }

    #[test]
    fn segments_without_word_timings_pass_through() {
        let mut t = transcript_with_words();
        t.segments[0].words.clear();
        let turns = vec![turn(1, 0.0, 4.0)];
        resegment_by_speaker(&mut t, &turns);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].speaker_index, 1);
    }

    #[test]
    fn empty_turns_leave_the_transcript_untouched() {
        let mut t = transcript_with_words();
        let before = t.segments.len();
        resegment_by_speaker(&mut t, &[]);
        assert_eq!(t.segments.len(), before);
    }

    #[test]
    fn smoothing_removes_an_isolated_flip() {
        // One stray window should not create a two-speaker exchange.
        let mut l = vec![0, 0, 1, 0, 0];
        smooth_labels(&mut l, 1);
        assert_eq!(l, vec![0, 0, 0, 0, 0]);
    }

    #[test]
    fn smoothing_preserves_a_genuine_turn_change() {
        let mut l = vec![0, 0, 0, 1, 1, 1];
        smooth_labels(&mut l, 1);
        assert_eq!(l, vec![0, 0, 0, 1, 1, 1], "real turns must survive");
    }

    #[test]
    fn smoothing_is_a_noop_on_tiny_inputs() {
        let mut l = vec![0, 1];
        smooth_labels(&mut l, 1);
        assert_eq!(l, vec![0, 1]);
    }
}
