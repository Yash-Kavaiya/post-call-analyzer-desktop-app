/**
 * Custom agents declared as TOML, and where they are read from.
 *
 * A plugin is data, never code — this panel lists what loaded and what did
 * not. Running one is metered spend, so it lives on the call, not here: a
 * "run all plugins" on Settings would quietly bill every transcribed call.
 */

import { useEffect, useState } from 'react'
import { listPlugins, type PluginsSnapshot } from '../../api'
import './PluginPanel.css'

export function PluginPanel() {
  const [snap, setSnap] = useState<PluginsSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    listPlugins()
      .then(setSnap)
      .catch((e: unknown) => setError(String(e)))
  }, [])

  const usable = snap?.plugins.filter((p) => p.name && !p.error) ?? []
  const broken = snap?.plugins.filter((p) => p.error) ?? []

  return (
    <section className="pp-root">
      <h3>Custom agents</h3>
      <p className="pp-intro">
        Drop a <code>.toml</code> file in the plugins folder. Each file is a question asked of
        the already-redacted transcript — nothing in one executes on this machine. Run them
        from a call, not from here: folding them into every batch would mean adding a file
        silently increased the cost of scoring.
      </p>

      {snap && (
        <p className="pp-dir">
          Folder: <code>{snap.dir}</code>
        </p>
      )}

      {usable.length > 0 && (
        <ul className="pp-list">
          {usable.map((p) => (
            <li key={p.path}>
              <span className="pp-name">{p.title ?? p.name}</span>
              <span className="pp-meta">
                {p.name}
                {p.findings ? ' · files findings' : ''}
              </span>
            </li>
          ))}
        </ul>
      )}

      {snap && usable.length === 0 && broken.length === 0 && (
        <p className="pp-empty">No plugins loaded. Two examples ship in the repo under examples/plugins.</p>
      )}

      {broken.length > 0 && (
        <ul className="pp-broken">
          {broken.map((p) => (
            <li key={p.path}>
              <span className="pp-name">{p.path.split(/[\\/]/).pop()}</span>
              <span className="pp-err">{p.error}</span>
            </li>
          ))}
        </ul>
      )}

      {error && <p className="pp-error">{error}</p>}
    </section>
  )
}
