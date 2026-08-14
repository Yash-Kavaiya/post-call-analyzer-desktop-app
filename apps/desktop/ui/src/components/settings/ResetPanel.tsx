/**
 * Clearing the corpus.
 *
 * Three scopes, because "start again" usually means one of three different
 * things, and the widest one throws away hours of transcription to fix a
 * scoring problem.
 *
 * The confirmation asks the user to type the scope name. A second "are you
 * sure?" button is a reflex to click; typing `all` is not. The dialog also
 * shows real counts pulled from the database rather than generic wording,
 * because "1,972 calls" is what actually stops a mistake.
 */

import { useEffect, useState } from 'react'
import { previewReset, resetData, type ResetOutcome, type ResetReport } from '../../api'
import './ResetPanel.css'

type Scope = 'analyses' | 'transcripts' | 'all'

const SCOPES: { id: Scope; label: string; detail: string }[] = [
  {
    id: 'analyses',
    label: 'Scores and findings',
    detail:
      'Clears every scorecard, finding and run. Calls and transcripts are kept, so re-scoring costs model calls but no transcription.',
  },
  {
    id: 'transcripts',
    label: 'Transcripts too',
    detail:
      'Also clears transcripts, redaction maps and timelines. Calls stay registered and go back to “discovered”; your audio files are never touched.',
  },
  {
    id: 'all',
    label: 'Everything',
    detail:
      'Empties the corpus — every call, transcript, score, finding and person. Your settings and audio files are kept.',
  },
]

export function ResetPanel() {
  const [scope, setScope] = useState<Scope>('analyses')
  const [preview, setPreview] = useState<ResetReport | null>(null)
  const [confirm, setConfirm] = useState('')
  const [backup, setBackup] = useState(true)
  const [busy, setBusy] = useState(false)
  const [done, setDone] = useState<ResetOutcome | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    setConfirm('')
    setDone(null)
    previewReset(scope)
      .then(setPreview)
      .catch((e: unknown) => setError(String(e)))
  }, [scope])

  const chosen = SCOPES.find((s) => s.id === scope)!
  const armed = confirm.trim().toLowerCase() === scope

  const run = () => {
    setBusy(true)
    setError(null)
    resetData(scope, backup)
      .then((outcome) => {
        setDone(outcome)
        setConfirm('')
        return previewReset(scope).then(setPreview)
      })
      .catch((e: unknown) => setError(String(e)))
      .finally(() => setBusy(false))
  }

  return (
    <section className="rp-root">
      <h3>Reset data</h3>
      <p className="rp-intro">
        Start again from a clean corpus. This cannot be undone from inside the app, so a
        copy of the database is taken first.
      </p>

      <div className="rp-scopes">
        {SCOPES.map((s) => (
          <label key={s.id} className={`rp-scope${scope === s.id ? ' rp-scope-on' : ''}`}>
            <input
              type="radio"
              name="reset-scope"
              checked={scope === s.id}
              onChange={() => setScope(s.id)}
            />
            <span>
              <strong>{s.label}</strong>
              <span className="rp-detail">{s.detail}</span>
            </span>
          </label>
        ))}
      </div>

      {preview && (
        <p className="rp-counts">
          This will delete{' '}
          <strong>
            {preview.calls} calls, {preview.transcripts} transcripts, {preview.analyses}{' '}
            analyses, {preview.findings} findings, {preview.people} people
          </strong>
          .
        </p>
      )}

      <label className="rp-backup">
        <input type="checkbox" checked={backup} onChange={(e) => setBackup(e.target.checked)} />
        <span>
          Copy the database first
          {!backup && <em className="rp-warn"> — without this there is no way back</em>}
        </span>
      </label>

      <div className="rp-confirm">
        <label>
          <span>
            Type <code>{scope}</code> to confirm
          </span>
          <input
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            placeholder={scope}
            aria-label={`Type ${scope} to confirm`}
          />
        </label>
        <button type="button" className="rp-danger" disabled={!armed || busy} onClick={run}>
          {busy ? 'Clearing…' : `Delete ${chosen.label.toLowerCase()}`}
        </button>
      </div>

      {done && (
        <p className="rp-ok">
          Removed {done.report.calls} calls, {done.report.transcripts} transcripts,{' '}
          {done.report.analyses} analyses, {done.report.findings} findings,{' '}
          {done.report.people} people.
          {done.backupPath && (
            <>
              {' '}
              Backup saved to <code>{done.backupPath}</code>.
            </>
          )}
        </p>
      )}
      {error && <p className="rp-error">{error}</p>}
    </section>
  )
}
