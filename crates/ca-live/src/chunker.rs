//! Cutting a live stream into transcribable pieces.
//!
//! The measurements that drove this are in the Phase 3 spec, and they rule out
//! the obvious design. whisper processes a 30-second mel window internally, so
//! a three-second chunk costs almost exactly what a ten-second chunk costs —
//! roughly one second on `base.en`. A short fixed cadence therefore burns the
//! entire budget on overhead and still falls behind the moment someone
//! hesitates.
//!
//! So chunks are cut at pauses instead. Two things follow, and both matter:
//!
//! - **Boundaries never fall mid-word**, so there is no need to re-transcribe
//!   an overlapping window to fix up the seam. Overlap re-transcription at
//!   these inference costs would not fit in realtime.
//! - **Silence is free.** Nothing is sent while nobody is speaking, and since
//!   the two sides of a call mostly alternate, the two channels together cost
//!   about what one continuous stream would.
//!
//! The detector is a fixed-threshold energy gate rather than the adaptive one
//! in `ca-audio`. That crate's detector needs the whole buffer to establish a
//! noise floor, which a live stream does not have — and here the consequence of
//! a wrong frame is a slightly early or late cut, not a wrong measurement.

/// One closed utterance, ready to transcribe.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub start_secs: f64,
    pub end_secs: f64,
    pub samples: Vec<f32>,
}

impl Chunk {
    pub fn duration_secs(&self) -> f64 {
        self.end_secs - self.start_secs
    }
}

/// Analysis frame. Short enough to place a boundary precisely, long enough that
/// the RMS of one is a meaningful level rather than a single glottal pulse.
const FRAME_MS: f64 = 20.0;

/// Below this a frame counts as silence. Phone audio is normalised well above
/// it, and room noise on an open mic sits well below.
const SILENCE_DBFS: f32 = -45.0;

/// Silence this long ends an utterance. Shorter than this is the gap between
/// words, and cutting there would split "four hundred" down the middle.
const CLOSE_AFTER_SILENCE_SECS: f64 = 0.4;

/// An utterance shorter than this is a cough or a back-channel, not something
/// worth a transcription round trip.
const MIN_SPEECH_SECS: f64 = 1.5;

/// Longest an utterance may run before it is cut anyway.
///
/// Without this, someone reading out an address holds the transcript hostage
/// until they pause. The cut costs one split word; not cutting costs the agent
/// seeing nothing for the whole monologue.
const MAX_CHUNK_SECS: f64 = 15.0;

/// Streaming voice-activity chunker for one channel.
pub struct Chunker {
    sample_rate: u32,
    frame_len: usize,
    /// Samples not yet forming a whole frame.
    pending: Vec<f32>,
    /// The utterance being accumulated, if any.
    current: Option<Utterance>,
    /// Seconds of audio pushed so far — the clock everything is stamped with.
    elapsed_secs: f64,
}

struct Utterance {
    start_secs: f64,
    samples: Vec<f32>,
    speech_secs: f64,
    trailing_silence_secs: f64,
}

impl Chunker {
    pub fn new(sample_rate: u32) -> Self {
        let frame_len = ((sample_rate as f64) * FRAME_MS / 1000.0).max(1.0) as usize;
        Self {
            sample_rate,
            frame_len,
            pending: Vec::with_capacity(frame_len * 4),
            current: None,
            elapsed_secs: 0.0,
        }
    }

    /// Whether a chunk is currently open — what drives the "listening" marker.
    pub fn speaking(&self) -> bool {
        self.current.is_some()
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.elapsed_secs
    }

    /// Feed newly captured samples. Returns every chunk that closed.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Chunk> {
        self.pending.extend_from_slice(samples);
        let frame_secs = self.frame_len as f64 / self.sample_rate as f64;
        let mut out = Vec::new();

        while self.pending.len() >= self.frame_len {
            let frame: Vec<f32> = self.pending.drain(..self.frame_len).collect();
            let voiced = rms_dbfs(&frame) > SILENCE_DBFS;

            match (&mut self.current, voiced) {
                (None, true) => {
                    self.current = Some(Utterance {
                        start_secs: self.elapsed_secs,
                        samples: frame,
                        speech_secs: frame_secs,
                        trailing_silence_secs: 0.0,
                    });
                }
                // Silence with nothing open is the common case and costs nothing.
                (None, false) => {}
                (Some(u), true) => {
                    u.samples.extend_from_slice(&frame);
                    u.speech_secs += frame_secs;
                    u.trailing_silence_secs = 0.0;
                }
                (Some(u), false) => {
                    // Trailing silence is kept in the audio: whisper transcribes
                    // a word ending at the very last sample less reliably.
                    u.samples.extend_from_slice(&frame);
                    u.trailing_silence_secs += frame_secs;
                }
            }

            self.elapsed_secs += frame_secs;

            let close = match &self.current {
                None => false,
                Some(u) => {
                    let long_enough = u.speech_secs >= MIN_SPEECH_SECS;
                    let paused = u.trailing_silence_secs >= CLOSE_AFTER_SILENCE_SECS;
                    let capped = (self.elapsed_secs - u.start_secs) >= MAX_CHUNK_SECS;
                    (long_enough && paused) || (capped && long_enough)
                }
            };
            if close {
                if let Some(c) = self.close() {
                    out.push(c);
                }
            } else if let Some(u) = &self.current {
                // A long stretch of pure silence after a too-short noise: drop
                // it rather than carrying a cough forward for fifteen seconds.
                if u.speech_secs < MIN_SPEECH_SECS
                    && u.trailing_silence_secs >= CLOSE_AFTER_SILENCE_SECS
                {
                    self.current = None;
                }
            }
        }
        out
    }

    /// Close whatever is open. Called when the call ends.
    pub fn flush(&mut self) -> Option<Chunk> {
        // No minimum here: at the end of a call the last few words are worth
        // more than the round trip is worth saving.
        self.close_unconditionally()
    }

    fn close(&mut self) -> Option<Chunk> {
        self.close_unconditionally()
    }

    fn close_unconditionally(&mut self) -> Option<Chunk> {
        let u = self.current.take()?;
        if u.samples.is_empty() {
            return None;
        }
        Some(Chunk {
            start_secs: u.start_secs,
            end_secs: u.start_secs + u.samples.len() as f64 / self.sample_rate as f64,
            samples: u.samples,
        })
    }
}

fn rms_dbfs(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return f32::NEG_INFINITY;
    }
    let sum: f32 = frame.iter().map(|s| s * s).sum();
    let rms = (sum / frame.len() as f32).sqrt();
    20.0 * rms.max(1e-10).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn silence(secs: f64) -> Vec<f32> {
        vec![0.0; (SR as f64 * secs) as usize]
    }

    /// A loud, obviously-voiced signal.
    fn speech(secs: f64) -> Vec<f32> {
        let n = (SR as f64 * secs) as usize;
        (0..n)
            .map(|i| (i as f32 / 40.0).sin() * 0.4)
            .collect::<Vec<f32>>()
    }

    #[test]
    fn silence_alone_produces_nothing() {
        let mut c = Chunker::new(SR);
        assert!(c.push(&silence(5.0)).is_empty());
        assert!(!c.speaking());
        assert!(c.flush().is_none());
    }

    #[test]
    fn speech_then_a_pause_closes_one_chunk() {
        let mut c = Chunker::new(SR);
        assert!(c.push(&speech(2.0)).is_empty(), "closed before the pause");
        let out = c.push(&silence(1.0));
        assert_eq!(out.len(), 1);
        assert!(out[0].duration_secs() >= 2.0);
        assert!(!c.speaking(), "the utterance is closed");
    }

    #[test]
    fn a_gap_between_words_does_not_split_the_utterance() {
        // 0.2s is the space between "four" and "hundred". Cutting there would
        // hand whisper two fragments instead of a number.
        let mut c = Chunker::new(SR);
        let mut out = c.push(&speech(1.0));
        out.extend(c.push(&silence(0.2)));
        out.extend(c.push(&speech(1.0)));
        assert!(out.is_empty(), "split on a word gap: {out:?}");
        assert!(c.speaking());
    }

    #[test]
    fn a_cough_is_not_worth_a_round_trip() {
        let mut c = Chunker::new(SR);
        let mut out = c.push(&speech(0.3));
        out.extend(c.push(&silence(1.0)));
        assert!(out.is_empty(), "transcribed a 0.3s noise");
        assert!(!c.speaking(), "the cough was dropped rather than held open");
    }

    #[test]
    fn an_unbroken_monologue_is_cut_at_the_cap() {
        // Otherwise someone reading an address holds the transcript hostage
        // until they pause for breath.
        let mut c = Chunker::new(SR);
        let out = c.push(&speech(20.0));
        assert_eq!(out.len(), 1, "got {} chunks", out.len());
        assert!(out[0].duration_secs() <= MAX_CHUNK_SECS + 0.1);
    }

    #[test]
    fn the_end_of_a_call_flushes_what_is_open() {
        // The last few words matter more than the round trip saved.
        let mut c = Chunker::new(SR);
        assert!(c.push(&speech(0.8)).is_empty());
        let last = c.flush().expect("the tail was dropped");
        assert!(last.duration_secs() > 0.5);
    }

    #[test]
    fn chunks_are_stamped_on_the_stream_clock() {
        // Timings become segment timings, which citations seek to. An offset
        // here puts every seek button in the wrong place.
        let mut c = Chunker::new(SR);
        c.push(&silence(3.0));
        c.push(&speech(2.0));
        let out = c.push(&silence(1.0));
        assert!(
            (out[0].start_secs - 3.0).abs() < 0.1,
            "got {}",
            out[0].start_secs
        );
    }

    #[test]
    fn samples_arriving_in_ragged_buffers_are_handled() {
        // A capture callback delivers whatever the device gives it, which is
        // never a whole number of analysis frames.
        let mut c = Chunker::new(SR);
        let audio = speech(2.0);
        let mut out = Vec::new();
        for piece in audio.chunks(577) {
            out.extend(c.push(piece));
        }
        out.extend(c.push(&silence(1.0)));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn two_utterances_separated_by_a_real_pause_are_two_chunks() {
        let mut c = Chunker::new(SR);
        let mut out = c.push(&speech(2.0));
        out.extend(c.push(&silence(1.0)));
        out.extend(c.push(&speech(2.0)));
        out.extend(c.push(&silence(1.0)));
        assert_eq!(out.len(), 2);
        assert!(out[1].start_secs > out[0].end_secs - 0.5);
    }
}
