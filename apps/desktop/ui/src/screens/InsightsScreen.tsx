/**
 * What the whole corpus shows, and how the agents are behaving.
 *
 * Two things sit on this page because they answer the same question from
 * opposite ends: "can I trust what I'm looking at". The corpus report says what
 * the calls contain; agent health says whether the machinery that produced it
 * is working, what it cost, and whether the model answering has changed.
 *
 * The patterns section is phrased carefully. These are associations found by
 * comparing groups, not causes, and the panel shows both group sizes so the
 * reader can judge the finding rather than take the ratio on trust.
 *
 * Sentiment is shown as a signed mean and a share of unhappy calls, never as a
 * ratio. It runs −1..1, so "2x lower sentiment" would be published for two
 * groups sitting on opposite sides of neutral — which is why `ca-insights`
 * excludes it from the pattern finder in the first place.
 */

import { useEffect, useState } from 'react'
import {
  getAgentHealth,
  getCorpusReport,
  getRiskRegister,
  type AgentHealth,
  type CorpusReport,
  type FindingCount,
} from '../api'
import './InsightsScreen.css'

function pct(v: number | null | undefined): string {
  return v == null ? '—' : `${Math.round(v * 100)}%`
}

/** A signed mean, so a negative corpus does not read as a positive one. */
function signed(v: number | null | undefined): string {
  if (v == null) return '—'
  return `${v > 0 ? '+' : v < 0 ? '−' : ''}${Math.abs(v).toFixed(2)}`
}

function sentimentTone(v: number | null | undefined): string {
  if (v == null) return ''
  return v <= -0.2 ? ' in-bad' : v >= 0.2 ? ' in-good' : ''
}

/**
 * Whether a lift is good news, which is not the same as whether it is up.
 *
 * More escalation risk is bad; a higher QA score is good. Colouring purely by
 * direction painted "calls mentioning 'resolved' score 1.1× higher" in the same
 * red as a risk warning.
 */
function isGood(outcome: string, lift: number): boolean {
  const higherIsBetter = outcome === 'QA score'
  return higherIsBetter ? lift >= 1 : lift < 1
}

export function InsightsScreen() {
  const [report, setReport] = useState<CorpusReport | null>(null)
  const [health, setHealth] = useState<AgentHealth[]>([])
  const [register, setRegister] = useState<FindingCount[]>([])
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    Promise.all([getCorpusReport(), getAgentHealth(), getRiskRegister()])
      .then(([r, h, reg]) => {
        setReport(r)
        setHealth(h)
        setRegister(reg)
      })
      .catch((e: unknown) => setError(String(e)))
  }, [])

  if (error) return <p className="in-error">Could not build the report: {error}</p>
  if (!report) return <p className="in-state">Loading…</p>

  const maxBucket = Math.max(1, ...report.qaDistribution.map((b) => b.calls))

  return (
    <div className="in-root">
      <h2>Insights</h2>

      {report.callsAnalysed === 0 ? (
        <p className="in-state">
          Nothing has been analysed yet, so there is nothing to aggregate. Configure a
          model in Settings and run analysis on some calls first.
        </p>
      ) : (
        <>
          <div className="in-tiles">
            <div className="in-tile">
              <span className="in-tile-label">Calls analysed</span>
              <span className="in-tile-value">{report.callsAnalysed}</span>
            </div>
            <div className="in-tile">
              <span className="in-tile-label">Mean QA</span>
              <span className="in-tile-value">
                {report.meanQa == null ? '—' : Math.round(report.meanQa)}
              </span>
            </div>
            <div className="in-tile">
              <span className="in-tile-label">Mean sentiment</span>
              <span className={`in-tile-value${sentimentTone(report.meanSentiment)}`}>
                {signed(report.meanSentiment)}
              </span>
              {report.negativeShare != null && (
                <span className="in-tile-sub">
                  {pct(report.negativeShare)} of scored calls left unhappy
                </span>
              )}
            </div>
            <div className="in-tile">
              <span className="in-tile-label">Compliance pass rate</span>
              <span
                className={`in-tile-value${
                  report.compliancePassRate != null && report.compliancePassRate < 1
                    ? ' in-bad'
                    : ''
                }`}
              >
                {pct(report.compliancePassRate)}
              </span>
              {report.failingCalls > 0 && (
                <span className="in-tile-sub">{report.failingCalls} calls failing</span>
              )}
            </div>
            <div className="in-tile">
              <span className="in-tile-label">Escalation risk</span>
              <span className="in-tile-value">{pct(report.meanEscalationRisk)}</span>
            </div>
            <div className="in-tile">
              <span className="in-tile-label">Churn risk</span>
              <span className="in-tile-value">{pct(report.meanChurnRisk)}</span>
            </div>
          </div>

          {register.length > 0 && (
            <section className="in-card">
              <h3>Risk register</h3>
              <p className="in-hint">
                Counted from the findings ledger, so it covers every agent that files
                findings and survives an agent being re-run. Each rule is shown at the
                worst severity it has ever been raised at, and blocking rules come
                first — a critical on four calls outranks an advisory on forty.
              </p>
              <ul className="in-register">
                {register.map((f) => (
                  <li key={f.code} className={f.blocking ? 'in-blocking' : undefined}>
                    <span className={`in-sev in-sev-${f.severity}`}>{f.severity}</span>
                    <span className="in-register-title">{f.title || f.code}</span>
                    <code className="in-register-code">{f.code}</code>
                    <span className="in-counted-n">
                      {f.calls} call{f.calls === 1 ? '' : 's'}
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {report.qaDistribution.length > 0 && (
            <section className="in-card">
              <h3>QA score distribution</h3>
              <ul className="in-hist">
                {report.qaDistribution.map((b) => (
                  <li key={b.floor}>
                    <span className="in-hist-label">
                      {b.floor}–{b.floor === 90 ? 100 : b.floor + 9}
                    </span>
                    <div className="in-hist-bar">
                      <div style={{ width: `${(b.calls / maxBucket) * 100}%` }} />
                    </div>
                    <span className="in-hist-count">{b.calls}</span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {report.patterns.length > 0 && (
            <section className="in-card">
              <h3>Patterns</h3>
              <p className="in-hint">
                Associations found by comparing calls that mention a term against those
                that don’t. Both group sizes are shown — these are not causes, and a
                ratio without a denominator is not a finding.
              </p>
              <ul className="in-patterns">
                {report.patterns.map((p, i) => (
                  <li key={`${p.term}-${p.outcome}-${i}`}>
                    <div className="in-pattern-head">
                      <strong>{p.term}</strong>
                      <span
                        className={`in-lift ${
                          isGood(p.outcome, p.lift) ? 'in-lift-good' : 'in-lift-bad'
                        }`}
                      >
                        {p.lift.toFixed(1)}× {p.outcome}
                      </span>
                    </div>
                    <p className="in-pattern-detail">
                      {p.withCalls} calls mentioning it average{' '}
                      {p.outcome === 'QA score'
                        ? Math.round(p.withMean)
                        : pct(p.withMean)}
                      , against{' '}
                      {p.outcome === 'QA score'
                        ? Math.round(p.withoutMean)
                        : pct(p.withoutMean)}{' '}
                      across {p.withoutCalls} that don’t.
                    </p>
                  </li>
                ))}
              </ul>
            </section>
          )}

          <div className="in-columns">
            {report.topTopics.length > 0 && (
              <section className="in-card">
                <h3>Most discussed</h3>
                <ul className="in-counted">
                  {report.topTopics.map((t) => (
                    <li key={t.label}>
                      <span>{t.label}</span>
                      <span className="in-counted-n">
                        {t.calls} · {pct(t.share)}
                      </span>
                    </li>
                  ))}
                </ul>
              </section>
            )}

            {report.rootCauseCategories.length > 0 && (
              <section className="in-card">
                <h3>Root cause by category</h3>
                <p className="in-hint">
                  The five buckets the root-cause agent normalises to. Free-text causes
                  differ on every call; these are what aggregate.
                </p>
                <ul className="in-counted">
                  {report.rootCauseCategories.map((t) => (
                    <li key={t.label}>
                      <span className="in-cat">{t.label}</span>
                      <span className="in-counted-n">
                        {t.calls} · {pct(t.share)}
                      </span>
                    </li>
                  ))}
                </ul>
              </section>
            )}

            {report.topIssues.length > 0 && (
              <section className="in-card">
                <h3>Root causes</h3>
                <ul className="in-counted">
                  {report.topIssues.map((t) => (
                    <li key={t.label}>
                      <span>{t.label}</span>
                      <span className="in-counted-n">{t.calls}</span>
                    </li>
                  ))}
                </ul>
              </section>
            )}

            {report.complianceIssues.length > 0 && (
              <section className="in-card">
                <h3>Compliance</h3>
                <ul className="in-counted">
                  {report.complianceIssues.map((t) => (
                    <li key={t.label}>
                      <span>{t.label}</span>
                      <span className="in-counted-n in-bad">{t.calls}</span>
                    </li>
                  ))}
                </ul>
              </section>
            )}
          </div>
        </>
      )}

      <section className="in-card">
        <h3>Agent health</h3>
        <p className="in-hint">
          What each agent has cost and whether it is working. More than one model
          listed means a router substituted one — scores across them are not
          comparable.
        </p>
        {health.length === 0 ? (
          <p className="in-state">No agent has run yet.</p>
        ) : (
          <div className="in-scroller">
            <table className="in-table">
              <thead>
                <tr>
                  <th scope="col">Agent</th>
                  <th scope="col">Runs</th>
                  <th scope="col">Failed</th>
                  <th scope="col">Mean latency</th>
                  <th scope="col">Spend</th>
                  <th scope="col">Models</th>
                </tr>
              </thead>
              <tbody>
                {health.map((h) => (
                  <tr key={h.agent}>
                    <td>{h.label}</td>
                    <td className="in-num">{h.runs}</td>
                    <td className={`in-num${h.failures > 0 ? ' in-bad' : ''}`}>
                      {h.failures > 0 ? `${h.failures} (${pct(h.failureRate)})` : '0'}
                    </td>
                    <td className="in-num">{(h.meanLatencyMs / 1000).toFixed(1)}s</td>
                    <td className="in-num">${h.costUsd.toFixed(4)}</td>
                    <td className={h.models.length > 1 ? 'in-bad' : undefined}>
                      {h.models.join(', ') || '—'}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  )
}

export default InsightsScreen
