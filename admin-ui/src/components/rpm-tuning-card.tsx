import { Fragment, useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { toast } from 'sonner'
import { ChevronDown, ChevronRight, Loader2, Check } from 'lucide-react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { getRpmAnalysis } from '@/api/credentials'
import { useCredentials, useSetCredentialRpm } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type { RpmAnalysisEntry } from '@/types/api'

// 推荐至少需要的样本分钟数（每分钟 1 个采样点），低于此显示「数据不足」
const MIN_SAMPLES = 3

interface RpmTuningCardProps {
  hours: number
}

interface Recommendation {
  /** 推荐 RPM 上限；null 表示数据不足 */
  value: number | null
  /** 是否在观测区间内触发过超阈值 429（true=找到了拐点，推荐更可信） */
  hitLimit: boolean
}

/**
 * 计算推荐 RPM：buckets 按 rpmLow 升序，从最低桶起向上累计，
 * 取「连续满足 429率 ≤ 阈值」的最高桶上界作为推荐值。
 * 一旦某桶 429率 超过阈值即视为拐点并停止。
 */
function recommend(entry: RpmAnalysisEntry, thresholdPct: number): Recommendation {
  if (entry.totalMinutes < MIN_SAMPLES || entry.buckets.length === 0) {
    return { value: null, hitLimit: false }
  }
  const t = thresholdPct / 100
  let rec: number | null = null
  let hitLimit = false
  for (const b of entry.buckets) {
    if (b.rate429 <= t) {
      rec = b.rpmHigh
    } else {
      hitLimit = true
      break
    }
  }
  return { value: rec, hitLimit }
}

export function RpmTuningCard({ hours }: RpmTuningCardProps) {
  const [thresholdPct, setThresholdPct] = useState<number>(2)
  const [expandedId, setExpandedId] = useState<number | null>(null)
  const applyMut = useSetCredentialRpm()

  const { data: analysis, isLoading } = useQuery({
    queryKey: ['rpm-analysis', hours],
    queryFn: () => getRpmAnalysis(hours),
    refetchInterval: 60_000,
  })
  const { data: creds } = useCredentials()

  // id -> 凭据实时信息（当前上限 / 累计 429 / 邮箱）
  const credMap = useMemo(() => {
    const m = new Map<
      number,
      { email: string | null; credentialRpm: number | null; rateLimitCount: number }
    >()
    for (const c of creds?.credentials ?? []) {
      m.set(c.id, {
        email: c.email ?? null,
        credentialRpm: c.credentialRpm ?? null,
        rateLimitCount: c.rateLimitCount ?? 0,
      })
    }
    return m
  }, [creds])

  // 有样本的号在前，按观测峰值降序
  const entries = useMemo(() => {
    const list = [...(analysis?.entries ?? [])]
    list.sort((a, b) => b.observedPeakRpm - a.observedPeakRpm)
    return list
  }, [analysis])

  const handleApply = (id: number, value: number) => {
    applyMut.mutate(
      { id, rpm: value },
      {
        onSuccess: () => toast.success(`已将 #${id} 的 RPM 上限设为 ${value}`),
        onError: (e) => toast.error('应用失败：' + extractErrorMessage(e)),
      }
    )
  }

  return (
    <Card className="mb-4">
      <CardHeader className="pb-2">
        <CardTitle className="text-base flex flex-wrap items-center justify-between gap-2">
          <span>RPM 调优 · 最佳 RPM 发现</span>
          <div className="flex items-center gap-2 text-xs font-normal">
            <span className="text-muted-foreground">429 率阈值</span>
            <Input
              type="number"
              min={0}
              max={100}
              step={0.5}
              value={thresholdPct}
              onChange={(e) =>
                setThresholdPct(Math.max(0, Math.min(100, Number(e.target.value) || 0)))
              }
              className="h-7 w-16"
            />
            <span className="text-muted-foreground">%</span>
          </div>
        </CardTitle>
        <p className="text-xs text-muted-foreground">
          按历史「每分钟 RPM × 该分钟 429 数」分桶，推荐 = 429 率 ≤ 阈值 的最高 RPM。
          需该号曾跑到高 RPM 并撞过 429 才能找到拐点；数据自本次升级后开始积累。
        </p>
      </CardHeader>
      <CardContent>
        {isLoading ? (
          <div className="text-sm text-muted-foreground py-6 text-center">
            加载分析数据…
          </div>
        ) : entries.length === 0 ? (
          <div className="text-sm text-muted-foreground py-6 text-center">
            暂无 RPM 分析数据（需运行并积累至少 {MIN_SAMPLES} 分钟采样）
          </div>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead className="w-8" />
                <TableHead>凭据</TableHead>
                <TableHead className="text-right">当前上限</TableHead>
                <TableHead className="text-right">观测峰值</TableHead>
                <TableHead className="text-right">累计 429</TableHead>
                <TableHead className="text-right">推荐 RPM</TableHead>
                <TableHead className="text-right">操作</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {entries.map((entry) => {
                const info = credMap.get(entry.id)
                const rec = recommend(entry, thresholdPct)
                const curCap = info?.credentialRpm ?? null
                const expanded = expandedId === entry.id
                const canApply =
                  rec.value != null && rec.value > 0 && rec.value !== curCap
                return (
                  <Fragment key={entry.id}>
                    <TableRow
                      className="cursor-pointer"
                      onClick={() =>
                        setExpandedId(expanded ? null : entry.id)
                      }
                    >
                      <TableCell className="py-2">
                        {expanded ? (
                          <ChevronDown className="h-4 w-4 text-muted-foreground" />
                        ) : (
                          <ChevronRight className="h-4 w-4 text-muted-foreground" />
                        )}
                      </TableCell>
                      <TableCell className="py-2">
                        <div className="flex flex-col">
                          <span className="font-mono text-xs">#{entry.id}</span>
                          <span className="text-xs text-muted-foreground truncate max-w-[160px]">
                            {entry.email || info?.email || '—'}
                          </span>
                        </div>
                      </TableCell>
                      <TableCell className="py-2 text-right text-sm">
                        {curCap != null ? (
                          <span className="font-mono">{curCap}</span>
                        ) : (
                          <span className="text-muted-foreground text-xs">
                            全局/无限制
                          </span>
                        )}
                      </TableCell>
                      <TableCell className="py-2 text-right font-mono text-sm">
                        {entry.observedPeakRpm}
                      </TableCell>
                      <TableCell className="py-2 text-right font-mono text-sm">
                        <span
                          className={
                            (info?.rateLimitCount ?? 0) > 0 ? 'text-red-500' : ''
                          }
                        >
                          {info?.rateLimitCount ?? 0}
                        </span>
                      </TableCell>
                      <TableCell className="py-2 text-right">
                        {rec.value == null ? (
                          <span className="text-xs text-muted-foreground">
                            数据不足
                          </span>
                        ) : (
                          <div className="flex items-center justify-end gap-1">
                            <Badge
                              className={
                                rec.hitLimit
                                  ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
                                  : 'bg-amber-500/15 text-amber-700 dark:text-amber-400'
                              }
                            >
                              {rec.value}
                            </Badge>
                            {!rec.hitLimit && (
                              <span
                                className="text-[10px] text-muted-foreground"
                                title="观测区间内未触发超阈值 429，推荐值为已观测峰值，实际上限可能更高"
                              >
                                未触限
                              </span>
                            )}
                          </div>
                        )}
                      </TableCell>
                      <TableCell
                        className="py-2 text-right"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <Button
                          size="sm"
                          variant="outline"
                          className="h-7"
                          disabled={!canApply || applyMut.isPending}
                          onClick={() =>
                            rec.value != null && handleApply(entry.id, rec.value)
                          }
                          title={
                            rec.value === curCap
                              ? '当前上限已等于推荐值'
                              : '将推荐值写入该号 RPM 上限'
                          }
                        >
                          {applyMut.isPending &&
                          applyMut.variables?.id === entry.id ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <>
                              <Check className="h-3.5 w-3.5 mr-1" />
                              应用
                            </>
                          )}
                        </Button>
                      </TableCell>
                    </TableRow>
                    {expanded && (
                      <TableRow>
                        <TableCell colSpan={7} className="bg-muted/30">
                          <BucketChart
                            entry={entry}
                            thresholdPct={thresholdPct}
                            recommended={rec.value}
                          />
                        </TableCell>
                      </TableRow>
                    )}
                  </Fragment>
                )
              })}
            </TableBody>
          </Table>
        )}
      </CardContent>
    </Card>
  )
}

function BucketChart({
  entry,
  thresholdPct,
  recommended,
}: {
  entry: RpmAnalysisEntry
  thresholdPct: number
  recommended: number | null
}) {
  const buckets = entry.buckets
  if (buckets.length === 0) {
    return (
      <div className="text-xs text-muted-foreground py-3">该号暂无分桶数据</div>
    )
  }
  // 429 率柱状图：高度 = rate429（按桶内最大率缩放，至少留出可见高度）
  const maxRate = Math.max(
    thresholdPct / 100,
    ...buckets.map((b) => b.rate429),
    0.0001
  )
  const t = thresholdPct / 100

  return (
    <div className="py-3 space-y-2">
      <div className="text-xs text-muted-foreground">
        桶宽 {entry.bucketWidth} RPM · 样本 {entry.totalMinutes} 分钟 · 柱高 = 429 率（虚线 = 阈值 {thresholdPct}%）
      </div>
      <div className="flex items-end gap-1 h-32">
        {buckets.map((b) => {
          const h = (b.rate429 / maxRate) * 100
          const over = b.rate429 > t
          const isRec =
            recommended != null && b.rpmHigh === recommended
          return (
            <div
              key={b.rpmLow}
              className="flex-1 flex flex-col items-center justify-end h-full min-w-[18px]"
              title={`RPM ${b.rpmLow}–${b.rpmHigh}\n429率 ${(b.rate429 * 100).toFixed(2)}%\n样本 ${b.minutes} 分钟 · 请求 ${b.requests} · 429 ${b.rl429}`}
            >
              <span className="text-[9px] text-muted-foreground mb-0.5">
                {(b.rate429 * 100).toFixed(0)}%
              </span>
              <div
                className={`w-full rounded-t ${
                  over
                    ? 'bg-red-500/70'
                    : isRec
                      ? 'bg-emerald-600'
                      : 'bg-emerald-500/50'
                }`}
                style={{ height: `${Math.max(2, h)}%` }}
              />
              <span className="text-[9px] text-muted-foreground mt-0.5 font-mono">
                {b.rpmLow}
              </span>
            </div>
          )
        })}
      </div>
      <div className="flex flex-wrap gap-3 text-[10px] text-muted-foreground">
        <span className="flex items-center gap-1">
          <span className="inline-block w-3 h-2 rounded-sm bg-emerald-500/50" />
          ≤ 阈值
        </span>
        <span className="flex items-center gap-1">
          <span className="inline-block w-3 h-2 rounded-sm bg-emerald-600" />
          推荐桶
        </span>
        <span className="flex items-center gap-1">
          <span className="inline-block w-3 h-2 rounded-sm bg-red-500/70" />
          超阈值（拐点）
        </span>
      </div>
    </div>
  )
}
