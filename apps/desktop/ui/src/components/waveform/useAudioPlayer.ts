import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { clamp } from './format'

export interface UseAudioPlayerOptions {
  /** Source URL for the audio (e.g. a `convertFileSrc` asset URL). `null` unloads. */
  src: string | null
  /** Playback rate applied on load. Defaults to `1`. */
  initialRate?: number
  /** Called once when playback reaches the end of the media. */
  onEnded?: () => void
}

export interface AudioPlayerControls {
  /** Current playhead position in seconds. Updated via rAF while playing. */
  currentTime: number
  /** Media duration in seconds; `0` until metadata has loaded. */
  duration: number
  playing: boolean
  rate: number
  /** True once metadata has loaded and seeking is meaningful. */
  ready: boolean
  /** Human-readable load/playback failure, or `null`. */
  error: string | null
  play: () => void
  pause: () => void
  toggle: () => void
  /** Seeks to an absolute position, clamped to the media bounds. */
  seek: (seconds: number) => void
  /** Seeks relative to the current position (negative rewinds). */
  skip: (deltaSeconds: number) => void
  setRate: (rate: number) => void
  /** The underlying element, for callers that need direct access. */
  element: HTMLAudioElement | null
}

function safeDuration(el: HTMLAudioElement): number {
  const d = el.duration
  return Number.isFinite(d) && d > 0 ? d : 0
}

/**
 * Wraps a detached `HTMLAudioElement`. Time updates come from
 * `requestAnimationFrame` while playing (the element's own `timeupdate` event
 * only fires ~4x/second, which visibly stutters a playhead) and the loop is
 * torn down whenever playback stops so an idle player costs nothing.
 */
export function useAudioPlayer(options: UseAudioPlayerOptions): AudioPlayerControls {
  const { src, initialRate = 1, onEnded } = options

  const audioRef = useRef<HTMLAudioElement | null>(null)
  if (audioRef.current === null && typeof window !== 'undefined') {
    const created = new Audio()
    created.preload = 'metadata'
    audioRef.current = created
  }

  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(0)
  const [playing, setPlaying] = useState(false)
  const [rate, setRateState] = useState(initialRate)
  const [ready, setReady] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const onEndedRef = useRef(onEnded)
  useEffect(() => {
    onEndedRef.current = onEnded
  }, [onEnded])

  // Media element events -> React state. Attached once for the element's life.
  useEffect(() => {
    const el = audioRef.current
    if (el === null) {
      return undefined
    }

    const handleMetadata = (): void => {
      setDuration(safeDuration(el))
      setReady(true)
      setError(null)
    }
    const handleDurationChange = (): void => {
      setDuration(safeDuration(el))
    }
    const handlePlay = (): void => {
      setPlaying(true)
    }
    const handlePause = (): void => {
      setPlaying(false)
      setCurrentTime(el.currentTime)
    }
    const handleEnded = (): void => {
      setPlaying(false)
      setCurrentTime(el.currentTime)
      const cb = onEndedRef.current
      if (cb !== undefined) {
        cb()
      }
    }
    const handleTimeUpdate = (): void => {
      setCurrentTime(el.currentTime)
    }
    const handleError = (): void => {
      setReady(false)
      setPlaying(false)
      setError('Unable to load audio')
    }

    el.addEventListener('loadedmetadata', handleMetadata)
    el.addEventListener('durationchange', handleDurationChange)
    el.addEventListener('play', handlePlay)
    el.addEventListener('pause', handlePause)
    el.addEventListener('ended', handleEnded)
    el.addEventListener('timeupdate', handleTimeUpdate)
    el.addEventListener('seeked', handleTimeUpdate)
    el.addEventListener('error', handleError)

    return () => {
      el.removeEventListener('loadedmetadata', handleMetadata)
      el.removeEventListener('durationchange', handleDurationChange)
      el.removeEventListener('play', handlePlay)
      el.removeEventListener('pause', handlePause)
      el.removeEventListener('ended', handleEnded)
      el.removeEventListener('timeupdate', handleTimeUpdate)
      el.removeEventListener('seeked', handleTimeUpdate)
      el.removeEventListener('error', handleError)
      el.pause()
    }
  }, [])

  // Load / unload the source.
  useEffect(() => {
    const el = audioRef.current
    if (el === null) {
      return
    }
    el.pause()
    setPlaying(false)
    setReady(false)
    setError(null)
    setCurrentTime(0)
    setDuration(0)

    if (src === null || src.length === 0) {
      el.removeAttribute('src')
      return
    }
    el.src = src
    el.load()
  }, [src])

  // Keep the element's rate in sync (re-applied after a source swap resets it).
  useEffect(() => {
    const el = audioRef.current
    if (el === null) {
      return
    }
    el.playbackRate = rate
    el.preservesPitch = true
  }, [rate, src])

  // Smooth playhead: one rAF loop, alive only while actually playing.
  useEffect(() => {
    if (!playing) {
      return undefined
    }
    let frame = 0
    const tick = (): void => {
      const el = audioRef.current
      if (el !== null) {
        setCurrentTime(el.currentTime)
      }
      frame = requestAnimationFrame(tick)
    }
    frame = requestAnimationFrame(tick)
    return () => {
      cancelAnimationFrame(frame)
    }
  }, [playing])

  const play = useCallback(() => {
    const el = audioRef.current
    if (el === null) {
      return
    }
    const started = el.play()
    if (started !== undefined) {
      started.then(
        () => {
          setError(null)
        },
        () => {
          setError('Playback was blocked')
          setPlaying(false)
        },
      )
    }
  }, [])

  const pause = useCallback(() => {
    audioRef.current?.pause()
  }, [])

  const toggle = useCallback(() => {
    const el = audioRef.current
    if (el === null) {
      return
    }
    if (el.paused) {
      play()
    } else {
      el.pause()
    }
  }, [play])

  const seek = useCallback((seconds: number) => {
    const el = audioRef.current
    if (el === null) {
      return
    }
    const max = safeDuration(el)
    const target = clamp(seconds, 0, max > 0 ? max : Math.max(0, seconds))
    el.currentTime = target
    setCurrentTime(target)
  }, [])

  const skip = useCallback(
    (deltaSeconds: number) => {
      const el = audioRef.current
      if (el === null) {
        return
      }
      seek(el.currentTime + deltaSeconds)
    },
    [seek],
  )

  const setRate = useCallback((next: number) => {
    setRateState(clamp(next, 0.25, 4))
  }, [])

  return useMemo(
    () => ({
      currentTime,
      duration,
      playing,
      rate,
      ready,
      error,
      play,
      pause,
      toggle,
      seek,
      skip,
      setRate,
      element: audioRef.current,
    }),
    [currentTime, duration, playing, rate, ready, error, play, pause, toggle, seek, skip, setRate],
  )
}
