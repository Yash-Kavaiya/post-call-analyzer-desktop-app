/**
 * Talk to the real database from a plain browser.
 *
 * `npm run dev` has no Tauri runtime, so historically every screen was drawn
 * from invented fixtures. That is fine for checking a layout and misleading
 * for everything else — you cannot tell how a list behaves against 1,972 calls
 * and 771 contacts by looking at nine hand-written rows, and a screen that
 * looks right on fixtures can be wrong on real data.
 *
 * With `ca serve` running, commands go to the real corpus instead. Anything
 * the bridge does not implement falls back to a fixture **and says so**, once
 * per command, so a screen is never quietly half-real.
 */

const ENDPOINT = 'http://127.0.0.1:8479/invoke'

/** Commands already reported as unsupported, so the warning is not repeated. */
const fellBack = new Set<string>()

export interface BridgeResult {
  handled: boolean
  value?: unknown
}

/** Whether `ca serve` is up. Checked once, before anything renders. */
export async function bridgeAvailable(): Promise<boolean> {
  try {
    const res = await fetch(ENDPOINT, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ cmd: 'get_stats', args: {} }),
    })
    if (!res.ok) return false
    const body = (await res.json()) as { ok?: boolean }
    return body.ok === true
  } catch {
    return false
  }
}

/**
 * Run one command against the real store.
 *
 * `handled: false` means the bridge does not implement it — the caller should
 * fall back to a fixture. A genuine backend error is thrown instead, because a
 * failing query is a bug to see, not a gap to paper over.
 */
export async function callBridge(
  cmd: string,
  args: Record<string, unknown>,
): Promise<BridgeResult> {
  const res = await fetch(ENDPOINT, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ cmd, args }),
  })
  const body = (await res.json()) as {
    ok?: boolean
    value?: unknown
    error?: string
    unsupported?: boolean
  }

  if (body.ok) return { handled: true, value: body.value }

  if (body.unsupported) {
    if (!fellBack.has(cmd)) {
      fellBack.add(cmd)
      // eslint-disable-next-line no-console
      console.warn(`[dev] \`${cmd}\` is not in the dev bridge — showing fixture data for it`)
    }
    return { handled: false }
  }

  throw new Error(body.error ?? `bridge failed: ${cmd}`)
}
