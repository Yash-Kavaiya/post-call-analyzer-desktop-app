import type { CSSProperties, HTMLAttributes } from 'react'
import './LinearProgress.css'

export type ProgressTone = 'primary' | 'success' | 'warning' | 'error'

/** Lets us set CSS custom properties in a `style` prop without `any`. */
type StyleWithVars = CSSProperties & Record<`--${string}`, string | number>

export interface LinearProgressProps
  extends Omit<HTMLAttributes<HTMLDivElement>, 'role' | 'children'> {
  /**
   * Progress in the range `0..max`. Omit (or pass `undefined`) to render the
   * indeterminate animation.
   */
  value?: number
  /** Upper bound for `value`. Defaults to 100. */
  max?: number
  /** Optional secondary "buffered" track fill, same units as `value`. */
  buffer?: number
  /** Accessible name for the bar. */
  label?: string
  /** Track thickness in px. Defaults to 4. */
  thickness?: number
  /** Colour role for the indicator. Defaults to `primary`. */
  tone?: ProgressTone
  /** Render the numeric percentage beside the bar. Determinate only. */
  showValue?: boolean
}

function clamp(n: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, n))
}

export function LinearProgress({
  value,
  max = 100,
  buffer,
  label,
  thickness = 4,
  tone = 'primary',
  showValue = false,
  className,
  style,
  ...rest
}: LinearProgressProps) {
  const indeterminate = value === undefined || Number.isNaN(value)
  const safeMax = max > 0 ? max : 100
  const pct = indeterminate ? 0 : clamp((value / safeMax) * 100, 0, 100)
  const bufferPct =
    buffer === undefined ? undefined : clamp((buffer / safeMax) * 100, 0, 100)

  const classes = [
    'm3-linear-progress',
    `m3-linear-progress--${tone}`,
    indeterminate
      ? 'm3-linear-progress--indeterminate'
      : 'm3-linear-progress--determinate',
    className,
  ]
    .filter(Boolean)
    .join(' ')

  const rootStyle: StyleWithVars = {
    ...style,
    '--m3-progress-thickness': `${thickness}px`,
    '--m3-progress-value': `${pct}%`,
    '--m3-progress-buffer': `${bufferPct ?? 0}%`,
  }

  return (
    <div className="m3-linear-progress__wrapper">
      <div
        {...rest}
        className={classes}
        style={rootStyle}
        role="progressbar"
        aria-label={label}
        aria-valuemin={indeterminate ? undefined : 0}
        aria-valuemax={indeterminate ? undefined : safeMax}
        aria-valuenow={indeterminate ? undefined : value}
        aria-valuetext={indeterminate ? undefined : `${Math.round(pct)}%`}
      >
        <div className="m3-linear-progress__track" />
        {bufferPct !== undefined && !indeterminate ? (
          <div className="m3-linear-progress__buffer" />
        ) : null}
        <div className="m3-linear-progress__indicator" />
        {indeterminate ? (
          <div className="m3-linear-progress__indicator m3-linear-progress__indicator--secondary" />
        ) : null}
      </div>
      {showValue && !indeterminate ? (
        <span className="m3-linear-progress__value">{Math.round(pct)}%</span>
      ) : null}
    </div>
  )
}

export default LinearProgress
