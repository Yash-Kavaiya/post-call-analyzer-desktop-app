//! Conversation metrics computed from the transcript, with no model involved.
//!
//! Talk ratio, interruptions, silences, monologues, speech rate, questions and
//! keyword frequency are all arithmetic over timings and words. Asking a
//! language model for them would be slower, cost money per call, and — the part
//! that matters — give a different answer each time for something that has one
//! correct answer.
//!
//! Everything here is deterministic and cheap enough to run on every call
//! during the pipeline rather than on demand.

use ca_core::transcript::{Segment, SpeakerRole, Transcript};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A gap between speech long enough that someone would notice it.
///
/// Below this, pauses are the ordinary rhythm of talking; above it, the caller
/// is waiting. Two seconds is where dead air starts to feel like dead air.
const SILENCE_SECS: f64 = 2.0;

/// Overlap shorter than this is two people finishing a word at once, not one
/// talking over the other.
const INTERRUPTION_SECS: f64 = 0.4;

/// A stretch where one speaker held the floor without the other contributing.
const MONOLOGUE_SECS: f64 = 30.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerStats {
    pub speaker_index: u8,
    pub role: SpeakerRole,
    /// Seconds this speaker was talking.
    pub speech_secs: f64,
    /// Share of total speech time, 0..=1. Silence is excluded, so the shares
    /// across speakers sum to 1 rather than to the call's duration.
    pub talk_share: f32,
    pub turns: u32,
    pub words: u32,
    pub words_per_minute: f32,
    /// Turns ending in a question mark. A rough proxy for who is probing.
    pub questions: u32,
    /// Longest single uninterrupted turn.
    pub longest_turn_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Interruption {
    pub at: f64,
    /// Who started talking while somebody else still was.
    pub by_speaker: u8,
    pub over_speaker: u8,
    pub overlap_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Silence {
    pub start: f64,
    pub end: f64,
    pub secs: f64,
    /// Who spoke last before the gap — the person who left the caller waiting.
    pub after_speaker: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Monologue {
    pub speaker_index: u8,
    pub start: f64,
    pub end: f64,
    pub secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Keyword {
    pub term: String,
    pub count: u32,
    /// First time it was said, so the UI can jump there.
    pub first_at: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMetrics {
    pub speakers: Vec<SpeakerStats>,
    pub interruptions: Vec<Interruption>,
    pub silences: Vec<Silence>,
    pub monologues: Vec<Monologue>,
    pub keywords: Vec<Keyword>,
    /// Seconds nobody was speaking, including the gaps below the threshold.
    pub total_silence_secs: f64,
    /// Share of the call with no speech, 0..=1.
    pub silence_ratio: f32,
    pub longest_silence_secs: f64,
    /// Agent's share of talk time, or `None` when no speaker is labelled agent.
    pub agent_talk_share: Option<f32>,
}

/// Words too common to be about anything. Deliberately short: an aggressive
/// list starts removing domain terms, and the frequency ranking already buries
/// most noise.
const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "if", "then", "so", "because", "as", "of", "at", "by",
    "for", "with", "about", "into", "to", "from", "in", "on", "off", "over", "under", "again",
    "is", "am", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do", "does",
    "did", "will", "would", "should", "could", "can", "may", "might", "must", "i", "you", "he",
    "she", "it", "we", "they", "me", "him", "her", "us", "them", "my", "your", "his", "its", "our",
    "their", "this", "that", "these", "those", "there", "here", "what", "which", "who", "when",
    "where", "why", "how", "all", "any", "both", "each", "some", "such", "no", "not", "only",
    "own", "same", "than", "too", "very", "just", "now", "okay", "ok", "yes", "yeah", "yep", "no",
    "hello", "hi", "hey", "please", "thank", "thanks", "sorry", "right", "well", "like", "know",
    "get", "got", "going", "go", "one", "two", "up", "out", "down", "let", "sure", "actually",
    "really", "mean", "say", "said", "see", "think", "want", "need",
];

/// Longest common word length worth ignoring outright.
const MIN_KEYWORD_LEN: usize = 3;

const MAX_KEYWORDS: usize = 12;

/// Compute every metric in one pass over the transcript.
pub fn analyse(transcript: &Transcript) -> ConversationMetrics {
    let mut segments: Vec<&Segment> = transcript.segments.iter().collect();
    segments.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut metrics = ConversationMetrics {
        speakers: speaker_stats(&segments),
        interruptions: interruptions(&segments),
        silences: silences(&segments),
        monologues: monologues(&segments),
        keywords: keywords(&segments),
        ..Default::default()
    };

    metrics.total_silence_secs = metrics.silences.iter().map(|s| s.secs).sum();
    metrics.longest_silence_secs = metrics
        .silences
        .iter()
        .map(|s| s.secs)
        .fold(0.0_f64, f64::max);

    let duration = transcript
        .duration_secs
        .max(segments.last().map(|s| s.end).unwrap_or(0.0));
    if duration > 0.0 {
        metrics.silence_ratio = (metrics.total_silence_secs / duration).clamp(0.0, 1.0) as f32;
    }

    metrics.agent_talk_share = metrics
        .speakers
        .iter()
        .find(|s| s.role == SpeakerRole::Agent)
        .map(|s| s.talk_share);

    metrics
}

fn speaker_stats(segments: &[&Segment]) -> Vec<SpeakerStats> {
    let mut by_speaker: HashMap<u8, SpeakerStats> = HashMap::new();

    for seg in segments {
        let entry = by_speaker.entry(seg.speaker_index).or_insert(SpeakerStats {
            speaker_index: seg.speaker_index,
            role: seg.role,
            speech_secs: 0.0,
            talk_share: 0.0,
            turns: 0,
            words: 0,
            words_per_minute: 0.0,
            questions: 0,
            longest_turn_secs: 0.0,
        });

        let dur = seg.duration();
        entry.speech_secs += dur;
        entry.turns += 1;
        entry.longest_turn_secs = entry.longest_turn_secs.max(dur);
        // Word count from the text rather than `words`, which some engines
        // leave empty even when the text is present.
        entry.words += seg.text.split_whitespace().count() as u32;
        if seg.text.trim_end().ends_with('?') {
            entry.questions += 1;
        }
    }

    let total: f64 = by_speaker.values().map(|s| s.speech_secs).sum();

    let mut out: Vec<SpeakerStats> = by_speaker.into_values().collect();
    for s in out.iter_mut() {
        if total > 0.0 {
            s.talk_share = (s.speech_secs / total) as f32;
        }
        if s.speech_secs > 0.0 {
            s.words_per_minute = (s.words as f64 / (s.speech_secs / 60.0)) as f32;
        }
    }
    out.sort_by_key(|s| s.speaker_index);
    out
}

fn interruptions(segments: &[&Segment]) -> Vec<Interruption> {
    let mut out = Vec::new();

    for (i, seg) in segments.iter().enumerate() {
        // Compare against every earlier segment still running at this point.
        // Only earlier ones: an interruption is defined by who started second.
        for prev in segments[..i].iter().rev().take(4) {
            if prev.speaker_index == seg.speaker_index {
                continue;
            }
            let overlap = (prev.end.min(seg.end) - seg.start).max(0.0);
            if overlap >= INTERRUPTION_SECS {
                out.push(Interruption {
                    at: seg.start,
                    by_speaker: seg.speaker_index,
                    over_speaker: prev.speaker_index,
                    overlap_secs: overlap,
                });
                break;
            }
        }
    }
    out
}

fn silences(segments: &[&Segment]) -> Vec<Silence> {
    let mut out = Vec::new();
    // Track the furthest point anyone had spoken to, not just the previous
    // segment's end: with overlapping turns the previous segment can end
    // earlier than one before it, which would invent a gap that never existed.
    let mut spoken_until = f64::NEG_INFINITY;
    let mut last_speaker = None;

    for seg in segments {
        if spoken_until.is_finite() {
            let gap = seg.start - spoken_until;
            if gap >= SILENCE_SECS {
                out.push(Silence {
                    start: spoken_until,
                    end: seg.start,
                    secs: gap,
                    after_speaker: last_speaker,
                });
            }
        }
        if seg.end > spoken_until {
            spoken_until = seg.end;
            last_speaker = Some(seg.speaker_index);
        }
    }
    out
}

fn monologues(segments: &[&Segment]) -> Vec<Monologue> {
    let mut out = Vec::new();
    let mut run: Option<(u8, f64, f64)> = None;

    for seg in segments {
        match run {
            Some((speaker, start, _)) if speaker == seg.speaker_index => {
                run = Some((speaker, start, seg.end));
            }
            other => {
                if let Some((speaker, start, end)) = other {
                    if end - start >= MONOLOGUE_SECS {
                        out.push(Monologue {
                            speaker_index: speaker,
                            start,
                            end,
                            secs: end - start,
                        });
                    }
                }
                run = Some((seg.speaker_index, seg.start, seg.end));
            }
        }
    }
    if let Some((speaker, start, end)) = run {
        if end - start >= MONOLOGUE_SECS {
            out.push(Monologue {
                speaker_index: speaker,
                start,
                end,
                secs: end - start,
            });
        }
    }
    out
}

/// Distinct keyword terms in a block of text.
///
/// The same rule the per-call keyword list uses, exposed for corpus
/// aggregation. Terms only — no counts or timings — because across calls the
/// unit of evidence is "this call mentioned it", not how often.
///
/// Pass redacted text. A corpus keyword list is exported and charted, and a
/// phone number is not a topic.
pub fn keywords_in(text: &str) -> Vec<String> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for raw in text.split_whitespace() {
        if raw.starts_with('[') {
            continue;
        }
        let term: String = raw
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '\'')
            .collect::<String>()
            .to_lowercase();
        if term.len() < MIN_KEYWORD_LEN || STOPWORDS.contains(&term.as_str()) {
            continue;
        }
        *counts.entry(term).or_insert(0) += 1;
    }

    let mut out: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(term, _)| term)
        .collect();
    // Sorted so a corpus report is byte-identical across runs.
    out.sort();
    out
}

fn keywords(segments: &[&Segment]) -> Vec<Keyword> {
    let mut counts: HashMap<String, (u32, f64)> = HashMap::new();

    for seg in segments {
        // Redacted text: a keyword list is shown, exported and sometimes sent
        // onward, and a phone number is not a topic.
        for raw in seg.outbound_text().split_whitespace() {
            let term: String = raw
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .collect::<String>()
                .to_lowercase();
            if term.len() < MIN_KEYWORD_LEN || STOPWORDS.contains(&term.as_str()) {
                continue;
            }
            // Redaction tokens are placeholders, not vocabulary.
            if term.starts_with('[') || raw.starts_with('[') {
                continue;
            }
            let entry = counts.entry(term).or_insert((0, seg.start));
            entry.0 += 1;
        }
    }

    let mut out: Vec<Keyword> = counts
        .into_iter()
        .filter(|(_, (count, _))| *count > 1)
        .map(|(term, (count, first_at))| Keyword {
            term,
            count,
            first_at,
        })
        .collect();

    // Frequency first; alphabetical within a tie so the order is stable across
    // runs rather than following HashMap iteration.
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.term.cmp(&b.term)));
    out.truncate(MAX_KEYWORDS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ca_core::ids::{CallId, SegmentId};
    use ca_core::transcript::Word;

    fn seg(id: &str, speaker: u8, role: SpeakerRole, start: f64, end: f64, text: &str) -> Segment {
        Segment {
            id: SegmentId::from(id),
            speaker_index: speaker,
            role,
            start,
            end,
            text: text.into(),
            redacted_text: None,
            words: Vec::<Word>::new(),
            confidence: None,
            language: None,
        }
    }

    fn transcript(segments: Vec<Segment>) -> Transcript {
        let duration = segments.last().map(|s| s.end).unwrap_or(0.0);
        Transcript {
            call_id: CallId::from("c1"),
            segments,
            speakers: vec![],
            primary_language: None,
            languages: vec![],
            engine: "t".into(),
            model: "m".into(),
            duration_secs: duration,
        }
    }

    #[test]
    fn talk_share_is_of_speech_time_not_call_length() {
        // A call that is half silence must not report both speakers at 25%;
        // the question "who dominated the conversation" is about the talking.
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 30.0, "hello"),
            seg("b", 1, SpeakerRole::Customer, 60.0, 70.0, "hi"),
        ]);
        let m = analyse(&t);
        let agent = m.speakers.iter().find(|s| s.speaker_index == 0).unwrap();
        assert!(
            (agent.talk_share - 0.75).abs() < 1e-4,
            "got {}",
            agent.talk_share
        );
        assert_eq!(m.agent_talk_share, Some(agent.talk_share));
    }

    #[test]
    fn a_brief_overlap_is_not_an_interruption() {
        // Two people finishing a word together happens constantly; calling it
        // an interruption would bury the ones that matter.
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 10.0, "one"),
            seg("b", 1, SpeakerRole::Customer, 9.8, 15.0, "two"),
        ]);
        assert!(analyse(&t).interruptions.is_empty());
    }

    #[test]
    fn talking_over_someone_is_recorded_with_who_did_it() {
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 10.0, "still explaining"),
            seg("b", 1, SpeakerRole::Customer, 7.0, 15.0, "cutting in"),
        ]);
        let m = analyse(&t);
        assert_eq!(m.interruptions.len(), 1);
        assert_eq!(m.interruptions[0].by_speaker, 1);
        assert_eq!(m.interruptions[0].over_speaker, 0);
        assert!((m.interruptions[0].overlap_secs - 3.0).abs() < 1e-6);
    }

    #[test]
    fn silence_is_measured_from_the_furthest_point_anyone_had_spoken() {
        // With overlapping turns the previous segment can end before an earlier
        // one. Measuring from it would invent a gap that never happened.
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 20.0, "long turn"),
            seg("b", 1, SpeakerRole::Customer, 5.0, 6.0, "mm"),
            seg("c", 0, SpeakerRole::Agent, 21.0, 25.0, "continuing"),
        ]);
        assert!(
            analyse(&t).silences.is_empty(),
            "invented a silence inside speech"
        );
    }

    #[test]
    fn a_real_gap_is_reported_with_who_left_the_caller_waiting() {
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 10.0, "let me check"),
            seg("b", 1, SpeakerRole::Customer, 25.0, 30.0, "still there?"),
        ]);
        let m = analyse(&t);
        assert_eq!(m.silences.len(), 1);
        assert_eq!(m.silences[0].after_speaker, Some(0));
        assert!((m.silences[0].secs - 15.0).abs() < 1e-6);
        assert!((m.longest_silence_secs - 15.0).abs() < 1e-6);
        assert!(
            (m.silence_ratio - 0.5).abs() < 1e-4,
            "got {}",
            m.silence_ratio
        );
    }

    #[test]
    fn consecutive_turns_by_one_speaker_form_a_monologue() {
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 15.0, "part one"),
            seg("b", 0, SpeakerRole::Agent, 15.0, 40.0, "part two"),
            seg("c", 1, SpeakerRole::Customer, 41.0, 45.0, "ok"),
        ]);
        let m = analyse(&t);
        assert_eq!(m.monologues.len(), 1);
        assert_eq!(m.monologues[0].speaker_index, 0);
        assert!((m.monologues[0].secs - 40.0).abs() < 1e-6);
    }

    #[test]
    fn a_short_run_is_not_a_monologue() {
        let t = transcript(vec![
            seg("a", 0, SpeakerRole::Agent, 0.0, 5.0, "hi"),
            seg("b", 1, SpeakerRole::Customer, 6.0, 9.0, "hello"),
        ]);
        assert!(analyse(&t).monologues.is_empty());
    }

    #[test]
    fn keywords_rank_by_frequency_and_ignore_filler() {
        let t = transcript(vec![
            seg(
                "a",
                0,
                SpeakerRole::Agent,
                0.0,
                10.0,
                "the payment failed and the payment retry",
            ),
            seg(
                "b",
                1,
                SpeakerRole::Customer,
                11.0,
                20.0,
                "yes the payment failed again",
            ),
        ]);
        let m = analyse(&t);
        assert_eq!(m.keywords[0].term, "payment");
        assert_eq!(m.keywords[0].count, 3);
        assert!(
            m.keywords.iter().all(|k| k.term != "the"),
            "stopword survived"
        );
        assert!(m.keywords.iter().all(|k| k.term != "yes"));
    }

    #[test]
    fn a_word_said_once_is_not_a_keyword() {
        let t = transcript(vec![seg(
            "a",
            0,
            SpeakerRole::Agent,
            0.0,
            5.0,
            "escalation",
        )]);
        assert!(analyse(&t).keywords.is_empty());
    }

    #[test]
    fn redaction_tokens_are_not_treated_as_vocabulary() {
        // The keyword list is shown and exported; a placeholder is not a topic,
        // and a real number must never become one either.
        let mut a = seg("a", 0, SpeakerRole::Agent, 0.0, 5.0, "call 9876543210 now");
        a.redacted_text = Some("call [PHONE_1] now".into());
        let mut b = seg(
            "b",
            0,
            SpeakerRole::Agent,
            6.0,
            10.0,
            "call 9876543210 again",
        );
        b.redacted_text = Some("call [PHONE_1] again".into());
        let m = analyse(&transcript(vec![a, b]));
        let terms: Vec<&str> = m.keywords.iter().map(|k| k.term.as_str()).collect();
        assert!(!terms.iter().any(|t| t.contains("phone")), "{terms:?}");
        assert!(
            !terms.iter().any(|t| t.contains("9876")),
            "raw PII became a keyword"
        );
    }

    #[test]
    fn questions_are_attributed_to_whoever_asked_them() {
        let t = transcript(vec![
            seg(
                "a",
                0,
                SpeakerRole::Agent,
                0.0,
                5.0,
                "can I take your order number?",
            ),
            seg("b", 1, SpeakerRole::Customer, 6.0, 9.0, "it is 4417"),
            seg("c", 0, SpeakerRole::Agent, 10.0, 14.0, "anything else?"),
        ]);
        let m = analyse(&t);
        assert_eq!(
            m.speakers
                .iter()
                .find(|s| s.speaker_index == 0)
                .unwrap()
                .questions,
            2
        );
        assert_eq!(
            m.speakers
                .iter()
                .find(|s| s.speaker_index == 1)
                .unwrap()
                .questions,
            0
        );
    }

    #[test]
    fn speech_rate_is_per_speaker_over_their_own_talking_time() {
        // 10 words in 30 seconds of speech is 20 wpm — not 10 words over the
        // whole call, which would depend on how long the other person talked.
        let t = transcript(vec![
            seg(
                "a",
                0,
                SpeakerRole::Agent,
                0.0,
                30.0,
                "one two three four five six seven eight nine ten",
            ),
            seg("b", 1, SpeakerRole::Customer, 30.0, 120.0, "yes"),
        ]);
        let m = analyse(&t);
        let agent = m.speakers.iter().find(|s| s.speaker_index == 0).unwrap();
        assert!(
            (agent.words_per_minute - 20.0).abs() < 0.01,
            "got {}",
            agent.words_per_minute
        );
    }

    #[test]
    fn an_empty_transcript_produces_zeroes_rather_than_panicking() {
        let m = analyse(&transcript(vec![]));
        assert!(m.speakers.is_empty());
        assert_eq!(m.silence_ratio, 0.0);
        assert_eq!(m.agent_talk_share, None);
    }

    #[test]
    fn out_of_order_segments_are_handled() {
        // Word-level resegmentation can emit turns out of order; sorting here
        // means the caller does not have to know that.
        let t = transcript(vec![
            seg("b", 1, SpeakerRole::Customer, 25.0, 30.0, "second"),
            seg("a", 0, SpeakerRole::Agent, 0.0, 10.0, "first"),
        ]);
        let m = analyse(&t);
        assert_eq!(m.silences.len(), 1, "ordering broke gap detection");
    }

    #[test]
    fn metrics_cross_the_wire_as_camel_case() {
        let t = transcript(vec![seg(
            "a",
            0,
            SpeakerRole::Agent,
            0.0,
            10.0,
            "hello there",
        )]);
        let v = serde_json::to_value(analyse(&t)).unwrap();
        for k in v.as_object().unwrap().keys() {
            assert!(!k.contains('_'), "ConversationMetrics serialises `{k}`");
        }
        let s = &v["speakers"][0];
        for k in s.as_object().unwrap().keys() {
            assert!(!k.contains('_'), "SpeakerStats serialises `{k}`");
        }
        assert!(s.get("wordsPerMinute").is_some());
    }
}
