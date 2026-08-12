/**
 * Ask questions about this call.
 *
 * The design constraint that shapes everything here: an ungrounded answer about
 * a call is worse than no answer, because it is fluent and specific and nothing
 * on screen distinguishes it from one that was checked. So every answer carries
 * its citations as buttons that seek the audio, and an answer whose citations
 * did not resolve is labelled as unverified rather than shown plain.
 *
 * Suggested questions are provided because a blank chat box gets used once. The
 * ones offered are the questions a supervisor actually asks — why was the
 * customer unhappy, did the agent follow policy, what should they have done.
 */

import { useCallback, useRef, useState } from 'react'
import { askCall, formatDuration, type Citation, type CopilotTurn } from '../../api'
import './CallCopilot.css'

const SUGGESTIONS = [
  'What was the customer actually calling about?',
  'Was the customer frustrated, and where?',
  'Did the agent follow the right process?',
  'What could the agent have done better?',
  'Was anything promised that might not be kept?',
]

interface Exchange {
  question: string
  answer: string
  citations: Citation[]
  grounded: boolean
}

export interface CallCopilotProps {
  callId: string
  onSeek: (seconds: number) => void
}

export function CallCopilot({ callId, onSeek }: CallCopilotProps) {
  const [question, setQuestion] = useState('')
  const [exchanges, setExchanges] = useState<Exchange[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)

  const ask = useCallback(
    async (text: string) => {
      const q = text.trim()
      if (!q || busy) return

      setBusy(true)
      setError(null)
      setQuestion('')

      // History is what the model saw, in the order it saw it — the transcript
      // is re-sent server-side every turn, so this carries only the dialogue.
      const history: CopilotTurn[] = exchanges.flatMap((e) => [
        { role: 'user', content: e.question },
        { role: 'assistant', content: e.answer },
      ])

      try {
        const a = await askCall(callId, q, history)
        setExchanges((prev) => [
          ...prev,
          { question: q, answer: a.answer, citations: a.citations, grounded: a.grounded },
        ])
      } catch (e) {
        setError(String(e))
        // Put the question back so it is not lost to a transient failure.
        setQuestion(q)
      } finally {
        setBusy(false)
        inputRef.current?.focus()
      }
    },
    [busy, callId, exchanges],
  )

  return (
    <div className="cp-root">
      {exchanges.length === 0 && (
        <ul className="cp-suggestions">
          {SUGGESTIONS.map((s) => (
            <li key={s}>
              <button type="button" onClick={() => void ask(s)} disabled={busy}>
                {s}
              </button>
            </li>
          ))}
        </ul>
      )}

      {exchanges.length > 0 && (
        <ol className="cp-thread">
          {exchanges.map((e, i) => (
            <li key={i}>
              <p className="cp-q">{e.question}</p>
              <div className="cp-a">
                <p>{e.answer}</p>
                {e.citations.length > 0 ? (
                  <ul className="cp-cites">
                    {e.citations.map((c, n) => (
                      <li key={`${c.segmentId}-${n}`}>
                        <button
                          type="button"
                          onClick={() => onSeek(c.start)}
                          title={c.quote ?? undefined}
                        >
                          {formatDuration(c.start)}
                        </button>
                      </li>
                    ))}
                  </ul>
                ) : (
                  /* Stated, not hidden. The answer may be right, but no part of
                     it has been checked against the call. */
                  <p className="cp-ungrounded">
                    Nothing in this answer could be traced to the transcript — treat it
                    as unverified.
                  </p>
                )}
              </div>
            </li>
          ))}
        </ol>
      )}

      {busy && <p className="cp-busy">Reading the call…</p>}
      {error && <p className="cp-error">{error}</p>}

      <form
        className="cp-form"
        onSubmit={(ev) => {
          ev.preventDefault()
          void ask(question)
        }}
      >
        <input
          ref={inputRef}
          value={question}
          onChange={(ev) => setQuestion(ev.target.value)}
          placeholder="Ask about this call…"
          disabled={busy}
          aria-label="Ask about this call"
        />
        <button type="submit" disabled={busy || !question.trim()}>
          Ask
        </button>
      </form>
    </div>
  )
}

export default CallCopilot
