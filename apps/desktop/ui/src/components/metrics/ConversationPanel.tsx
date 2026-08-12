/**
 * How the conversation went, as distinct from what was said.
 *
 * None of this comes from a model. Talk ratio, interruptions, silences and
 * speech rate are arithmetic over the diarised timings, so they are exact,
 * free, and identical on every run — which is what makes them worth putting at
 * the top of the page. The model-derived judgements sit below, where their
 * uncertainty belongs.
 *
 * Every number that points at a moment is clickable: a metric you cannot go
 * and listen to is trivia.
 */

import { formatDuration, type ConversationMetrics, type Speaker } from '../../api'
import './ConversationPanel.css'

export interface ConversationPanelProps {
  metrics: ConversationMetrics
  speakers: Speaker[]
  duration: number
  onSeek: (seconds: number) => void
}

function pct(v: number): string {
  return `${Math.round(v * 100)}%`
}

function ts(seconds: number): string {
  return formatDuration(seconds)
}

/** Who a speaker index belongs to, preferring the diarised role. */
function speakerLabel(index: number, speakers: Speaker[]): string {
  const s = speakers.find((x) => x.index === index)
  if (s && s.role !== 'unknown') return s.role === 'agent' ? 'Agent' : 'Customer'
  return `Speaker ${index}`
}

export function ConversationPanel({
  metrics,
  speakers,
  duration,
  onSeek,
}: ConversationPanelProps) {
  const { speakers: stats, interruptions, silences, monologues, keywords } = metrics

  return (
    <div className="cm-root">
      {/* Talk ratio first: it is the one number that characterises a call
          before you have listened to any of it. */}
      <section className="cm-block">
        <h4>Talk time</h4>
        <div className="cm-bar" role="img" aria-label="Share of speaking time">
          {stats.map((s) => (
            <div
              key={s.speakerIndex}
              className={`cm-bar-seg cm-spk-${s.speakerIndex % 4}`}
              style={{ width: `${s.talkShare * 100}%` }}
              title={`${speakerLabel(s.speakerIndex, speakers)} — ${pct(s.talkShare)}`}
            />
          ))}
        </div>
        <ul className="cm-legend">
          {stats.map((s) => (
            <li key={s.speakerIndex}>
              <span className={`cm-dot cm-spk-${s.speakerIndex % 4}`} />
              <span className="cm-legend-name">{speakerLabel(s.speakerIndex, speakers)}</span>
              <span className="cm-legend-val">{pct(s.talkShare)}</span>
              <span className="cm-legend-sub">
                {formatDuration(s.speechSecs)} · {s.turns} turns ·{' '}
                {Math.round(s.wordsPerMinute)} wpm
                {s.questions > 0 && ` · ${s.questions} question${s.questions === 1 ? '' : 's'}`}
              </span>
            </li>
          ))}
        </ul>
      </section>

      <section className="cm-block">
        <h4>Flow</h4>
        <dl className="cm-tiles">
          <div className="cm-tile">
            <dt>Interruptions</dt>
            <dd>{interruptions.length}</dd>
          </div>
          <div className="cm-tile">
            <dt>Silence</dt>
            <dd>{pct(metrics.silenceRatio)}</dd>
            <span className="cm-tile-sub">
              longest {formatDuration(metrics.longestSilenceSecs)}
            </span>
          </div>
          <div className="cm-tile">
            <dt>Monologues</dt>
            <dd>{monologues.length}</dd>
            <span className="cm-tile-sub">over 30s unbroken</span>
          </div>
          <div className="cm-tile">
            <dt>Length</dt>
            <dd>{formatDuration(duration)}</dd>
          </div>
        </dl>
      </section>

      {interruptions.length > 0 && (
        <section className="cm-block">
          <h4>Who talked over whom</h4>
          <ul className="cm-events">
            {interruptions.slice(0, 8).map((i, n) => (
              <li key={`${i.at}-${n}`}>
                <button type="button" onClick={() => onSeek(i.at)}>
                  <span className="cm-at">{ts(i.at)}</span>
                  <span>
                    {speakerLabel(i.bySpeaker, speakers)} cut in on{' '}
                    {speakerLabel(i.overSpeaker, speakers)}
                  </span>
                  <span className="cm-ev-val">{i.overlapSecs.toFixed(1)}s</span>
                </button>
              </li>
            ))}
          </ul>
          {interruptions.length > 8 && (
            <p className="cm-more">and {interruptions.length - 8} more</p>
          )}
        </section>
      )}

      {silences.length > 0 && (
        <section className="cm-block">
          <h4>Dead air</h4>
          <ul className="cm-events">
            {silences.slice(0, 6).map((s, n) => (
              <li key={`${s.start}-${n}`}>
                <button type="button" onClick={() => onSeek(s.start)}>
                  <span className="cm-at">{ts(s.start)}</span>
                  <span>
                    {s.afterSpeaker != null
                      ? `after ${speakerLabel(s.afterSpeaker, speakers)}`
                      : 'gap'}
                  </span>
                  <span className="cm-ev-val">{Math.round(s.secs)}s</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      {keywords.length > 0 && (
        <section className="cm-block">
          <h4>What it was about</h4>
          <ul className="cm-keywords">
            {keywords.map((k) => (
              <li key={k.term}>
                <button type="button" onClick={() => onSeek(k.firstAt)} title={`First said at ${ts(k.firstAt)}`}>
                  {k.term}
                  <span className="cm-kw-count">{k.count}</span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}

export default ConversationPanel
