import type { SpeakerRole } from './types'

/**
 * Number of distinct speaker colour slots. Speaker indices wrap around this, so
 * the waveform and the transcript always agree on which colour a speaker gets.
 *
 * The actual colour values live in `speakers.css` as `--ca-speaker-N` custom
 * properties; nothing here hardcodes a colour.
 */
export const SPEAKER_SLOT_COUNT = 6

/** Maps an arbitrary speaker index onto a colour slot in `0..SPEAKER_SLOT_COUNT-1`. */
export function speakerSlot(speakerIndex: number): number {
  if (!Number.isFinite(speakerIndex)) {
    return 0
  }
  const i = Math.trunc(speakerIndex) % SPEAKER_SLOT_COUNT
  return i < 0 ? i + SPEAKER_SLOT_COUNT : i
}

/** The CSS custom property name backing a speaker's colour. */
export function speakerCssVarName(speakerIndex: number): string {
  return '--ca-speaker-' + String(speakerSlot(speakerIndex))
}

/** A `var(...)` reference usable directly in inline styles. */
export function speakerColorVar(speakerIndex: number): string {
  return 'var(' + speakerCssVarName(speakerIndex) + ')'
}

/** Human label for a speaker, preferring the diarisation role when known. */
export function speakerLabel(speakerIndex: number, role: SpeakerRole): string {
  if (role === 'agent') {
    return 'Agent'
  }
  if (role === 'customer') {
    return 'Customer'
  }
  return 'Speaker ' + String(Math.max(0, Math.trunc(speakerIndex)) + 1)
}
