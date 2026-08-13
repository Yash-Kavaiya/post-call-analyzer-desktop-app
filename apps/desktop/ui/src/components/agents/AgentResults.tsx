/**
 * What every agent found, and what each one cost.
 *
 * Eight agents produce eight different payload shapes. Rather than eight
 * bespoke renderers that drift apart, each agent gets a small formatter and
 * everything else — the header, the citations, the accounting strip — is shared.
 * An agent whose shape is not recognised still renders its raw JSON, so adding
 * an agent to the battery never produces a blank panel.
 *
 * The accounting strip is not decoration. A batch that quietly degraded — one
 * agent failing on every call, or a router substituting a cheaper model — looks
 * healthy from the call list, because the calls still reach `analyzed`. Cost,
 * latency, the answering model and the error live next to the output that came
 * from them.
 */

import { useEffect, useState } from 'react'
import { formatDuration, getAgentRuns, type AgentRun, type Citation } from '../../api'
import './AgentResults.css'

/**
 * Payloads are read field by field rather than cast to their report type.
 *
 * These come out of a SQLite blob written by whichever build ran the agent, so a
 * row can predate a field or spell one differently. Reading defensively costs a
 * `String(…)` per field and means an older row renders with a gap in it instead
 * of throwing on access and blanking the whole panel.
 */
function rows(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value) ? (value as Record<string, unknown>[]) : []
}

function cites(value: unknown): Citation[] {
  return Array.isArray(value) ? (value as Citation[]) : []
}

/** Severity, ordered so the worst sorts first and unknown labels land last. */
const SEVERITIES = ['critical', 'high', 'medium', 'low', 'info'] as const

function severityRank(raw: unknown): number {
  const i = SEVERITIES.indexOf(String(raw ?? '') as (typeof SEVERITIES)[number])
  return i === -1 ? SEVERITIES.length : i
}

export interface AgentResultsProps {
  callId: string
  onSeek: (seconds: number) => void
  /** Bumped when a run finishes, so results refresh without a remount. */
  refreshToken?: number
}

function CitationList({
  citations,
  onSeek,
}: {
  citations: Citation[]
  onSeek: (s: number) => void
}) {
  if (!citations?.length) return null
  return (
    <ul className="ag-cites">
      {citations.map((c, i) => (
        <li key={`${c.segmentId}-${i}`}>
          <button type="button" onClick={() => onSeek(c.start)} title={c.quote ?? undefined}>
            {formatDuration(c.start)}
          </button>
        </li>
      ))}
    </ul>
  )
}

function Meter({ label, value }: { label: string; value: number }) {
  const clamped = Math.max(0, Math.min(1, value))
  return (
    <div className="ag-meter">
      <span className="ag-meter-label">{label}</span>
      <div className="ag-meter-track">
        <div className="ag-meter-fill" style={{ width: `${clamped * 100}%` }} />
      </div>
      <span className="ag-meter-val">{Math.round(clamped * 100)}%</span>
    </div>
  )
}

/**
 * A signed score on a −1..1 scale, drawn from the centre out.
 *
 * Sentiment cannot use {@link Meter}: a 0..1 bar filled from the left renders
 * −0.8 and +0.8 identically once clamped, which inverts the only thing the
 * reading says.
 */
function SignedScore({ value }: { value: number }) {
  const clamped = Math.max(-1, Math.min(1, value))
  const tone = clamped <= -0.2 ? 'neg' : clamped >= 0.2 ? 'pos' : 'neutral'
  const width = `${(Math.abs(clamped) / 2) * 100}%`
  return (
    <div className={`ag-signed ag-signed-${tone}`}>
      <strong>
        {clamped > 0 ? '+' : clamped < 0 ? '−' : ''}
        {Math.abs(clamped).toFixed(2)}
      </strong>
      <div className="ag-signed-track">
        <span className="ag-signed-zero" />
        <span
          className="ag-signed-fill"
          style={clamped < 0 ? { right: '50%', width } : { left: '50%', width }}
        />
      </div>
      <span className="ag-signed-word">
        {tone === 'neg' ? 'negative' : tone === 'pos' ? 'positive' : 'neutral'}
      </span>
    </div>
  )
}

function SeverityChip({ severity }: { severity: string }) {
  const known = severityRank(severity) < SEVERITIES.length
  return (
    <span className={`ag-sev ag-sev-${known ? severity : 'unknown'}`}>
      {known ? severity : 'unrated'}
    </span>
  )
}

/**
 * The sentiment trajectory as a line the reader can click into.
 *
 * Plotted against each point's own timestamp rather than its index, so a call
 * that sours in the last thirty seconds looks like that and not like a steady
 * decline. Points carry validated citations, so every dot is a seek target.
 */
function Trajectory({
  points,
  onSeek,
}: {
  points: { t: number; score: number; label: string; speaker: string }[]
  onSeek: (s: number) => void
}) {
  if (points.length === 0) return null

  const first = points[0].t
  const last = points[points.length - 1].t
  const span = last - first
  // A single evidenced moment is a normal outcome — the agent drops any point it
  // cannot tie to a real segment. Centre it rather than pinning it to the left
  // edge, where it would read as the start of a line that is missing.
  const x = (t: number) => (span > 0 ? ((t - first) / span) * 100 : 50)
  // Score −1..1 onto a 0..100 box, top-down: +1 at the top.
  const y = (score: number) => ((1 - Math.max(-1, Math.min(1, score))) / 2) * 100

  const line = points.map((p) => `${x(p.t)},${y(p.score)}`).join(' ')

  return (
    <div className="ag-traj">
      <svg
        className="ag-traj-plot"
        viewBox="0 0 100 100"
        preserveAspectRatio="none"
        role="img"
        aria-label={`Sentiment across ${points.length} moment${
          points.length === 1 ? '' : 's'
        }, from ${formatDuration(first)} to ${formatDuration(last)}`}
      >
        <line className="ag-traj-zero" x1="0" y1="50" x2="100" y2="50" />
        {points.length > 1 && <polyline className="ag-traj-line" points={line} />}
        {/* Zero-length round-capped strokes, not circles. The viewBox is
            stretched to the panel width, so a <circle> renders as a wide
            ellipse; a stroke marked non-scaling stays round at any width. */}
        {points.map((p, i) => (
          <line
            key={i}
            className={`ag-traj-dot ag-traj-${p.score < -0.2 ? 'neg' : p.score > 0.2 ? 'pos' : 'neutral'}`}
            x1={x(p.t)}
            y1={y(p.score)}
            x2={x(p.t)}
            y2={y(p.score)}
          />
        ))}
      </svg>
      <ul className="ag-traj-moments">
        {points.map((p, i) => (
          <li key={i}>
            <button type="button" onClick={() => onSeek(p.t)}>
              <span className="ag-traj-at">{formatDuration(p.t)}</span>
              <span className="ag-traj-label">{p.label || '—'}</span>
              <span className="ag-traj-score">
                {p.score > 0 ? '+' : p.score < 0 ? '−' : ''}
                {Math.abs(p.score).toFixed(2)}
              </span>
              <span className="ag-traj-who">{p.speaker}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** Payload renderers, keyed by agent id. */
function Payload({
  agent,
  payload,
  onSeek,
}: {
  agent: string
  payload: unknown
  onSeek: (s: number) => void
}) {
  const p = payload as Record<string, unknown>

  switch (agent) {
    case 'executive_summary': {
      return (
        <div className="ag-body">
          <p className="ag-lede">{String(p.summary ?? '')}</p>
          <dl className="ag-pairs">
            <div>
              <dt>Primary issue</dt>
              <dd>{String(p.primaryIssue ?? '—')}</dd>
            </div>
            <div>
              <dt>Resolution</dt>
              <dd>{String(p.resolution ?? '—')}</dd>
            </div>
            <div>
              <dt>Outcome</dt>
              <dd className={`ag-outcome ag-outcome-${String(p.customerOutcome ?? 'neutral')}`}>
                {String(p.customerOutcome ?? '—')}
              </dd>
            </div>
            <div>
              <dt>Risk</dt>
              <dd>{String(p.risk ?? '—')}</dd>
            </div>
          </dl>
          <CitationList citations={cites(p.citations)} onSeek={onSeek} />
        </div>
      )
    }

    case 'customer_experience': {
      return (
        <div className="ag-body">
          <div className="ag-meters">
            <Meter label="Effort" value={Number(p.effort ?? 0)} />
            <Meter label="Frustration" value={Number(p.frustration ?? 0)} />
            <Meter label="Resolution" value={Number(p.resolutionConfidence ?? 0)} />
            <Meter label="Escalation risk" value={Number(p.escalationRisk ?? 0)} />
            <Meter label="Churn risk" value={Number(p.churnRisk ?? 0)} />
          </div>
          <p className="ag-note">{String(p.summary ?? '')}</p>
          <CitationList citations={cites(p.citations)} onSeek={onSeek} />
        </div>
      )
    }

    case 'coaching': {
      const strengths = rows(p.strengths)
      const improvements = rows(p.improvements)
      return (
        <div className="ag-body">
          {p.summary ? <p className="ag-note">{String(p.summary)}</p> : null}
          {strengths.length > 0 && (
            <>
              <h5 className="ag-sub">Worth repeating</h5>
              {strengths.map((n, i) => (
                <div key={i} className="ag-note-card ag-good">
                  <strong>{String(n.behaviour ?? '')}</strong>
                  <p>{String(n.observation ?? '')}</p>
                  <CitationList citations={cites(n.citations)} onSeek={onSeek} />
                </div>
              ))}
            </>
          )}
          {improvements.length > 0 && (
            <>
              <h5 className="ag-sub">To work on</h5>
              {improvements.map((n, i) => (
                <div key={i} className="ag-note-card">
                  <strong>{String(n.behaviour ?? '')}</strong>
                  <p>{String(n.observation ?? '')}</p>
                  <p className="ag-rec">{String(n.recommendation ?? '')}</p>
                  {n.suggestedPhrase ? (
                    <blockquote className="ag-phrase">“{String(n.suggestedPhrase)}”</blockquote>
                  ) : null}
                  <CitationList citations={cites(n.citations)} onSeek={onSeek} />
                </div>
              ))}
            </>
          )}
        </div>
      )
    }

    case 'sentiment': {
      const trajectory = rows(p.trajectory).map((pt) => ({
        t: Number(pt.t ?? 0),
        score: Number(pt.score ?? 0),
        label: String(pt.label ?? ''),
        speaker: String(pt.speaker ?? 'overall'),
      }))
      return (
        <div className="ag-body">
          <SignedScore value={Number(p.overall ?? 0)} />
          {p.summary ? <p className="ag-note">{String(p.summary)}</p> : null}
          {trajectory.length > 0 ? (
            <Trajectory points={trajectory} onSeek={onSeek} />
          ) : (
            /* The agent drops trajectory points it could not tie to a real
               segment, so an overall score with no moments is a normal
               outcome — and saying so beats an empty chart frame. */
            <p className="ag-note">
              No moment on this call could be tied to a cited segment, so there is
              no trajectory to plot — only the whole-call score above.
            </p>
          )}
        </div>
      )
    }

    case 'compliance': {
      const findings = rows(p.findings).sort(
        (a, b) => severityRank(a.severity) - severityRank(b.severity),
      )
      const passed = p.passed === true
      return (
        <div className="ag-body">
          <div className="ag-verdict">
            <span className={`ag-verdict-badge ag-verdict-${passed ? 'pass' : 'fail'}`}>
              {passed ? 'pass' : 'fail'}
            </span>
            <span className="ag-note">
              {findings.length === 0
                ? 'Nothing to report against this call.'
                : `${findings.length} finding${findings.length === 1 ? '' : 's'}`}
            </span>
          </div>
          {p.summary ? <p className="ag-note">{String(p.summary)}</p> : null}
          {findings.map((f, i) => (
            <div
              key={String(f.id ?? i)}
              className={`ag-note-card${
                severityRank(f.severity) <= severityRank('high') ? ' ag-bad' : ''
              }`}
            >
              <div className="ag-finding-head">
                <strong>{String(f.title ?? f.code ?? 'Finding')}</strong>
                <SeverityChip severity={String(f.severity ?? '')} />
              </div>
              {f.code ? <code className="ag-code">{String(f.code)}</code> : null}
              <p>{String(f.detail ?? '')}</p>
              <CitationList citations={cites(f.citations)} onSeek={onSeek} />
            </div>
          ))}
        </div>
      )
    }

    case 'root_cause': {
      const factors = Array.isArray(p.contributingFactors)
        ? (p.contributingFactors as unknown[]).map(String).filter(Boolean)
        : []
      const category = String(p.category ?? 'other')
      return (
        <div className="ag-body">
          <div className="ag-cause-head">
            <span className={`ag-cat ag-cat-${category}`}>{category}</span>
            {/* Confidence is the model's own, so it is labelled as a claim
                rather than shown as a bare percentage that reads as measured. */}
            <span className="ag-note">
              model confidence {Math.round(Number(p.confidence ?? 0) * 100)}%
            </span>
          </div>
          <p className="ag-lede">{String(p.primaryCause ?? '—')}</p>
          {factors.length > 0 && (
            <>
              <h5 className="ag-sub">Contributing factors</h5>
              <ul className="ag-list">
                {factors.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            </>
          )}
          {p.recommendedFix ? (
            <div className="ag-fix">
              <h5 className="ag-sub">Recommended fix</h5>
              <p>{String(p.recommendedFix)}</p>
            </div>
          ) : null}
          {p.summary ? <p className="ag-note">{String(p.summary)}</p> : null}
          <CitationList citations={cites(p.citations)} onSeek={onSeek} />
        </div>
      )
    }

    case 'action_items': {
      const items = rows(p.items)
      return (
        <div className="ag-body">
          {p.summary ? <p className="ag-note">{String(p.summary)}</p> : null}
          {items.length === 0 ? (
            <p className="ag-note">Nothing is owed after this call.</p>
          ) : (
            <ol className="ag-actions">
              {items.map((it, i) => {
                const priority = String(it.priority ?? 'medium')
                return (
                  <li key={i} className={`ag-action ag-prio-${priority}`}>
                    <div className="ag-action-head">
                      <strong>{String(it.title ?? '')}</strong>
                      <span className={`ag-prio ag-prio-chip-${priority}`}>{priority}</span>
                    </div>
                    {it.detail ? <p>{String(it.detail)}</p> : null}
                    <div className="ag-action-meta">
                      <span className="ag-owner">{String(it.owner ?? 'agent')}</span>
                      {/* Only when the conversation actually gave one — an
                          invented due date is worse than none. */}
                      {it.due ? <span className="ag-due">due {String(it.due)}</span> : null}
                    </div>
                    <CitationList citations={cites(it.citations)} onSeek={onSeek} />
                  </li>
                )
              })}
            </ol>
          )}
        </div>
      )
    }

    case 'qa_scorecard': {
      const criteria = rows(p.criteria)
      return (
        <div className="ag-body">
          <div className="ag-score">
            <strong>{Math.round(Number(p.overall ?? 0))}</strong>
            <span>/100</span>
          </div>
          {p.summary ? <p className="ag-note">{String(p.summary)}</p> : null}
          {criteria.map((c, i) => (
            <div key={i} className="ag-criterion">
              <div className="ag-criterion-head">
                <span>{String(c.label ?? '')}</span>
                <span className="ag-criterion-score">
                  {c.notApplicable ? 'n/a' : `${Math.round(Number(c.score ?? 0) * 100)}%`}
                </span>
              </div>
              <p className="ag-note">{String(c.reasoning ?? '')}</p>
              <CitationList citations={cites(c.citations)} onSeek={onSeek} />
            </div>
          ))}
        </div>
      )
    }

    default: {
      // An agent this build does not have a renderer for still shows what it
      // produced. A blank panel would read as "found nothing".
      return (
        <div className="ag-body">
          <pre className="ag-raw">{JSON.stringify(payload, null, 2)}</pre>
        </div>
      )
    }
  }
}

export function AgentResults({ callId, onSeek, refreshToken = 0 }: AgentResultsProps) {
  const [runs, setRuns] = useState<AgentRun[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    getAgentRuns(callId)
      .then((r) => {
        if (!cancelled) {
          setRuns(r)
          setError(null)
        }
      })
      .catch((e: unknown) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false))
    return () => {
      cancelled = true
    }
  }, [callId, refreshToken])

  if (error) return <p className="ag-error">Could not load analysis: {error}</p>
  if (loading) return <p className="ag-empty">Loading analysis…</p>
  if (runs.length === 0) {
    return (
      <p className="ag-empty">
        No agent has run on this call yet. Configure a model in Settings, then use
        “Transcribe and analyse this call”.
      </p>
    )
  }

  return (
    <div className="ag-root">
      {runs.map((run) => (
        <article key={run.agent} className={`ag-card${run.error ? ' ag-failed' : ''}`}>
          <header className="ag-head">
            <h4>{run.label}</h4>
            {run.error ? <span className="ag-chip ag-chip-failed">failed</span> : null}
          </header>

          {run.error ? (
            <p className="ag-errline">{run.error}</p>
          ) : (
            <Payload agent={run.agent} payload={run.payload} onSeek={onSeek} />
          )}

          {/* Accounting sits with the output it produced, not on a separate
              screen — that is what makes a substituted model noticeable. */}
          <footer className="ag-meta">
            <span>{run.model}</span>
            <span>·</span>
            <span>{run.provider}</span>
            <span>·</span>
            <span>
              {run.inputTokens.toLocaleString()} in / {run.outputTokens.toLocaleString()} out
            </span>
            <span>·</span>
            <span>{(run.latencyMs / 1000).toFixed(1)}s</span>
            {run.costUsd != null && (
              <>
                <span>·</span>
                <span>${run.costUsd.toFixed(4)}</span>
              </>
            )}
          </footer>
        </article>
      ))}
    </div>
  )
}

export default AgentResults
