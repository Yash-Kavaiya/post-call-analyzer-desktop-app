/**
 * Sending this machine's metrics to a collector, and reading what it holds.
 *
 * The token goes to the OS credential store and never comes back — the screen
 * is told only whether one exists, the same rule the provider keys follow. That
 * is also why there is no browser dashboard: a page would need the token in a
 * URL or a cookie, and a token in a URL is a token in every proxy log. Here it
 * can stay in Credential Manager where it belongs.
 *
 * The numbers are deliberately blunt about what they cover. A collector holding
 * a thousand calls nobody has scored must not report a mean of 0, which reads
 * identically to a genuinely terrible team.
 */

import { useEffect, useState } from 'react'
import {
  getSyncReport,
  getSyncSettings,
  runSync,
  saveSyncSettings,
  type Rollup,
  type SyncOutcome,
  type SyncSettings,
} from '../../api'
import './CollectorPanel.css'

/** A measure, or a dash. Never a zero standing in for "not measured". */
function num(v: number | null | undefined, digits = 1, suffix = ''): string {
  return v == null ? '—' : `${v.toFixed(digits)}${suffix}`
}

export function CollectorPanel() {
  const [settings, setSettings] = useState<SyncSettings | null>(null)
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState<'save' | 'sync' | 'report' | null>(null)
  const [outcome, setOutcome] = useState<SyncOutcome | null>(null)
  const [report, setReport] = useState<Rollup | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = () => {
    getSyncSettings()
      .then((s) => {
        setSettings(s)
        setUrl(s.url ?? '')
      })
      .catch((e: unknown) => setError(String(e)))
  }

  useEffect(load, [])

  const save = () => {
    setBusy('save')
    setError(null)
    // `undefined` leaves the stored token alone; re-saving a URL must not
    // silently wipe a working token.
    saveSyncSettings(url.trim() || null, token.trim() === '' ? undefined : token.trim())
      .then((s) => {
        setSettings(s)
        setToken('')
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(null))
  }

  const push = () => {
    setBusy('sync')
    setError(null)
    runSync()
      .then(setOutcome)
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(null))
  }

  const fetchReport = () => {
    setBusy('report')
    setError(null)
    getSyncReport()
      .then(setReport)
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(null))
  }

  const ready = Boolean(settings?.url && settings.hasToken)

  return (
    <section className="cp-root">
      <h3>Collector</h3>
      <p className="cp-intro">
        Send this machine&rsquo;s scores and finding codes to a shared collector. Transcripts,
        names and numbers never leave — only metrics and opaque participant ids.
      </p>

      <div className="cp-row">
        <label className="cp-field cp-wide">
          <span>Collector URL</span>
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="http://collector:8480"
          />
        </label>
        <label className="cp-field">
          <span>Token {settings?.hasToken && <em className="cp-set">saved</em>}</span>
          <input
            type="password"
            value={token}
            onChange={(e) => setToken(e.target.value)}
            placeholder={settings?.hasToken ? 'leave blank to keep' : 'paste the token'}
          />
        </label>
        <button type="button" onClick={save} disabled={busy !== null}>
          {busy === 'save' ? 'Saving…' : 'Save'}
        </button>
      </div>

      <p className="cp-note">
        The token is stored in Windows Credential Manager and never shown again — this
        panel only knows whether one exists.
      </p>

      <div className="cp-actions">
        <button type="button" onClick={push} disabled={!ready || busy !== null}>
          {busy === 'sync' ? 'Sending…' : 'Send metrics'}
        </button>
        <button
          type="button"
          className="cp-ghost"
          onClick={fetchReport}
          disabled={!ready || busy !== null}
        >
          {busy === 'report' ? 'Loading…' : 'Read the collector'}
        </button>
      </div>

      {outcome && (
        <p className="cp-ok">
          Sent {outcome.pushed}
          {outcome.withheld > 0 && (
            <> · {outcome.withheld} withheld for carrying call content</>
          )}
          {outcome.rejected > 0 && <> · {outcome.rejected} rejected by the collector</>}
        </p>
      )}

      {report && (
        <div className="cp-report">
          <div className="cp-stats">
            <div className="cp-stat">
              <span className="cp-stat-value">{report.calls}</span>
              <span className="cp-stat-label">calls held</span>
            </div>
            <div className="cp-stat">
              {/* The count the mean covers, beside the mean. A mean over 7 of
                  1,972 is a different claim from a mean over all of them. */}
              <span className="cp-stat-value">{report.scoredCalls}</span>
              <span className="cp-stat-label">carry a QA score</span>
            </div>
            <div className="cp-stat">
              <span className="cp-stat-value">{num(report.meanQa, 1, '%')}</span>
              <span className="cp-stat-label">mean QA</span>
            </div>
            <div className="cp-stat">
              <span className="cp-stat-value">{num(report.meanSentiment, 2)}</span>
              <span className="cp-stat-label">mean sentiment</span>
            </div>
          </div>

          {report.agents.length > 0 && (
            <>
              <h4 className="cp-sub">Agents</h4>
              <table className="cp-table">
                <thead>
                  <tr>
                    <th>Agent</th>
                    <th>Calls</th>
                    <th>Mean QA</th>
                    <th>Failing</th>
                  </tr>
                </thead>
                <tbody>
                  {report.agents.slice(0, 10).map((a) => (
                    <tr key={a.agentId}>
                      <td className="cp-id" title={a.agentId}>
                        {a.agentId}
                      </td>
                      <td>{a.calls}</td>
                      <td>{num(a.meanQa)}</td>
                      <td>{a.failingCalls}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <p className="cp-note">
                Ids, not names. The collector never receives one, which is the point.
              </p>
            </>
          )}

          {report.codes.length > 0 && (
            <>
              <h4 className="cp-sub">Recurring findings</h4>
              <table className="cp-table">
                <thead>
                  <tr>
                    <th>Code</th>
                    <th>Severity</th>
                    <th>Calls</th>
                  </tr>
                </thead>
                <tbody>
                  {report.codes.slice(0, 10).map((c) => (
                    <tr key={`${c.code}-${c.severity}`}>
                      <td>{c.code}</td>
                      <td>
                        <span className={`cp-sev cp-sev-${c.severity}`}>{c.severity}</span>
                      </td>
                      <td>{c.calls}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          )}
        </div>
      )}

      {error && <p className="cp-error">{error}</p>}
    </section>
  )
}
