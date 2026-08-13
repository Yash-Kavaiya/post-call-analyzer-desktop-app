/**
 * A call while it is still happening.
 *
 * Everything here is read by someone who is mid-conversation, which sets the
 * priorities: alerts sit above the transcript because a breach you can still
 * fix outranks a record of one, and the suggested line sits at the bottom edge
 * where the eye returns between sentences.
 *
 * Two honesty rules the design depends on:
 *
 * - **Transcript text is never revised.** Chunks are cut at pauses and
 *   transcribed whole, so a line that appears is final. The `▌` marker is
 *   driven by voice activity, not by a provisional guess, and it never turns
 *   into text in place.
 * - **The live transcript is a preview, not the record.** It is produced by a
 *   smaller model for speed. The call is re-transcribed properly at hang-up,
 *   so nothing shown here is what ends up in the corpus — and the screen says
 *   so rather than letting it be assumed.
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import {
  formatDuration,
  listAudioDevices,
  onLiveEvent,
  startLiveSession,
  stopLiveSession,
  type AudioDevices,
  type Finding,
  type LiveChannel,
  type LiveEvent,
  type Severity,
} from '../api'
import './LiveScreen.css'

export interface LiveScreenProps {
  /** Opens the finished call once the battery has run. */
  onOpenCall: (callId: string) => void
}

interface Line {
  id: string
  channel: LiveChannel
  at: number
  text: string
}

interface MonitorState {
  sentiment: number
  risk: Severity
  headline: string
  sayNext: string | null
}

type Phase = 'idle' | 'starting' | 'live' | 'ending'

/** A meter that reads as "is this device actually working". */
function Level({ label, dbfs }: { label: string; dbfs: number }) {
  // −60 dBFS is inaudible, 0 is clipping. Anything below the floor shows empty
  // rather than negative, which is what a muted or unplugged device looks like.
  const pct = Math.max(0, Math.min(1, (dbfs + 60) / 60))
  return (
    <div className="lv-level">
      <span className="lv-level-label">{label}</span>
      <div className="lv-level-track">
        <div
          className={`lv-level-fill${pct > 0.9 ? ' lv-hot' : ''}`}
          style={{ width: `${pct * 100}%` }}
        />
      </div>
    </div>
  )
}

function Sparkline({ points }: { points: number[] }) {
  // Two points is a comparison, not a trend, and it draws as a straight line
  // spanning the whole box — which reads as a divider rather than a chart. The
  // number beside it already says where sentiment is right now.
  if (points.length < 3) return null
  const step = 100 / (points.length - 1)
  const y = (v: number) => ((1 - Math.max(-1, Math.min(1, v))) / 2) * 100
  const line = points.map((p, i) => `${i * step},${y(p)}`).join(' ')
  return (
    <svg className="lv-spark" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden="true">
      <line className="lv-spark-zero" x1="0" y1="50" x2="100" y2="50" />
      <polyline className="lv-spark-line" points={line} />
    </svg>
  )
}

export function LiveScreen({ onOpenCall }: LiveScreenProps) {
  const [phase, setPhase] = useState<Phase>('idle')
  const [devices, setDevices] = useState<AudioDevices | null>(null)
  const [input, setInput] = useState<string | null>(null)
  const [output, setOutput] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [warnings, setWarnings] = useState<string[]>([])

  const [callId, setCallId] = useState<string | null>(null)
  const [transcribing, setTranscribing] = useState(true)
  const [elapsed, setElapsed] = useState(0)
  const [levels, setLevels] = useState({ agent: -60, customer: -60 })
  const [speaking, setSpeaking] = useState<Record<LiveChannel, boolean>>({
    agent: false,
    customer: false,
  })
  const [lines, setLines] = useState<Line[]>([])
  const [findings, setFindings] = useState<Finding[]>([])
  const [monitor, setMonitor] = useState<MonitorState | null>(null)
  const [trend, setTrend] = useState<number[]>([])

  const tail = useRef<HTMLDivElement | null>(null)

  useEffect(() => {
    listAudioDevices()
      .then(setDevices)
      .catch((e: unknown) => setError(String(e)))
  }, [])

  // The transcript follows the conversation rather than making the reader
  // chase it — they are talking, not scrolling.
  useEffect(() => {
    tail.current?.scrollIntoView({ block: 'end' })
  }, [lines])

  useEffect(() => {
    const off = onLiveEvent((e: LiveEvent) => {
      switch (e.kind) {
        case 'started':
          setCallId(e.callId)
          setPhase('live')
          break
        case 'level':
          setLevels({ agent: e.agentDbfs, customer: e.customerDbfs })
          setElapsed(e.elapsedSecs)
          break
        case 'speaking':
          setSpeaking((s) => ({ ...s, [e.channel]: e.speaking }))
          break
        case 'stable':
          setLines((l) => [
            ...l,
            { id: e.segmentId, channel: e.channel, at: e.startSecs, text: e.text },
          ])
          break
        case 'rule':
          setFindings((f) => [e.finding, ...f])
          break
        case 'monitor':
          setMonitor({
            sentiment: e.sentiment,
            risk: e.risk,
            headline: e.headline,
            sayNext: e.sayNext,
          })
          setTrend((t) => [...t, e.sentiment].slice(-40))
          break
        case 'warning':
          setWarnings((w) => [e.message, ...w].slice(0, 3))
          break
        case 'ended':
          setPhase('ending')
          setElapsed(e.durationSecs)
          break
      }
    })
    return off
  }, [])

  const start = useCallback(async () => {
    setPhase('starting')
    setError(null)
    setWarnings([])
    setLines([])
    setFindings([])
    setMonitor(null)
    setTrend([])
    try {
      const started = await startLiveSession(input, output)
      setCallId(started.callId)
      setTranscribing(started.transcribing)
      setPhase('live')
    } catch (e) {
      setError(String(e))
      setPhase('idle')
    }
  }, [input, output])

  const stop = useCallback(async () => {
    setPhase('ending')
    try {
      await stopLiveSession()
    } catch (e) {
      setError(String(e))
    }
  }, [])

  // The backend computes `blocking` from one shared threshold; the severity
  // check is the fallback for a payload that predates the field, so the two can
  // never disagree about what fails a call.
  const blocking = findings.filter(
    (f) => f.blocking ?? (f.severity === 'critical' || f.severity === 'high'),
  )

  if (phase === 'idle') {
    return (
      <div className="lv-root">
        <h2>Live call</h2>
        {error && <p className="lv-error">{error}</p>}

        {devices && !devices.supported ? (
          <p className="lv-state">
            Audio capture is not available in this build, so a live call cannot be
            recorded here.
          </p>
        ) : (
          <section className="lv-setup">
            <p className="lv-hint">
              Your microphone is recorded as the agent and whatever your softphone is
              playing is recorded as the customer, so the two sides stay separate and
              speaker labels are exact. Use a headset — on speakers the microphone also
              picks up the customer, and both sides transcribe the same words.
            </p>

            <label className="lv-field">
              <span>Microphone (you)</span>
              <select value={input ?? ''} onChange={(e) => setInput(e.target.value || null)}>
                <option value="">System default</option>
                {devices?.inputs.map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </select>
            </label>

            <label className="lv-field">
              <span>Playback device (the customer)</span>
              <select value={output ?? ''} onChange={(e) => setOutput(e.target.value || null)}>
                <option value="">System default</option>
                {devices?.outputs.map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </select>
            </label>

            <button type="button" className="lv-start" onClick={start}>
              Start listening
            </button>
          </section>
        )}
      </div>
    )
  }

  return (
    <div className="lv-root">
      <header className="lv-head">
        <span className={`lv-dot${phase === 'live' ? ' lv-recording' : ''}`} />
        <h2>{phase === 'ending' ? 'Wrapping up' : 'Live call'}</h2>
        <span className="lv-clock">{formatDuration(elapsed)}</span>
        <div className="lv-meters">
          <Level label="You" dbfs={levels.agent} />
          <Level label="Them" dbfs={levels.customer} />
        </div>
        {phase === 'live' ? (
          <button type="button" className="lv-stop" onClick={stop}>
            End call
          </button>
        ) : (
          callId && (
            <button type="button" className="lv-open" onClick={() => onOpenCall(callId)}>
              Open the call
            </button>
          )
        )}
      </header>

      {phase === 'ending' && (
        <p className="lv-hint">
          Recording stopped. The call is being transcribed properly and scored by the
          full agent battery — it will appear in Calls when that finishes.
        </p>
      )}

      {!transcribing && phase === 'live' && (
        <p className="lv-hint">
          No live transcription model is loaded, so there is no running transcript.
          The call is still being recorded, the checks below still run, and the full
          transcript is produced when the call ends.
        </p>
      )}

      {warnings.map((w, i) => (
        <p key={i} className="lv-warn">
          {w}
        </p>
      ))}
      {error && <p className="lv-error">{error}</p>}

      {blocking.length > 0 && (
        <section className="lv-alerts">
          {blocking.map((f) => (
            <div key={f.id} className="lv-alert">
              <span className={`lv-sev lv-sev-${f.severity}`}>{f.severity}</span>
              <span className="lv-alert-title">{f.title}</span>
              <span className="lv-alert-at">
                {f.citations[0] ? formatDuration(f.citations[0].start) : ''}
              </span>
            </div>
          ))}
        </section>
      )}

      {monitor && (
        <section className="lv-monitor">
          <div className="lv-sentiment">
            <span className="lv-sentiment-label">Sentiment</span>
            <Sparkline points={trend} />
            <strong
              className={
                monitor.sentiment <= -0.2
                  ? 'lv-bad'
                  : monitor.sentiment >= 0.2
                    ? 'lv-good'
                    : undefined
              }
            >
              {monitor.sentiment > 0 ? '+' : monitor.sentiment < 0 ? '−' : ''}
              {Math.abs(monitor.sentiment).toFixed(2)}
            </strong>
          </div>
          <p className="lv-headline">{monitor.headline}</p>
        </section>
      )}

      <section className="lv-transcript">
        {lines.length === 0 ? (
          <p className="lv-state">Listening…</p>
        ) : (
          lines.map((l) => (
            <p key={l.id} className={`lv-line lv-${l.channel}`}>
              <span className="lv-at">{formatDuration(l.at)}</span>
              <span className="lv-who">{l.channel === 'agent' ? 'YOU' : 'THEM'}</span>
              <span className="lv-text">{l.text}</span>
            </p>
          ))
        )}
        {(speaking.agent || speaking.customer) && (
          <p className="lv-line lv-listening">
            <span className="lv-at" />
            <span className="lv-who">{speaking.agent ? 'YOU' : 'THEM'}</span>
            <span className="lv-text lv-caret">▌</span>
          </p>
        )}
        <div ref={tail} />
      </section>

      {/* Only rendered when the model had evidence for it. An invented line is
          read aloud to a customer, so the backend drops unevidenced ones and
          this simply does not appear. */}
      {monitor?.sayNext && (
        <footer className="lv-saynext">
          <span className="lv-saynext-label">Say next</span>
          <p>“{monitor.sayNext}”</p>
        </footer>
      )}
    </div>
  )
}

export default LiveScreen
