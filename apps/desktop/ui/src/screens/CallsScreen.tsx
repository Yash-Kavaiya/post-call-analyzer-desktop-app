/**
 * Call list.
 *
 * Paginated rather than virtualised: the backend already answers with a page,
 * so fetching 50 rows keeps both the query and the DOM small at 1,968 calls.
 * Search is debounced so typing does not fire a query per keystroke.
 */

import { useEffect, useState } from 'react'
import { formatDuration, listCalls, type CallSummary } from '../api'
import './CallsScreen.css'

export interface CallsScreenProps {
  onSelect: (callId: string) => void
}

const PAGE_SIZE = 50

/**
 * Sentiment as a signed reading, or a dash when the agent has not run.
 *
 * Blank and 0.00 are different states and must not look alike: one is an
 * unassessed call, the other is a call the agent judged as neutral.
 */
function sentimentCell(value: number | null) {
  if (value == null) return <span className="calls-none">—</span>
  const tone = value <= -0.2 ? 'bad' : value >= 0.2 ? 'ok' : 'neutral'
  return (
    <span className={`calls-sent calls-sent-${tone}`}>
      {value > 0 ? '+' : value < 0 ? '−' : ''}
      {Math.abs(value).toFixed(2)}
    </span>
  )
}

/**
 * The compliance verdict, with the blocking-finding count when it failed.
 *
 * The count comes from the findings ledger rather than the agent's payload, so
 * it stays right for any agent that files findings.
 */
function complianceCell(passed: boolean | null, blocking: number) {
  if (passed == null) return <span className="calls-none">—</span>
  if (passed) return <span className="calls-verdict calls-ok">pass</span>
  return (
    <span className="calls-verdict calls-bad">
      fail{blocking > 0 ? ` · ${blocking}` : ''}
    </span>
  )
}

function stageTone(stage: string): string {
  switch (stage) {
    case 'analyzed':
    case 'indexed':
    case 'redacted':
      return 'ok'
    case 'failed':
      return 'bad'
    case 'discovered':
      return 'idle'
    default:
      return 'busy'
  }
}

export function CallsScreen({ onSelect }: CallsScreenProps) {
  const [calls, setCalls] = useState<CallSummary[]>([])
  const [page, setPage] = useState(0)
  const [search, setSearch] = useState('')
  const [debounced, setDebounced] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    const t = setTimeout(() => {
      setDebounced(search)
      setPage(0)
    }, 250)
    return () => clearTimeout(t)
  }, [search])

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    listCalls(page * PAGE_SIZE, PAGE_SIZE, debounced || undefined)
      .then((rows) => {
        if (!cancelled) {
          setCalls(rows)
          setError(null)
        }
      })
      .catch((e: unknown) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false))
    return () => {
      cancelled = true
    }
  }, [page, debounced])

  return (
    <div className="calls-root">
      <header className="calls-header">
        <h2>Calls</h2>
        <input
          type="search"
          className="calls-search"
          placeholder="Search by name…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
      </header>

      {error && <p className="calls-error">Could not load calls: {error}</p>}

      {loading && calls.length === 0 ? (
        <p className="calls-state">Loading…</p>
      ) : calls.length === 0 ? (
        <p className="calls-state">
          {debounced ? `No calls match “${debounced}”.` : 'No calls yet — ingest a folder first.'}
        </p>
      ) : (
        <table className="calls-table">
          <thead>
            <tr>
              <th>Call</th>
              <th>Duration</th>
              <th>Stage</th>
              <th>Speakers</th>
              <th>QA</th>
              <th>Sentiment</th>
              <th>Compliance</th>
            </tr>
          </thead>
          <tbody>
            {calls.map((c) => (
              <tr key={c.id} onClick={() => onSelect(c.id)} tabIndex={0}
                  onKeyDown={(e) => e.key === 'Enter' && onSelect(c.id)}>
                <td className="calls-name" title={c.displayName}>{c.displayName}</td>
                <td>{formatDuration(c.durationSecs)}</td>
                <td>
                  <span className={`calls-stage calls-${stageTone(c.stage)}`}>{c.stage}</span>
                </td>
                <td>{c.speakerCount ?? '—'}</td>
                <td>
                  {c.overall != null ? (
                    `${Math.round(c.overall)}%`
                  ) : (
                    <span className="calls-none">—</span>
                  )}
                </td>
                <td>{sentimentCell(c.sentiment)}</td>
                <td>{complianceCell(c.compliancePassed, c.blockingFindings)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <footer className="calls-pager">
        <button type="button" disabled={page === 0} onClick={() => setPage((p) => p - 1)}>
          Previous
        </button>
        <span>Page {page + 1}</span>
        <button
          type="button"
          disabled={calls.length < PAGE_SIZE}
          onClick={() => setPage((p) => p + 1)}
        >
          Next
        </button>
      </footer>
    </div>
  )
}

export default CallsScreen
