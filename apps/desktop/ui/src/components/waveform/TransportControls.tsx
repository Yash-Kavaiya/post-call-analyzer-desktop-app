import { formatTimecode } from './format'
import './TransportControls.css'

const DEFAULT_RATES: readonly number[] = [0.75, 1, 1.25, 1.5, 2]

export interface TransportControlsProps {
  playing: boolean
  /** Playhead position in seconds. */
  currentTime: number
  /** Media length in seconds. */
  duration: number
  /** Current playback rate. */
  rate: number
  onTogglePlay: () => void
  /** Called with a signed offset in seconds (negative rewinds). */
  onSkip: (deltaSeconds: number) => void
  onRateChange: (rate: number) => void
  /** Size of the skip buttons in seconds. Defaults to 5. */
  skipSeconds?: number
  /** Selectable playback rates. Defaults to `[0.75, 1, 1.25, 1.5, 2]`. */
  rates?: readonly number[]
  /** Greys out every control (e.g. while audio is still loading). */
  disabled?: boolean
  className?: string
}

function formatRate(rate: number): string {
  return (Number.isInteger(rate) ? String(rate) : rate.toFixed(2).replace(/0$/, '')) + '×'
}

export function TransportControls(props: TransportControlsProps) {
  const {
    playing,
    currentTime,
    duration,
    rate,
    onTogglePlay,
    onSkip,
    onRateChange,
    skipSeconds = 5,
    rates = DEFAULT_RATES,
    disabled = false,
    className,
  } = props

  const rootClassName = className !== undefined ? 'tc-root ' + className : 'tc-root'
  const skipLabel = String(skipSeconds)

  return (
    <div className={rootClassName}>
      <div className="tc-buttons">
        <button
          type="button"
          className="tc-btn"
          onClick={() => {
            onSkip(-skipSeconds)
          }}
          disabled={disabled}
          aria-label={'Back ' + skipLabel + ' seconds'}
          title={'Back ' + skipLabel + 's'}
        >
          <span aria-hidden="true">−{skipLabel}s</span>
        </button>

        <button
          type="button"
          className="tc-btn tc-btn-primary"
          onClick={onTogglePlay}
          disabled={disabled}
          aria-label={playing ? 'Pause' : 'Play'}
          title={playing ? 'Pause' : 'Play'}
        >
          <svg className="tc-icon" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            {playing ? (
              <path d="M8 5h3v14H8zm5 0h3v14h-3z" fill="currentColor" />
            ) : (
              <path d="M8 5l11 7-11 7z" fill="currentColor" />
            )}
          </svg>
        </button>

        <button
          type="button"
          className="tc-btn"
          onClick={() => {
            onSkip(skipSeconds)
          }}
          disabled={disabled}
          aria-label={'Forward ' + skipLabel + ' seconds'}
          title={'Forward ' + skipLabel + 's'}
        >
          <span aria-hidden="true">+{skipLabel}s</span>
        </button>
      </div>

      <div className="tc-time" role="status" aria-live="off">
        <span className="tc-time-current">{formatTimecode(currentTime, true)}</span>
        <span className="tc-time-sep">/</span>
        <span className="tc-time-total">{formatTimecode(duration)}</span>
      </div>

      <div className="tc-rates" role="group" aria-label="Playback speed">
        {rates.map((value) => (
          <button
            key={value}
            type="button"
            className={value === rate ? 'tc-rate tc-rate-active' : 'tc-rate'}
            onClick={() => {
              onRateChange(value)
            }}
            disabled={disabled}
            aria-pressed={value === rate}
          >
            {formatRate(value)}
          </button>
        ))}
      </div>
    </div>
  )
}

export default TransportControls
