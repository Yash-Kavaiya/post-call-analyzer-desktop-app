import { useMemo } from 'react'
import type { EChartsOption } from 'echarts'
import { ChartFrame } from './ChartFrame'
import type { ChartEventParams } from './ChartFrame'
import { useChartTheme, withAlpha } from './chartTheme'

export interface BarDatum {
  label: string
  value: number
  /** Optional token-derived override, e.g. `theme.success`. Never a literal. */
  color?: string
}

export interface BarChartProps {
  data: BarDatum[]
  /** Lay bars horizontally (better for long category names). */
  horizontal?: boolean
  height?: number
  loading?: boolean
  error?: string | null
  emptyLabel?: string
  ariaLabel?: string
  /** Formats the value in tooltips and bar labels. */
  valueFormatter?: (value: number) => string
  /** Fires when a bar is clicked. */
  onSelect?: (datum: BarDatum, index: number) => void
  /** Draw a value label at the end of each bar. */
  showValueLabels?: boolean
}

/**
 * Categorical bar chart - used for both the stage breakdown and the score
 * distribution histogram. All colour comes from `useChartTheme()`.
 */
export function BarChart(props: BarChartProps) {
  const {
    data,
    horizontal = false,
    height = 240,
    loading = false,
    error = null,
    emptyLabel = 'Nothing to chart yet',
    ariaLabel,
    valueFormatter,
    onSelect,
    showValueLabels = true,
  } = props
  const theme = useChartTheme()

  const format = useMemo(
    () => valueFormatter ?? ((value: number) => value.toLocaleString()),
    [valueFormatter],
  )

  const option = useMemo<EChartsOption>(() => {
    const categories = data.map((datum) => datum.label)
    const values = data.map((datum) => ({
      value: datum.value,
      itemStyle: { color: datum.color ?? theme.primary },
    }))

    const categoryAxis = {
      type: 'category' as const,
      data: categories,
      axisLine: { lineStyle: { color: theme.outline } },
      axisTick: { show: false },
      axisLabel: {
        color: theme.onSurfaceVariant,
        fontFamily: theme.fontFamily,
        fontSize: 11,
        hideOverlap: true,
      },
    }

    const valueAxis = {
      type: 'value' as const,
      axisLine: { show: false },
      axisTick: { show: false },
      splitLine: { lineStyle: { color: withAlpha(theme.outline, 0.35) } },
      axisLabel: {
        color: theme.onSurfaceVariant,
        fontFamily: theme.fontFamily,
        fontSize: 11,
        formatter: (value: number) => format(value),
      },
    }

    return {
      backgroundColor: 'transparent',
      textStyle: { color: theme.onSurfaceVariant, fontFamily: theme.fontFamily, fontSize: 12 },
      animationDuration: 320,
      grid: { left: 4, right: showValueLabels ? 28 : 12, top: 12, bottom: 4, containLabel: true },
      tooltip: {
        trigger: 'item',
        backgroundColor: theme.surfaceContainerHigh,
        borderColor: theme.outline,
        borderWidth: 1,
        padding: [8, 12],
        textStyle: { color: theme.onSurface, fontFamily: theme.fontFamily, fontSize: 12 },
        extraCssText: 'border-radius: 12px; box-shadow: none;',
        formatter: (params: unknown) => {
          const entry = Array.isArray(params) ? params[0] : params
          const record = entry as { name?: string; value?: number }
          const raw = typeof record.value === 'number' ? record.value : 0
          return `${record.name ?? ''}<br/><strong>${format(raw)}</strong>`
        },
      },
      xAxis: horizontal ? valueAxis : categoryAxis,
      yAxis: horizontal ? categoryAxis : valueAxis,
      series: [
        {
          type: 'bar',
          data: values,
          barMaxWidth: horizontal ? 22 : 40,
          itemStyle: { borderRadius: horizontal ? [0, 6, 6, 0] : [6, 6, 0, 0] },
          emphasis: { itemStyle: { color: theme.primary, opacity: 0.85 } },
          label: {
            show: showValueLabels,
            position: horizontal ? 'right' : 'top',
            color: theme.onSurfaceVariant,
            fontFamily: theme.fontFamily,
            fontSize: 11,
            formatter: (params: { value?: unknown }) =>
              typeof params.value === 'number' ? format(params.value) : '',
          },
          cursor: onSelect === undefined ? 'default' : 'pointer',
        },
      ],
    }
  }, [data, horizontal, theme, format, showValueLabels, onSelect])

  const handlers = useMemo(() => {
    if (onSelect === undefined) return undefined
    return {
      click: (params: ChartEventParams) => {
        const index = params.dataIndex ?? -1
        const datum = data[index]
        if (datum !== undefined) onSelect(datum, index)
      },
    }
  }, [onSelect, data])

  return (
    <ChartFrame
      option={option}
      height={height}
      loading={loading}
      error={error}
      isEmpty={data.length === 0}
      emptyLabel={emptyLabel}
      ariaLabel={ariaLabel}
      onEvents={handlers}
    />
  )
}
