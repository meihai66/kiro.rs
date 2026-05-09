import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ColumnDef } from '@tanstack/react-table'
import { toast } from 'sonner'
import { Copy, Plus, RefreshCw } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
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
import {
  createApiKey,
  deleteApiKey,
  listApiKeys,
  updateApiKey,
} from '@/api/api-keys'
import { extractErrorMessage } from '@/lib/utils'
import type { ApiKeyItem, CreateApiKeyRequest } from '@/types/api'

function formatLastUsed(s: string | null | undefined): string {
  if (!s) return '从未'
  const d = new Date(s)
  const diff = Date.now() - d.getTime()
  if (diff < 0) return '刚刚'
  const sec = Math.floor(diff / 1000)
  if (sec < 60) return `${sec}s 前`
  const min = Math.floor(sec / 60)
  if (min < 60) return `${min}m 前`
  const hr = Math.floor(min / 60)
  if (hr < 24) return `${hr}h 前`
  return `${Math.floor(hr / 24)}d 前`
}

export function ApiKeysPage() {
  const queryClient = useQueryClient()
  const { data, isLoading, refetch } = useQuery({
    queryKey: ['api-keys'],
    queryFn: listApiKeys,
    refetchInterval: 5_000,
  })

  const [createOpen, setCreateOpen] = useState(false)
  const [editTarget, setEditTarget] = useState<ApiKeyItem | null>(null)
  const [revealKey, setRevealKey] = useState<string | null>(null)

  const keys = data?.keys ?? []

  const toggleMutation = useMutation({
    mutationFn: ({ id, enabled }: { id: number; enabled: boolean }) =>
      updateApiKey(id, { enabled }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['api-keys'] })
    },
    onError: (e) => toast.error('操作失败：' + extractErrorMessage(e)),
  })

  const deleteMutation = useMutation({
    mutationFn: deleteApiKey,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['api-keys'] })
      toast.success('已删除')
    },
    onError: (e) => toast.error('删除失败：' + extractErrorMessage(e)),
  })

  const columns: ColumnDef<ApiKeyItem, unknown>[] = useMemo(
    () => [
      {
        accessorKey: 'id',
        header: 'ID',
        cell: ({ row }) => (
          <span className="font-mono text-xs">#{row.original.id}</span>
        ),
      },
      {
        accessorKey: 'name',
        header: '名称',
        cell: ({ row }) => (
          <div className="flex flex-col gap-0.5">
            <span className="text-xs font-medium">{row.original.name}</span>
            {row.original.description && (
              <span className="text-[11px] text-muted-foreground truncate max-w-[160px]">
                {row.original.description}
              </span>
            )}
          </div>
        ),
      },
      {
        id: 'key',
        header: 'Key',
        cell: ({ row }) => (
          <div className="flex items-center gap-1">
            <span className="font-mono text-xs">{row.original.keyMasked}</span>
            <Button
              size="icon"
              variant="ghost"
              className="h-6 w-6"
              title="复制完整 Key"
              onClick={() => {
                navigator.clipboard.writeText(row.original.key)
                toast.success('已复制 Key')
              }}
            >
              <Copy className="h-3 w-3" />
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="h-6 px-1 text-[11px]"
              onClick={() => setRevealKey(row.original.key)}
            >
              查看
            </Button>
          </div>
        ),
      },
      {
        id: 'status',
        header: '状态',
        cell: ({ row }) =>
          row.original.enabled ? (
            <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 text-xs">
              启用
            </Badge>
          ) : (
            <Badge variant="destructive" className="text-xs">
              禁用
            </Badge>
          ),
      },
      {
        id: 'concurrent',
        header: '并发',
        cell: ({ row }) => {
          const c = row.original
          const limit = c.maxConcurrent === 0 ? '无限' : c.maxConcurrent
          return (
            <span className="font-mono text-xs">
              {c.inFlight} / {limit}
            </span>
          )
        },
      },
      {
        id: 'cache',
        header: '缓存比例',
        cell: ({ row }) => {
          const c = row.original
          if (c.cacheReadMinPct === 0 && c.cacheReadMaxPct === 0) {
            return <span className="text-xs text-muted-foreground">真实</span>
          }
          return (
            <span className="font-mono text-xs">
              {c.cacheReadMinPct}~{c.cacheReadMaxPct}%
            </span>
          )
        },
      },
      {
        id: 'success',
        header: '成功',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-emerald-600">
            {row.original.successCount}
          </span>
        ),
      },
      {
        id: 'fail',
        header: '失败',
        cell: ({ row }) => (
          <span
            className={`font-mono text-xs ${row.original.failCount > 0 ? 'text-red-500' : 'text-muted-foreground'}`}
          >
            {row.original.failCount}
          </span>
        ),
      },
      {
        id: 'lastUsed',
        header: '最后使用',
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
            <div className="flex gap-1">
              <Button
                size="sm"
                variant="outline"
                className="h-7 px-2 text-xs"
                onClick={() =>
                  toggleMutation.mutate({ id: c.id, enabled: !c.enabled })
                }
              >
                {c.enabled ? '禁用' : '启用'}
              </Button>
              <Button
                size="sm"
                variant="outline"
                className="h-7 px-2 text-xs"
                onClick={() => setEditTarget(c)}
              >
                编辑
              </Button>
              <Button
                size="sm"
                variant="destructive"
                className="h-7 px-2 text-xs"
                onClick={() => {
                  if (confirm(`确定删除 API Key "${c.name}"？`))
                    deleteMutation.mutate(c.id)
                }}
              >
                删除
              </Button>
            </div>
          )
        },
        enableSorting: false,
      },
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  )

  return (
    <div className="flex flex-col flex-1 min-h-0">
      <div className="flex items-center justify-between mb-4 shrink-0">
        <h1 className="text-2xl font-semibold">API Keys</h1>
        <div className="flex items-center gap-2">
          <Button variant="ghost" size="icon" onClick={() => refetch()}>
            <RefreshCw className={`h-5 w-5 ${isLoading ? 'animate-spin' : ''}`} />
          </Button>
          <Button onClick={() => setCreateOpen(true)} size="sm">
            <Plus className="h-4 w-4 mr-2" />
            新建
          </Button>
        </div>
      </div>

      <DataTable
        columns={columns}
        data={keys}
        getRowId={(row) => String(row.id)}
        emptyText="暂无 API Key，点击右上角「新建」"
      />

      <CreateKeyDialog
        open={createOpen}
        onOpenChange={setCreateOpen}
        onCreated={(key) => {
          queryClient.invalidateQueries({ queryKey: ['api-keys'] })
          setRevealKey(key.key)
        }}
      />

      <EditKeyDialog
        target={editTarget}
        onOpenChange={(open) => {
          if (!open) setEditTarget(null)
        }}
        onSaved={() => {
          queryClient.invalidateQueries({ queryKey: ['api-keys'] })
          setEditTarget(null)
        }}
      />

      <RevealKeyDialog
        open={revealKey !== null}
        keyValue={revealKey}
        onOpenChange={(open) => {
          if (!open) setRevealKey(null)
        }}
      />
    </div>
  )
}

function CreateKeyDialog({
  open,
  onOpenChange,
  onCreated,
}: {
  open: boolean
  onOpenChange: (b: boolean) => void
  onCreated: (key: { key: string }) => void
}) {
  const [name, setName] = useState('')
  const [desc, setDesc] = useState('')
  const [customKey, setCustomKey] = useState('')
  const [maxConcurrent, setMaxConcurrent] = useState(0)
  const [cacheMin, setCacheMin] = useState(0)
  const [cacheMax, setCacheMax] = useState(0)

  const mutation = useMutation({
    mutationFn: (req: CreateApiKeyRequest) => createApiKey(req),
    onSuccess: (key) => {
      onCreated(key)
      onOpenChange(false)
      setName('')
      setDesc('')
      setCustomKey('')
      setMaxConcurrent(0)
      setCacheMin(0)
      setCacheMax(0)
    },
    onError: (e) => toast.error('创建失败：' + extractErrorMessage(e)),
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>新建 API Key</DialogTitle>
          <DialogDescription>
            可以为不同客户端创建独立 Key，每个 Key 可单独限并发、配缓存比例模拟。
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          <div>
            <label className="text-sm font-medium">名称 *</label>
            <Input
              placeholder="如 my-app"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          <div>
            <label className="text-sm font-medium">描述</label>
            <Input value={desc} onChange={(e) => setDesc(e.target.value)} />
          </div>
          <div>
            <label className="text-sm font-medium">
              自定义 Key（留空自动生成 sk-kiro-...）
            </label>
            <Input
              placeholder="可选"
              value={customKey}
              onChange={(e) => setCustomKey(e.target.value)}
            />
          </div>
          <div className="grid grid-cols-3 gap-2">
            <div>
              <label className="text-sm font-medium">最大并发</label>
              <Input
                type="number"
                min={0}
                value={maxConcurrent}
                onChange={(e) =>
                  setMaxConcurrent(Math.max(0, Number(e.target.value) || 0))
                }
              />
              <p className="text-[11px] text-muted-foreground">0=无限</p>
            </div>
            <div>
              <label className="text-sm font-medium">缓存命中率最小</label>
              <Input
                type="number"
                min={0}
                max={100}
                value={cacheMin}
                onChange={(e) => setCacheMin(Number(e.target.value) || 0)}
              />
              <p className="text-[11px] text-muted-foreground">0~100%</p>
            </div>
            <div>
              <label className="text-sm font-medium">缓存命中率最大</label>
              <Input
                type="number"
                min={0}
                max={100}
                value={cacheMax}
                onChange={(e) => setCacheMax(Number(e.target.value) || 0)}
              />
              <p className="text-[11px] text-muted-foreground">0~100%</p>
            </div>
          </div>
          <p className="text-xs text-muted-foreground">
            缓存命中率：min/max 都为 0 = 不模拟（输出真实值）。例如 50/55 → 每次请求随机抽取
            50%~55% 作为 cache_read 占总输入的比例。
          </p>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button
            onClick={() =>
              mutation.mutate({
                name: name.trim(),
                description: desc.trim() || undefined,
                customKey: customKey.trim() || undefined,
                maxConcurrent,
                cacheReadMinPct: cacheMin,
                cacheReadMaxPct: cacheMax,
              })
            }
            disabled={!name.trim() || mutation.isPending}
          >
            {mutation.isPending ? '创建中…' : '创建'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function EditKeyDialog({
  target,
  onOpenChange,
  onSaved,
}: {
  target: ApiKeyItem | null
  onOpenChange: (b: boolean) => void
  onSaved: () => void
}) {
  const [name, setName] = useState('')
  const [desc, setDesc] = useState('')
  const [maxConcurrent, setMaxConcurrent] = useState(0)
  const [cacheMin, setCacheMin] = useState(0)
  const [cacheMax, setCacheMax] = useState(0)

  // 同步 target 到表单
  useMemo(() => {
    if (target) {
      setName(target.name)
      setDesc(target.description ?? '')
      setMaxConcurrent(target.maxConcurrent)
      setCacheMin(target.cacheReadMinPct)
      setCacheMax(target.cacheReadMaxPct)
    }
  }, [target])

  const mutation = useMutation({
    mutationFn: () => {
      if (!target) throw new Error('未选中')
      return updateApiKey(target.id, {
        name: name.trim() || target.name,
        description: desc.trim() || null,
        maxConcurrent,
        cacheReadMinPct: cacheMin,
        cacheReadMaxPct: cacheMax,
      })
    },
    onSuccess: () => {
      toast.success('已保存')
      onSaved()
    },
    onError: (e) => toast.error('保存失败：' + extractErrorMessage(e)),
  })

  if (!target) return null

  return (
    <Dialog open={target !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>编辑 API Key #{target.id}</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          <div>
            <label className="text-sm font-medium">名称</label>
            <Input value={name} onChange={(e) => setName(e.target.value)} />
          </div>
          <div>
            <label className="text-sm font-medium">描述</label>
            <Input value={desc} onChange={(e) => setDesc(e.target.value)} />
          </div>
          <div className="grid grid-cols-3 gap-2">
            <div>
              <label className="text-sm font-medium">最大并发</label>
              <Input
                type="number"
                min={0}
                value={maxConcurrent}
                onChange={(e) =>
                  setMaxConcurrent(Math.max(0, Number(e.target.value) || 0))
                }
              />
            </div>
            <div>
              <label className="text-sm font-medium">缓存最小%</label>
              <Input
                type="number"
                min={0}
                max={100}
                value={cacheMin}
                onChange={(e) => setCacheMin(Number(e.target.value) || 0)}
              />
            </div>
            <div>
              <label className="text-sm font-medium">缓存最大%</label>
              <Input
                type="number"
                min={0}
                max={100}
                value={cacheMax}
                onChange={(e) => setCacheMax(Number(e.target.value) || 0)}
              />
            </div>
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button onClick={() => mutation.mutate()} disabled={mutation.isPending}>
            {mutation.isPending ? '保存中…' : '保存'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function RevealKeyDialog({
  open,
  keyValue,
  onOpenChange,
}: {
  open: boolean
  keyValue: string | null
  onOpenChange: (b: boolean) => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>API Key 完整值</DialogTitle>
          <DialogDescription>
            请妥善保存；之后可以随时回到此页面再次查看。
          </DialogDescription>
        </DialogHeader>
        <div className="rounded-md border bg-muted/30 p-3 font-mono text-xs break-all select-all">
          {keyValue ?? ''}
        </div>
        <DialogFooter>
          <Button
            onClick={() => {
              if (keyValue) {
                navigator.clipboard.writeText(keyValue)
                toast.success('已复制')
              }
              onOpenChange(false)
            }}
          >
            复制并关闭
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
