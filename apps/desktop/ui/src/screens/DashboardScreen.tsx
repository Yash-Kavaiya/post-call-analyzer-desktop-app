/**
 * Corpus overview: how much has been processed, what it cost, how it scored.
 */

import { useEffect, useState } from 'react'
import {
  formatDuration,
  formatHours,
  getStats,
  listCalls,
  type CallSummary,
  type Stats,
} from '../api'
import './DashboardScreen.css'

export interface DashboardScreenProps {
  onSelect: (callId: string) => void
}

/** Pipeline order, so the funnel reads left-to-right as work progresses. */
const STAGE_ORDER = [
  'discovered',
  'decoded',
  'transcribed',
  'diarized',
  'redacted',
  'analyzed',
  'indexed',
  'failed',
]

function Tile({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="dash-tile">
      <span className="dash-tile-label">{label}</span>
      <span className="dash-tile-value">{value}</span>
      {hint && <span className="dash-tile-hint">{hint}</span>}
    </div>
  )
}

export function DashboardScreen({ onSelect }: DashboardScreenProps) {
  const [stats, setStats] = useState<Stats | null>(null)
  const [recent, setRecent] = useState<CallSummary[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    Promise.all([getStats(), listCalls(0, 8)])
      .then(([s, r]) => {
        setStats(s)
        setRecent(r)
      })
      .catch((e: unknown) => setError(String(e)))
  }, [])

  if (error) return <p className="dash-error">Could not load stats: {error}</p>
  if (!stats) return <p className="dash-state">Loading…</p>

  const scored = recent.filter((c) => c.overall != null)
  const meanScore =
    scored.length > 0
      ? Math.round(scored.reduce((a, c) => a + (c.overall ?? 0), 0) / scored.length)
      : null

  const maxStage = Math.max(1, ...Object.values(stats.byStage))
  const processed = stats.totalCalls - stats.outstanding

  return (
    <div className="dash-root">
      <h2>Overview</h2>

      <div className="dash-tiles">
        <Tile label="Calls" value={String(stats.totalCalls)} />
        <Tile
          label="Processed"
          value={String(processed)}
          hint={`${stats.outstanding} outstanding`}
        />
        <Tile label="Audio" value={formatHours(stats.audioSecs)} />
        <Tile
          label="Spend"
          value={`$${stats.costUsd.toFixed(2)}`}
          hint={`${stats.inputTokens.toLocaleString()} in / ${stats.outputTokens.toLocaleString()} out`}
        />
        <Tile label="Mean QA" value={meanScore != null ? `${meanScore}%` : '—'} />
      </div>

      <section className="dash-panel">
        <h3>Pipeline</h3>
        {Object.keys(stats.byStage).length === 0 ? (
          <p className="dash-state">Nothing ingested yet.</p>
        ) : (
          <ul className="dash-funnel">
            {STAGE_ORDER.filter((s) => stats.byStage[s]).map((stage) => {
              const n = stats.byStage[stage]
              return (
                <li key={stage} className={stage === 'failed' ? 'dash-failed' : undefined}>
                  <span className="dash-funnel-label">{stage}</span>
                  <div className="dash-funnel-bar">
                    <div style={{ width: `${(n / maxStage) * 100}%` }} />
                  </div>
                  <span className="dash-funnel-count">{n}</span>
                </li>
              )
            })}
          </ul>
        )}
      </section>

      <section className="dash-panel">
        <h3>Recent calls</h3>
        {recent.length === 0 ? (
          <p className="dash-state">No calls yet.</p>
        ) : (
          <ul className="dash-recent">
            {recent.map((c) => (
              <li key={c.id}>
                <button type="button" onClick={() => onSelect(c.id)}>
                  <span className="dash-recent-name">{c.displayName}</span>
                  <span className="dash-recent-dur">{formatDuration(c.durationSecs)}</span>
                  <span className="dash-recent-stage">{c.stage}</span>
                  <span className="dash-recent-score">
                    {c.overall != null ? `${Math.round(c.overall)}%` : '—'}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  )
}

export default DashboardScreen
