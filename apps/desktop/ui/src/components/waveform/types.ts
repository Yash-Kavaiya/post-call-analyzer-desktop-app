/**
 * Data contract mirrored from the Rust backend.
 *
 * These are declared locally (rather than imported from `src/api.ts`) so this
 * folder can be type-checked and reused independently. The shapes are
 * structurally identical to the backend payloads, so values coming from the
 * Tauri command layer assign to them without conversion.
 */

/** One resolution of precomputed waveform peaks. */
export interface PeakLevel {
  /** Seconds of audio represented by a single bucket. */
  bucketSecs: number
  /** Interleaved `[min, max, min, max, ...]` in -1..1 — one pair per bucket. */
  minMax: number[]
  /** Normalised 0..1 energy — one value per bucket, used for the heat overlay. */
  energy: number[]
}

/** All available peak resolutions, ordered coarsest `bucketSecs` first. */
export interface WaveformPeaks {
  levels: PeakLevel[]
  durationSecs: number
}

/** A single recognised word with its timing. */
export interface Word {
  text: string
  /** Seconds from the start of the recording. */
  start: number
  /** Seconds from the start of the recording. */
  end: number
  /** 0..1 ASR confidence, or `null` when the engine did not report one. */
  confidence: number | null
}

/** Who a diarised segment belongs to. */
export type SpeakerRole = 'agent' | 'customer' | 'unknown'

/** One diarised, transcribed stretch of speech. */
export interface Segment {
  id: string
  speakerIndex: number
  role: SpeakerRole
  start: number
  end: number
  text: string
  /** Redacted rendering of `text`, present only when redaction found something. */
  redactedText: string | null
  words: Word[]
  confidence: number | null
  language: string | null
}

/** A stretch of silence/hold detected between speech. */
export interface PauseRegion {
  start: number
  end: number
}
