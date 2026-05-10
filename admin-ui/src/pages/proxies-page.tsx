import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ColumnDef, RowSelectionState } from '@tanstack/react-table'
import { toast } from 'sonner'
import {
  AlertTriangle,
  CalendarClock,
  CheckCircle2,
  PlayCircle,
  Plus,
  RefreshCw,
  Sliders,
  Trash2,
  Unlink,
  XCircle,
} from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { DataTable } from '@/components/data-table'
import { ImportProxyDialog } from '@/components/import-proxy-dialog'
import {
  batchDeleteProxies,
  batchExtendProxies,
  batchSetProxySlots,
  batchTestProxies,
  batchUnbindProxies,
  getProxies,
  getProxyAlerts,
  rotateProxiesNow,
  testProxy,
} from '@/api/proxies'
import { extractErrorMessage } from '@/lib/utils'
import type {
  ProxyAlertItem,
  ProxyEntryItem,
  ProxyTestResult,
} from '@/types/api'

function formatRemaining(secs: number): string {
  if (secs <= 0) return '已过期'
  const days = Math.floor(secs / 86400)
  const hours = Math.floor((secs % 86400) / 3600)
  if (days > 0) return `${days}d ${hours}h`
  const mins = Math.floor((secs % 3600) / 60)
  return `${hours}h ${mins}m`
}

function statusBadge(status: string, remainingSecs: number) {
  if (status === 'expired') {
    return (
      <Badge variant="outline" className="border-muted text-muted-foreground">
        已过期
      </Badge>
    )
  }
  if (status === 'expiring' || (status === 'active' && remainingSecs < 86400)) {
    return (
      <Badge className="bg-yellow-500/15 text-yellow-700 dark:text-yellow-400">
        即将到期
      </Badge>
    )
  }
  if (status === 'full') {
    return (
      <Badge variant="secondary">已满</Badge>
    )
  }
  return (
    <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
      可用
    </Badge>
  )
}

function maskUrl(url: string, username?: string | null): string {
  // 不输出账号密码到 URL，仅显示 scheme://host:port + (有/无 user)
  return username ? `${url}  (auth: ${username})` : url
}

export function ProxiesPage() {
  const queryClient = useQueryClient()
  const { data, isLoading, refetch } = useQuery({
    queryKey: ['proxies'],
    queryFn: getProxies,
    refetchInterval: 30_000,
  })
  const { data: alertsData } = useQuery({
    queryKey: ['proxy-alerts'],
    queryFn: getProxyAlerts,
    refetchInterval: 30_000,
  })

  const [importOpen, setImportOpen] = useState(false)
  const [rowSelection, setRowSelection] = useState<RowSelectionState>({})
  const [showAlerts, setShowAlerts] = useState(false)
  const [statusFilter, setStatusFilter] =
    useState<'all' | 'available' | 'expiring' | 'expired' | 'full'>('all')
  const [searchQuery, setSearchQuery] = useState('')
  const [slotsDialogOpen, setSlotsDialogOpen] = useState(false)
  const [extendDialogOpen, setExtendDialogOpen] = useState(false)
  const [batchSlotsValue, setBatchSlotsValue] = useState(1)
  const [batchExpiresAt, setBatchExpiresAt] = useState('')
  const [testResults, setTestResults] = useState<Map<string, ProxyTestResult>>(
    new Map()
  )
  const [testingIds, setTestingIds] = useState<Set<string>>(new Set())

  const selectedIds = useMemo(
    () => Object.keys(rowSelection).filter((k) => rowSelection[k]),
    [rowSelection]
  )

  const proxies = data?.proxies ?? []

  // 与统计卡口径对齐：「即将过期」含 status=expiring 与 active 但 <24h
  const isExpiringSoon = (p: ProxyEntryItem) =>
    p.status === 'expiring' || (p.status === 'active' && p.remainingSecs < 86400)

  const statusCounts = useMemo(
    () => ({
      all: proxies.length,
      available: proxies.filter((p) => p.status === 'active' || p.status === 'expiring').length,
      expiring: proxies.filter(isExpiringSoon).length,
      expired: proxies.filter((p) => p.status === 'expired').length,
      full: proxies.filter((p) => p.status === 'full').length,
    }),
    [proxies]
  )

  const filteredProxies = useMemo(() => {
    let list = proxies
    switch (statusFilter) {
      case 'available':
        list = list.filter((p) => p.status === 'active' || p.status === 'expiring')
        break
      case 'expiring':
        list = list.filter(isExpiringSoon)
        break
      case 'expired':
        list = list.filter((p) => p.status === 'expired')
        break
      case 'full':
        list = list.filter((p) => p.status === 'full')
        break
    }
    const q = searchQuery.trim().toLowerCase()
    if (q) {
      list = list.filter((p) => {
        const haystack = [p.id, p.url, p.username ?? '', p.label ?? '']
          .join('\n')
          .toLowerCase()
        return haystack.includes(q)
      })
    }
    return list
  }, [proxies, statusFilter, searchQuery])

  const columns: ColumnDef<ProxyEntryItem, unknown>[] = useMemo(
    () => [
      {
        id: 'select',
        header: ({ table }) => (
          <Checkbox
            checked={
              table.getIsAllPageRowsSelected() ||
              (table.getIsSomePageRowsSelected() && 'indeterminate')
            }
            onCheckedChange={(value) =>
              table.toggleAllPageRowsSelected(!!value)
            }
            aria-label="全选"
          />
        ),
        cell: ({ row }) => (
          <Checkbox
            checked={row.getIsSelected()}
            onCheckedChange={(value) => row.toggleSelected(!!value)}
            aria-label="选择行"
          />
        ),
        enableSorting: false,
      },
      {
        accessorKey: 'id',
        header: 'ID',
        cell: ({ row }) => (
          <span className="font-mono text-xs">{row.original.id}</span>
        ),
      },
      {
        id: 'address',
        header: '地址 (脱敏)',
        cell: ({ row }) => (
          <div className="text-xs font-mono">
            {maskUrl(row.original.url, row.original.username)}
          </div>
        ),
      },
      {
        accessorKey: 'usedSlots',
        header: '槽位',
        cell: ({ row }) => (
          <span className="font-mono text-xs">
            {row.original.usedSlots}/{row.original.slots}
          </span>
        ),
      },
      {
        accessorKey: 'remainingSecs',
        header: '剩余有效期',
        cell: ({ row }) => (
          <span className="text-xs">
            {formatRemaining(row.original.remainingSecs)}
          </span>
        ),
      },
      {
        accessorKey: 'status',
        header: '状态',
        cell: ({ row }) =>
          statusBadge(row.original.status, row.original.remainingSecs),
      },
      {
        id: 'bound',
        header: '绑定凭据',
        cell: ({ row }) =>
          row.original.boundCredentialIds.length > 0 ? (
            <span className="font-mono text-xs">
              {row.original.boundCredentialIds.join(', ')}
            </span>
          ) : (
            <span className="text-xs text-muted-foreground">—</span>
          ),
      },
      {
        accessorKey: 'expiresAt',
        header: '到期时间',
        cell: ({ row }) => (
          <span className="text-xs text-muted-foreground">
            {new Date(row.original.expiresAt).toLocaleString()}
          </span>
        ),
      },
      {
        id: 'test',
        header: '测试',
        cell: ({ row }) => {
          const id = row.original.id
          if (testingIds.has(id)) {
            return <span className="text-xs text-yellow-600">测试中…</span>
          }
          const r = testResults.get(id)
          if (!r) {
            return <span className="text-xs text-muted-foreground">—</span>
          }
          if (!r.ok) {
            return (
              <div
                className="text-xs text-red-500 max-w-[140px] truncate"
                title={r.error ?? '失败'}
              >
                ✗ {r.error || '失败'}
              </div>
            )
          }
          return (
            <div className="text-xs flex flex-col gap-0.5">
              <span className="font-mono text-emerald-600">
                {r.ip ?? 'OK'}
              </span>
              <span className="text-muted-foreground">{r.elapsedMs} ms</span>
            </div>
          )
        },
      },
      {
        id: 'actions',
        header: '操作',
        cell: ({ row }) => {
          const id = row.original.id
          return (
            <Button
              size="sm"
              variant="outline"
              className="h-7 px-2 text-xs"
              disabled={testingIds.has(id)}
              onClick={() => handleTestSingle(id)}
            >
              {testingIds.has(id) ? '测试中…' : '测试'}
            </Button>
          )
        },
        enableSorting: false,
      },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [testResults, testingIds]
  )

  const handleTestSingle = async (id: string) => {
    setTestingIds((prev) => {
      const next = new Set(prev)
      next.add(id)
      return next
    })
    try {
      const r = await testProxy(id)
      setTestResults((prev) => {
        const next = new Map(prev)
        next.set(id, r)
        return next
      })
      if (r.ok) toast.success(`代理 ${id} 可用：${r.ip ?? ''} (${r.elapsedMs}ms)`)
      else toast.error(`代理 ${id} 测试失败：${r.error ?? '未知'}`)
    } catch (e) {
      toast.error(`测试失败：${extractErrorMessage(e)}`)
    } finally {
      setTestingIds((prev) => {
        const next = new Set(prev)
        next.delete(id)
        return next
      })
    }
  }

  const handleBatchTest = async () => {
    if (selectedIds.length === 0) return
    setTestingIds((prev) => {
      const next = new Set(prev)
      selectedIds.forEach((id) => next.add(id))
      return next
    })
    try {
      const resp = await batchTestProxies(selectedIds)
      setTestResults((prev) => {
        const next = new Map(prev)
        resp.results.forEach((r) => next.set(r.id, r))
        return next
      })
      if (resp.failCount === 0) {
        toast.success(`已测试 ${resp.okCount} 个代理，全部可用`)
      } else {
        toast.warning(
          `测试完成：可用 ${resp.okCount} / 失败 ${resp.failCount}（共 ${resp.total}）`
        )
      }
    } catch (e) {
      toast.error(`批量测试失败：${extractErrorMessage(e)}`)
    } finally {
      setTestingIds((prev) => {
        const next = new Set(prev)
        selectedIds.forEach((id) => next.delete(id))
        return next
      })
    }
  }

  const batchDelete = useMutation({
    mutationFn: () =>
      batchDeleteProxies({ ids: selectedIds, force: true }),
    onSuccess: (resp) => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      setRowSelection({})
      if (resp.failCount === 0) {
        toast.success(`已删除 ${resp.successCount} 个代理`)
      } else {
        toast.warning(
          `删除完成：成功 ${resp.successCount}，失败 ${resp.failCount}`
        )
      }
    },
    onError: (e) => toast.error(`删除失败：${extractErrorMessage(e)}`),
  })

  const batchUnbind = useMutation({
    mutationFn: () => batchUnbindProxies({ ids: selectedIds }),
    onSuccess: (resp) => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      setRowSelection({})
      toast.success(`已解绑 ${resp.successCount} 个代理`)
    },
    onError: (e) => toast.error(`解绑失败：${extractErrorMessage(e)}`),
  })

  const batchSlots = useMutation({
    mutationFn: () =>
      batchSetProxySlots({
        ids: selectedIds,
        slots: batchSlotsValue,
        force: true,
      }),
    onSuccess: (resp) => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      setSlotsDialogOpen(false)
      setRowSelection({})
      toast.success(
        `已更新 ${resp.successCount} 个代理槽位为 ${batchSlotsValue}`
      )
    },
    onError: (e) => toast.error(`调整槽位失败：${extractErrorMessage(e)}`),
  })

  const batchExtend = useMutation({
    mutationFn: () =>
      batchExtendProxies({
        ids: selectedIds,
        expiresAt: new Date(batchExpiresAt).toISOString(),
      }),
    onSuccess: (resp) => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      setExtendDialogOpen(false)
      setRowSelection({})
      toast.success(`已更新 ${resp.successCount} 个代理到期时间`)
    },
    onError: (e) => toast.error(`延长到期失败：${extractErrorMessage(e)}`),
  })

  const rotateMutation = useMutation({
    mutationFn: rotateProxiesNow,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      queryClient.invalidateQueries({ queryKey: ['proxy-alerts'] })
      toast.success('已触发轮换扫描')
    },
    onError: (e) => toast.error(`触发失败：${extractErrorMessage(e)}`),
  })

  const enabled = data?.enabled ?? false

  return (
    <div className="flex flex-col flex-1 min-h-0">
      {/* 顶栏 */}
      <div className="flex items-center justify-between mb-4 shrink-0">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold">代理池</h1>
          {!enabled && (
            <Badge variant="outline" className="border-yellow-400 text-yellow-700">
              未启用（请在 config.json 设置 proxyPoolEnabled=true）
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => setShowAlerts((s) => !s)}
          >
            <AlertTriangle className="h-4 w-4 mr-2" />
            告警
            {alertsData && alertsData.total > 0 && (
              <Badge variant="secondary" className="ml-2 h-5">
                {alertsData.total}
              </Badge>
            )}
          </Button>
          <Button
            variant="ghost"
            size="icon"
            onClick={() => refetch()}
            disabled={isLoading}
          >
            <RefreshCw className={`h-5 w-5 ${isLoading ? 'animate-spin' : ''}`} />
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={() => rotateMutation.mutate()}
            disabled={!enabled || rotateMutation.isPending}
          >
            <PlayCircle className="h-4 w-4 mr-2" />
            立即轮换
          </Button>
          <Button onClick={() => setImportOpen(true)} size="sm" disabled={!enabled}>
            <Plus className="h-4 w-4 mr-2" />
            批量导入
          </Button>
        </div>
      </div>

      {/* 统计卡片 */}
      <div className="grid gap-4 md:grid-cols-4 mb-4">
        <StatCard label="代理总数" value={proxies.length} />
        <StatCard
          label="可用代理"
          value={
            proxies.filter(
              (p) => p.status === 'active' || p.status === 'expiring'
            ).length
          }
        />
        <StatCard
          label="已绑定凭据"
          value={proxies.reduce((s, p) => s + p.usedSlots, 0)}
        />
        <StatCard
          label="即将过期 (<24h)"
          value={
            proxies.filter(
              (p) => p.status === 'expiring' || p.remainingSecs < 86400
            ).length
          }
          tone="warn"
        />
      </div>

      {/* 状态筛选 + 搜索 */}
      <div className="mb-3 flex flex-wrap items-center gap-2">
        {(
          [
            { key: 'all', label: '全部' },
            { key: 'available', label: '可用' },
            { key: 'expiring', label: '即将过期' },
            { key: 'expired', label: '已过期' },
            { key: 'full', label: '已满' },
          ] as const
        ).map(({ key, label }) => (
          <Button
            key={key}
            variant={statusFilter === key ? 'default' : 'outline'}
            size="sm"
            onClick={() => {
              if (statusFilter !== key) setRowSelection({})
              setStatusFilter(key)
            }}
          >
            {label}
            <span className="ml-2 text-xs opacity-70">{statusCounts[key]}</span>
          </Button>
        ))}
        <Input
          type="search"
          placeholder="搜索 IP / 端口 / 账号 / 备注"
          value={searchQuery}
          onChange={(e) => {
            setRowSelection({})
            setSearchQuery(e.target.value)
          }}
          className="ml-2 h-8 w-64 text-xs"
        />
        <span className="text-xs text-muted-foreground">
          匹配 {filteredProxies.length} / 共 {proxies.length}
        </span>
      </div>

      {/* 告警面板 */}
      {showAlerts && alertsData && (
        <Card className="mb-4">
          <CardHeader className="pb-2">
            <CardTitle className="text-sm">最近告警</CardTitle>
          </CardHeader>
          <CardContent className="max-h-64 overflow-auto space-y-1">
            {alertsData.alerts.length === 0 ? (
              <div className="text-sm text-muted-foreground">暂无告警</div>
            ) : (
              alertsData.alerts
                .slice()
                .reverse()
                .map((a, i) => <AlertRow key={i} alert={a} />)
            )}
          </CardContent>
        </Card>
      )}

      {/* 选中操作工具栏 */}
      {selectedIds.length > 0 && (
        <div className="mb-3 flex flex-wrap items-center gap-2 rounded-md border bg-muted/30 px-3 py-2">
          <span className="text-sm">已选 {selectedIds.length} 项</span>
          <span className="flex-1" />
          <Button
            size="sm"
            variant="outline"
            onClick={handleBatchTest}
            disabled={selectedIds.some((id) => testingIds.has(id))}
          >
            <PlayCircle className="h-4 w-4 mr-2" />
            批量测试
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => batchUnbind.mutate()}
            disabled={batchUnbind.isPending}
          >
            <Unlink className="h-4 w-4 mr-2" />
            批量解绑
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => setSlotsDialogOpen(true)}
          >
            <Sliders className="h-4 w-4 mr-2" />
            调槽位
          </Button>
          <Button
            size="sm"
            variant="outline"
            onClick={() => setExtendDialogOpen(true)}
          >
            <CalendarClock className="h-4 w-4 mr-2" />
            延长到期
          </Button>
          <Button
            size="sm"
            variant="destructive"
            onClick={() => {
              if (
                confirm(
                  `确定删除 ${selectedIds.length} 个代理？被绑定的凭据将自动解绑并禁用。`
                )
              ) {
                batchDelete.mutate()
              }
            }}
            disabled={batchDelete.isPending}
          >
            <Trash2 className="h-4 w-4 mr-2" />
            批量删除
          </Button>
        </div>
      )}

      {/* 表格 */}
      <DataTable
        columns={columns}
        data={filteredProxies}
        rowSelection={rowSelection}
        onRowSelectionChange={setRowSelection}
        getRowId={(row) => row.id}
        emptyText={
          enabled
            ? statusFilter === 'all'
              ? '代理池为空，点击"批量导入"添加代理'
              : '当前筛选下没有代理'
            : '代理池未启用'
        }
      />

      {/* 导入对话框 */}
      <ImportProxyDialog open={importOpen} onOpenChange={setImportOpen} />

      {/* 调整槽位对话框 */}
      <Dialog open={slotsDialogOpen} onOpenChange={setSlotsDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>调整槽位</DialogTitle>
            <DialogDescription>
              将选中的 {selectedIds.length} 个代理槽位统一调整。如果新值小于已绑定数，超出部分会被强制解绑（凭据自动禁用）。
            </DialogDescription>
          </DialogHeader>
          <div>
            <label className="text-sm font-medium">新槽位数</label>
            <Input
              type="number"
              min={1}
              value={batchSlotsValue}
              onChange={(e) =>
                setBatchSlotsValue(Math.max(1, Number(e.target.value) || 1))
              }
            />
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setSlotsDialogOpen(false)}
            >
              取消
            </Button>
            <Button
              onClick={() => batchSlots.mutate()}
              disabled={batchSlots.isPending}
            >
              确定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* 延长到期对话框 */}
      <Dialog open={extendDialogOpen} onOpenChange={setExtendDialogOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>延长到期时间</DialogTitle>
            <DialogDescription>
              将选中的 {selectedIds.length} 个代理统一设置为同一到期时间。
            </DialogDescription>
          </DialogHeader>
          <div>
            <label className="text-sm font-medium">到期时间</label>
            <Input
              type="datetime-local"
              value={batchExpiresAt}
              onChange={(e) => setBatchExpiresAt(e.target.value)}
            />
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setExtendDialogOpen(false)}
            >
              取消
            </Button>
            <Button
              onClick={() => batchExtend.mutate()}
              disabled={batchExtend.isPending || !batchExpiresAt}
            >
              确定
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
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
      <CardHeader className="pb-2">
        <CardTitle className="text-sm font-medium text-muted-foreground">
          {label}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <div className={`text-2xl font-bold ${valueClass}`}>{value}</div>
      </CardContent>
    </Card>
  )
}

function AlertRow({ alert }: { alert: ProxyAlertItem }) {
  const Icon =
    alert.level === 'error'
      ? XCircle
      : alert.level === 'warn'
        ? AlertTriangle
        : CheckCircle2
  const colorClass =
    alert.level === 'error'
      ? 'text-red-500'
      : alert.level === 'warn'
        ? 'text-yellow-500'
        : 'text-emerald-500'
  return (
    <div className="flex items-start gap-2 text-xs py-1 border-b last:border-0">
      <Icon className={`h-4 w-4 mt-0.5 ${colorClass}`} />
      <div className="flex-1">
        <div className="text-muted-foreground">
          {new Date(alert.at).toLocaleString()}
        </div>
        <div>{alert.message}</div>
      </div>
    </div>
  )
}
