/**
 * Chart theming bridge.
 *
 * ECharts paints to a canvas, so it cannot consume `var(--token)` directly:
 * every colour has to be handed over as a concrete value. This module is the
 * single place where that translation happens. It reads the Material 3 custom
 * properties off the document, resolves them to real rgb() strings, and hands
 * charts a plain palette object.
 *
 * No colour literal is authored in the chart layer. When a token is missing the
 * value degrades to another token, and ultimately to the document's own
 * computed foreground/background - still theme-derived, never invented.
 */
import { useSyncExternalStore } from 'react'

export interface ChartTheme {
  /** Changes whenever any resolved colour changes. Use as a React `key`. */
  key: string
  primary: string
  onSurface: string
  onSurfaceVariant: string
  outline: string
  surface: string
  surfaceContainerLow: string
  surfaceContainerHigh: string
  error: string
  success: string
  warning: string
  /** Token-derived categorical series colours. */
  categorical: string[]
  fontFamily: string
}

type ColorSlot = Exclude<keyof ChartTheme, 'key' | 'categorical' | 'fontFamily'>

/**
 * Token lookup order per slot. The first custom property that resolves to a
 * real colour wins, so both the short aliases and the canonical M3 names work.
 */
const TOKENS: Record<ColorSlot, string[]> = {
  primary: ['--primary', '--md-sys-color-primary'],
  onSurface: ['--on-surface', '--md-sys-color-on-surface'],
  onSurfaceVariant: ['--on-surface-variant', '--md-sys-color-on-surface-variant'],
  outline: ['--outline', '--md-sys-color-outline'],
  surface: ['--surface', '--md-sys-color-surface'],
  surfaceContainerLow: ['--surface-container-low', '--md-sys-color-surface-container-low'],
  surfaceContainerHigh: ['--surface-container-high', '--md-sys-color-surface-container-high'],
  error: ['--error', '--md-sys-color-error'],
  success: ['--success', '--md-sys-color-success'],
  warning: ['--warning', '--md-sys-color-warning'],
}

/* -------------------------------------------------------------------------- */
/* Colour resolution                                                          */
/* -------------------------------------------------------------------------- */

let probeEl: HTMLElement | null = null

/** A hidden element used to let the browser resolve any CSS colour syntax. */
function probe(): HTMLElement | null {
  if (typeof document === 'undefined' || document.body === null) return null
  if (probeEl !== null && probeEl.isConnected) return probeEl
  const el = document.createElement('span')
  el.setAttribute('aria-hidden', 'true')
  el.style.position = 'absolute'
  el.style.width = '0'
  el.style.height = '0'
  el.style.overflow = 'hidden'
  el.style.pointerEvents = 'none'
  document.body.appendChild(el)
  probeEl = el
  return el
}

/**
 * Resolves an authored token value (hex, rgb(), hsl(), colour keyword, ...)
 * into the browser's canonical `rgb()` form, which zrender can parse.
 * Returns null when the value is absent or not a valid colour.
 */
function resolveColor(raw: string): string | null {
  const value = raw.trim()
  if (value === '') return null
  const el = probe()
  if (el === null) return null
  el.style.color = ''
  el.style.color = value
  // An invalid declaration is dropped by the CSSOM, leaving the property empty.
  if (el.style.color === '') return null
  const computed = window.getComputedStyle(el).color
  el.style.color = ''
  return computed === '' ? null : computed
}

interface Rgba {
  r: number
  g: number
  b: number
  a: number
}

function parseRgba(color: string): Rgba | null {
  const match = /^rgba?\(([^)]+)\)$/i.exec(color.trim())
  if (match === null) return null
  const parts = match[1]
    .replace(/\//g, ' ')
    .split(/[\s,]+/)
    .filter((part) => part !== '')
  if (parts.length < 3) return null
  const r = Number.parseFloat(parts[0])
  const g = Number.parseFloat(parts[1])
  const b = Number.parseFloat(parts[2])
  const a = parts.length > 3 ? Number.parseFloat(parts[3]) : 1
  if (!Number.isFinite(r) || !Number.isFinite(g) || !Number.isFinite(b)) return null
  return { r, g, b, a: Number.isFinite(a) ? a : 1 }
}

function clamp(value: number, min: number, max: number): number {
  if (value < min) return min
  if (value > max) return max
  return value
}

function rgbaString(rgba: Rgba): string {
  const r = Math.round(clamp(rgba.r, 0, 255))
  const g = Math.round(clamp(rgba.g, 0, 255))
  const b = Math.round(clamp(rgba.b, 0, 255))
  const a = clamp(rgba.a, 0, 1)
  return a >= 1 ? `rgb(${r}, ${g}, ${b})` : `rgba(${r}, ${g}, ${b}, ${a.toFixed(3)})`
}

/** Returns `color` scaled to `alpha` (0..1) of its current opacity. */
export function withAlpha(color: string, alpha: number): string {
  const rgba = parseRgba(color)
  if (rgba === null) return color
  return rgbaString({ r: rgba.r, g: rgba.g, b: rgba.b, a: rgba.a * clamp(alpha, 0, 1) })
}

/** Linear blend: `t = 0` yields `from`, `t = 1` yields `to`. */
export function mixColors(from: string, to: string, t: number): string {
  const a = parseRgba(from)
  const b = parseRgba(to)
  if (a === null || b === null) return from
  const ratio = clamp(t, 0, 1)
  return rgbaString({
    r: a.r + (b.r - a.r) * ratio,
    g: a.g + (b.g - a.g) * ratio,
    b: a.b + (b.b - a.b) * ratio,
    a: a.a + (b.a - a.a) * ratio,
  })
}

/* -------------------------------------------------------------------------- */
/* Theme snapshot                                                             */
/* -------------------------------------------------------------------------- */

function readTheme(): ChartTheme {
  const rootStyle =
    typeof document === 'undefined' ? null : window.getComputedStyle(document.documentElement)
  const bodyStyle =
    typeof document === 'undefined' || document.body === null
      ? null
      : window.getComputedStyle(document.body)

  // Last-resort values come from the document itself, so they still follow the
  // active theme even if the token sheet has not loaded yet.
  const fallbackFg = resolveColor(bodyStyle === null ? '' : bodyStyle.color) ?? 'currentColor'
  const fallbackBg =
    resolveColor(bodyStyle === null ? '' : bodyStyle.backgroundColor) ?? 'transparent'

  const slot = (names: string[], fallback: string): string => {
    if (rootStyle === null) return fallback
    for (const name of names) {
      const resolved = resolveColor(rootStyle.getPropertyValue(name))
      if (resolved !== null) return resolved
    }
    return fallback
  }

  const surface = slot(TOKENS.surface, fallbackBg)
  const onSurface = slot(TOKENS.onSurface, fallbackFg)
  const onSurfaceVariant = slot(TOKENS.onSurfaceVariant, onSurface)
  const outline = slot(TOKENS.outline, mixColors(onSurface, surface, 0.6))
  const primary = slot(TOKENS.primary, onSurface)
  const error = slot(TOKENS.error, onSurface)
  const success = slot(TOKENS.success, primary)
  const warning = slot(TOKENS.warning, onSurfaceVariant)
  const surfaceContainerLow = slot(TOKENS.surfaceContainerLow, surface)
  const surfaceContainerHigh = slot(TOKENS.surfaceContainerHigh, surface)

  const tokenFont = (rootStyle === null ? '' : rootStyle.getPropertyValue('--font-sans')).trim()
  const fontFamily =
    tokenFont !== '' ? tokenFont : bodyStyle === null ? 'sans-serif' : bodyStyle.fontFamily

  // Categorical ramp: every entry is derived from a token, never authored.
  const categorical = [
    primary,
    success,
    warning,
    error,
    mixColors(primary, onSurfaceVariant, 0.5),
    mixColors(success, onSurface, 0.35),
    mixColors(error, surface, 0.35),
    mixColors(warning, onSurface, 0.35),
  ]

  return {
    key: `${categorical.join('|')}|${surface}|${onSurface}|${outline}`,
    primary,
    onSurface,
    onSurfaceVariant,
    outline,
    surface,
    surfaceContainerLow,
    surfaceContainerHigh,
    error,
    success,
    warning,
    categorical,
    fontFamily,
  }
}

let snapshot: ChartTheme | null = null
const listeners = new Set<() => void>()
let mutationObserver: MutationObserver | null = null
let colorSchemeQuery: MediaQueryList | null = null

function refresh(): void {
  const next = readTheme()
  if (snapshot !== null && snapshot.key === next.key) return
  snapshot = next
  for (const listener of listeners) listener()
}

function install(): void {
  if (typeof document === 'undefined' || mutationObserver !== null) return
  // A theme switch shows up as an attribute change on <html>; a late-arriving
  // stylesheet shows up as a <head> mutation.
  mutationObserver = new MutationObserver(refresh)
  mutationObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['class', 'style', 'data-theme'],
  })
  mutationObserver.observe(document.head, { childList: true })
  colorSchemeQuery = window.matchMedia('(prefers-color-scheme: dark)')
  colorSchemeQuery.addEventListener('change', refresh)
  // Tokens may only resolve after the first paint.
  window.requestAnimationFrame(refresh)
}

function uninstall(): void {
  if (mutationObserver !== null) {
    mutationObserver.disconnect()
    mutationObserver = null
  }
  if (colorSchemeQuery !== null) {
    colorSchemeQuery.removeEventListener('change', refresh)
    colorSchemeQuery = null
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  install()
  return () => {
    listeners.delete(listener)
    if (listeners.size === 0) uninstall()
  }
}

function getSnapshot(): ChartTheme {
  if (snapshot === null) snapshot = readTheme()
  return snapshot
}

/** Subscribes to the live, token-derived chart palette. */
export function useChartTheme(): ChartTheme {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
}

/** Picks the token colour that matches a 0..100 quality score. */
export function scoreColor(theme: ChartTheme, percent: number): string {
  if (percent >= 80) return theme.success
  if (percent >= 60) return theme.primary
  if (percent >= 40) return theme.warning
  return theme.error
}
