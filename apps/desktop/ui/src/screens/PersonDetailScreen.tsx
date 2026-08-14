/**
 * One person: what their calls add up to, and the calls themselves.
 *
 * Every measure here can be absent, and absent is drawn as a dash rather than
 * a zero. A contact with 196 calls and no analysis is the normal case in a
 * corpus that has only just been ingested, and that screen has to be readable
 * without inventing numbers to fill it.
 */

import { useEffect, useState } from 'react'
import { day, getPerson, type MonthPoint, type PersonDetail } from '../api'
import './PersonDetailScreen.css'

export interface PersonDetailScreenProps {
  personId: string
  onBack: () => void
  onOpenCall: (callId: string) => void
}

/** A measure, or a dash. Never a zero standing in for "not measured". */
function score(v: number | null | undefined, digits = 0, suffix = ''): string {
  return v == null ? '—' : `${v.toFixed(digits)}${suffix}`
}

function percent(v: number | null): string {
  return v == null ? '—' : `${Math.round(v * 100)}%`
}

/**
 * A monthly trend.
 *
 * Below three points there is no trend to draw — two points render as a
 * straight line spanning the box, which reads as a divider rather than a
 * chart. The same rule the live sentiment sparkline learned the hard way.
 */
function Trend({ points, lo, hi, label }: { points: MonthPoint[]; lo: number; hi: number; label: string }) {
  if (points.length < 3) {
    return (
      <div className="pd-trend pd-trend-empty">
        <span className="pd-trend-label">{label}</span>
        <span className="pd-none">
          {points.length === 0 ? 'nothing scored yet' : 'not enough months to trend'}
        </span>
      </div>
    )
  }
  const step = 100 / (points.length - 1)
  const y = (v: number) => ((hi - Math.max(lo, Math.min(hi, v))) / (hi - lo)) * 100
  const line = points.map((p, i) => `${i * step},${y(p.value)}`).join(' ')
  const first = points[0]
  const last = points[points.length - 1]
  return (
    <div className="pd-trend">
      <div className="pd-trend-head">
        <span className="pd-trend-label">{label}</span>
        <span className="pd-trend-range">
          {first.month} → {last.month}
        </span>
      </div>
      <svg className="pd-spark" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
        <polyline className="pd-spark-line" points={line} />
      </svg>
    </div>
  )
}

export function PersonDetailScreen({ personId, onBack, onOpenCall }: PersonDetailScreenProps) {
  const [detail, setDetail] = useState<PersonDetail | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    getPerson(personId)
      .then((d) => !cancelled && setDetail(d))
      .catch((e: unknown) => !cancelled && setError(String(e)))
    return () => {
      cancelled = true
    }
  }, [personId])

  if (error) return <p className="pd-error">Could not load: {error}</p>
  if (!detail) return <p className="pd-state">Loading…</p>

  const { summary, facts, calls } = detail

  return (
    <div className="pd-root">
      <button type="button" className="pd-back" onClick={onBack}>
        ← People
      </button>

      <header className="pd-header">
        <h2>{summary.displayName}</h2>
        <span className={`pd-kind pd-kind-${summary.kind}`}>{summary.kind}</span>
        <code className="pd-handle">{summary.handle}</code>
      </header>

      <section className="pd-stats">
        <div className="pd-stat">
          <span className="pd-stat-value">{facts.calls}</span>
          <span className="pd-stat-label">calls</span>
        </div>
        <div className="pd-stat">
          <span className="pd-stat-value">{score(facts.meanQa, 0, facts.meanQa == null ? '' : '%')}</span>
          <span className="pd-stat-label">mean QA</span>
        </div>
        <div className="pd-stat">
          <span className="pd-stat-value">{score(facts.meanSentiment, 2)}</span>
          <span className="pd-stat-label">mean sentiment</span>
        </div>
        <div className="pd-stat">
          <span className="pd-stat-value">{percent(facts.compliancePassRate)}</span>
          <span className="pd-stat-label">compliance pass</span>
        </div>
        <div className="pd-stat">
          <span className="pd-stat-value">{percent(detail.repeatContactRate)}</span>
          <span className="pd-stat-label" title="Share of calls where this contact also called in the previous 30 days">
            repeat rate
          </span>
        </div>
        <div className="pd-stat">
          <span className="pd-stat-value pd-stat-dates">
            {day(facts.firstCall)} – {day(facts.lastCall)}
          </span>
          <span className="pd-stat-label">active</span>
        </div>
      </section>

      <section className="pd-trends">
        <Trend points={detail.qaTrend} lo={0} hi={100} label="QA by month" />
        <Trend points={detail.sentimentTrend} lo={-1} hi={1} label="Sentiment by month" />
      </section>

      <section>
        <h3 className="pd-section">Calls</h3>
        {calls.length === 0 ? (
          <p className="pd-state">No calls.</p>
        ) : (
          <table className="pd-table">
            <thead>
              <tr>
                <th>When</th>
                <th>Sequence</th>
                <th>QA</th>
                <th>Sentiment</th>
              </tr>
            </thead>
            <tbody>
              {calls.map((c) => (
                <tr
                  key={c.callId}
                  onClick={() => onOpenCall(c.callId)}
                  tabIndex={0}
                  onKeyDown={(e) => e.key === 'Enter' && onOpenCall(c.callId)}
                >
                  <td className="pd-num">{day(c.occurredAt)}</td>
                  <td>
                    {c.sequence === 1 ? (
                      <span className="pd-first">first contact</span>
                    ) : (
                      <span className="pd-repeat">call #{c.sequence}</span>
                    )}
                  </td>
                  <td className="pd-num">{score(c.qaOverall, 0, c.qaOverall == null ? '' : '%')}</td>
                  <td className="pd-num">{score(c.sentiment, 2)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  )
}
