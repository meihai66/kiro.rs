import { useEffect, useMemo, useRef, useState } from 'react'
import { CheckCircle2, Loader2, Plus, Upload, Wallet } from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ColumnDef, RowSelectionState } from '@tanstack/react-table'
import { toast } from 'sonner'

import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { BalanceDialog } from '@/components/balance-dialog'
import {
  BatchVerifyDialog,
  type VerifyResult,
} from '@/components/batch-verify-dialog'
import { BindProxyDialog } from '@/components/bind-proxy-dialog'
import { CredentialCard } from '@/components/credential-card'
import { DataTable } from '@/components/data-table'
import { ImportTokenJsonDialog } from '@/components/import-token-json-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  deleteCredential as deleteCredentialApi,
  forceRefreshToken as forceRefreshTokenApi,
  getCredentialBalance,
  setCredentialAllowOveruse as setCredentialAllowOveruseApi,
  setCredentialDisabled as setCredentialDisabledApi,
  setCredentialPriority as setCredentialPriorityApi,
  setOveragePreference,
} from '@/api/credentials'
import {
  getProxies,
  testProxy as testProxyApi,
  unbindCredentialProxy,
} from '@/api/proxies'

type BatchAction =
  | 'enable'
  | 'disable'
  | 'setPriority'
  | 'overageOn'
  | 'overageOff'
  | 'allowOveruseOn'
  | 'allowOveruseOff'
  | 'verify'
  | 'refreshToken'
  | 'queryBalance'
  | 'delete'
import {
  useCachedBalances,
  useCredentials,
  useDeleteCredential,
  useForceRefreshToken,
  useSetAllowOveruse,
  useSetDisabled,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import type {
  BalanceResponse,
  CachedBalanceInfo,
  CredentialStatusItem,
  ProxyEntryItem,
  ProxyTestResult,
} from '@/types/api'

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未'
  const date = new Date(lastUsedAt)
  const diff = Date.now() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds}s 前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m 前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours}h 前`
  return `${Math.floor(hours / 24)}d 前`
}

interface CellContext {
  cachedBalanceMap: Map<number, CachedBalanceInfo>
  liveBalanceMap: Map<number, BalanceResponse>
  loadingBalanceIds: Set<number>
  proxyMap: Map<string, ProxyEntryItem>
  proxyTestResults: Map<string, ProxyTestResult>
  testingProxyIds: Set<string>
  onViewBalance: (id: number, force: boolean) => void
  onForceRefresh: (id: number) => void
  onBindProxy: (id: number) => void
  onUnbindProxy: (id: number) => void
  onTestProxy: (slotId: string) => void
  onToggleOverage: (cred: CredentialStatusItem) => void
  onToggleAllowOveruse: (id: number, allow: boolean) => void
  onShowDetail: (cred: CredentialStatusItem) => void
  onToggleDisabled: (cred: CredentialStatusItem) => void
  onDelete: (cred: CredentialStatusItem) => void
}

type StatusFilter = 'normal' | 'disabled' | 'failing' | 'rateLimited'
const STATUS_OPTIONS: { value: StatusFilter; label: string }[] = [
  { value: 'normal', label: '正常' },
  { value: 'disabled', label: '已禁用' },
  { value: 'failing', label: '失败' },
  { value: 'rateLimited', label: '限流中' },
]

/** 单一真源：根据快照字段计算凭据"逻辑状态"。
 * - disabled: 强制禁用
 * - rateLimited: 当前处于 RateLimitExceeded 冷却中
 * - failing: 连续失败计数 > 0 或 token 刷新失败
 * - normal: 其他
 */
function classifyCredential(
  c: CredentialStatusItem
): 'disabled' | 'rateLimited' | 'failing' | 'normal' {
  if (c.disabled) return 'disabled'
  // 当前正处于 RateLimitExceeded 冷却 → 显示为"限流中"
  const reason = (c.cooldownReason ?? '').toString()
  if (
    reason &&
    (c.cooldownRemainingSecs ?? 0) > 0 &&
    /RateLimitExceeded/i.test(reason)
  ) {
    return 'rateLimited'
  }
  if (c.failureCount > 0 || c.refreshFailureCount > 0) return 'failing'
  return 'normal'
}

// 从 url 抠出 host:port 用于显示
function shortProxyAddr(url: string): string {
  if (!url) return ''
  if (!url.includes('://')) return url
  try {
    const u = new URL(url)
    return u.host
  } catch {
    return url
  }
}

function buildColumns(ctx: CellContext): ColumnDef<CredentialStatusItem, unknown>[] {
  return [
    {
      id: 'select',
      header: ({ table }) => (
        <Checkbox
          checked={
            table.getIsAllPageRowsSelected() ||
            (table.getIsSomePageRowsSelected() && 'indeterminate')
          }
          onCheckedChange={(v) => table.toggleAllPageRowsSelected(!!v)}
        />
      ),
      cell: ({ row }) => (
        <Checkbox
          checked={row.getIsSelected()}
          onCheckedChange={(v) => row.toggleSelected(!!v)}
        />
      ),
      enableSorting: false,
    },
    {
      id: 'rowNumber',
      header: '#',
      cell: ({ row, table }) => {
        const p = table.getState().pagination
        return (
          <span className="font-mono text-xs text-muted-foreground">
            {row.index - p.pageIndex * p.pageSize + 1}
          </span>
        )
      },
      enableSorting: false,
    },
    {
      accessorKey: 'email',
      header: '邮箱',
      cell: ({ row }) => {
        const c = row.original
        return (
          <span
            className="inline-flex items-center gap-1.5 text-xs max-w-[220px]"
            title={
              (c.email ?? '') +
              (c.allowOveruse ? '（已开启允许超额使用）' : '')
            }
          >
            {c.allowOveruse && (
              <span
                className="inline-block h-2 w-2 rounded-full bg-orange-500 shrink-0"
                title="允许超额使用：开"
              />
            )}
            <span className="truncate">
              {c.email || <span className="text-muted-foreground">—</span>}
            </span>
          </span>
        )
      },
    },
    {
      id: 'status',
      header: '状态',
      cell: ({ row }) => {
        const c = row.original
        const cls = classifyCredential(c)
        if (cls === 'disabled') {
          return (
            <Badge
              variant="destructive"
              className="text-xs"
              title={c.disabledReason ?? undefined}
            >
              已禁用{c.disabledReason ? ` · ${c.disabledReason}` : ''}
            </Badge>
          )
        }
        if (cls === 'rateLimited') {
          const remaining = c.cooldownRemainingSecs ?? 0
          return (
            <Badge
              className="bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 text-xs"
              title={`429 限流冷却中，剩余约 ${remaining}s`}
            >
              限流 · {remaining}s
            </Badge>
          )
        }
        if (cls === 'failing') {
          const parts: string[] = []
          if (c.failureCount > 0) parts.push(`API ${c.failureCount}`)
          if (c.refreshFailureCount > 0)
            parts.push(`刷 ${c.refreshFailureCount}`)
          return (
            <Badge
              className="bg-red-500/15 text-red-700 dark:text-red-400 text-xs"
              title="连续失败计数，成功一次后自动清零"
            >
              失败 · {parts.join(' / ')}
            </Badge>
          )
        }
        return (
          <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 text-xs">
            正常
          </Badge>
        )
      },
    },
    {
      id: 'reqStats',
      header: '统计',
      cell: ({ row }) => {
        const c = row.original
        return (
          <span className="font-mono text-xs whitespace-nowrap">
            <span className="text-emerald-600">{c.successCount}</span>
            <span className="text-muted-foreground mx-1">/</span>
            <span
              className={c.rateLimitCount > 0 ? 'text-yellow-600' : 'text-muted-foreground'}
            >
              {c.rateLimitCount}
            </span>
            <span className="text-muted-foreground mx-1">/</span>
            <span
              className={c.failureCount > 0 ? 'text-red-500' : 'text-muted-foreground'}
            >
              {c.failureCount}
            </span>
          </span>
        )
      },
    },
    {
      accessorKey: 'authMethod',
      header: '认证',
      cell: ({ row }) => (
        <span className="text-xs">
          {row.original.authMethod ?? (
            <span className="text-muted-foreground">—</span>
          )}
        </span>
      ),
    },
    {
      accessorKey: 'priority',
      header: '优先级',
      cell: ({ row }) => (
        <span className="font-mono text-xs">{row.original.priority}</span>
      ),
    },
    {
      id: 'balance',
      header: '已用 / 额度',
      cell: ({ row }) => {
        const id = row.original.id
        const live = ctx.liveBalanceMap.get(id)
        const cached = ctx.cachedBalanceMap.get(id)
        const loading = ctx.loadingBalanceIds.has(id)
        if (loading) return <span className="text-xs">…</span>
        const limit = live?.usageLimit ?? cached?.usageLimit ?? 0
        if (limit <= 0) {
          return <span className="text-xs text-muted-foreground">—</span>
        }
        // 已使用 = limit - remaining；当 remaining < 0 时表示已超额，超出部分为 |remaining|
        const remaining = live?.remaining ?? cached?.remaining ?? 0
        const baseUsed = Math.min(limit, Math.max(0, limit - remaining))
        const overage = remaining < 0 ? -remaining : 0
        const totalUsed = baseUsed + overage
        const pct = (totalUsed / limit) * 100
        const color =
          overage > 0
            ? 'text-red-500'
            : pct >= 90
              ? 'text-red-500'
              : pct >= 70
                ? 'text-yellow-600'
                : 'text-emerald-600'
        return (
          <span
            className={`text-xs font-mono whitespace-nowrap ${color}`}
            title={`已用 ${totalUsed.toFixed(2)}（含超额 ${overage.toFixed(2)}），合计 ${pct.toFixed(1)}%`}
          >
            {totalUsed.toFixed(1)} / {limit.toFixed(0)}
            {overage > 0 && (
              <span className="ml-1 text-[10px] font-semibold text-red-600">
                超 +{overage.toFixed(1)}
              </span>
            )}
          </span>
        )
      },
    },
    {
      id: 'proxy',
      header: '代理',
      cell: ({ row }) => {
        const c = row.original
        const slot = c.proxySlotId
        if (!slot) {
          return (
            <div className="flex flex-col items-start gap-1">
              <Badge className="bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 text-xs">
                未绑定
              </Badge>
              <Button
                size="sm"
                variant="outline"
                className="h-6 px-2 text-xs"
                onClick={() => ctx.onBindProxy(c.id)}
              >
                绑定
              </Button>
            </div>
          )
        }
        const proxy = ctx.proxyMap.get(slot)
        const display = proxy ? shortProxyAddr(proxy.url) : slot
        const testing = ctx.testingProxyIds.has(slot)
        const test = ctx.proxyTestResults.get(slot)
        return (
          <div className="flex flex-col gap-1 min-w-[180px]">
            <span className="font-mono text-xs" title={proxy?.url ?? slot}>
              {display}
            </span>
            {testing ? (
              <span className="text-[11px] text-yellow-600">测试中…</span>
            ) : test ? (
              test.ok ? (
                <span
                  className="text-[11px] text-emerald-600"
                  title={`出口 ${test.ip ?? '?'} ${test.elapsedMs}ms`}
                >
                  ✓ {test.ip ?? 'OK'} · {test.elapsedMs}ms
                </span>
              ) : (
                <span
                  className="text-[11px] text-red-500 truncate max-w-[180px]"
                  title={test.error ?? '失败'}
                >
                  ✗ {test.error || '失败'}
                </span>
              )
            ) : null}
            <div className="flex gap-1">
              <Button
                size="sm"
                variant="outline"
                className="h-6 px-2 text-xs"
                disabled={testing}
                onClick={() => ctx.onTestProxy(slot)}
              >
                测试
              </Button>
              <Button
                size="sm"
                variant="ghost"
                className="h-6 px-2 text-xs text-destructive"
                onClick={() => ctx.onUnbindProxy(c.id)}
              >
                解绑
              </Button>
            </div>
          </div>
        )
      },
    },
    {
      accessorKey: 'inFlight',
      header: '并发',
      cell: ({ row }) => {
        const v = row.original.inFlight
        return (
          <span
            className={`font-mono text-xs ${v > 0 ? 'font-bold text-emerald-600' : 'text-muted-foreground'}`}
          >
            {v}
          </span>
        )
      },
    },
    {
      accessorKey: 'rpm',
      header: 'RPM',
      cell: ({ row }) => {
        const c = row.original
        const cap = c.credentialRpm ?? null
        return (
          <span
            className="font-mono text-xs whitespace-nowrap"
            title={
              cap
                ? `当前实时 RPM=${c.rpm}；本号上限 ${cap}/min（凭据级覆盖）`
                : `当前实时 RPM=${c.rpm}；本号沿用全局策略`
            }
          >
            {c.rpm}
            {cap !== null && (
              <span className="ml-1 text-[10px] text-muted-foreground">
                /{cap}
              </span>
            )}
          </span>
        )
      },
    },
    {
      id: 'lastUsed',
      header: '最后调用',
      cell: ({ row }) => (
        <span className="text-xs text-muted-foreground whitespace-nowrap">
          {formatLastUsed(row.original.lastUsedAt)}
        </span>
      ),
    },
    {
      id: 'actions',
      header: '操作',
      cell: ({ row }) => {
        const c = row.original
        return (
          <div className="flex flex-wrap gap-1 min-w-[280px]">
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              onClick={() => ctx.onToggleDisabled(c)}
            >
              {c.disabled ? '启用' : '禁用'}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              onClick={() => ctx.onViewBalance(c.id, true)}
            >
              查余额
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              onClick={() => ctx.onForceRefresh(c.id)}
            >
              刷新
            </Button>
            <button
              type="button"
              onClick={() => ctx.onToggleOverage(c)}
              title={
                c.overageStatus === 'ENABLED'
                  ? '账号超额已开启，点击关闭'
                  : c.overageStatus === 'DISABLED'
                    ? '账号超额已关闭，点击开启'
                    : '账号超额状态未知（先点击查余额获取，或点击此按钮切换）'
              }
              className={
                'inline-flex h-7 items-center justify-center rounded-md px-2 text-xs font-medium text-white whitespace-nowrap transition-colors ' +
                (c.overageStatus === 'ENABLED'
                  ? 'bg-emerald-500 hover:bg-emerald-600'
                  : c.overageStatus === 'DISABLED'
                    ? 'bg-red-500 hover:bg-red-600'
                    : 'bg-muted-foreground/60 hover:bg-muted-foreground/80')
              }
            >
              {c.overageStatus === 'ENABLED'
                ? '超额开'
                : c.overageStatus === 'DISABLED'
                  ? '超额关'
                  : '超额?'}
            </button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              onClick={() => ctx.onShowDetail(c)}
            >
              详情
            </Button>
            <Button
              size="sm"
              variant="destructive"
              className="h-7 px-2 text-xs"
              disabled={!c.disabled}
              title={c.disabled ? '删除' : '需先禁用才能删除'}
              onClick={() => ctx.onDelete(c)}
            >
              删除
            </Button>
          </div>
        )
      },
      enableSorting: false,
    },
  ]
}

export function CredentialsPage() {
  const queryClient = useQueryClient()
  const { data, isLoading, error, refetch } = useCredentials()
  const { data: cachedBalancesData } = useCachedBalances()
  const { mutate: deleteCredential } = useDeleteCredential()
  const { mutate: forceRefreshToken } = useForceRefreshToken()
  const { mutate: setDisabledMut } = useSetDisabled()
  const { mutate: setAllowOveruseMut } = useSetAllowOveruse()

  const [rowSelection, setRowSelection] = useState<RowSelectionState>({})
  const [batchAction, setBatchAction] = useState<BatchAction>('enable')
  const [batchPriorityValue, setBatchPriorityValue] = useState(0)
  const [rangeStart, setRangeStart] = useState('')
  const [rangeEnd, setRangeEnd] = useState('')
  const [visibleRows, setVisibleRows] = useState<CredentialStatusItem[]>([])
  // 多选筛选：空集合表示"全部"
  const [statusFilters, setStatusFilters] = useState<Set<StatusFilter>>(
    () => new Set()
  )
  const [usageLimitFilters, setUsageLimitFilters] = useState<Set<string>>(
    () => new Set()
  )

  const toggleStatusFilter = (v: StatusFilter) => {
    setStatusFilters((prev) => {
      const next = new Set(prev)
      if (next.has(v)) next.delete(v)
      else next.add(v)
      return next
    })
  }
  const toggleUsageLimitFilter = (v: string) => {
    setUsageLimitFilters((prev) => {
      const next = new Set(prev)
      if (next.has(v)) next.delete(v)
      else next.add(v)
      return next
    })
  }
  const [addOpen, setAddOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const [verifyOpen, setVerifyOpen] = useState(false)
  const [bindProxyId, setBindProxyId] = useState<number | null>(null)
  const [detailCred, setDetailCred] = useState<CredentialStatusItem | null>(
    null
  )
  const [balanceTargetId, setBalanceTargetId] = useState<number | null>(null)
  const [balanceForce, setBalanceForce] = useState(false)
  const [balanceDialogOpen, setBalanceDialogOpen] = useState(false)
  const [verifying, setVerifying] = useState(false)
  const [verifyProgress, setVerifyProgress] = useState({ current: 0, total: 0 })
  const [verifyResults, setVerifyResults] = useState<Map<number, VerifyResult>>(
    new Map()
  )
  const [liveBalances, setLiveBalances] = useState<Map<number, BalanceResponse>>(
    new Map()
  )
  const [loadingBalanceIds, setLoadingBalanceIds] = useState<Set<number>>(
    new Set()
  )
  const [proxyTestResults, setProxyTestResults] = useState<
    Map<string, ProxyTestResult>
  >(new Map())
  const [testingProxyIds, setTestingProxyIds] = useState<Set<string>>(new Set())
  const cancelVerifyRef = useRef(false)

  // 拉代理池数据，用于在凭据表格中根据 slotId 查 url
  const { data: proxiesData } = useQuery({
    queryKey: ['proxies'],
    queryFn: getProxies,
    refetchInterval: 60_000,
  })
  const proxyMap = useMemo(
    () =>
      new Map<string, ProxyEntryItem>(
        (proxiesData?.proxies ?? []).map((p) => [p.id, p])
      ),
    [proxiesData]
  )

  const cachedBalanceMap = useMemo(
    () =>
      new Map<number, CachedBalanceInfo>(
        cachedBalancesData?.balances.map((b) => [b.id, b]) ?? []
      ),
    [cachedBalancesData]
  )

  const allCredentials = data?.credentials ?? []

  // 取凭据当前生效的额度上限：优先实时余额，回退到缓存余额
  const limitFor = (c: CredentialStatusItem): number => {
    const live = liveBalances.get(c.id)
    if (live && live.usageLimit > 0) return Math.round(live.usageLimit)
    const cached = cachedBalanceMap.get(c.id)
    return cached?.usageLimit && cached.usageLimit > 0
      ? Math.round(cached.usageLimit)
      : 0
  }

  // 额度上限选项（动态从实时/缓存余额收集；任一来源命中即纳入）
  const usageLimitOptions = useMemo(() => {
    const set = new Set<number>()
    allCredentials.forEach((c) => {
      const lim = limitFor(c)
      if (lim > 0) set.add(lim)
    })
    return Array.from(set).sort((a, b) => a - b)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allCredentials, cachedBalanceMap, liveBalances])

  // 各状态分类下的凭据数量（用于在筛选标签后展示）
  const statusCountMap = useMemo(() => {
    const counts: Record<StatusFilter, number> = {
      normal: 0,
      disabled: 0,
      failing: 0,
      rateLimited: 0,
    }
    allCredentials.forEach((c) => {
      counts[classifyCredential(c)] += 1
    })
    return counts
  }, [allCredentials])

  // 各额度档位下的凭据数量（含 __unknown__ 桶）
  const usageLimitCountMap = useMemo(() => {
    const map = new Map<string, number>()
    map.set('__unknown__', 0)
    allCredentials.forEach((c) => {
      const lim = limitFor(c)
      const key = lim > 0 ? String(lim) : '__unknown__'
      map.set(key, (map.get(key) ?? 0) + 1)
    })
    return map
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allCredentials, cachedBalanceMap, liveBalances])

  // 应用筛选（多选 OR：空集合表示全部）
  const credentials = useMemo(() => {
    return allCredentials.filter((c) => {
      // 状态：选中的任一标签命中即通过
      if (statusFilters.size > 0) {
        const cls = classifyCredential(c)
        if (!statusFilters.has(cls)) return false
      }
      // 额度：选中的任一档位命中即通过；优先按实时余额匹配
      if (usageLimitFilters.size > 0) {
        const lim = limitFor(c)
        const isUnknown = lim <= 0
        const ok =
          (usageLimitFilters.has('__unknown__') && isUnknown) ||
          (!isUnknown && usageLimitFilters.has(String(lim)))
        if (!ok) return false
      }
      return true
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    allCredentials,
    statusFilters,
    usageLimitFilters,
    cachedBalanceMap,
    liveBalances,
  ])

  const selectedIds = useMemo(
    () => Object.keys(rowSelection).filter((k) => rowSelection[k]).map(Number),
    [rowSelection]
  )

  const handleViewBalance = (id: number, force: boolean) => {
    setBalanceTargetId(id)
    setBalanceForce(force)
    if (force) {
      queryClient.invalidateQueries({ queryKey: ['credential-balance', id] })
    }
    setBalanceDialogOpen(true)
  }

  const handleForceRefresh = (id: number) => {
    forceRefreshToken(id, {
      onSuccess: (r) => toast.success(r.message),
      onError: (e) => toast.error('刷新失败: ' + (e as Error).message),
    })
  }

  const overageMutation = useMutation({
    mutationFn: ({ id, status }: { id: number; status: 'ENABLED' | 'DISABLED' }) =>
      setOveragePreference(id, { overage_status: status }),
    onSuccess: (r) => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      if (r.ok) {
        toast.success(`超额计费已切换为 ${r.overage_status}`)
      } else {
        toast.error(`切换失败：${r.error ?? '未知'}`)
      }
    },
    onError: (e) => toast.error('切换失败: ' + extractErrorMessage(e)),
  })

  const handleToggleOverage = (cred: CredentialStatusItem) => {
    const next: 'ENABLED' | 'DISABLED' =
      cred.overageStatus === 'ENABLED' ? 'DISABLED' : 'ENABLED'
    overageMutation.mutate({ id: cred.id, status: next })
  }

  const handleToggleAllowOveruse = (id: number, allow: boolean) => {
    setAllowOveruseMut(
      { id, allow },
      {
        onSuccess: (r) => toast.success(r.message),
        onError: (e) => toast.error('操作失败: ' + extractErrorMessage(e)),
      }
    )
  }

  const handleToggleDisabled = (cred: CredentialStatusItem) => {
    setDisabledMut(
      { id: cred.id, disabled: !cred.disabled },
      {
        onSuccess: (r) => toast.success(r.message),
        onError: (e) => toast.error('操作失败: ' + (e as Error).message),
      }
    )
  }

  // 按"当前表格页"的行序号区间 [start, end]（1-based）勾选；不跨页
  const applyIdRange = () => {
    if (visibleRows.length === 0) {
      toast.error('当前页无数据')
      return
    }
    const startN = parseInt(rangeStart || '1', 10)
    const endRaw = rangeEnd.trim()
    const endN = endRaw ? parseInt(endRaw, 10) : startN
    if (
      isNaN(startN) ||
      isNaN(endN) ||
      startN < 1 ||
      endN < 1 ||
      startN > endN
    ) {
      toast.error('请输入有效区间，1 ≤ 起 ≤ 止')
      return
    }
    const lo = Math.max(1, startN)
    const hi = Math.min(visibleRows.length, endN)
    if (lo > visibleRows.length) {
      toast.warning(`当前页只有 ${visibleRows.length} 行`)
      return
    }
    const next: RowSelectionState = { ...rowSelection }
    let matched = 0
    for (let i = lo - 1; i <= hi - 1; i++) {
      const row = visibleRows[i]
      if (!row) continue
      next[String(row.id)] = true
      matched++
    }
    setRowSelection(next)
    toast.success(`已勾选当前页第 ${lo}–${hi} 行（${matched} 个）`)
  }

  const handleUnbindProxy = async (credentialId: number) => {
    if (!confirm(`确认解绑凭据 #${credentialId} 的代理？解绑后凭据会被自动禁用。`))
      return
    try {
      await unbindCredentialProxy(credentialId)
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      toast.success(`凭据 #${credentialId} 已解绑代理（已禁用）`)
    } catch (e) {
      toast.error('解绑失败：' + extractErrorMessage(e))
    }
  }

  const handleTestProxy = async (slotId: string) => {
    setTestingProxyIds((prev) => {
      const next = new Set(prev)
      next.add(slotId)
      return next
    })
    try {
      const r = await testProxyApi(slotId)
      setProxyTestResults((prev) => {
        const next = new Map(prev)
        next.set(slotId, r)
        return next
      })
      if (r.ok)
        toast.success(`代理 ${slotId} 可用：${r.ip ?? ''} (${r.elapsedMs}ms)`)
      else toast.error(`代理 ${slotId} 测试失败：${r.error ?? '未知'}`)
    } catch (e) {
      toast.error('测试失败：' + extractErrorMessage(e))
    } finally {
      setTestingProxyIds((prev) => {
        const next = new Set(prev)
        next.delete(slotId)
        return next
      })
    }
  }

  const handleDeleteOne = (cred: CredentialStatusItem) => {
    if (!cred.disabled) {
      toast.error('请先禁用凭据再删除')
      return
    }
    if (!confirm(`确认删除凭据 #${cred.id}（${cred.email || '无邮箱'}）？此操作无法撤销。`))
      return
    deleteCredential(cred.id, {
      onSuccess: (r) => toast.success(r.message),
      onError: (e) => toast.error('删除失败: ' + (e as Error).message),
    })
  }

  const columns = useMemo(
    () =>
      buildColumns({
        cachedBalanceMap,
        liveBalanceMap: liveBalances,
        loadingBalanceIds,
        proxyMap,
        proxyTestResults,
        testingProxyIds,
        onViewBalance: handleViewBalance,
        onForceRefresh: handleForceRefresh,
        onBindProxy: (id) => setBindProxyId(id),
        onUnbindProxy: handleUnbindProxy,
        onTestProxy: handleTestProxy,
        onToggleOverage: handleToggleOverage,
        onToggleAllowOveruse: handleToggleAllowOveruse,
        onShowDetail: (c) => setDetailCred(c),
        onToggleDisabled: handleToggleDisabled,
        onDelete: handleDeleteOne,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      cachedBalanceMap,
      liveBalances,
      loadingBalanceIds,
      proxyMap,
      proxyTestResults,
      testingProxyIds,
      overageMutation.isPending,
    ]
  )

  // 选中变化时重置过期项
  useEffect(() => {
    if (!data?.credentials) return
    const validIds = new Set(data.credentials.map((c) => String(c.id)))
    setRowSelection((prev) => {
      const next: RowSelectionState = {}
      Object.entries(prev).forEach(([k, v]) => {
        if (validIds.has(k)) next[k] = v
      })
      return next
    })
  }, [data?.credentials])

  // ============ 批量操作 ============

  // 工具：并发跑一组 promise，按 settled 结果统计 ok/fail；只在最后 invalidate 一次
  const runBatchConcurrent = async <T,>(
    ids: number[],
    fn: (id: number) => Promise<T>,
    isOk: (r: T) => boolean = () => true
  ): Promise<{ ok: number; fail: number }> => {
    const results = await Promise.allSettled(ids.map((id) => fn(id)))
    let ok = 0
    let fail = 0
    for (const r of results) {
      if (r.status === 'fulfilled' && isOk(r.value)) ok++
      else fail++
    }
    return { ok, fail }
  }

  const handleBatchDelete = async () => {
    const disabled = selectedIds
      .map((id) => credentials.find((c) => c.id === id))
      .filter((c): c is CredentialStatusItem => !!c && c.disabled)
    if (disabled.length === 0) {
      toast.error('选中的凭据中没有已禁用项')
      return
    }
    const skipped = selectedIds.length - disabled.length
    if (
      !confirm(
        `确认删除 ${disabled.length} 个已禁用凭据？${skipped > 0 ? `（${skipped} 个未禁用将跳过）` : ''}`
      )
    )
      return
    const { ok, fail } = await runBatchConcurrent(
      disabled.map((c) => c.id),
      (id) => deleteCredentialApi(id)
    )
    setRowSelection({})
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    queryClient.invalidateQueries({ queryKey: ['cached-balances'] })
    if (fail === 0) toast.success(`已删除 ${ok} 个`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  // 批量启用/禁用
  const handleBatchSetDisabled = async (disabled: boolean) => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      setCredentialDisabledApi(id, disabled)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    if (fail === 0) toast.success(`已${disabled ? '禁用' : '启用'} ${ok} 个`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  // 批量改优先级
  const handleBatchSetPriority = async (priority: number) => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      setCredentialPriorityApi(id, priority)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    if (fail === 0) toast.success(`已设置 ${ok} 个凭据优先级为 ${priority}`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  const [batchRunning, setBatchRunning] = useState(false)
  const runBatchAction = async () => {
    if (batchRunning) return
    if (selectedIds.length === 0) {
      toast.error('请先勾选凭据')
      return
    }
    setBatchRunning(true)
    try {
      switch (batchAction) {
        case 'enable':
          await handleBatchSetDisabled(false)
          break
        case 'disable':
          await handleBatchSetDisabled(true)
          break
        case 'setPriority':
          await handleBatchSetPriority(batchPriorityValue)
          break
        case 'overageOn':
          await handleBatchSetOverage('ENABLED')
          break
        case 'overageOff':
          await handleBatchSetOverage('DISABLED')
          break
        case 'allowOveruseOn':
          await handleBatchSetAllowOveruse(true)
          break
        case 'allowOveruseOff':
          await handleBatchSetAllowOveruse(false)
          break
        case 'verify':
          await handleBatchVerify()
          break
        case 'refreshToken':
          await handleBatchForceRefresh()
          break
        case 'queryBalance':
          await handleQueryBalances()
          break
        case 'delete':
          await handleBatchDelete()
          break
      }
    } finally {
      setBatchRunning(false)
    }
  }

  // 批量设置「允许超额使用」
  const handleBatchSetAllowOveruse = async (allow: boolean) => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      setCredentialAllowOveruseApi(id, allow)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    if (fail === 0)
      toast.success(`已${allow ? '开启' : '关闭'} ${ok} 个凭据的允许超额`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  // 批量超额开/关（账号侧 overage 偏好）
  const handleBatchSetOverage = async (status: 'ENABLED' | 'DISABLED') => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(
      selectedIds,
      (id) => setOveragePreference(id, { overage_status: status }),
      (r) => r.ok
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    if (fail === 0) toast.success(`已切换 ${ok} 个凭据超额计费为 ${status}`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  const handleBatchForceRefresh = async () => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      forceRefreshTokenApi(id)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    if (fail === 0) toast.success(`已刷新 ${ok} 个`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  const handleBatchVerify = async () => {
    if (selectedIds.length === 0) return
    cancelVerifyRef.current = false
    setVerifying(true)
    setVerifyProgress({ current: 0, total: selectedIds.length })
    const initial = new Map<number, VerifyResult>()
    selectedIds.forEach((id) => initial.set(id, { id, status: 'pending' }))
    setVerifyResults(initial)
    setVerifyOpen(true)

    let ok = 0
    for (let i = 0; i < selectedIds.length; i++) {
      if (cancelVerifyRef.current) break
      const id = selectedIds[i]
      setVerifyResults((prev) => {
        const next = new Map(prev)
        next.set(id, { id, status: 'verifying' })
        return next
      })
      try {
        const balance = await getCredentialBalance(id)
        ok++
        setVerifyResults((prev) => {
          const next = new Map(prev)
          next.set(id, {
            id,
            status: 'success',
            usage: `${balance.currentUsage}/${balance.usageLimit}`,
          })
          return next
        })
      } catch (e) {
        setVerifyResults((prev) => {
          const next = new Map(prev)
          next.set(id, {
            id,
            status: 'failed',
            error: extractErrorMessage(e),
          })
          return next
        })
      }
      setVerifyProgress({ current: i + 1, total: selectedIds.length })
      if (i < selectedIds.length - 1 && !cancelVerifyRef.current) {
        await new Promise((r) => setTimeout(r, 2000))
      }
    }
    setVerifying(false)
    if (!cancelVerifyRef.current) toast.success(`验活完成：成功 ${ok}/${selectedIds.length}`)
  }

  // 查询当前页/选中项余额
  const handleQueryBalances = async () => {
    const ids =
      selectedIds.length > 0
        ? selectedIds
        : credentials.filter((c) => !c.disabled).map((c) => c.id)
    if (ids.length === 0) {
      toast.error('没有可查询的凭据')
      return
    }
    let ok = 0
    let fail = 0
    for (const id of ids) {
      setLoadingBalanceIds((prev) => {
        const next = new Set(prev)
        next.add(id)
        return next
      })
      try {
        const b = await getCredentialBalance(id)
        ok++
        setLiveBalances((prev) => {
          const next = new Map(prev)
          next.set(id, b)
          return next
        })
      } catch {
        fail++
      } finally {
        setLoadingBalanceIds((prev) => {
          const next = new Set(prev)
          next.delete(id)
          return next
        })
      }
    }
    if (fail === 0) toast.success(`查询完成：成功 ${ok}/${ids.length}`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-20">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-primary mx-auto mb-4" />
          <p className="text-muted-foreground">加载中…</p>
        </div>
      </div>
    )
  }

  if (error) {
    return (
      <div className="flex items-center justify-center py-20">
        <Card className="w-full max-w-md">
          <CardContent className="pt-6 text-center">
            <div className="text-red-500 mb-4">加载失败</div>
            <p className="text-muted-foreground mb-4">
              {(error as Error).message}
            </p>
            <Button onClick={() => refetch()}>重试</Button>
          </CardContent>
        </Card>
      </div>
    )
  }

  return (
    <>
      {/* 顶栏 */}
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold">凭据管理</h1>
          <Badge variant="secondary">总数 {data?.total ?? 0}</Badge>
          <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
            可用 {data?.available ?? 0}
          </Badge>
        </div>
        <div className="flex items-center gap-2">
          <Button variant="outline" size="sm" onClick={handleQueryBalances}>
            <Wallet className="h-4 w-4 mr-2" />
            查询余额
          </Button>
          <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
            <Upload className="h-4 w-4 mr-2" />
            导入
          </Button>
          <Button size="sm" onClick={() => setAddOpen(true)}>
            <Plus className="h-4 w-4 mr-2" />
            添加
          </Button>
        </div>
      </div>

      {/* 筛选条 — 多选连续按钮组 */}
      <div className="mb-3 flex flex-wrap items-center gap-3 text-xs">
        <div className="flex items-center gap-1">
          <span className="text-muted-foreground mr-1">状态</span>
          <div className="inline-flex rounded-md border overflow-hidden">
            {STATUS_OPTIONS.map((opt, i) => {
              const active = statusFilters.has(opt.value)
              const count = statusCountMap[opt.value]
              return (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => toggleStatusFilter(opt.value)}
                  className={
                    'h-7 px-3 text-xs transition-colors ' +
                    (i > 0 ? 'border-l ' : '') +
                    (active
                      ? 'bg-primary text-primary-foreground'
                      : 'bg-background hover:bg-muted')
                  }
                >
                  {opt.label}
                  <span
                    className={
                      'ml-1 text-[10px] ' +
                      (active ? 'opacity-90' : 'text-muted-foreground')
                    }
                  >
                    ({count})
                  </span>
                </button>
              )
            })}
          </div>
        </div>

        <div className="flex items-center gap-1">
          <span className="text-muted-foreground mr-1">额度</span>
          <div className="inline-flex rounded-md border overflow-hidden">
            {[
              { value: '__unknown__', label: '未知' },
              ...usageLimitOptions.map((n) => ({
                value: String(n),
                label: String(n),
              })),
            ].map((opt, i) => {
              const active = usageLimitFilters.has(opt.value)
              const count = usageLimitCountMap.get(opt.value) ?? 0
              return (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => toggleUsageLimitFilter(opt.value)}
                  className={
                    'h-7 px-3 text-xs transition-colors ' +
                    (i > 0 ? 'border-l ' : '') +
                    (active
                      ? 'bg-primary text-primary-foreground'
                      : 'bg-background hover:bg-muted')
                  }
                >
                  {opt.label}
                  <span
                    className={
                      'ml-1 text-[10px] ' +
                      (active ? 'opacity-90' : 'text-muted-foreground')
                    }
                  >
                    ({count})
                  </span>
                </button>
              )
            })}
          </div>
        </div>

        <span className="text-muted-foreground">
          匹配 {credentials.length} / 共 {allCredentials.length}
        </span>
        {(statusFilters.size > 0 || usageLimitFilters.size > 0) && (
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-xs"
            onClick={() => {
              setStatusFilters(new Set())
              setUsageLimitFilters(new Set())
            }}
          >
            清除筛选
          </Button>
        )}
      </div>

      {/* 快捷选择 + 选中工具栏 */}
      <div className="mb-3 flex flex-wrap items-center gap-2 rounded-md border bg-muted/30 px-3 py-2">
        <Button
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={() => {
            const next: RowSelectionState = { ...rowSelection }
            visibleRows.forEach((c) => {
              next[String(c.id)] = true
            })
            setRowSelection(next)
          }}
        >
          全选当前页
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={() => {
            const next: RowSelectionState = { ...rowSelection }
            visibleRows.forEach((c) => {
              const k = String(c.id)
              if (next[k]) delete next[k]
              else next[k] = true
            })
            setRowSelection(next)
          }}
        >
          反选
        </Button>
        <Button
          size="sm"
          variant="ghost"
          className="h-7 px-2 text-xs"
          onClick={() => setRowSelection({})}
          disabled={selectedIds.length === 0}
        >
          取消选择
        </Button>

        {/* 当前页行号范围选（不跨页） */}
        <div className="flex items-center gap-1 text-xs">
          <span className="text-muted-foreground">当前页第</span>
          <Input
            type="number"
            min={1}
            max={visibleRows.length || undefined}
            placeholder="起"
            value={rangeStart}
            onChange={(e) => setRangeStart(e.target.value)}
            className="h-7 w-16 text-xs"
          />
          <span className="text-muted-foreground">到</span>
          <Input
            type="number"
            min={1}
            max={visibleRows.length || undefined}
            placeholder="止"
            value={rangeEnd}
            onChange={(e) => setRangeEnd(e.target.value)}
            className="h-7 w-16 text-xs"
            onKeyDown={(e) => {
              if (e.key === 'Enter') applyIdRange()
            }}
          />
          <span className="text-muted-foreground">行</span>
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-xs"
            onClick={applyIdRange}
          >
            勾选
          </Button>
        </div>

        <span className="text-sm ml-1">
          {selectedIds.length > 0 ? (
            <Badge variant="secondary">已选 {selectedIds.length} 项</Badge>
          ) : (
            <span className="text-muted-foreground">未选中</span>
          )}
        </span>

        <span className="flex-1" />

        {/* 批量操作下拉 + 数值输入 + 执行 */}
        <select
          value={batchAction}
          onChange={(e) => setBatchAction(e.target.value as BatchAction)}
          className="h-7 rounded border bg-background px-2 text-xs"
        >
          <option value="enable">启用</option>
          <option value="disable">禁用</option>
          <option value="setPriority">改优先级</option>
          <option value="overageOn">账号超额开</option>
          <option value="overageOff">账号超额关</option>
          <option value="allowOveruseOn">允许超额:开</option>
          <option value="allowOveruseOff">允许超额:关</option>
          <option value="verify">验活</option>
          <option value="refreshToken">刷新</option>
          <option value="queryBalance">查余额</option>
          <option value="delete">删除(仅禁用)</option>
        </select>
        {batchAction === 'setPriority' && (
          <Input
            type="number"
            min={0}
            value={batchPriorityValue}
            onChange={(e) =>
              setBatchPriorityValue(Math.max(0, Number(e.target.value) || 0))
            }
            className="h-7 w-20 text-xs"
          />
        )}
        <Button
          size="sm"
          variant={batchAction === 'delete' ? 'destructive' : 'default'}
          className="h-7 px-3 text-xs"
          onClick={() => runBatchAction()}
          disabled={batchRunning}
          title={
            batchRunning
              ? '执行中…'
              : selectedIds.length === 0
                ? '请先勾选凭据'
                : `对 ${selectedIds.length} 个凭据执行`
          }
        >
          {batchRunning ? (
            <>
              <Loader2 className="h-3 w-3 mr-1 animate-spin" />
              执行中
            </>
          ) : (
            <>
              执行
              {selectedIds.length > 0 && (
                <span className="ml-1 text-[10px] opacity-80">
                  ({selectedIds.length})
                </span>
              )}
            </>
          )}
        </Button>
      </div>

      {/* 表格 */}
      <DataTable
        columns={columns}
        data={credentials}
        rowSelection={rowSelection}
        onRowSelectionChange={setRowSelection}
        getRowId={(row) => String(row.id)}
        emptyText="暂无凭据，请点击右上角导入或添加"
        onVisibleRowsChange={setVisibleRows}
        paginationStorageKey="credentials-table:pagination"
      />

      {verifying && !verifyOpen && (
        <Button
          onClick={() => setVerifyOpen(true)}
          size="sm"
          variant="secondary"
          className="fixed bottom-4 right-4 shadow-lg"
        >
          <CheckCircle2 className="h-4 w-4 mr-2 animate-spin" />
          验活中… {verifyProgress.current}/{verifyProgress.total}
        </Button>
      )}

      {/* 详情对话框（复用 CredentialCard） */}
      <Dialog
        open={detailCred !== null}
        onOpenChange={(open) => !open && setDetailCred(null)}
      >
        <DialogContent className="max-w-3xl max-h-[90vh] p-0 gap-0 flex flex-col">
          <DialogHeader className="px-6 pt-6 pb-3 border-b shrink-0">
            <DialogTitle className="pr-8">
              凭据详情 #{detailCred?.id}
            </DialogTitle>
          </DialogHeader>
          {detailCred && (
            <div className="flex-1 overflow-y-auto p-6">
              <CredentialCard
                credential={detailCred}
                cachedBalance={cachedBalanceMap.get(detailCred.id)}
                onViewBalance={handleViewBalance}
                selected={false}
                onToggleSelect={() => {}}
                balance={liveBalances.get(detailCred.id) ?? null}
                loadingBalance={loadingBalanceIds.has(detailCred.id)}
                onBindProxy={(id) => {
                  setDetailCred(null)
                  setBindProxyId(id)
                }}
              />
            </div>
          )}
        </DialogContent>
      </Dialog>

      {/* 余额对话框 */}
      <BalanceDialog
        credentialId={balanceTargetId}
        open={balanceDialogOpen}
        onOpenChange={(open) => {
          setBalanceDialogOpen(open)
          if (!open) {
            setBalanceForce(false)
            queryClient.invalidateQueries({ queryKey: ['cached-balances'] })
          }
        }}
        forceRefresh={balanceForce}
      />

      <AddCredentialDialog open={addOpen} onOpenChange={setAddOpen} />
      <ImportTokenJsonDialog open={importOpen} onOpenChange={setImportOpen} />
      <BatchVerifyDialog
        open={verifyOpen}
        onOpenChange={setVerifyOpen}
        verifying={verifying}
        progress={verifyProgress}
        results={verifyResults}
        onCancel={() => {
          cancelVerifyRef.current = true
          setVerifying(false)
        }}
      />
      <BindProxyDialog
        credentialId={bindProxyId}
        open={bindProxyId !== null}
        onOpenChange={(open) => {
          if (!open) setBindProxyId(null)
        }}
      />
    </>
  )
}
