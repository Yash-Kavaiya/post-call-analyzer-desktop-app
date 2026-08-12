import type { CSSProperties, MouseEvent, ReactNode } from 'react'
import './Chip.css'

/**
 * `assist` - a suggested action.
 * `filter` - a toggle; selection is reflected with `aria-pressed`.
 * `input`  - represents a user-entered value; may carry a remove affordance.
 */
export type ChipVariant = 'assist' | 'filter' | 'input'

/** Style token a status dot can be tinted with. */
export type ChipDotTone =
  | 'primary'
  | 'success'
  | 'warning'
  | 'error'
  | 'info'
  | 'neutral'

/** Lets us set CSS custom properties in a `style` prop without `any`. */
type StyleWithVars = CSSProperties & Record<`--${string}`, string | number>

export interface ChipProps {
  variant?: ChipVariant
  /** Selection state for the `filter` variant. */
  selected?: boolean
  /** Fired when a `filter` chip is toggled, with the next selection state. */
  onSelectedChange?: (selected: boolean) => void
  /** Click handler for `assist` (and any) chips. */
  onClick?: (event: MouseEvent<HTMLElement>) => void
  /**
   * Show a leading status dot. Either a semantic tone or any CSS colour
   * (e.g. `var(--success)` or `#34a853`).
   */
  dot?: ChipDotTone | string
  /** Icon before the label. Ignored when `dot` is set. */
  leadingIcon?: ReactNode
  /** Icon after the label. */
  trailingIcon?: ReactNode
  /** Render a remove button. Mainly for the `input` variant. */
  onRemove?: () => void
  /** Accessible label for the remove button. */
  removeLabel?: string
  disabled?: boolean
  /** Slightly smaller chip for dense rows. */
  dense?: boolean
  className?: string
  style?: CSSProperties
  title?: string
  id?: string
  children?: ReactNode
}

const TONES: ReadonlySet<string> = new Set<string>([
  'primary',
  'success',
  'warning',
  'error',
  'info',
  'neutral',
])

function dotColorFor(dot: string): string {
  if (!TONES.has(dot)) return dot
  if (dot === 'neutral') return 'var(--outline)'
  return `var(--${dot})`
}

export function Chip({
  variant = 'assist',
  selected = false,
  onSelectedChange,
  onClick,
  dot,
  leadingIcon,
  trailingIcon,
  onRemove,
  removeLabel = 'Remove',
  disabled = false,
  dense = false,
  className,
  style,
  children,
  ...rest
}: ChipProps) {
  const classes = [
    'm3-chip',
    `m3-chip--${variant}`,
    selected && variant === 'filter' ? 'm3-chip--selected' : null,
    dense ? 'm3-chip--dense' : null,
    disabled ? 'm3-chip--disabled' : null,
    className,
  ]
    .filter(Boolean)
    .join(' ')

  const resolvedStyle: StyleWithVars = dot
    ? { ...style, '--m3-chip-dot': dotColorFor(dot) }
    : { ...style }

  const content = (
    <>
      <span className="m3-chip__state-layer" aria-hidden="true" />
      {dot ? <span className="m3-chip__dot" aria-hidden="true" /> : null}
      {!dot && leadingIcon ? (
        <span className="m3-chip__icon" aria-hidden="true">
          {leadingIcon}
        </span>
      ) : null}
      {variant === 'filter' && selected && !dot ? (
        <svg
          className="m3-chip__check"
          viewBox="0 0 24 24"
          aria-hidden="true"
          focusable="false"
        >
          <path d="M9 16.2 4.8 12l-1.4 1.4L9 19 21 7l-1.4-1.4z" />
        </svg>
      ) : null}
      <span className="m3-chip__label">{children}</span>
      {trailingIcon ? (
        <span className="m3-chip__icon" aria-hidden="true">
          {trailingIcon}
        </span>
      ) : null}
    </>
  )

  // `input` chips can host a nested remove button, so they cannot themselves be
  // a <button> - nested buttons are invalid HTML and break keyboard order.
  if (variant === 'input' || onRemove) {
    return (
      <span {...rest} className={classes} style={resolvedStyle}>
        {onClick ? (
          <button
            type="button"
            className="m3-chip__pressable"
            disabled={disabled}
            onClick={onClick}
          >
            {content}
          </button>
        ) : (
          <span className="m3-chip__static">{content}</span>
        )}
        {onRemove ? (
          <button
            type="button"
            className="m3-chip__remove"
            aria-label={removeLabel}
            disabled={disabled}
            onClick={() => onRemove()}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
              <path d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z" />
            </svg>
          </button>
        ) : null}
      </span>
    )
  }

  return (
    <button
      {...rest}
      type="button"
      className={classes}
      style={resolvedStyle}
      disabled={disabled}
      aria-pressed={variant === 'filter' ? selected : undefined}
      onClick={(event) => {
        onClick?.(event)
        if (variant === 'filter') onSelectedChange?.(!selected)
      }}
    >
      {content}
    </button>
  )
}

export default Chip
