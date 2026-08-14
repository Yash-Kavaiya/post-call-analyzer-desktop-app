/**
 * Who is on the calls.
 *
 * Two lists in one screen because they answer different questions from the
 * same table: agents roll up into scorecards, contacts into relationship
 * history. Search hits both the display name and the normalised handle, so a
 * saved contact is findable by the number it was saved under.
 *
 * Every score column can be absent, and absent renders as a dash. With 7 of
 * 1,972 calls analysed, a mean drawn as 0 would be a fabricated number sitting
 * in the same column as real ones.
 */

import { useEffect, useState } from 'react'
import { attributeCalls, day, listPeople, type PersonSummary } from '../api'
import './PeopleScreen.css'

export interface PeopleScreenProps {
  onOpenPerson: (personId: string) => void
}

type Filter = 'all' | 'agent' | 'contact'

const FILTERS: { id: Filter; label: string }[] = [
  { id: 'all', label: 'Everyone' },
  { id: 'agent', label: 'Agents' },
  { id: 'contact', label: 'Contacts' },
]

export function PeopleScreen({ onOpenPerson }: PeopleScreenProps) {
  const [people, setPeople] = useState<PersonSummary[]>([])
  const [filter, setFilter] = useState<Filter>('all')
  const [search, setSearch] = useState('')
  const [debounced, setDebounced] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [note, setNote] = useState<string | null>(null)

  useEffect(() => {
    const t = setTimeout(() => setDebounced(search), 250)
    return () => clearTimeout(t)
  }, [search])

  const load = () => {
    setLoading(true)
    listPeople(filter === 'all' ? undefined : filter, debounced || undefined)
      .then((rows) => {
        setPeople(rows)
        setError(null)
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setLoading(false))
  }

  useEffect(load, [filter, debounced])

  const reattribute = () => {
    setBusy(true)
    setNote(null)
    attributeCalls(false, false)
      .then((r) => {
        setNote(
          `${r.attributed} of ${r.scanned} attributed · ` +
            `${r.skippedManual} kept as set by hand · ${r.unmatched} matched no pattern`,
        )
        load()
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(false))
  }

  return (
    <div className="people-root">
      <header className="people-header">
        <h2>People</h2>
        <div className="people-filters" role="group" aria-label="Filter by kind">
          {FILTERS.map((f) => (
            <button
              key={f.id}
              type="button"
              className={`people-filter${filter === f.id ? ' people-filter-on' : ''}`}
              onClick={() => setFilter(f.id)}
            >
              {f.label}
            </button>
          ))}
        </div>
        <input
          type="search"
          className="people-search"
          placeholder="Search name or number…"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <button
          type="button"
          className="people-action"
          onClick={reattribute}
          disabled={busy}
          title="Re-derive participants from every call's filename"
        >
          {busy ? 'Working…' : 'Re-run attribution'}
        </button>
      </header>

      {note && <p className="people-note">{note}</p>}
      {error && <p className="people-error">Could not load people: {error}</p>}

      {loading && people.length === 0 ? (
        <p className="people-state">Loading…</p>
      ) : people.length === 0 ? (
        <p className="people-state">
          {debounced
            ? `Nobody matches “${debounced}”.`
            : 'Nobody yet — run attribution to derive participants from filenames.'}
        </p>
      ) : (
        <>
          <table className="people-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Kind</th>
                <th>Role</th>
                <th>Team</th>
                <th>Calls</th>
                <th>First call</th>
                <th>Last call</th>
              </tr>
            </thead>
            <tbody>
              {people.map((p) => (
                <tr
                  key={p.id}
                  onClick={() => onOpenPerson(p.id)}
                  tabIndex={0}
                  onKeyDown={(e) => e.key === 'Enter' && onOpenPerson(p.id)}
                >
                  <td className="people-name" title={p.handle}>
                    {p.displayName}
                  </td>
                  <td>
                    <span className={`people-kind people-kind-${p.kind}`}>{p.kind}</span>
                  </td>
                  <td>
                    {/* A contact has no role. Showing `agent` there would be a
                        meaningless value wearing the look of a real one. */}
                    {p.kind === 'agent' ? (
                      <span className={`people-role people-role-${p.role}`}>{p.role}</span>
                    ) : (
                      <span className="people-none">&mdash;</span>
                    )}
                  </td>
                  <td>{p.team ?? <span className="people-none">&mdash;</span>}</td>
                  <td className="people-num">{p.calls}</td>
                  <td className="people-num">{day(p.firstCall)}</td>
                  <td className="people-num">{day(p.lastCall)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="people-count">{people.length} people</p>
        </>
      )}
    </div>
  )
}
