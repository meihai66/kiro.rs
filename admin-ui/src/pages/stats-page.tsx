import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Loader2, RotateCcw } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { useCredentials, useResetAllStats } from '@/hooks/use-credentials'
import { getProxies } from '@/api/proxies'
import { getRpmHistoryAggregate, getStatsSummary } from '@/api/credentials'
import { RpmTuningCard } from '@/components/rpm-tuning-card'
import { extractErrorMessage } from '@/lib/utils'

const HOURS_OPTIONS = [1, 6, 24, 72, 168] as const

export function StatsPage() {
  const [hours, setHours] = useState<number>(24)
  const { data: creds } = useCredentials()
  const resetMut = useResetAllStats()
  const { data: pool } = useQuery({
    queryKey: ['proxies'],
    queryFn: getProxies,
    refetchInterval: 30_000,
  })
  const { data: rpmAgg } = useQuery({
    queryKey: ['rpm-aggregate', hours],
    queryFn: () => getRpmHistoryAggregate(hours),
    refetchInterval: 60_000,
  })
  const { data: summary } = useQuery({
    queryKey: ['stats-summary'],
    queryFn: getStatsSummary,
    refetchInterval: 30_000,
  })

  const formatUptime = (secs: number) => {
    const d = Math.floor(secs / 86400)
    const h = Math.floor((secs % 86400) / 3600)
    const m = Math.floor((secs % 3600) / 60)
    if (d > 0) return `${d}d ${h}h ${m}m`
    if (h > 0) return `${h}h ${m}m`
    return `${m}m`
  }

  const credList = creds?.credentials ?? []
  const totalCreds = credList.length
  const enabledCount = credList.filter((c) => !c.disabled).length
  const disabledCount = totalCreds - enabledCount
  const noProxyCount = credList.filter(
    (c) => !c.proxySlotId && !c.disabled
  ).length
  // 累计错误数（不随成功清零）；区别于「当前处于连续失败状态的凭据数」
  const totalErrors = credList.reduce((s, c) => s + (c.errorCount ?? 0), 0)
  const totalInFlight = credList.reduce((s, c) => s + (c.inFlight ?? 0), 0)
  const totalRpm = credList.reduce((s, c) => s + (c.rpm ?? 0), 0)
  const overageOn = credList.filter((c) => c.overageStatus === 'ENABLED').length
  const overageOff = credList.filter(
    (c) => c.overageStatus === 'DISABLED'
  ).length

  const proxyList = pool?.proxies ?? []
  const totalProxies = proxyList.length
  const activeProxies = proxyList.filter(
    (p) => p.status === 'active' || p.status === 'expiring'
  ).length
  const expiringProxies = proxyList.filter(
    (p) => p.status === 'expiring' || (p.remainingSecs > 0 && p.remainingSecs < 86400)
  ).length
  const expiredProxies = proxyList.filter((p) => p.status === 'expired').length
  const totalSlots = proxyList.reduce((s, p) => s + (p.slots ?? 0), 0)
  const usedSlots = proxyList.reduce((s, p) => s + (p.usedSlots ?? 0), 0)

  // 订阅分布
  const subscriptionDist = useMemo(() => {
    const map = new Map<string, number>()
    credList.forEach((c) => {
      const t = c.subscriptionTitle || '未知'
      map.set(t, (map.get(t) ?? 0) + 1)
    })
    return Array.from(map.entries()).sort((a, b) => b[1] - a[1])
  }, [credList])

  // 端点分布
  const endpointDist = useMemo(() => {
    const map = new Map<string, number>()
    credList.forEach((c) => {
      const t = c.effectiveEndpoint || 'ide'
      map.set(t, (map.get(t) ?? 0) + 1)
    })
    return Array.from(map.entries()).sort((a, b) => b[1] - a[1])
  }, [credList])

  // 全局 RPM 折线图
  const rpmChart = useMemo(() => {
    const points = rpmAgg?.points ?? []
    if (points.length === 0) return null
    const max = Math.max(1, ...points.map((p) => p.count))
    const sum = points.reduce((s, p) => s + p.count, 0)
    const avg = sum / Math.max(1, points.length)
    const w = 800
    const h = 100
    const xs = (i: number) =>
      points.length === 1 ? w / 2 : (i / (points.length - 1)) * w
    const linePath = points
      .map(
        (p, i) =>
          `${i === 0 ? 'M' : 'L'} ${xs(i).toFixed(1)} ${(h - (p.count / max) * h).toFixed(1)}`
      )
      .join(' ')
    return { linePath, max, sum, avg, count: points.length, w, h }
  }, [rpmAgg])

  // 排行榜：按 RPM 取前 5
  const topByRpm = useMemo(
    () =>
      [...credList]
        .filter((c) => (c.rpm ?? 0) > 0)
        .sort((a, b) => (b.rpm ?? 0) - (a.rpm ?? 0))
        .slice(0, 5),
    [credList]
  )

  // 余额低排行
  const lowBalanceCreds = useMemo(
    () =>
      [...credList]
        .filter((c) => !c.disabled)
        .slice(0, 5)
        .map((c) => ({ id: c.id, email: c.email })),
    [credList]
  )

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <h1 className="text-2xl font-semibold">统计</h1>
        <div className="flex items-center gap-2">
          <div className="flex items-center gap-1">
            {HOURS_OPTIONS.map((h) => (
              <Button
                key={h}
                size="sm"
                variant={hours === h ? 'secondary' : 'ghost'}
                className="h-8"
                onClick={() => setHours(h)}
              >
                {h < 24 ? `${h}h` : `${h / 24}d`}
              </Button>
            ))}
          </div>
          <Button
            size="sm"
            variant="destructive"
            className="h-8"
            disabled={resetMut.isPending}
            title="清空所有 API Key 累计请求计数 + 每凭据 success/错误/429 计数 + 产出价值。不清 RPM 历史和错误日志"
            onClick={() => {
              if (
                !confirm(
                  '确认清空统计数据？\n\n会重置：\n• 总请求次数 / 成功 / 失败\n• 每凭据成功次数、累计错误、429 累计\n• 每凭据积分 / 产出价值\n\n不会动：连续失败计数、最后调用时间、RPM 历史、错误日志。'
                )
              )
                return
              resetMut.mutate(undefined, {
                onSuccess: (r) => toast.success(r.message),
                onError: (e) =>
                  toast.error('清空失败：' + extractErrorMessage(e)),
              })
            }}
          >
            {resetMut.isPending ? (
              <Loader2 className="h-3.5 w-3.5 mr-1 animate-spin" />
            ) : (
              <RotateCcw className="h-3.5 w-3.5 mr-1" />
            )}
            清空统计
          </Button>
        </div>
      </div>

      {/* 服务级摘要 */}
      <div className="grid gap-3 md:grid-cols-4 mb-4">
        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              运行时间
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">
              {summary ? formatUptime(summary.uptimeSecs) : '—'}
            </div>
            {summary && (
              <p className="text-[11px] text-muted-foreground">
                自 {new Date(summary.startedAt).toLocaleString()}
              </p>
            )}
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              总请求次数
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">
              {summary?.totalRequests ?? 0}
            </div>
            <p className="text-[11px] text-muted-foreground">
              成功 {summary?.totalSuccess ?? 0} · 失败{' '}
              <span
                className={
                  (summary?.totalFail ?? 0) > 0 ? 'text-red-500' : ''
                }
              >
                {summary?.totalFail ?? 0}
              </span>
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              成功率
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold text-emerald-600">
              {summary && summary.totalRequests > 0
                ? ((summary.totalSuccess / summary.totalRequests) * 100).toFixed(1) + '%'
                : '—'}
            </div>
          </CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="text-xs font-medium text-muted-foreground">
              当前总并发
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="text-2xl font-bold">{totalInFlight}</div>
            <p className="text-[11px] text-muted-foreground">
              当前 RPM {totalRpm}
            </p>
          </CardContent>
        </Card>
      </div>

      {/* 顶部数字卡片 */}
      <div className="grid gap-3 md:grid-cols-4 lg:grid-cols-6 mb-4">
        <StatCard label="凭据总数" value={totalCreds} />
        <StatCard label="可用" value={enabledCount} tone="ok" />
        <StatCard label="已禁用" value={disabledCount} tone="warn" />
        <StatCard label="未绑代理" value={noProxyCount} tone={noProxyCount > 0 ? 'warn' : undefined} />
        <StatCard label="累计错误" value={totalErrors} tone={totalErrors > 0 ? 'warn' : undefined} />
        <StatCard label="超额开启" value={overageOn} tone="ok" />
      </div>

      <div className="grid gap-3 md:grid-cols-4 lg:grid-cols-6 mb-4">
        <StatCard label="实时并发" value={totalInFlight} tone={totalInFlight > 0 ? 'ok' : undefined} />
        <StatCard label="实时 RPM" value={totalRpm} />
        <StatCard label="超额关闭" value={overageOff} />
        <StatCard label="代理总数" value={totalProxies} />
        <StatCard label="代理可用" value={activeProxies} tone="ok" />
        <StatCard
          label="即将过期"
          value={expiringProxies}
          tone={expiringProxies > 0 ? 'warn' : undefined}
        />
      </div>

      {/* 全局 RPM 折线 */}
      <Card className="mb-4">
        <CardHeader className="pb-2">
          <CardTitle className="text-base flex items-center justify-between">
            <span>
              全局 RPM 历史（过去 {hours < 24 ? `${hours} 小时` : `${hours / 24} 天`}）
            </span>
            {rpmChart && (
              <span className="text-xs font-normal text-muted-foreground">
                峰值 {rpmChart.max} · 总请求 {rpmChart.sum} · 均值 {rpmChart.avg.toFixed(1)} · 数据点 {rpmChart.count}
              </span>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent>
          {rpmChart ? (
            <svg
              viewBox={`0 0 ${rpmChart.w} ${rpmChart.h}`}
              preserveAspectRatio="none"
              className="w-full h-32"
            >
              <path
                d={rpmChart.linePath}
                fill="none"
                stroke="currentColor"
                strokeWidth={1.5}
                className="text-emerald-500"
              />
            </svg>
          ) : (
            <div className="text-sm text-muted-foreground py-8 text-center">
              暂无 RPM 历史数据（首次启动需运行 1 分钟以上产生数据点）
            </div>
          )}
        </CardContent>
      </Card>

      {/* RPM 调优：最佳 RPM 发现 */}
      <RpmTuningCard hours={hours} />

      {/* 三栏：分布 + 排行 */}
      <div className="grid gap-3 md:grid-cols-3">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-base">订阅分布</CardTitle>
          </CardHeader>
          <CardContent>
            {subscriptionDist.length === 0 ? (
              <div className="text-sm text-muted-foreground">暂无</div>
            ) : (
              <ul className="space-y-2 text-sm">
                {subscriptionDist.map(([title, count]) => {
                  const pct = totalCreds > 0 ? (count / totalCreds) * 100 : 0
                  return (
                    <li key={title}>
                      <div className="flex items-center justify-between mb-1">
                        <span className="truncate">{title}</span>
                        <span className="font-mono text-xs">
                          {count} ({pct.toFixed(0)}%)
                        </span>
                      </div>
                      <div className="h-1.5 bg-muted rounded-full overflow-hidden">
                        <div
                          className="h-full bg-emerald-500"
                          style={{ width: `${pct}%` }}
                        />
                      </div>
                    </li>
                  )
                })}
              </ul>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-base">Endpoint 分布</CardTitle>
          </CardHeader>
          <CardContent>
            {endpointDist.length === 0 ? (
              <div className="text-sm text-muted-foreground">暂无</div>
            ) : (
              <ul className="space-y-2 text-sm">
                {endpointDist.map(([ep, count]) => {
                  const pct = totalCreds > 0 ? (count / totalCreds) * 100 : 0
                  return (
                    <li key={ep}>
                      <div className="flex items-center justify-between mb-1">
                        <span className="font-mono">{ep}</span>
                        <span className="font-mono text-xs">
                          {count} ({pct.toFixed(0)}%)
                        </span>
                      </div>
                      <div className="h-1.5 bg-muted rounded-full overflow-hidden">
                        <div
                          className="h-full bg-blue-500"
                          style={{ width: `${pct}%` }}
                        />
                      </div>
                    </li>
                  )
                })}
              </ul>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-base">RPM TOP 5</CardTitle>
          </CardHeader>
          <CardContent>
            {topByRpm.length === 0 ? (
              <div className="text-sm text-muted-foreground">暂无活跃凭据</div>
            ) : (
              <ul className="space-y-2 text-sm">
                {topByRpm.map((c) => (
                  <li
                    key={c.id}
                    className="flex items-center justify-between gap-2"
                  >
                    <div className="flex flex-col min-w-0">
                      <span className="font-mono text-xs">#{c.id}</span>
                      <span className="text-xs text-muted-foreground truncate">
                        {c.email || '—'}
                      </span>
                    </div>
                    <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
                      RPM {c.rpm}
                    </Badge>
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>
      </div>

      {/* 代理池槽位使用 */}
      <Card className="mt-4">
        <CardHeader className="pb-2">
          <CardTitle className="text-base">
            代理槽位使用 {usedSlots} / {totalSlots}
            {expiredProxies > 0 && (
              <Badge variant="outline" className="ml-2 border-muted text-muted-foreground">
                已过期 {expiredProxies}
              </Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent>
          {totalSlots === 0 ? (
            <div className="text-sm text-muted-foreground">代理池为空</div>
          ) : (
            <div className="h-3 bg-muted rounded-full overflow-hidden">
              <div
                className="h-full bg-blue-500"
                style={{
                  width: `${(usedSlots / Math.max(1, totalSlots)) * 100}%`,
                }}
              />
            </div>
          )}
          <div className="mt-2 text-xs text-muted-foreground">
            空闲槽位 {totalSlots - usedSlots} · 即将过期 {expiringProxies} · 仅
            {lowBalanceCreds.length > 0 ? `查看「凭据」页详情` : ''}
          </div>
        </CardContent>
      </Card>
    </>
  )
}

function StatCard({
  label,
  value,
  tone,
}: {
  label: string
  value: number
  tone?: 'warn' | 'ok'
}) {
  const valueClass =
    tone === 'warn'
      ? 'text-yellow-600'
      : tone === 'ok'
        ? 'text-emerald-600'
        : ''
  return (
    <Card>
      <CardHeader className="pb-1">
        <CardTitle className="text-xs font-medium text-muted-foreground">
          {label}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className={`text-2xl font-bold ${valueClass}`}>{value}</div>
      </CardContent>
    </Card>
  )
}
