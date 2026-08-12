import type { ButtonHTMLAttributes, ReactNode } from 'react'
import './IconButton.css'

export type IconButtonVariant = 'standard' | 'filled' | 'tonal' | 'outlined'

export type IconButtonSize = 'small' | 'medium' | 'large'

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, 'aria-label'> {
  /**
   * Accessible name. Required - an icon-only control has no visible text,
   * so this is the only thing a screen reader can announce.
   */
  label: string
  variant?: IconButtonVariant
  size?: IconButtonSize
  /**
   * Toggle state. When provided the button reports `aria-pressed` and picks
   * up the selected styling.
   */
  selected?: boolean
  /** The icon itself - an svg, an img, or a glyph. */
  children: ReactNode
}

export function IconButton({
  label,
  variant = 'standard',
  size = 'medium',
  selected,
  disabled = false,
  className,
  type = 'button',
  children,
  ...rest
}: IconButtonProps) {
  const classes = [
    'm3-icon-button',
    `m3-icon-button--${variant}`,
    `m3-icon-button--${size}`,
    selected ? 'm3-icon-button--selected' : null,
    className,
  ]
    .filter(Boolean)
    .join(' ')

  return (
    <button
      {...rest}
      type={type}
      className={classes}
      disabled={disabled}
      aria-label={label}
      aria-pressed={selected}
      title={rest.title ?? label}
    >
      <span className="m3-icon-button__state-layer" aria-hidden="true" />
      <span className="m3-icon-button__icon" aria-hidden="true">
        {children}
      </span>
    </button>
  )
}

export default IconButton
