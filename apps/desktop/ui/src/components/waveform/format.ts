function pad2(value: number): string {
  return value < 10 ? '0' + String(value) : String(value)
}

/**
 * Formats seconds as `m:ss`, `h:mm:ss`, or with one decimal when `withTenths`.
 * Negative and non-finite inputs clamp to zero so the readout never flickers.
 */
export function formatTimecode(seconds: number, withTenths = false): string {
  const safe = Number.isFinite(seconds) && seconds > 0 ? seconds : 0
  const whole = Math.floor(safe)
  const hours = Math.floor(whole / 3600)
  const minutes = Math.floor((whole % 3600) / 60)
  const secs = whole % 60

  let out: string
  if (hours > 0) {
    out = String(hours) + ':' + pad2(minutes) + ':' + pad2(secs)
  } else {
    out = String(minutes) + ':' + pad2(secs)
  }

  if (withTenths) {
    const tenths = Math.floor((safe - whole) * 10)
    out += '.' + String(tenths)
  }
  return out
}

/** Clamps `value` into `[min, max]`. */
export function clamp(value: number, min: number, max: number): number {
  if (!Number.isFinite(value)) {
    return min
  }
  if (value < min) {
    return min
  }
  if (value > max) {
    return max
  }
  return value
}
