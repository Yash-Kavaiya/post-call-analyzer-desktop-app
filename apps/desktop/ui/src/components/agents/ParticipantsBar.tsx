/**
 * Who was on this call, and where it sits in the contact's history.
 *
 * The repeat marker is the point of the thing. "5th call from this contact in
 * 30 days" is the sentence a supervisor acts on, and until identity existed the
 * product could not form it at all.
 *
 * Editing either side sets provenance to `manual`, which is what stops the next
 * backfill from overwriting the correction.
 */

import { useEffect, useState } from 'react'
import { assignCallPerson, getCallPeople, type CallPeople } from '../../api'
import './ParticipantsBar.css'

export interface ParticipantsBarProps {
  callId: string
  onOpenPerson?: (personId: string) => void
}

/** Where the attribution came from, in words a user can act on. */
const ORIGIN: Record<CallPeople['attribution'], string> = {
  none: 'not attributed',
  rule: 'from the filename',
  manual: 'set by hand',
  live: 'from the live session',
}

export function ParticipantsBar({ callId, onOpenPerson }: ParticipantsBarProps) {
  const [people, setPeople] = useState<CallPeople | null>(null)
  const [editing, setEditing] = useState(false)
  const [agent, setAgent] = useState('')
  const [contact, setContact] = useState('')
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const load = () => {
    getCallPeople(callId)
      .then((p) => {
        setPeople(p)
        setAgent(p.agentName ?? '')
        setContact(p.contactName ?? '')
      })
      .catch((e: unknown) => setError(String(e)))
  }

  useEffect(load, [callId])

  if (error) return <p className="pb-error">Could not load participants: {error}</p>
  if (!people) return null

  const save = () => {
    setSaving(true)
    assignCallPerson(callId, agent.trim() || null, contact.trim() || null)
      .then(() => {
        setEditing(false)
        load()
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setSaving(false))
  }

  const repeat = people.sequence != null && people.sequence > 1

  return (
    <section className="pb-root">
      {editing ? (
        <div className="pb-edit">
          <label className="pb-field">
            <span>Agent</span>
            <input value={agent} onChange={(e) => setAgent(e.target.value)} placeholder="unassigned" />
          </label>
          <label className="pb-field">
            <span>Contact</span>
            <input
              value={contact}
              onChange={(e) => setContact(e.target.value)}
              placeholder="unassigned"
            />
          </label>
          <button type="button" onClick={save} disabled={saving}>
            {saving ? 'Saving…' : 'Save'}
          </button>
          <button
            type="button"
            className="pb-ghost"
            onClick={() => {
              setEditing(false)
              setAgent(people.agentName ?? '')
              setContact(people.contactName ?? '')
            }}
          >
            Cancel
          </button>
        </div>
      ) : (
        <div className="pb-view">
          <span className="pb-slot">
            <span className="pb-label">Agent</span>
            {people.agentId && onOpenPerson ? (
              <button type="button" className="pb-link" onClick={() => onOpenPerson(people.agentId!)}>
                {people.agentName}
              </button>
            ) : (
              <span className="pb-none">{people.agentName ?? 'unassigned'}</span>
            )}
          </span>

          <span className="pb-slot">
            <span className="pb-label">Contact</span>
            {people.contactId && onOpenPerson ? (
              <button
                type="button"
                className="pb-link"
                onClick={() => onOpenPerson(people.contactId!)}
              >
                {people.contactName}
              </button>
            ) : (
              <span className="pb-none">{people.contactName ?? 'unassigned'}</span>
            )}
          </span>

          {repeat && (
            <span className="pb-repeat" title="Position in this contact's history, oldest first">
              call #{people.sequence} of {people.contactCalls} from this contact
            </span>
          )}
          {people.sequence === 1 && <span className="pb-first">first contact</span>}

          <span className="pb-origin">{ORIGIN[people.attribution]}</span>
          <button type="button" className="pb-ghost" onClick={() => setEditing(true)}>
            Edit
          </button>
        </div>
      )}
    </section>
  )
}
