import { Fragment, memo, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { CSSProperties } from 'react'
import type { Segment, Word } from '../waveform/types'
import { speakerColorVar, speakerLabel } from '../waveform/speakers'
import { formatTimecode } from '../waveform/format'
import '../waveform/speakers.css'
import './TranscriptView.css'

/** How long a programmatic smooth scroll may keep firing `scroll` events. */
const PROGRAMMATIC_SCROLL_MS = 900
/** Don't bother re-scrolling for sub-pixel corrections. */
const SCROLL_EPSILON_PX = 8

export interface TranscriptViewProps {
  /** Diarised transcript segments; sorted internally, order does not matter. */
  segments: readonly Segment[]
  /** Playhead position in seconds, used for segment + word highlighting. */
  currentTime: number
  /** Called when a segment or a single word is clicked. */
  onSeek: (seconds: number) => void
  /** Prefer `redactedText` over `text` where available. Defaults to true. */
  showRedacted?: boolean
  /** Follow the playhead by scrolling. Defaults to true. */
  autoScroll?: boolean
  /** Auto-scroll stays suppressed this long after a manual scroll. Defaults to 3000. */
  manualScrollPauseMs?: number
  /** Words below this confidence get a subtle underline. Defaults to 0.5. */
  lowConfidenceThreshold?: number
  /** Shown when there are no segments. */
  emptyMessage?: string
  className?: string
}

/** Index of the segment covering `time`, or -1 when the playhead is in a gap. */
function findActiveIndex(segments: readonly Segment[], time: number): number {
  let lo = 0
  let hi = segments.length - 1
  let candidate = -1
  while (lo <= hi) {
    const mid = (lo + hi) >> 1
    if (segments[mid].start <= time) {
      candidate = mid
      lo = mid + 1
    } else {
      hi = mid - 1
    }
  }
  if (candidate < 0) {
    return -1
  }
  // Segments may overlap slightly, so check a small window backwards.
  const floor = Math.max(0, candidate - 8)
  for (let i = candidate; i >= floor; i -= 1) {
    if (segments[i].start <= time && time < segments[i].end) {
      return i
    }
  }
  return -1
}

/** Index of the word covering `time` inside a segment, or -1. */
function findActiveWord(words: readonly Word[], time: number): number {
  for (let i = 0; i < words.length; i += 1) {
    const word = words[i]
    if (time >= word.start && time < word.end) {
      return i
    }
  }
  return -1
}

interface SegmentBlockProps {
  segment: Segment
  isActive: boolean
  /** -1 unless this block is the active one. */
  activeWordIndex: number
  showRedacted: boolean
  lowConfidenceThreshold: number
  /** Stable across renders so `memo` actually holds. */
  onSeek: (seconds: number) => void
}

const SegmentBlock = memo(function SegmentBlock(props: SegmentBlockProps) {
  const { segment, isActive, activeWordIndex, showRedacted, lowConfidenceThreshold, onSeek } = props

  const isRedacted = showRedacted && segment.redactedText !== null
  // Word timings describe `text`, not the redacted rendering, so word-level
  // highlighting is only safe when the original text is on screen.
  const useWords = !isRedacted && segment.words.length > 0
  const bodyText = isRedacted && segment.redactedText !== null ? segment.redactedText : segment.text

  const style = { '--ca-segment-color': speakerColorVar(segment.speakerIndex) } as CSSProperties
  const className = isActive ? 'tv-segment tv-segment-active' : 'tv-segment'

  return (
    <div
      className={className}
      style={style}
      data-segment-id={segment.id}
      onClick={() => {
        onSeek(segment.start)
      }}
    >
      <div className="tv-meta">
        <span className="tv-speaker">
          <span className="tv-dot" aria-hidden="true" />
          {speakerLabel(segment.speakerIndex, segment.role)}
        </span>
        <button
          type="button"
          className="tv-time"
          onClick={(event) => {
            event.stopPropagation()
            onSeek(segment.start)
          }}
          aria-label={'Play from ' + formatTimecode(segment.start)}
        >
          {formatTimecode(segment.start)}
        </button>
        {isRedacted ? (
          <span className="tv-badge" title="Sensitive content was redacted">
            Redacted
          </span>
        ) : null}
        {segment.language !== null ? <span className="tv-lang">{segment.language}</span> : null}
      </div>

      <p className="tv-text">
        {useWords
          ? segment.words.map((word, index) => {
              let wordClass = 'tv-word'
              if (index === activeWordIndex) {
                wordClass += ' tv-word-active'
              }
              if (word.confidence !== null && word.confidence < lowConfidenceThreshold) {
                wordClass += ' tv-word-low'
              }
              return (
                <Fragment key={String(index) + ':' + String(word.start)}>
                  <span
                    className={wordClass}
                    title={
                      word.confidence !== null
                        ? 'Confidence ' + String(Math.round(word.confidence * 100)) + '%'
                        : undefined
                    }
                    onClick={(event) => {
                      event.stopPropagation()
                      onSeek(word.start)
                    }}
                  >
                    {word.text}
                  </span>{' '}
                </Fragment>
              )
            })
          : bodyText}
      </p>
    </div>
  )
})

export function TranscriptView(props: TranscriptViewProps) {
  const {
    segments,
    currentTime,
    onSeek,
    showRedacted = true,
    autoScroll = true,
    manualScrollPauseMs = 3000,
    lowConfidenceThreshold = 0.5,
    emptyMessage = 'No transcript yet',
    className,
  } = props

  const scrollRef = useRef<HTMLDivElement | null>(null)
  const listRef = useRef<HTMLDivElement | null>(null)
  const lastManualScrollRef = useRef(0)
  const programmaticUntilRef = useRef(0)
  const onSeekRef = useRef(onSeek)
  const [retryTick, setRetryTick] = useState(0)

  useEffect(() => {
    onSeekRef.current = onSeek
  }, [onSeek])

  // Stable identity keeps every memoised SegmentBlock from re-rendering when
  // the parent passes a fresh inline `onSeek`.
  const seek = useCallback((seconds: number) => {
    onSeekRef.current(seconds)
  }, [])

  const ordered = useMemo(() => {
    const copy = segments.slice()
    copy.sort((a, b) => a.start - b.start)
    return copy
  }, [segments])

  const activeIndex = useMemo(() => findActiveIndex(ordered, currentTime), [ordered, currentTime])

  const activeWordIndex = useMemo(() => {
    if (activeIndex < 0) {
      return -1
    }
    const active = ordered[activeIndex]
    if (showRedacted && active.redactedText !== null) {
      return -1
    }
    return findActiveWord(active.words, currentTime)
  }, [ordered, activeIndex, currentTime, showRedacted])

  // Track manual scrolling so auto-scroll never fights the user.
  useEffect(() => {
    const container = scrollRef.current
    if (container === null) {
      return undefined
    }

    const markManual = (): void => {
      lastManualScrollRef.current = performance.now()
      // A deliberate gesture wins even mid-animation.
      programmaticUntilRef.current = 0
    }
    const onScroll = (): void => {
      if (performance.now() < programmaticUntilRef.current) {
        return
      }
      lastManualScrollRef.current = performance.now()
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (
        event.key === 'ArrowUp' ||
        event.key === 'ArrowDown' ||
        event.key === 'PageUp' ||
        event.key === 'PageDown' ||
        event.key === 'Home' ||
        event.key === 'End' ||
        event.key === ' '
      ) {
        markManual()
      }
    }

    container.addEventListener('wheel', markManual, { passive: true })
    container.addEventListener('touchmove', markManual, { passive: true })
    container.addEventListener('keydown', onKeyDown)
    container.addEventListener('scroll', onScroll, { passive: true })
    return () => {
      container.removeEventListener('wheel', markManual)
      container.removeEventListener('touchmove', markManual)
      container.removeEventListener('keydown', onKeyDown)
      container.removeEventListener('scroll', onScroll)
    }
  }, [])

  // Bring the active segment into view, unless the user scrolled recently.
  useEffect(() => {
    if (!autoScroll || activeIndex < 0) {
      return undefined
    }
    const container = scrollRef.current
    const list = listRef.current
    if (container === null || list === null) {
      return undefined
    }

    const sinceManual = performance.now() - lastManualScrollRef.current
    if (sinceManual < manualScrollPauseMs) {
      // Try again as soon as the suppression window closes.
      const timer = window.setTimeout(() => {
        setRetryTick((value) => value + 1)
      }, manualScrollPauseMs - sinceManual + 20)
      return () => {
        window.clearTimeout(timer)
      }
    }

    const element = list.children.item(activeIndex)
    if (!(element instanceof HTMLElement)) {
      return undefined
    }
    // Measured from rects rather than `offsetTop`: that property is relative to
    // the nearest *positioned* ancestor, which is not necessarily this
    // container, so it silently returns the element's offset within the whole
    // page and scrolls the transcript to a position unrelated to the playhead.
    const offsetInContainer =
      element.getBoundingClientRect().top -
      container.getBoundingClientRect().top +
      container.scrollTop
    const centred = offsetInContainer - container.clientHeight / 2 + element.offsetHeight / 2
    const maxTop = Math.max(0, container.scrollHeight - container.clientHeight)
    const top = Math.max(0, Math.min(centred, maxTop))
    if (Math.abs(container.scrollTop - top) < SCROLL_EPSILON_PX) {
      return undefined
    }
    programmaticUntilRef.current = performance.now() + PROGRAMMATIC_SCROLL_MS
    container.scrollTo({ top, behavior: 'smooth' })
    return undefined
  }, [autoScroll, activeIndex, manualScrollPauseMs, retryTick])

  const rootClassName = className !== undefined ? 'tv-root ' + className : 'tv-root'

  return (
    <div className={rootClassName}>
      <div className="tv-scroll" ref={scrollRef} tabIndex={0} aria-label="Transcript">
        {ordered.length === 0 ? (
          <p className="tv-empty">{emptyMessage}</p>
        ) : (
          <div className="tv-list" ref={listRef}>
            {ordered.map((segment, index) => (
              <SegmentBlock
                key={segment.id}
                segment={segment}
                isActive={index === activeIndex}
                activeWordIndex={index === activeIndex ? activeWordIndex : -1}
                showRedacted={showRedacted}
                lowConfidenceThreshold={lowConfidenceThreshold}
                onSeek={seek}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  )
}

export default TranscriptView
