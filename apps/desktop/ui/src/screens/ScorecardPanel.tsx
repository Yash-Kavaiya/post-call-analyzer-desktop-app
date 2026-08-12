/**
 * QA scorecard.
 *
 * Two presentational rules carry real weight here:
 *
 * - Not-applicable criteria are shown as excluded, never as a zero. A criterion
 *   the model could not evidence is not the same as one the agent failed, and
 *   rendering them alike would quietly slander the agent.
 * - Citations are clickable. A score with no way to check it is an opinion; the
 *   whole point of validating segment ids in the backend is that every claim
 *   here jumps to the moment in the call that supports it.
 */

import type { CriterionScore, Scorecard } from '../api'
import './ScorecardPanel.css'

export interface ScorecardPanelProps {
  scorecard: Scorecard | null
  /** Jump the shared playhead to a citation. */
  onSeek: (seconds: number) => void
}

function scoreBand(score: number): 'good' | 'fair' | 'poor' {
  if (score >= 80) return 'good'
  if (score >= 55) return 'fair'
  return 'poor'
}

function CriterionRow({
  criterion,
  onSeek,
}: {
  criterion: CriterionScore
  onSeek: (seconds: number) => void
}) {
  const pct = Math.round(criterion.score * 100)

  return (
    <li className={`sc-criterion${criterion.notApplicable ? ' sc-na' : ''}`}>
      <div className="sc-crit-head">
        <span className="sc-crit-label">{criterion.label}</span>
        {criterion.notApplicable ? (
          <span className="sc-crit-na">not scored</span>
        ) : (
          <span className={`sc-crit-score sc-${scoreBand(pct)}`}>{pct}%</span>
        )}
      </div>

      {!criterion.notApplicable && (
        <div className="sc-bar" role="presentation">
          <div className={`sc-bar-fill sc-${scoreBand(pct)}`} style={{ width: `${pct}%` }} />
        </div>
      )}

      <p className="sc-reasoning">{criterion.reasoning}</p>

      {criterion.citations.length > 0 && (
        <div className="sc-citations">
          {criterion.citations.map((c) => (
            <button
              key={`${c.segmentId}-${c.start}`}
              type="button"
              className="sc-citation"
              onClick={() => onSeek(c.start)}
              title={c.quote ?? 'Jump to this moment'}
            >
              {formatTs(c.start)}
            </button>
          ))}
        </div>
      )}

      {criterion.weight !== 1 && (
        <span className="sc-weight">weight ×{criterion.weight}</span>
      )}
    </li>
  )
}

function formatTs(secs: number): string {
  const m = Math.floor(secs / 60)
  const s = Math.floor(secs % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}

export function ScorecardPanel({ scorecard, onSeek }: ScorecardPanelProps) {
  if (!scorecard) {
    return (
      <div className="sc-empty">
        <h3>No scorecard yet</h3>
        <p>
          Run the QA agent on this call to score it against the rubric. Every
          criterion will link back to the moment that justifies it.
        </p>
      </div>
    )
  }

  const overall = Math.round(scorecard.overall)
  const scored = scorecard.criteria.filter((c) => !c.notApplicable).length

  return (
    <div className="sc-root">
      <div className="sc-overall">
        <div className={`sc-ring sc-${scoreBand(overall)}`}>
          <span className="sc-ring-value">{overall}</span>
          <span className="sc-ring-unit">/100</span>
        </div>
        <div className="sc-overall-meta">
          <span>
            {scored} of {scorecard.criteria.length} criteria scored
          </span>
          <span className="sc-rubric">
            {scorecard.rubricId} v{scorecard.rubricVersion}
          </span>
        </div>
      </div>

      {scorecard.summary && <p className="sc-summary">{scorecard.summary}</p>}

      <ul className="sc-list">
        {scorecard.criteria.map((c) => (
          <CriterionRow key={c.key} criterion={c} onSeek={onSeek} />
        ))}
      </ul>
    </div>
  )
}

export default ScorecardPanel
