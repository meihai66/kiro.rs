import { useMemo } from 'react'
import { useQuery } from '@tanstack/react-query'
import { getRpmHistory } from '@/api/credentials'

interface RpmSparklineProps {
  credentialId: number
  hours?: number
}

export function RpmSparkline({ credentialId, hours = 24 }: RpmSparklineProps) {
  const { data, isLoading } = useQuery({
    queryKey: ['rpm-history', credentialId, hours],
    queryFn: () => getRpmHistory(credentialId, hours),
    refetchInterval: 60_000,
  })

  const { path, max, count } = useMemo(() => {
    const points = data?.points ?? []
    if (points.length === 0) {
      return { path: '', max: 0, count: 0 }
    }
    const max = Math.max(1, ...points.map((p) => p.count))
    const w = 400
    const h = 60
    const xs = (i: number) =>
      points.length === 1 ? w / 2 : (i / (points.length - 1)) * w
    const path = points
      .map(
        (p, i) =>
          `${i === 0 ? 'M' : 'L'} ${xs(i).toFixed(1)} ${(h - (p.count / max) * h).toFixed(1)}`
      )
      .join(' ')
    return { path, max, count: points.length }
  }, [data])

  if (isLoading) {
    return (
      <div className="text-xs text-muted-foreground">加载 RPM 历史…</div>
    )
  }

  if (count === 0) {
    return (
      <div className="text-xs text-muted-foreground">
        暂无 RPM 历史数据（首次需运行至少 1 分钟产生数据点）
      </div>
    )
  }

  return (
    <div className="space-y-1">
      <div className="rounded border bg-muted/30 p-2">
        <svg viewBox="0 0 400 60" className="w-full h-16" preserveAspectRatio="none">
          <path
            d={path}
            fill="none"
            stroke="currentColor"
            strokeWidth={1.5}
            className="text-emerald-500"
          />
        </svg>
      </div>
      <div className="text-xs text-muted-foreground">
        最近 {hours}h · 每分钟 RPM · 峰值 {max} · 数据点 {count}
      </div>
    </div>
  )
}
