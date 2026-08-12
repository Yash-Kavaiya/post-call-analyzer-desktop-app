import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { CSSProperties, KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent } from 'react'
import type { PauseRegion, PeakLevel, Segment, SpeakerRole, WaveformPeaks } from './types'
import { SPEAKER_SLOT_COUNT, speakerColorVar, speakerLabel, speakerSlot } from './speakers'
import { clamp, formatTimecode } from './format'
import './speakers.css'
import './WaveformTimeline.css'

/** Narrowest window the user can zoom into, in seconds. */
const MIN_VISIBLE_SECS = 0.5
/** Energy is quantised into this many alpha steps so the draw loop can batch. */
const ALPHA_STEPS = 12
const MIN_ALPHA = 0.3
const MAX_ALPHA = 1
const PAUSE_ALPHA = 0.3
const CENTER_ALPHA = 0.4
const CURSOR_ALPHA = 0.45
/** Playhead auto-follow stays out of the way this long after the user pans/zooms. */
const FOLLOW_SUPPRESS_MS = 2000

export interface WaveformTimelineProps {
  /** Precomputed multi-resolution peaks from the backend. */
  peaks: WaveformPeaks | null
  /** Diarised segments; used to colour bars by speaker. */
  segments?: readonly Segment[]
  /** Silence/hold regions rendered as muted bands behind the waveform. */
  pauses?: readonly PauseRegion[]
  /** Playhead position in seconds. */
  currentTime: number
  /** Overrides `peaks.durationSecs` when the media length is known more precisely. */
  duration?: number
  /** Canvas height in CSS pixels. Defaults to 128. */
  height?: number
  /** Emitted on click-to-seek, drag-to-scrub and keyboard seeking. */
  onSeek: (seconds: number) => void
  /** Keep the playhead in view while zoomed in. Defaults to true. */
  followPlayhead?: boolean
  /** Render the zoom/fit toolbar. Defaults to true. */
  showControls?: boolean
  /** Render the speaker colour legend. Defaults to true. */
  showLegend?: boolean
  className?: string
}

interface View {
  zoom: number
  offset: number
}

interface Palette {
  speakers: string[]
  unassigned: string
  pause: string
  center: string
  playhead: string
  cursor: string
  background: string
}

interface DrawState {
  peaks: WaveformPeaks | null
  segments: readonly Segment[]
  pauses: readonly PauseRegion[]
  currentTime: number
  viewStart: number
  viewSecs: number
  /** Backing-store size in device pixels. */
  width: number
  height: number
  dpr: number
  /** Hover position in device pixels, or -1 when the pointer is away. */
  hoverX: number
  palette: Palette
  paletteVersion: number
  sceneDirty: boolean
}

const EMPTY_SEGMENTS: readonly Segment[] = []
const EMPTY_PAUSES: readonly PauseRegion[] = []

/* ------------------------------------------------------------------ palette */

function createPalette(): Palette {
  const speakers: string[] = []
  for (let i = 0; i < SPEAKER_SLOT_COUNT; i += 1) {
    // Named CSS colour, only ever used if the design tokens are missing.
    speakers.push('gray')
  }
  return {
    speakers,
    unassigned: 'gray',
    pause: 'gray',
    center: 'gray',
    playhead: 'gray',
    cursor: 'gray',
    background: 'transparent',
  }
}

function readVar(style: CSSStyleDeclaration, name: string, fallback: string): string {
  const value = style.getPropertyValue(name).trim()
  return value.length > 0 ? value : fallback
}

/**
 * Resolves the CSS custom properties the canvas needs into concrete colour
 * strings. Canvas cannot consume `var(...)`, so this is the one bridge between
 * the token layer and the renderer — it re-runs on theme changes only.
 */
function readPalette(element: Element, into: Palette): void {
  const style = getComputedStyle(element)
  for (let i = 0; i < SPEAKER_SLOT_COUNT; i += 1) {
    into.speakers[i] = readVar(style, '--ca-speaker-' + String(i), 'gray')
  }
  into.unassigned = readVar(style, '--ca-waveform-unassigned', 'gray')
  into.pause = readVar(style, '--ca-waveform-pause', 'gray')
  into.center = readVar(style, '--ca-waveform-center-line', 'gray')
  into.playhead = readVar(style, '--ca-waveform-playhead', 'gray')
  into.cursor = readVar(style, '--ca-waveform-cursor', 'gray')
  into.background = readVar(style, '--ca-waveform-background', 'transparent')
}

/* --------------------------------------------------------------- view maths */

function normaliseView(zoom: number, offset: number, duration: number): View {
  if (duration <= 0) {
    return { zoom: 1, offset: 0 }
  }
  const maxZoom = Math.max(1, duration / MIN_VISIBLE_SECS)
  const z = clamp(zoom, 1, maxZoom)
  const visible = duration / z
  return { zoom: z, offset: clamp(offset, 0, Math.max(0, duration - visible)) }
}

/** Zooms by `factor`, keeping the time under `anchor` (0..1 of width) fixed. */
function zoomView(view: View, factor: number, anchor: number, duration: number): View {
  if (duration <= 0) {
    return view
  }
  const anchorTime = view.offset + anchor * (duration / view.zoom)
  const zoomed = normaliseView(view.zoom * factor, view.offset, duration)
  return normaliseView(zoomed.zoom, anchorTime - anchor * (duration / zoomed.zoom), duration)
}

/** Pans by a fraction of the visible window (positive scrolls forward). */
function panView(view: View, fraction: number, duration: number): View {
  if (duration <= 0) {
    return view
  }
  return normaliseView(view.zoom, view.offset + fraction * (duration / view.zoom), duration)
}

/**
 * Picks the coarsest level that still supplies at least one bucket per output
 * column — the least data that can fill the pixels without upsampling.
 */
function pickLevel(levels: PeakLevel[], viewSecs: number, columns: number): PeakLevel | null {
  let best: PeakLevel | null = null
  let finest: PeakLevel | null = null
  for (let i = 0; i < levels.length; i += 1) {
    const level = levels[i]
    if (!(level.bucketSecs > 0) || level.minMax.length < 2) {
      continue
    }
    if (finest === null || level.bucketSecs < finest.bucketSecs) {
      finest = level
    }
    if (viewSecs / level.bucketSecs >= columns) {
      if (best === null || level.bucketSecs > best.bucketSecs) {
        best = level
      }
    }
  }
  return best !== null ? best : finest
}

/* ------------------------------------------------------------------ drawing */

/**
 * Renders everything static for the current viewport into the offscreen scene
 * canvas. Only the playhead and hover cursor are drawn per frame, so playback
 * costs one `drawImage` plus two `fillRect`s.
 *
 * The loop allocates nothing: colours are pre-resolved strings, alpha is
 * quantised, and consecutive bars sharing a colour/alpha accumulate into a
 * single path before being filled.
 */
function drawScene(scene: HTMLCanvasElement, state: DrawState): void {
  const ctx = scene.getContext('2d')
  if (ctx === null) {
    return
  }
  const width = state.width
  const height = state.height
  const palette = state.palette

  ctx.setTransform(1, 0, 0, 1, 0, 0)
  ctx.globalAlpha = 1
  ctx.clearRect(0, 0, width, height)
  if (palette.background !== 'transparent') {
    ctx.fillStyle = palette.background
    ctx.fillRect(0, 0, width, height)
  }

  const viewStart = state.viewStart
  const viewSecs = state.viewSecs
  if (!(viewSecs > 0) || width <= 0 || height <= 0) {
    return
  }
  const secsPerCol = viewSecs / width
  const mid = height / 2
  const hairline = state.dpr < 1 ? 1 : Math.round(state.dpr)

  // Pause bands sit behind everything else.
  const pauses = state.pauses
  if (pauses.length > 0) {
    ctx.fillStyle = palette.pause
    ctx.globalAlpha = PAUSE_ALPHA
    for (let i = 0; i < pauses.length; i += 1) {
      const region = pauses[i]
      const x0 = (region.start - viewStart) / secsPerCol
      const x1 = (region.end - viewStart) / secsPerCol
      if (x1 < 0 || x0 > width) {
        continue
      }
      const left = x0 < 0 ? 0 : x0
      const right = x1 > width ? width : x1
      ctx.fillRect(left, 0, Math.max(right - left, 1), height)
    }
  }

  // Centre line, drawn under the bars.
  ctx.fillStyle = palette.center
  ctx.globalAlpha = CENTER_ALPHA
  ctx.fillRect(0, Math.round(mid - hairline / 2), width, hairline)

  const peaks = state.peaks
  const level = peaks !== null ? pickLevel(peaks.levels, viewSecs, width) : null
  const amplitude = mid - hairline * 2
  if (level === null || amplitude <= 0) {
    ctx.globalAlpha = 1
    return
  }

  const minMax = level.minMax
  const energy = level.energy
  const bucketSecs = level.bucketSecs
  const bucketCount = Math.min(minMax.length >> 1, energy.length)
  const segments = state.segments

  let segIndex = 0
  let currentColor = ''
  let currentStep = -1
  let pathOpen = false

  for (let x = 0; x < width; x += 1) {
    const tStart = viewStart + x * secsPerCol
    const tEnd = tStart + secsPerCol

    let b0 = Math.floor(tStart / bucketSecs)
    let b1 = Math.ceil(tEnd / bucketSecs)
    if (b1 <= b0) {
      b1 = b0 + 1
    }
    if (b0 < 0) {
      b0 = 0
    }
    if (b1 > bucketCount) {
      b1 = bucketCount
    }
    if (b0 >= b1) {
      continue
    }

    let lo = 1
    let hi = -1
    let heat = 0
    for (let b = b0; b < b1; b += 1) {
      const mn = minMax[b * 2]
      const mx = minMax[b * 2 + 1]
      if (mn < lo) {
        lo = mn
      }
      if (mx > hi) {
        hi = mx
      }
      const e = energy[b]
      if (e > heat) {
        heat = e
      }
    }
    if (hi < lo) {
      lo = 0
      hi = 0
    }

    // Which speaker owns this column? Columns advance monotonically in time,
    // so the segment cursor only ever moves forward.
    const tMid = tStart + secsPerCol * 0.5
    while (segIndex < segments.length && segments[segIndex].end <= tMid) {
      segIndex += 1
    }
    let color = palette.unassigned
    if (segIndex < segments.length && segments[segIndex].start <= tMid) {
      color = palette.speakers[speakerSlot(segments[segIndex].speakerIndex)]
    }

    const step = Math.round(clamp(heat, 0, 1) * (ALPHA_STEPS - 1))
    if (color !== currentColor || step !== currentStep) {
      if (pathOpen) {
        ctx.fill()
      }
      ctx.beginPath()
      ctx.fillStyle = color
      ctx.globalAlpha = MIN_ALPHA + (MAX_ALPHA - MIN_ALPHA) * (step / (ALPHA_STEPS - 1))
      currentColor = color
      currentStep = step
      pathOpen = true
    }

    let top = mid - hi * amplitude
    let barHeight = (mid - lo * amplitude) - top
    if (barHeight < hairline) {
      top = mid - hairline / 2
      barHeight = hairline
    }
    ctx.rect(x, top, 1, barHeight)
  }

  if (pathOpen) {
    ctx.fill()
  }
  ctx.globalAlpha = 1
}

/* ---------------------------------------------------------------- component */

export function WaveformTimeline(props: WaveformTimelineProps) {
  const {
    peaks,
    segments = EMPTY_SEGMENTS,
    pauses = EMPTY_PAUSES,
    currentTime,
    duration: durationProp,
    height = 128,
    onSeek,
    followPlayhead = true,
    showControls = true,
    showLegend = true,
    className,
  } = props

  const wrapRef = useRef<HTMLDivElement | null>(null)
  const canvasRef = useRef<HTMLCanvasElement | null>(null)
  const sceneRef = useRef<HTMLCanvasElement | null>(null)
  const frameRef = useRef(0)
  const durationRef = useRef(0)
  const lastInteractionRef = useRef(0)
  const dragRef = useRef<{ mode: 'none' | 'scrub' | 'pan'; startClientX: number; startOffset: number }>({
    mode: 'none',
    startClientX: 0,
    startOffset: 0,
  })

  const stateRef = useRef<DrawState | null>(null)
  if (stateRef.current === null) {
    stateRef.current = {
      peaks: null,
      segments: EMPTY_SEGMENTS,
      pauses: EMPTY_PAUSES,
      currentTime: 0,
      viewStart: 0,
      viewSecs: 0,
      width: 0,
      height: 0,
      dpr: 1,
      hoverX: -1,
      palette: createPalette(),
      paletteVersion: -1,
      sceneDirty: true,
    }
  }

  const [size, setSize] = useState({ width: 0, height: 0 })
  const [dpr, setDpr] = useState(() => (typeof window === 'undefined' ? 1 : window.devicePixelRatio || 1))
  const [paletteVersion, setPaletteVersion] = useState(0)
  const [view, setView] = useState<View>({ zoom: 1, offset: 0 })

  const rawDuration = durationProp !== undefined ? durationProp : peaks !== null ? peaks.durationSecs : 0
  const duration = Number.isFinite(rawDuration) && rawDuration > 0 ? rawDuration : 0
  const clampedView = normaliseView(view.zoom, view.offset, duration)
  const zoom = clampedView.zoom
  const offset = clampedView.offset
  const viewSecs = duration > 0 ? duration / zoom : 0

  const sortedSegments = useMemo(() => {
    const copy = segments.slice()
    copy.sort((a, b) => a.start - b.start)
    return copy
  }, [segments])

  const sortedPauses = useMemo(() => {
    const copy = pauses.slice()
    copy.sort((a, b) => a.start - b.start)
    return copy
  }, [pauses])

  const legend = useMemo(() => {
    const seen = new Map<number, SpeakerRole>()
    for (let i = 0; i < sortedSegments.length; i += 1) {
      const seg = sortedSegments[i]
      if (!seen.has(seg.speakerIndex)) {
        seen.set(seg.speakerIndex, seg.role)
      }
    }
    const out: { index: number; role: SpeakerRole }[] = []
    seen.forEach((role, index) => {
      out.push({ index, role })
    })
    out.sort((a, b) => a.index - b.index)
    return out
  }, [sortedSegments])

  const markInteraction = useCallback(() => {
    lastInteractionRef.current = performance.now()
  }, [])

  /** Composites the cached scene, then the playhead and hover cursor. */
  const render = useCallback(() => {
    const canvas = canvasRef.current
    const state = stateRef.current
    if (canvas === null || state === null) {
      return
    }
    const width = state.width
    const height = state.height
    if (width <= 0 || height <= 0) {
      return
    }

    let scene = sceneRef.current
    if (scene === null) {
      scene = document.createElement('canvas')
      sceneRef.current = scene
    }
    if (scene.width !== width || scene.height !== height) {
      scene.width = width
      scene.height = height
      state.sceneDirty = true
    }
    if (canvas.width !== width || canvas.height !== height) {
      canvas.width = width
      canvas.height = height
      state.sceneDirty = true
    }
    if (state.sceneDirty) {
      drawScene(scene, state)
      state.sceneDirty = false
    }

    const ctx = canvas.getContext('2d')
    if (ctx === null) {
      return
    }
    ctx.setTransform(1, 0, 0, 1, 0, 0)
    ctx.globalAlpha = 1
    ctx.clearRect(0, 0, width, height)
    ctx.drawImage(scene, 0, 0)

    const hairline = state.dpr < 1 ? 1 : Math.round(state.dpr)

    if (state.hoverX >= 0 && state.hoverX <= width) {
      ctx.globalAlpha = CURSOR_ALPHA
      ctx.fillStyle = state.palette.cursor
      ctx.fillRect(Math.round(state.hoverX - hairline / 2), 0, hairline, height)
    }

    if (state.viewSecs > 0) {
      const x = ((state.currentTime - state.viewStart) / state.viewSecs) * width
      if (x >= -2 && x <= width + 2) {
        const thickness = Math.max(2, hairline * 2)
        ctx.globalAlpha = 1
        ctx.fillStyle = state.palette.playhead
        ctx.fillRect(Math.round(x - thickness / 2), 0, thickness, height)
      }
    }
    ctx.globalAlpha = 1
  }, [])

  /** Coalesces every redraw request into at most one frame. */
  const schedule = useCallback(() => {
    if (frameRef.current !== 0) {
      return
    }
    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = 0
      render()
    })
  }, [render])

  // Mirror props/derived state into the draw state and decide what is dirty.
  // Runs after every commit; there is no dependency array on purpose.
  useEffect(() => {
    const state = stateRef.current
    if (state === null) {
      return
    }
    const deviceWidth = Math.max(0, Math.round(size.width * dpr))
    const deviceHeight = Math.max(0, Math.round(size.height * dpr))
    const sceneDirty =
      state.peaks !== peaks ||
      state.segments !== sortedSegments ||
      state.pauses !== sortedPauses ||
      state.viewStart !== offset ||
      state.viewSecs !== viewSecs ||
      state.width !== deviceWidth ||
      state.height !== deviceHeight ||
      state.dpr !== dpr ||
      state.paletteVersion !== paletteVersion

    state.peaks = peaks
    state.segments = sortedSegments
    state.pauses = sortedPauses
    state.viewStart = offset
    state.viewSecs = viewSecs
    state.width = deviceWidth
    state.height = deviceHeight
    state.dpr = dpr
    state.paletteVersion = paletteVersion
    state.currentTime = currentTime
    if (sceneDirty) {
      state.sceneDirty = true
    }
    durationRef.current = duration
    schedule()
  })

  useEffect(
    () => () => {
      if (frameRef.current !== 0) {
        cancelAnimationFrame(frameRef.current)
        frameRef.current = 0
      }
    },
    [],
  )

  // Element size.
  useEffect(() => {
    const element = wrapRef.current
    if (element === null) {
      return undefined
    }
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0]
      if (entry === undefined) {
        return
      }
      const rect = entry.contentRect
      setSize((prev) =>
        prev.width === rect.width && prev.height === rect.height
          ? prev
          : { width: rect.width, height: rect.height },
      )
    })
    observer.observe(element)
    return () => {
      observer.disconnect()
    }
  }, [])

  // devicePixelRatio changes (moving the window between displays, OS zoom).
  useEffect(() => {
    let query: MediaQueryList | null = null
    let disposed = false

    function onChange(): void {
      if (!disposed) {
        attach()
      }
    }
    function attach(): void {
      const current = window.devicePixelRatio || 1
      setDpr(current)
      if (query !== null) {
        query.removeEventListener('change', onChange)
      }
      query = window.matchMedia('(resolution: ' + String(current) + 'dppx)')
      query.addEventListener('change', onChange)
    }

    attach()
    return () => {
      disposed = true
      if (query !== null) {
        query.removeEventListener('change', onChange)
      }
    }
  }, [])

  // Resolve CSS tokens to concrete colours, and re-resolve when the theme moves.
  useEffect(() => {
    const element = wrapRef.current
    const state = stateRef.current
    if (element === null || state === null) {
      return undefined
    }
    const refresh = (): void => {
      readPalette(element, state.palette)
      setPaletteVersion((v) => v + 1)
    }
    refresh()

    const observer = new MutationObserver(refresh)
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['class', 'style', 'data-theme'],
    })
    const query = window.matchMedia('(prefers-color-scheme: dark)')
    query.addEventListener('change', refresh)
    return () => {
      observer.disconnect()
      query.removeEventListener('change', refresh)
    }
  }, [])

  // Wheel must be non-passive so ctrl+wheel does not zoom the whole window.
  useEffect(() => {
    const element = wrapRef.current
    if (element === null) {
      return undefined
    }
    const onWheel = (event: WheelEvent): void => {
      const total = durationRef.current
      if (total <= 0) {
        return
      }
      event.preventDefault()
      markInteraction()
      const rect = element.getBoundingClientRect()
      const width = rect.width > 0 ? rect.width : 1
      if (event.ctrlKey || event.metaKey) {
        const anchor = clamp((event.clientX - rect.left) / width, 0, 1)
        const factor = Math.exp(-event.deltaY * 0.002)
        setView((v) => zoomView(v, factor, anchor, total))
      } else {
        const delta = event.deltaX !== 0 ? event.deltaX : event.deltaY
        setView((v) => panView(v, delta / width, total))
      }
    }
    element.addEventListener('wheel', onWheel, { passive: false })
    return () => {
      element.removeEventListener('wheel', onWheel)
    }
  }, [markInteraction])

  // Keep the playhead visible while zoomed in, unless the user just moved the view.
  useEffect(() => {
    if (!followPlayhead || zoom <= 1 || viewSecs <= 0) {
      return
    }
    if (performance.now() - lastInteractionRef.current < FOLLOW_SUPPRESS_MS) {
      return
    }
    const margin = viewSecs * 0.08
    if (currentTime < offset + margin || currentTime > offset + viewSecs - margin) {
      setView((v) => normaliseView(v.zoom, currentTime - (duration / v.zoom) * 0.5, duration))
    }
  }, [followPlayhead, currentTime, zoom, offset, viewSecs, duration])

  const timeAtClientX = useCallback(
    (clientX: number): number => {
      const element = wrapRef.current
      if (element === null || duration <= 0) {
        return 0
      }
      const rect = element.getBoundingClientRect()
      const ratio = rect.width > 0 ? (clientX - rect.left) / rect.width : 0
      return clamp(offset + ratio * viewSecs, 0, duration)
    },
    [duration, offset, viewSecs],
  )

  const handlePointerDown = (event: ReactPointerEvent<HTMLDivElement>): void => {
    if (duration <= 0 || (event.button !== 0 && event.button !== 1)) {
      return
    }
    event.preventDefault()
    markInteraction()
    wrapRef.current?.setPointerCapture(event.pointerId)
    const drag = dragRef.current
    if (event.button === 1 || event.shiftKey) {
      drag.mode = 'pan'
      drag.startClientX = event.clientX
      drag.startOffset = offset
    } else {
      drag.mode = 'scrub'
      onSeek(timeAtClientX(event.clientX))
    }
  }

  const handlePointerMove = (event: ReactPointerEvent<HTMLDivElement>): void => {
    const element = wrapRef.current
    const state = stateRef.current
    if (element === null || state === null) {
      return
    }
    const rect = element.getBoundingClientRect()
    state.hoverX = (event.clientX - rect.left) * dpr

    const drag = dragRef.current
    if (drag.mode === 'pan') {
      markInteraction()
      const width = rect.width > 0 ? rect.width : 1
      const shift = ((event.clientX - drag.startClientX) / width) * viewSecs
      setView((v) => normaliseView(v.zoom, drag.startOffset - shift, duration))
    } else if (drag.mode === 'scrub') {
      markInteraction()
      onSeek(timeAtClientX(event.clientX))
    } else {
      schedule()
    }
  }

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>): void => {
    dragRef.current.mode = 'none'
    if (wrapRef.current?.hasPointerCapture(event.pointerId) === true) {
      wrapRef.current.releasePointerCapture(event.pointerId)
    }
  }

  const handlePointerLeave = (): void => {
    const state = stateRef.current
    if (state !== null) {
      state.hoverX = -1
      schedule()
    }
  }

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>): void => {
    if (duration <= 0) {
      return
    }
    const step = event.shiftKey ? 10 : 1
    if (event.key === 'ArrowLeft') {
      event.preventDefault()
      markInteraction()
      onSeek(clamp(currentTime - step, 0, duration))
    } else if (event.key === 'ArrowRight') {
      event.preventDefault()
      markInteraction()
      onSeek(clamp(currentTime + step, 0, duration))
    } else if (event.key === 'Home') {
      event.preventDefault()
      markInteraction()
      onSeek(0)
    } else if (event.key === 'End') {
      event.preventDefault()
      markInteraction()
      onSeek(duration)
    }
  }

  const applyZoom = (factor: number): void => {
    markInteraction()
    setView((v) => zoomView(v, factor, 0.5, durationRef.current))
  }

  const fit = (): void => {
    markInteraction()
    setView({ zoom: 1, offset: 0 })
  }

  const hasData = duration > 0 && peaks !== null && peaks.levels.length > 0
  const rootClassName = className !== undefined ? 'wf-root ' + className : 'wf-root'

  return (
    <div className={rootClassName}>
      <div
        ref={wrapRef}
        className="wf-canvas-wrap"
        style={{ height: String(height) + 'px' }}
        role="slider"
        tabIndex={0}
        aria-label="Call waveform"
        aria-valuemin={0}
        aria-valuemax={duration}
        aria-valuenow={currentTime}
        aria-valuetext={formatTimecode(currentTime, true) + ' of ' + formatTimecode(duration)}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onPointerLeave={handlePointerLeave}
        onKeyDown={handleKeyDown}
        onDoubleClick={fit}
      >
        <canvas ref={canvasRef} className="wf-canvas" />
        {!hasData ? <p className="wf-empty">No waveform available</p> : null}
      </div>

      {showControls ? (
        <div className="wf-toolbar">
          <div className="wf-range">
            {formatTimecode(offset)} – {formatTimecode(offset + viewSecs)}
          </div>
          <div className="wf-zoom">
            <button
              type="button"
              className="wf-btn"
              onClick={() => {
                applyZoom(0.5)
              }}
              disabled={!hasData || zoom <= 1}
              aria-label="Zoom out"
            >
              −
            </button>
            <span className="wf-zoom-value">{zoom < 10 ? zoom.toFixed(1) : String(Math.round(zoom))}×</span>
            <button
              type="button"
              className="wf-btn"
              onClick={() => {
                applyZoom(2)
              }}
              disabled={!hasData}
              aria-label="Zoom in"
            >
              +
            </button>
            <button type="button" className="wf-btn wf-btn-text" onClick={fit} disabled={!hasData}>
              Fit
            </button>
          </div>
        </div>
      ) : null}

      {showLegend && legend.length > 0 ? (
        <ul className="wf-legend">
          {legend.map((entry) => (
            <li key={entry.index} className="wf-legend-item">
              <span
                className="wf-legend-swatch"
                style={{ '--ca-swatch': speakerColorVar(entry.index) } as CSSProperties}
              />
              {speakerLabel(entry.index, entry.role)}
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  )
}

export default WaveformTimeline
