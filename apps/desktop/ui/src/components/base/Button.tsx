import type { ButtonHTMLAttributes, ReactNode } from 'react'
import './Button.css'

/** Material 3 button emphasis levels, highest to lowest. */
export type ButtonVariant = 'filled' | 'tonal' | 'outlined' | 'text'

export type ButtonSize = 'small' | 'medium' | 'large'

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** Emphasis level. Defaults to `filled`. */
  variant?: ButtonVariant
  /** Height / padding scale. Defaults to `medium`. */
  size?: ButtonSize
  /** Stretch to the full width of the parent. */
  fullWidth?: boolean
  /** Icon rendered before the label. */
  leadingIcon?: ReactNode
  /** Icon rendered after the label. */
  trailingIcon?: ReactNode
  /**
   * Show a spinner and block interaction. The label stays mounted so the
   * button does not change width mid-flight.
   */
  loading?: boolean
  children?: ReactNode
}

export function Button({
  variant = 'filled',
  size = 'medium',
  fullWidth = false,
  leadingIcon,
  trailingIcon,
  loading = false,
  disabled = false,
  className,
  type = 'button',
  children,
  ...rest
}: ButtonProps) {
  const classes = [
    'm3-button',
    `m3-button--${variant}`,
    `m3-button--${size}`,
    fullWidth ? 'm3-button--full-width' : null,
    loading ? 'm3-button--loading' : null,
    className,
  ]
    .filter(Boolean)
    .join(' ')

  return (
    <button
      {...rest}
      type={type}
      className={classes}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
    >
      <span className="m3-button__state-layer" aria-hidden="true" />
      {loading ? (
        <span className="m3-button__spinner" aria-hidden="true" />
      ) : leadingIcon ? (
        <span className="m3-button__icon" aria-hidden="true">
          {leadingIcon}
        </span>
      ) : null}
      <span className="m3-button__label">{children}</span>
      {!loading && trailingIcon ? (
        <span className="m3-button__icon" aria-hidden="true">
          {trailingIcon}
        </span>
      ) : null}
    </button>
  )
}

export default Button
