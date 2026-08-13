/**
 * One call: waveform, synced transcript, and everything the agent battery found.
 *
 * Every panel shares a single playhead. Clicking a transcript word, a waveform
 * position, a sentiment moment, a compliance finding or a scorecard citation all
 * seek the same audio element — that shared clock is what makes a citation
 * checkable rather than decorative.
 */

import { useCallback, useEffect, useMemo, useState } from 'react'
import { TransportControls, WaveformTimeline, useAudioPlayer } from '../components/waveform'
import { TranscriptView } from '../components/transcript/TranscriptView'
import { ConversationPanel } from '../components/metrics/ConversationPanel'
import { AgentResults } from '../components/agents/AgentResults'
import { RiskBanner } from '../components/agents/RiskBanner'
import { CallCopilot } from '../components/copilot/CallCopilot'
import {
  formatDuration,
  getCall,
  getScorecard,
  getWaveform,
  mediaUrl,
  onPipelineProgress,
  onRunFinished,
  processCall,
  type CallDetail,
  type Scorecard,
  type WaveformPeaks,
} from '../api'
import { ScorecardPanel } from './ScorecardPanel'
import './CallDetailScreen.css'

export interface CallDetailScreenProps {
  callId: string
  onBack: () => void
}

export function CallDetailScreen({ callId, onBack }: CallDetailScreenProps) {
  const [detail, setDetail] = useState<CallDetail | null>(null)
  const [peaks, setPeaks] = useState<WaveformPeaks | null>(null)
  const [scorecard, setScorecard] = useState<Scorecard | null>(null)
  const [src, setSrc] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [working, setWorking] = useState(false)
  const [stage, setStage] = useState<string | null>(null)
  const [reload, setReload] = useState(0)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)

    // Fetch in parallel: the transcript is the slowest of the three and there
    // is no reason for the waveform to wait behind it.
    Promise.all([getCall(callId), getWaveform(callId), getScorecard(callId)])
      .then(async ([d, p, s]) => {
        if (cancelled) return
        setDetail(d)
        setPeaks(p)
        setScorecard(s)
        if (d.mediaPath) {
          try {
            setSrc(await mediaUrl(d.mediaPath))
          } catch {
            // Playback is a bonus; the transcript is still fully usable.
            setSrc(null)
          }
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(String(e))
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })

    return () => {
      cancelled = true
    }
  }, [callId, reload])

  // While this call is being processed, follow its stages and refetch when the
  // run ends — otherwise the screen keeps showing the empty state it had when
  // the button was pressed.
  useEffect(() => {
    const offProgress = onPipelineProgress((e) => {
      if (e.callId !== callId) return
      if (e.kind === 'stage' && e.stage) setStage(e.stage)
      if (e.kind === 'failed') {
        setError(e.error ?? 'processing failed')
        setWorking(false)
      }
    })
    const offRun = onRunFinished(() => {
      setWorking(false)
      setStage(null)
      setReload((n) => n + 1)
    })
    return () => {
      offProgress()
      offRun()
    }
  }, [callId])

  const runProcessing = useCallback(async () => {
    setWorking(true)
    setError(null)
    setStage('starting')
    try {
      await processCall(callId, true)
    } catch (e) {
      setError(String(e))
      setWorking(false)
    }
  }, [callId])

  const player = useAudioPlayer({ src })

  const handleSeek = useCallback(
    (seconds: number) => {
      player.seek(seconds)
    },
    [player],
  )

  const segments = useMemo(() => detail?.segments ?? [], [detail])
  /** Registered, but nothing has been decoded against it. */
  const unprocessed = !loading && segments.length === 0 && peaks === null
  const duration = detail?.durationSecs ?? peaks?.durationSecs ?? player.duration

  if (loading) return <div className="cd-state">Loading call…</div>
  if (error) return <div className="cd-state cd-error">Could not load this call: {error}</div>
  if (!detail) return <div className="cd-state">Call not found.</div>

  return (
    <div className="cd-root">
      <header className="cd-header">
        <button type="button" className="cd-back" onClick={onBack}>
          ← Calls
        </button>
        <div className="cd-title">
          <h2>{detail.displayName}</h2>
          <span className="cd-meta">
            {formatDuration(duration)} · {segments.length} segments ·{' '}
            {detail.speakers.length || 1} speaker
            {(detail.speakers.length || 1) === 1 ? '' : 's'}
            {detail.model ? ` · ${detail.model}` : ''}
          </span>
        </div>
      </header>

      {unprocessed ? (
        /* No transcript and no peaks means this recording has been registered
           but never decoded. Saying that, and offering the one action that
           fixes it, beats three empty panels that look broken. */
        <section className="cd-unprocessed">
          <h3>This call hasn't been processed yet</h3>
          <p>
            It is registered but no audio has been decoded, so there is no waveform and
            no transcript. Processing runs the stages in order — decode, transcribe,
            separate the speakers, redact personal data — and then scores it if an
            analysis model is set up in Settings.
          </p>
          <button type="button" onClick={runProcessing} disabled={working}>
            {working ? `Working… ${stage ?? ''}` : 'Transcribe and analyse this call'}
          </button>
          {working && (
            <p className="cd-note">
              A few minutes of audio takes roughly the same again to transcribe.
            </p>
          )}
        </section>
      ) : (
        !src && (
          <p className="cd-note">
            Audio unavailable for playback — the transcript and timeline still work.
          </p>
        )
      )}
      {player.error && <p className="cd-note cd-error">Playback error: {player.error}</p>}

      {/* Above the waveform, not with the agent panels: a critical finding is
          the thing to see before deciding whether to listen to the call at all.
          Renders nothing when there is nothing blocking. */}
      {!unprocessed && (
        <RiskBanner callId={callId} onSeek={handleSeek} refreshToken={reload} />
      )}

      <section className="cd-waveform">
        <WaveformTimeline
          peaks={peaks}
          segments={segments}
          currentTime={player.currentTime}
          duration={duration}
          onSeek={handleSeek}
        />
        <TransportControls
          playing={player.playing}
          currentTime={player.currentTime}
          duration={duration}
          rate={player.rate}
          onTogglePlay={player.toggle}
          onSkip={(delta) => player.seek(player.currentTime + delta)}
          onRateChange={player.setRate}
        />
      </section>

      {detail.metrics && detail.metrics.speakers.length > 0 && (
        <section className="cd-metrics">
          <ConversationPanel
            metrics={detail.metrics}
            speakers={detail.speakers}
            duration={duration}
            onSeek={handleSeek}
          />
        </section>
      )}

      {!unprocessed && (
        <section className="cd-agents">
          <h3>Ask about this call</h3>
          <CallCopilot callId={callId} onSeek={handleSeek} />
        </section>
      )}

      {!unprocessed && (
        <section className="cd-agents">
          <h3>AI analysis</h3>
          <AgentResults callId={callId} onSeek={handleSeek} refreshToken={reload} />
        </section>
      )}

      <div className="cd-body">
        <section className="cd-transcript">
          <TranscriptView
            segments={segments}
            currentTime={player.currentTime}
            onSeek={handleSeek}
          />
        </section>
        <aside className="cd-scorecard">
          <ScorecardPanel scorecard={scorecard} onSeek={handleSeek} />
        </aside>
      </div>
    </div>
  )
}

export default CallDetailScreen
