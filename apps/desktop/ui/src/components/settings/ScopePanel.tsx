/**
 * Who this machine is signed in as, and what that narrows the app to.
 *
 * The honesty note is not decoration. While everything runs against one local
 * SQLite file, a role here changes what the app *shows* — it cannot stop
 * anyone who can open that file from reading all of it. Presenting a view
 * filter as access control is how people end up trusting it with data it was
 * never protecting.
 */

import { useEffect, useState } from 'react'
import { getViewScope, listTeams, setOperator, type TeamSummary, type ViewScopeInfo } from '../../api'
import './ScopePanel.css'

const SEES: Record<string, string> = {
  agent: 'only calls they handled',
  supervisor: 'every call handled by their team',
  admin: 'every call',
}

export function ScopePanel() {
  const [scope, setScope] = useState<ViewScopeInfo | null>(null)
  const [teams, setTeams] = useState<TeamSummary[]>([])
  const [handle, setHandle] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = () => {
    getViewScope()
      .then((s) => {
        setScope(s)
        setHandle(s.handle ?? '')
      })
      .catch((e: unknown) => setError(String(e)))
    listTeams()
      .then(setTeams)
      .catch(() => setTeams([]))
  }

  useEffect(load, [])

  const save = (next: string | null) => {
    setBusy(true)
    setError(null)
    setOperator(next)
      .then(load)
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(false))
  }

  return (
    <section className="sc-root">
      <h3>Who is using this machine</h3>
      <p className="sc-intro">
        Setting an agent handle narrows the app to what that person is allowed to see.
        Leave it blank to see the whole corpus.
      </p>

      <div className="sc-row">
        <label className="sc-field">
          <span>Agent handle</span>
          <input
            value={handle}
            onChange={(e) => setHandle(e.target.value)}
            placeholder="nobody — showing everything"
          />
        </label>
        <button type="button" onClick={() => save(handle.trim() || null)} disabled={busy}>
          {busy ? 'Saving…' : 'Save'}
        </button>
        {scope?.handle && (
          <button type="button" className="sc-ghost" onClick={() => save(null)} disabled={busy}>
            Clear
          </button>
        )}
      </div>

      {scope && (
        <p className="sc-state">
          <span className={`sc-role sc-role-${scope.role}`}>{scope.role}</span>
          {' — sees '}
          {SEES[scope.role] ?? 'an unknown set of calls'}
          {scope.team && (
            <>
              {' ('}
              <strong>{scope.team}</strong>
              {')'}
            </>
          )}
          {scope.handle && !scope.unrestricted && scope.role === 'agent' && (
            <> · a handle that matches nobody shows an empty corpus rather than the whole one</>
          )}
        </p>
      )}

      {teams.length > 0 && (
        <p className="sc-teams">
          Teams: {teams.map((t) => `${t.name} (${t.members})`).join(' · ')}
        </p>
      )}

      <p className="sc-warn">
        <strong>This is a view filter, not access control.</strong> Every role here reads
        the same local database file, so anyone who can open that file can read every
        call regardless of what this is set to. Enforcement that actually holds needs the
        sync server, where the scope is applied before data leaves the machine.
      </p>

      {error && <p className="sc-error">{error}</p>}
    </section>
  )
}
