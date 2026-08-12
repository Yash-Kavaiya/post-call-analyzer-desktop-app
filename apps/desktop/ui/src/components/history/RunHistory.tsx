/**
 * Every batch this machine has run, and what each one did to each call.
 *
 * The live queue on the ingest screen answers "what is happening now" and
 * loses everything the moment you navigate away. This answers the questions
 * that come up afterwards — what did last night's run do, which files failed
 * and why, has this one been attempted before — so it reads from the database
 * rather than from event state.
 *
 * Items are fetched per run, on expand. A corpus run has 1,968 of them; loading
 * every run's items up front would mean tens of thousands of rows in memory to
 * show a list of six headings.
 */

import { useCallback, useEffect, useState } from 'react'
import {
  formatWhen,
  getRunItems,
  listRuns,
  runDuration,
  type RunItem,
  type RunSummary,
} from '../../api'
import './RunHistory.css'

/** Runs per page. */
const RUN_PAGE = 10
/** Items per page within an expanded run. */
const ITEM_PAGE = 50

export interface RunHistoryProps {
  /**
   * Changes whenever a run finishes, so the list refetches. A number rather
   * than a callback registration: the parent already owns the event
   * subscription, and two subscribers to one event is a bug waiting to happen.
   */
  refreshToken?: number
}

function StatusChip({ status }: { status: string }) {
  return <span className={`rh-chip rh-chip-${status}`}>{status}</span>
}

function ItemList({ run }: { run: RunSummary }) {
  const [items, setItems] = useState<RunItem[]>([])
  const [offset, setOffset] = useState(0)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const loadMore = useCallback(
    async (from: number) => {
      setLoading(true)
      try {
        const page = await getRunItems(run.id, from, ITEM_PAGE)
        setItems((prev) => (from === 0 ? page : [...prev, ...page]))
        setOffset(from + page.length)
        setError(null)
      } catch (e) {
        setError(String(e))
      } finally {
        setLoading(false)
      }
    },
    [run.id],
  )

  useEffect(() => {
    void loadMore(0)
    // Refetch while a run is live so an expanded row keeps up with it.
  }, [loadMore, run.status, run.itemCount])

  if (error) return <p className="rh-error">Could not load this run: {error}</p>
  if (items.length === 0 && !loading) {
    return <p className="rh-empty">This run recorded no calls.</p>
  }

  return (
    <div className="rh-items">
      <ul>
        {items.map((it) => (
          <li key={it.callId} className={`rh-item rh-item-${it.status}`}>
            <span className="rh-item-name" title={it.name}>
              {it.name}
            </span>
            <span className="rh-item-stage">{it.stage}</span>
            <StatusChip status={it.status} />
            <span className="rh-item-secs">
              {it.secs != null ? `${Math.round(it.secs)}s` : '—'}
            </span>
            {it.error && <span className="rh-item-error">{it.error}</span>}
          </li>
        ))}
      </ul>
      {offset < run.itemCount && (
        <button type="button" className="rh-more" onClick={() => void loadMore(offset)} disabled={loading}>
          {loading ? 'Loading…' : `Show more (${run.itemCount - offset} left)`}
        </button>
      )}
    </div>
  )
}

export function RunHistory({ refreshToken = 0 }: RunHistoryProps) {
  const [runs, setRuns] = useState<RunSummary[]>([])
  const [offset, setOffset] = useState(0)
  const [expanded, setExpanded] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const load = useCallback(async (from: number) => {
    setLoading(true)
    try {
      const page = await listRuns(from, RUN_PAGE)
      setRuns((prev) => (from === 0 ? page : [...prev, ...page]))
      setOffset(from + page.length)
      setError(null)
    } catch (e) {
      setError(String(e))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load(0)
  }, [load, refreshToken])

  if (error) return <p className="rh-error">Could not load history: {error}</p>

  if (runs.length === 0) {
    return (
      <p className="rh-empty">
        {loading ? 'Loading history…' : 'No batches have been run yet.'}
      </p>
    )
  }

  return (
    <div className="rh-root">
      <ul className="rh-runs">
        {runs.map((run) => {
          const isOpen = expanded === run.id
          const took = runDuration(run.startedAt, run.finishedAt)
          return (
            <li key={run.id} className={`rh-run rh-run-${run.status}`}>
              <button
                type="button"
                className="rh-run-head"
                aria-expanded={isOpen}
                onClick={() => setExpanded(isOpen ? null : run.id)}
              >
                <span className="rh-caret" aria-hidden="true">
                  {isOpen ? '▾' : '▸'}
                </span>
                <span className="rh-run-label">{run.label}</span>
                <StatusChip status={run.status} />
                <span className="rh-run-when" title={run.startedAt}>
                  {formatWhen(run.startedAt)}
                </span>
                <span className="rh-run-counts">
                  {run.processed} done
                  {run.failed > 0 && <em className="rh-run-failed"> · {run.failed} failed</em>}
                </span>
                <span className="rh-run-took">{took ?? '—'}</span>
              </button>
              {isOpen && <ItemList run={run} />}
            </li>
          )
        })}
      </ul>
      {runs.length === offset && runs.length % RUN_PAGE === 0 && (
        <button type="button" className="rh-more" onClick={() => void load(offset)} disabled={loading}>
          {loading ? 'Loading…' : 'Older runs'}
        </button>
      )}
    </div>
  )
}

export default RunHistory
