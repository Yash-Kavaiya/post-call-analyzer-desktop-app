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
          <CitationList citations={(p.citations as Citation[]) ?? []} onSeek={onSeek} />
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
          <CitationList citations={(p.citations as Citation[]) ?? []} onSeek={onSeek} />
        </div>
      )
    }

    case 'coaching': {
      const strengths = (p.strengths as Record<string, unknown>[]) ?? []
      const improvements = (p.improvements as Record<string, unknown>[]) ?? []
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
                  <CitationList citations={(n.citations as Citation[]) ?? []} onSeek={onSeek} />
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
                  <CitationList citations={(n.citations as Citation[]) ?? []} onSeek={onSeek} />
                </div>
              ))}
            </>
          )}
        </div>
      )
    }

    case 'qa_scorecard': {
      const criteria = (p.criteria as Record<string, unknown>[]) ?? []
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
              <CitationList citations={(c.citations as Citation[]) ?? []} onSeek={onSeek} />
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
