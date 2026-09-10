/**
 * What custom agents found on this call.
 *
 * Rendered generically, because a plugin declares its own fields: this
 * component cannot know that `within_sla` is a boolean or that `signals` is a
 * list until it sees one. Anything it cannot render as a scalar is shown as
 * formatted JSON rather than as `[object Object]`, which is the usual outcome
 * of a generic renderer meeting a shape it did not expect.
 *
 * Renders nothing when no plugin is installed and none have run. If plugins
 * are loaded, the panel stays visible with a run button — hiding it then
 * would look identical to "this product has no custom agents".
 */

import { useEffect, useState } from 'react'
import {
  getPluginAnalyses,
  listPlugins,
  runPlugins,
  type PluginAnalysis,
  type PluginRunOutcome,
} from '../../api'
import './PluginResults.css'

export interface PluginResultsProps {
  callId: string
  /** Bumped by the parent to force a reload after a re-run. */
  refreshToken?: unknown
}

/** Turn `within_sla` into `Within sla`, which is what a person reads. */
function label(key: string): string {
  const spaced = key.replace(/([a-z0-9])([A-Z])/g, '$1 $2').replace(/[_-]+/g, ' ')
  return spaced.charAt(0).toUpperCase() + spaced.slice(1)
}

function Value({ value }: { value: unknown }) {
  if (value === null || value === undefined) {
    // A plugin field the model left out. Not false, not zero.
    return <span className="pr-none">not reported</span>
  }
  if (typeof value === 'boolean') {
    return <span className={`pr-bool pr-bool-${value}`}>{value ? 'yes' : 'no'}</span>
  }
  if (typeof value === 'number') {
    return <span className="pr-num">{Number.isInteger(value) ? value : value.toFixed(2)}</span>
  }
  if (typeof value === 'string') {
    return <span>{value}</span>
  }
  if (Array.isArray(value) && value.every((v) => typeof v === 'string')) {
    return value.length === 0 ? (
      <span className="pr-none">none</span>
    ) : (
      <span className="pr-list">
        {value.map((v, i) => (
          <span key={i} className="pr-chip">
            {v}
          </span>
        ))}
      </span>
    )
  }
  return <pre className="pr-json">{JSON.stringify(value, null, 2)}</pre>
}

export function PluginResults({ callId, refreshToken }: PluginResultsProps) {
  const [rows, setRows] = useState<PluginAnalysis[]>([])
  const [usable, setUsable] = useState(0)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<PluginRunOutcome | null>(null)
  const [tick, setTick] = useState(0)

  useEffect(() => {
    let cancelled = false
    Promise.all([getPluginAnalyses(callId), listPlugins()])
      .then(([r, snap]) => {
        if (cancelled) return
        setRows(r)
        setUsable(snap.plugins.filter((p) => p.name && !p.error).length)
      })
      .catch((e: unknown) => !cancelled && setError(String(e)))
    return () => {
      cancelled = true
    }
  }, [callId, refreshToken, tick])

  const run = (force: boolean) => {
    setBusy(true)
    setError(null)
    runPlugins(callId, force)
      .then((o) => {
        setOutcome(o)
        setTick((n) => n + 1)
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(false))
  }

  if (error && rows.length === 0 && usable === 0) {
    return <p className="pr-error">Could not load custom agents: {error}</p>
  }
  if (rows.length === 0 && usable === 0) return null

  return (
    <section className="pr-root">
      <header className="pr-head">
        <h3 className="pr-heading">Custom agents</h3>
        {usable > 0 && (
          <button type="button" className="pr-run" onClick={() => run(rows.length > 0)} disabled={busy}>
            {busy ? 'Running…' : rows.length > 0 ? 'Re-run' : 'Run custom agents'}
          </button>
        )}
      </header>
      {outcome && (
        <p className="pr-outcome">
          {outcome.ran} ran
          {outcome.skipped > 0 && <> · {outcome.skipped} already had a result</>}
          {outcome.failed > 0 && <> · {outcome.failed} failed</>}
        </p>
      )}
      {error && <p className="pr-error">{error}</p>}
      {rows.length === 0 && usable > 0 && !busy && (
        <p className="pr-none">No custom agent has run on this call yet.</p>
      )}
      {rows.map((row) => (
        <article key={row.plugin} className="pr-card">
          <header className="pr-card-head">
            <span className="pr-name">{row.plugin}</span>
            <span className="pr-model">
              {row.provider} · {row.model}
            </span>
          </header>

          {row.error ? (
            /* A failed run is recorded rather than absent, so it can say so
               here. Silence would look identical to never having run. */
            <p className="pr-failed">This agent failed: {row.error}</p>
          ) : (
            <dl className="pr-fields">
              {Object.entries(row.payload ?? {}).map(([key, value]) => (
                <div key={key} className="pr-field">
                  <dt>{label(key)}</dt>
                  <dd>
                    <Value value={value} />
                  </dd>
                </div>
              ))}
            </dl>
          )}
        </article>
      ))}
    </section>
  )
}
