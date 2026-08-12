import type { HTMLAttributes, KeyboardEvent, ReactNode } from 'react'
import './Card.css'

/** M3 elevation levels 0-5. */
export type CardElevation = 0 | 1 | 2 | 3 | 4 | 5

export type CardVariant = 'elevated' | 'filled' | 'outlined'

export type CardPadding = 'none' | 'compact' | 'comfortable' | 'spacious'

export interface CardProps extends HTMLAttributes<HTMLDivElement> {
  /** Shadow depth, 0-5. Defaults to 1. */
  elevation?: CardElevation
  /** Surface treatment. Defaults to `elevated`. */
  variant?: CardVariant
  /** Inner padding preset. Defaults to `comfortable`. */
  padding?: CardPadding
  /**
   * Make the card behave like a button: hover/press state layer, tab stop,
   * and Enter/Space activation. Pair with `onClick`.
   */
  interactive?: boolean
  /** Raise the card by one level while hovered (interactive cards only). */
  raiseOnHover?: boolean
  children?: ReactNode
}

export function Card({
  elevation = 1,
  variant = 'elevated',
  padding = 'comfortable',
  interactive = false,
  raiseOnHover = true,
  className,
  children,
  onClick,
  onKeyDown,
  tabIndex,
  role,
  ...rest
}: CardProps) {
  const classes = [
    'm3-card',
    `m3-card--${variant}`,
    `m3-card--elevation-${elevation}`,
    `m3-card--padding-${padding}`,
    interactive ? 'm3-card--interactive' : null,
    interactive && raiseOnHover ? 'm3-card--raise' : null,
    className,
  ]
    .filter(Boolean)
    .join(' ')

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    onKeyDown?.(event)
    if (!interactive || !onClick || event.defaultPrevented) return
    if (event.key === 'Enter' || event.key === ' ' || event.key === 'Spacebar') {
      event.preventDefault()
      event.currentTarget.click()
    }
  }

  return (
    <div
      {...rest}
      className={classes}
      onClick={onClick}
      onKeyDown={handleKeyDown}
      tabIndex={interactive ? (tabIndex ?? 0) : tabIndex}
      role={interactive ? (role ?? 'button') : role}
    >
      {interactive ? (
        <span className="m3-card__state-layer" aria-hidden="true" />
      ) : null}
      {children}
    </div>
  )
}

// `title` is omitted from the inherited attributes: HTML's `title` is the
// tooltip string, and this component's `title` is rendered content. Keeping
// both under one name makes the interface unimplementable.
export interface CardHeaderProps extends Omit<HTMLAttributes<HTMLDivElement>, 'title'> {
  title?: ReactNode
  subtitle?: ReactNode
  /** Rendered on the trailing edge - typically an IconButton or Chip. */
  actions?: ReactNode
  /** Rendered on the leading edge - typically an icon or avatar. */
  leading?: ReactNode
}

export function CardHeader({
  title,
  subtitle,
  actions,
  leading,
  className,
  children,
  ...rest
}: CardHeaderProps) {
  return (
    <div
      {...rest}
      className={['m3-card__header', className].filter(Boolean).join(' ')}
    >
      {leading ? <div className="m3-card__leading">{leading}</div> : null}
      <div className="m3-card__headings">
        {title ? <div className="m3-card__title">{title}</div> : null}
        {subtitle ? <div className="m3-card__subtitle">{subtitle}</div> : null}
        {children}
      </div>
      {actions ? <div className="m3-card__actions">{actions}</div> : null}
    </div>
  )
}

export default Card
