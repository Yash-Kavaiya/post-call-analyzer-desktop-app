/**
 * The findings on a call that are serious enough to fail it.
 *
 * Read from the findings ledger rather than from the compliance agent's payload.
 * Two reasons that matters: the ledger is written by *any* agent that files
 * findings, not just compliance, and it is the durable record — it survives the
 * agent being re-run under a different model, which is what the corpus risk
 * register is counted from.
 *
 * Deliberately not a second copy of the compliance panel. That panel, further
 * down the page, carries every finding at every severity with its full detail.
 * This one answers a different question, at the top of the screen, before a
 * supervisor has scrolled: is there something on this call that needs acting on.
 * A call with nothing blocking renders nothing at all.
 */

import { useEffect, useState } from 'react'
import { formatDuration, getFindings, type Finding } from '../../api'
import './RiskBanner.css'

export interface RiskBannerProps {
  callId: string
  onSeek: (seconds: number) => void
  /** Bumped when a run finishes, so the banner refreshes without a remount. */
  refreshToken?: number
}

export function RiskBanner({ callId, onSeek, refreshToken = 0 }: RiskBannerProps) {
  const [findings, setFindings] = useState<Finding[]>([])

  useEffect(() => {
    let cancelled = false
    getFindings(callId)
      .then((f) => !cancelled && setFindings(f))
      // Silent on failure by design: this is an addition to a page that works
      // without it, and an error strip here would push the call itself down.
      .catch(() => !cancelled && setFindings([]))
    return () => {
      cancelled = true
    }
  }, [callId, refreshToken])

  const blocking = findings.filter((f) => f.blocking)
  if (blocking.length === 0) return null

  return (
    <aside className="rb-root" aria-label="Findings that fail this call">
      <header className="rb-head">
        <span className="rb-badge">
          {blocking.length} blocking finding{blocking.length === 1 ? '' : 's'}
        </span>
        <span className="rb-note">
          Anything at high or critical severity fails the call.
        </span>
      </header>
      <ul className="rb-list">
        {blocking.map((f) => (
          <li key={f.id}>
            <span className={`rb-sev rb-sev-${f.severity}`}>{f.severity}</span>
            <span className="rb-title">{f.title || f.code}</span>
            <span className="rb-detail">{f.detail}</span>
            {f.citations.length > 0 && (
              <button
                type="button"
                className="rb-at"
                onClick={() => onSeek(f.citations[0].start)}
                title={f.citations[0].quote ?? undefined}
              >
                {formatDuration(f.citations[0].start)}
              </button>
            )}
          </li>
        ))}
      </ul>
    </aside>
  )
}

export default RiskBanner
