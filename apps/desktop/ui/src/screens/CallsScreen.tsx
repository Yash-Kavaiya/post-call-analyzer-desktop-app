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
                <td>{c.overall != null ? `${Math.round(c.overall)}%` : '—'}</td>
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
