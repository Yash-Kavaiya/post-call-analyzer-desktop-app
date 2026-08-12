import type { ReactNode } from 'react'
import type { EChartsOption } from 'echarts'
import ReactECharts from 'echarts-for-react'
import { useChartTheme } from './chartTheme'
import './charts.css'

/** The subset of an ECharts event payload the screens actually consume. */
export interface ChartEventParams {
  componentType?: string
  seriesIndex?: number
  seriesName?: string
  dataIndex?: number
  name?: string
  value?: unknown
}

export type ChartEventHandlers = Record<string, (params: ChartEventParams) => void>

export interface ChartFrameProps {
  /** Fully built ECharts option. Build it with `useChartTheme()` colours. */
  option: EChartsOption
  height?: number
  /** Shows a skeleton instead of the chart. */
  loading?: boolean
  /** Shows an inline error instead of the chart. Never swallow failures. */
  error?: string | null
  /** Shows the empty state instead of the chart. */
  isEmpty?: boolean
  emptyLabel?: string
  emptyHint?: ReactNode
  onRetry?: () => void
  onEvents?: ChartEventHandlers
  /** Accessible description of what the chart shows. */
  ariaLabel?: string
  className?: string
}

const DEFAULT_HEIGHT = 240

/**
 * Presentational shell shared by every chart: fixed height, loading skeleton,
 * empty and error states, and the ECharts mount itself.
 *
 * The chart is keyed on the theme so a light/dark switch rebuilds the canvas
 * with the new palette rather than merging colours into a stale instance.
 */
export function ChartFrame(props: ChartFrameProps) {
  const {
    option,
    height = DEFAULT_HEIGHT,
    loading = false,
    error = null,
    isEmpty = false,
    emptyLabel = 'No data yet',
    emptyHint,
    onRetry,
    onEvents,
    ariaLabel,
    className,
  } = props
  const theme = useChartTheme()
  const style = { height: `${height}px`, width: '100%' }

  if (error !== null && error !== '') {
    return (
      <div className={`cac-frame cac-frame--error ${className ?? ''}`} style={style} role="alert">
        <p className="cac-frame__title">Chart unavailable</p>
        <p className="cac-frame__detail">{error}</p>
        {onRetry !== undefined && (
          <button type="button" className="cac-frame__retry" onClick={onRetry}>
            Retry
          </button>
        )}
      </div>
    )
  }

  if (loading) {
    return (
      <div
        className={`cac-frame cac-frame--loading ${className ?? ''}`}
        style={style}
        role="status"
        aria-busy="true"
        aria-label={ariaLabel === undefined ? 'Loading chart' : `Loading ${ariaLabel}`}
      >
        <span className="cac-skeleton" />
      </div>
    )
  }

  if (isEmpty) {
    return (
      <div className={`cac-frame cac-frame--empty ${className ?? ''}`} style={style}>
        <p className="cac-frame__title">{emptyLabel}</p>
        {emptyHint !== undefined && <div className="cac-frame__detail">{emptyHint}</div>}
      </div>
    )
  }

  return (
    <div className={`cac-frame ${className ?? ''}`} role="img" aria-label={ariaLabel}>
      <ReactECharts
        key={theme.key}
        option={option}
        style={style}
        opts={{ renderer: 'canvas' }}
        notMerge={false}
        lazyUpdate
        onEvents={onEvents}
      />
    </div>
  )
}
