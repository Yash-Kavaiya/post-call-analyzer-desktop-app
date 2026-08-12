/**
 * Ingest and batch control.
 *
 * The live queue is the fiddly part: a run over 1,968 files emits thousands of
 * events. Calling setState per event would re-render the list continuously and
 * lock the UI, so events are buffered and flushed on an interval — the queue
 * stays readable and the main thread stays free.
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import { RunHistory } from '../components/history/RunHistory'
import { SourcePicker } from '../components/ingest/SourcePicker'
import {
  getStartup,
  onPipelineProgress,
  onRunFinished,
  runBatch,
  type ProgressEvent,
  type Startup,
} from '../api'
import './IngestScreen.css'

interface QueueRow {
  callId: string
  name: string
  stage: string
  status: 'running' | 'done' | 'failed'
  secs?: number
  error?: string
}

/** How often buffered progress events are flushed into React state. */
const FLUSH_MS = 250
/**
 * Rows kept in the *live* queue.
 *
 * A run over 1,968 files would otherwise put 1,968 nodes in the DOM and make
 * the page crawl. Dropping them used to lose them; every row is now written to
 * the run history first, so this cap only limits what is on screen, and the
 * count of what it is hiding is shown rather than silently swallowed.
 */
const MAX_ROWS = 200

export function IngestScreen() {
  const [limit, setLimit] = useState<string>('10')
  const [concurrency, setConcurrency] = useState<string>('2')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [startup, setStartup] = useState<Startup | null>(null)
  const [rows, setRows] = useState<QueueRow[]>([])
  const [counts, setCounts] = useState({ done: 0, failed: 0 })
  const [seen, setSeen] = useState(0)
  /** Bumped when a run finishes, so the history refetches. */
  const [historyToken, setHistoryToken] = useState(0)

  // Buffer between the event stream and React.
  const buffer = useRef<ProgressEvent[]>([])
  /** Every call id this run has touched, so the cap can say what it is hiding. */
  const seenIds = useRef<Set<string>>(new Set())

  useEffect(() => {
    getStartup().then(setStartup).catch(() => setStartup(null))
  }, [])

  useEffect(() => {
    const unsubscribe = onPipelineProgress((e) => {
      buffer.current.push(e)
    })

    const timer = setInterval(() => {
      if (buffer.current.length === 0) return
      const batch = buffer.current
      buffer.current = []

      for (const e of batch) {
        if (e.callId) seenIds.current.add(e.callId)
      }
      setSeen(seenIds.current.size)

      setRows((prev) => {
        const next = new Map(prev.map((r) => [r.callId, r]))
        for (const e of batch) {
          if (!e.callId) continue
          const existing = next.get(e.callId)
          // Copied rather than mutated: `existing` is the object React is
          // holding in state, and editing it in place makes the row's history
          // invisible to any future memo or comparison.
          const row: QueueRow = existing
            ? { ...existing }
            : {
                callId: e.callId,
                name: e.name || e.callId,
                stage: 'queued',
                status: 'running',
              }
          if (e.kind === 'stage' && e.stage) row.stage = e.stage
          if (e.kind === 'finished') {
            row.status = 'done'
            // The last `stage` event is authoritative. Naming a stage here
            // instead would freeze the terminal stage into the UI — it read
            // `redacted` even for calls that went on to be scored.
            row.secs = e.secs ?? undefined
          }
          if (e.kind === 'failed') {
            row.status = 'failed'
            row.error = e.error ?? 'unknown error'
          }
          next.set(e.callId, row)
        }
        return Array.from(next.values()).slice(-MAX_ROWS)
      })

      setCounts((c) => {
        let { done, failed } = c
        for (const e of batch) {
          if (e.kind === 'finished') done += 1
          if (e.kind === 'failed') failed += 1
        }
        return { done, failed }
      })

      if (batch.some((e) => e.kind === 'done')) setBusy(false)
    }, FLUSH_MS)

    const unsubscribeRun = onRunFinished(() => {
      setBusy(false)
      setHistoryToken((n) => n + 1)
    })

    return () => {
      unsubscribe()
      unsubscribeRun()
      clearInterval(timer)
    }
  }, [])

  const doRun = useCallback(async () => {
    setBusy(true)
    setError(null)
    setMessage(null)
    // The previous run is not lost by clearing this - it has already been
    // written to the history below, which is where it now lives.
    setRows([])
    setCounts({ done: 0, failed: 0 })
    seenIds.current = new Set()
    setSeen(0)
    setHistoryToken((n) => n + 1)
    try {
      const parsedLimit = limit.trim() === '' ? null : Number(limit)
      await runBatch(
        parsedLimit != null && Number.isFinite(parsedLimit) ? parsedLimit : null,
        Number(concurrency) || 2,
      )
    } catch (e) {
      setError(String(e))
      setBusy(false)
    }
  }, [limit, concurrency])

  return (
    <div className="ing-root">
      <h2>Ingest</h2>

      {startup && (!startup.whisperBinaryPresent || !startup.whisperModelPresent) && (
        <p className="ing-warn">
          Transcription is unavailable:{' '}
          {!startup.whisperBinaryPresent && `whisper binary missing (${startup.whisperBinary})`}
          {!startup.whisperBinaryPresent && !startup.whisperModelPresent && '; '}
          {!startup.whisperModelPresent && `model missing (${startup.whisperModel})`}
        </p>
      )}
      {startup?.englishOnly && (
        <p className="ing-note">
          The loaded model is English-only — non-English calls will transcribe poorly.
        </p>
      )}

      <section className="ing-card">
        <h3>1 · Where the calls are</h3>
        <SourcePicker
          busy={busy}
          onBusy={setBusy}
          onResult={(m) => {
            setMessage(m)
            setError(null)
          }}
          onError={(m) => {
            setError(m)
            setMessage(null)
          }}
        />
      </section>

      <section className="ing-card">
        <h3>2 · Run the batch</h3>
        <div className="ing-row">
          <label>
            Limit
            <input
              type="number"
              min={1}
              value={limit}
              onChange={(e) => setLimit(e.target.value)}
              placeholder="all"
            />
          </label>
          <label>
            Concurrency
            <input
              type="number"
              min={1}
              max={8}
              value={concurrency}
              onChange={(e) => setConcurrency(e.target.value)}
            />
          </label>
          <button type="button" onClick={doRun} disabled={busy}>
            {busy ? 'Running…' : 'Run'}
          </button>
        </div>
        <p className="ing-hint">
          Transcription already uses several cores, so 2 files at a time is
          usually faster than more.
        </p>
      </section>

      {message && <p className="ing-ok">{message}</p>}
      {error && <p className="ing-error">{error}</p>}

      {(rows.length > 0 || busy) && (
        <section className="ing-card">
          <h3>
            Queue <span className="ing-counts">{counts.done} done · {counts.failed} failed</span>
          </h3>
          <ul className="ing-queue">
            {rows.map((r) => (
              <li key={r.callId} className={`ing-q-${r.status}`}>
                <span className="ing-q-name">{r.name}</span>
                <span className="ing-q-stage">{r.stage}</span>
                {r.secs != null && <span className="ing-q-secs">{Math.round(r.secs)}s</span>}
                {r.error && <span className="ing-q-error">{r.error}</span>}
              </li>
            ))}
          </ul>
          {seen > rows.length && (
            <p className="ing-hint">
              Showing the most recent {rows.length} of {seen} calls in this run — all of
              them are in the history below.
            </p>
          )}
        </section>
      )}

      <section className="ing-card">
        <h3>History</h3>
        <p className="ing-hint">
          Every batch this machine has run. Expand one to see what it did to each call.
        </p>
        <RunHistory refreshToken={historyToken} />
      </section>
    </div>
  )
}

export default IngestScreen
