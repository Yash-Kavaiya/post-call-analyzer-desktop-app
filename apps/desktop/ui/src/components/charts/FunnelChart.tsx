import { useMemo } from 'react'
import type { EChartsOption } from 'echarts'
import { ChartFrame } from './ChartFrame'
import type { ChartEventParams } from './ChartFrame'
import { mixColors, useChartTheme } from './chartTheme'

export interface FunnelDatum {
  label: string
  value: number
  /** Token-derived override for a stage that needs to stand out (e.g. failed). */
  color?: string
}

export interface FunnelChartProps {
  data: FunnelDatum[]
  height?: number
  loading?: boolean
  error?: string | null
  emptyLabel?: string
  ariaLabel?: string
  valueFormatter?: (value: number) => string
  onSelect?: (datum: FunnelDatum, index: number) => void
}

/**
 * Pipeline stage funnel. Stages arrive already ordered; the series keeps that
 * order rather than re-sorting, so a stage that has run dry is still visible in
 * its correct position.
 */
export function FunnelChart(props: FunnelChartProps) {
  const {
    data,
    height = 280,
    loading = false,
    error = null,
    emptyLabel = 'No calls in the pipeline yet',
    ariaLabel,
    valueFormatter,
    onSelect,
  } = props
  const theme = useChartTheme()

  const format = useMemo(
    () => valueFormatter ?? ((value: number) => value.toLocaleString()),
    [valueFormatter],
  )

  const option = useMemo<EChartsOption>(() => {
    const count = Math.max(data.length - 1, 1)
    const items = data.map((datum, index) => ({
      name: datum.label,
      value: datum.value,
      itemStyle: {
        // Ramp from primary toward the surface so later stages read as lighter.
        color: datum.color ?? mixColors(theme.primary, theme.surface, (index / count) * 0.55),
        borderColor: theme.surface,
        borderWidth: 2,
      },
    }))

    return {
      backgroundColor: 'transparent',
      textStyle: { color: theme.onSurfaceVariant, fontFamily: theme.fontFamily, fontSize: 12 },
      animationDuration: 320,
      tooltip: {
        trigger: 'item',
        backgroundColor: theme.surfaceContainerHigh,
        borderColor: theme.outline,
        borderWidth: 1,
        padding: [8, 12],
        textStyle: { color: theme.onSurface, fontFamily: theme.fontFamily, fontSize: 12 },
        extraCssText: 'border-radius: 12px; box-shadow: none;',
        formatter: (params: unknown) => {
          const record = params as { name?: string; value?: number }
          const raw = typeof record.value === 'number' ? record.value : 0
          return `${record.name ?? ''}<br/><strong>${format(raw)}</strong>`
        },
      },
      series: [
        {
          type: 'funnel',
          left: '4%',
          right: '4%',
          top: 8,
          bottom: 8,
          minSize: '24%',
          maxSize: '100%',
          sort: 'none',
          gap: 3,
          data: items,
          label: {
            show: true,
            position: 'inside',
            color: theme.surface,
            fontFamily: theme.fontFamily,
            fontSize: 12,
            formatter: (params: { name?: string; value?: unknown }) =>
              `${params.name ?? ''}  ${typeof params.value === 'number' ? format(params.value) : ''}`,
          },
          labelLine: { show: false },
          emphasis: { label: { fontWeight: 'bold' } },
          cursor: onSelect === undefined ? 'default' : 'pointer',
        },
      ],
    }
  }, [data, theme, format, onSelect])

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

  const total = data.reduce((sum, datum) => sum + datum.value, 0)

  return (
    <ChartFrame
      option={option}
      height={height}
      loading={loading}
      error={error}
      isEmpty={data.length === 0 || total === 0}
      emptyLabel={emptyLabel}
      ariaLabel={ariaLabel}
      onEvents={handlers}
    />
  )
}
