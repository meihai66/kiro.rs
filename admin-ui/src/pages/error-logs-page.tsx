import { useMemo, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Eye, Loader2, Trash2 } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  clearErrorLogs,
  deleteErrorLog,
  getErrorLog,
  getErrorLogKindStats,
  listErrorLogs,
} from '@/api/error-logs'
import { extractErrorMessage } from '@/lib/utils'

const KIND_LABEL: Record<string, string> = {
  input_too_long: '输入过长',
  improperly_formed: '请求格式错误',
  no_credentials: '无可用凭据',
  all_cooling_down: '全冷却',
  quota_exhausted: '配额耗尽',
  network_error: '网络错误',
  upstream_transient: '上游瞬态',
  rate_limit: '限流',
  auth: '认证失败',
  upstream_error: '上游错误',
  stream_interrupted: '流中断',
}

function fmtTime(t: string): string {
  const d = new Date(t)
  return d.toLocaleString('zh-CN', { hour12: false })
}

function statusColor(code: number): string {
  if (code >= 500) return 'bg-red-500/15 text-red-700 dark:text-red-400'
  if (code === 429) return 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400'
  if (code === 400 || code === 422) return 'bg-orange-500/15 text-orange-700 dark:text-orange-400'
  if (code === 401 || code === 403) return 'bg-rose-500/15 text-rose-700 dark:text-rose-400'
  return 'bg-muted text-foreground'
}

export function ErrorLogsPage() {
  const queryClient = useQueryClient()

  // 过滤器
  const [statusCodesInput, setStatusCodesInput] = useState('')
  const [kindsInput, setKindsInput] = useState('')
  const [credentialIdInput, setCredentialIdInput] = useState('')
  const [sinceInput, setSinceInput] = useState('') // datetime-local
  const [untilInput, setUntilInput] = useState('')

  // 已应用的过滤
  const [statusCodes, setStatusCodes] = useState('')
  const [kinds, setKinds] = useState('')
  const [credentialId, setCredentialId] = useState<number | undefined>()
  const [since, setSince] = useState<string | undefined>()
  const [until, setUntil] = useState<string | undefined>()

  const [page, setPage] = useState(0)
  const [pageSize, setPageSize] = useState(50)
  const [detailId, setDetailId] = useState<number | null>(null)

  const params = useMemo(
    () => ({
      statusCodes: statusCodes || undefined,
      errorKinds: kinds || undefined,
      credentialId,
      since,
      until,
      limit: pageSize,
      offset: page * pageSize,
    }),
    [statusCodes, kinds, credentialId, since, until, page, pageSize]
  )

  const { data, isLoading, refetch, isFetching } = useQuery({
    queryKey: ['error-logs', params],
    queryFn: () => listErrorLogs(params),
    placeholderData: keepPreviousData,
  })

  // 各类型累计次数（修剪不影响）
  const { data: kindStats } = useQuery({
    queryKey: ['error-log-kind-stats'],
    queryFn: getErrorLogKindStats,
    refetchInterval: 30_000,
  })

  const total = data?.total ?? 0
  const items = data?.items ?? []
  const totalPages = Math.max(1, Math.ceil(total / pageSize))

  const applyFilters = () => {
    setStatusCodes(
      statusCodesInput
        .split(',')
        .map((s) => s.trim())
        .filter((s) => /^\d+$/.test(s))
        .join(',')
    )
    setKinds(
      kindsInput
        .split(',')
        .map((s) => s.trim())
        .filter((s) => s.length > 0)
        .join(',')
    )
    const cred = parseInt(credentialIdInput.trim(), 10)
    setCredentialId(Number.isFinite(cred) && cred > 0 ? cred : undefined)
    setSince(sinceInput ? new Date(sinceInput).toISOString() : undefined)
    setUntil(untilInput ? new Date(untilInput).toISOString() : undefined)
    setPage(0)
  }

  const resetFilters = () => {
    setStatusCodesInput('')
    setKindsInput('')
    setCredentialIdInput('')
    setSinceInput('')
    setUntilInput('')
    setStatusCodes('')
    setKinds('')
    setCredentialId(undefined)
    setSince(undefined)
    setUntil(undefined)
    setPage(0)
  }

  const setPreset = (label: '1h' | '24h' | '7d') => {
    const now = new Date()
    const start = new Date(now)
    if (label === '1h') start.setHours(now.getHours() - 1)
    if (label === '24h') start.setDate(now.getDate() - 1)
    if (label === '7d') start.setDate(now.getDate() - 7)
    setSinceInput(toDatetimeLocal(start))
    setUntilInput(toDatetimeLocal(now))
  }

  const deleteMut = useMutation({
    mutationFn: (id: number) => deleteErrorLog(id),
    onSuccess: () => {
      toast.success('已删除')
      queryClient.invalidateQueries({ queryKey: ['error-logs'] })
      queryClient.invalidateQueries({ queryKey: ['error-log-kind-stats'] })
    },
    onError: (e) => toast.error('删除失败：' + extractErrorMessage(e)),
  })

  const clearMut = useMutation({
    mutationFn: (before?: string) => clearErrorLogs({ before }),
    onSuccess: (r) => {
      toast.success(`已清空 ${r.deleted} 条`)
      queryClient.invalidateQueries({ queryKey: ['error-logs'] })
      queryClient.invalidateQueries({ queryKey: ['error-log-kind-stats'] })
    },
    onError: (e) => toast.error('清空失败：' + extractErrorMessage(e)),
  })

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold">错误日志</h1>
          <Badge variant="secondary" title="当前留存条数（符合过滤条件）">
            留存 {total}
          </Badge>
          {kindStats && (
            <Badge
              variant="secondary"
              title={`累计发生次数（每类仅保留最新 ${kindStats.maxPerKind} 条日志，设置页的全局数量/天数清理会进一步压低留存；累计计数不受任何清理影响）`}
            >
              累计 {kindStats.totalCumulative}
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => refetch()}
            disabled={isFetching}
          >
            {isFetching && <Loader2 className="h-3 w-3 mr-1 animate-spin" />}
            刷新
          </Button>
          <Button
            variant="destructive"
            size="sm"
            onClick={() => {
              if (
                !confirm(
                  '清空全部错误日志？此操作不可恢复，各类型的累计计数也会一并归零。\n（如需按时间清理，请在过滤里设 until 后再清空对应区间——但当前按钮直接清全部）'
                )
              )
                return
              clearMut.mutate(undefined)
            }}
            disabled={clearMut.isPending}
          >
            清空全部
          </Button>
        </div>
      </div>

      {/* 过滤栏 */}
      <div className="rounded-md border bg-muted/30 p-3 mb-3 space-y-2 text-xs">
        <div className="grid grid-cols-1 md:grid-cols-5 gap-2">
          <div className="flex flex-col gap-1">
            <label className="text-muted-foreground">状态码（逗号分隔）</label>
            <Input
              placeholder="429,502,503"
              value={statusCodesInput}
              onChange={(e) => setStatusCodesInput(e.target.value)}
              className="h-8 text-xs"
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-muted-foreground">错误分类（kind）</label>
            <Input
              placeholder="rate_limit,auth"
              value={kindsInput}
              onChange={(e) => setKindsInput(e.target.value)}
              className="h-8 text-xs"
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-muted-foreground">凭据 ID</label>
            <Input
              type="number"
              min={1}
              placeholder="例如 3"
              value={credentialIdInput}
              onChange={(e) => setCredentialIdInput(e.target.value)}
              className="h-8 text-xs"
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-muted-foreground">起始时间</label>
            <Input
              type="datetime-local"
              value={sinceInput}
              onChange={(e) => setSinceInput(e.target.value)}
              className="h-8 text-xs"
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-muted-foreground">截止时间</label>
            <Input
              type="datetime-local"
              value={untilInput}
              onChange={(e) => setUntilInput(e.target.value)}
              className="h-8 text-xs"
            />
          </div>
        </div>
        {kindStats && kindStats.kinds.length > 0 && (
          <div className="flex flex-wrap items-center gap-1.5">
            <span className="text-muted-foreground">
              各类累计（每类留存最新 {kindStats.maxPerKind} 条）
            </span>
            {kindStats.kinds.map((k) => (
              <button
                key={k.errorKind}
                type="button"
                title={`点击按该类型过滤 · 留存 ${k.retained} 条`}
                onClick={() => {
                  setKindsInput(k.errorKind)
                  setKinds(k.errorKind)
                  setPage(0)
                }}
                className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 hover:bg-muted transition-colors ${
                  kinds === k.errorKind ? 'bg-muted border-foreground/30' : ''
                }`}
              >
                <span>{KIND_LABEL[k.errorKind] ?? k.errorKind}</span>
                <span className="font-mono text-muted-foreground">{k.total}</span>
              </button>
            ))}
          </div>
        )}
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-muted-foreground">快捷</span>
          <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={() => setPreset('1h')}>1h</Button>
          <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={() => setPreset('24h')}>24h</Button>
          <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={() => setPreset('7d')}>7d</Button>
          <span className="flex-1" />
          <Button size="sm" variant="outline" className="h-7" onClick={resetFilters}>
            清除
          </Button>
          <Button size="sm" className="h-7" onClick={applyFilters}>
            应用过滤
          </Button>
        </div>
      </div>

      {/* 表格 */}
      <div className="rounded-md border">
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="whitespace-nowrap">时间</TableHead>
              <TableHead>凭据</TableHead>
              <TableHead>状态码</TableHead>
              <TableHead>分类</TableHead>
              <TableHead>模型</TableHead>
              <TableHead>摘要</TableHead>
              <TableHead className="text-right">操作</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {isLoading ? (
              <TableRow>
                <TableCell colSpan={7} className="h-24 text-center text-muted-foreground">
                  加载中…
                </TableCell>
              </TableRow>
            ) : items.length === 0 ? (
              <TableRow>
                <TableCell colSpan={7} className="h-24 text-center text-muted-foreground">
                  没有匹配的错误记录
                </TableCell>
              </TableRow>
            ) : (
              items.map((it) => (
                <TableRow key={it.id}>
                  <TableCell className="font-mono text-xs whitespace-nowrap">
                    {fmtTime(it.at)}
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {it.credentialId ?? <span className="text-muted-foreground">—</span>}
                  </TableCell>
                  <TableCell>
                    <Badge className={`text-xs ${statusColor(it.statusCode)}`}>
                      {it.statusCode}
                    </Badge>
                  </TableCell>
                  <TableCell>
                    <Badge variant="outline" className="text-xs">
                      {KIND_LABEL[it.errorKind] ?? it.errorKind}
                    </Badge>
                  </TableCell>
                  <TableCell className="font-mono text-xs">
                    {it.model ?? <span className="text-muted-foreground">—</span>}
                  </TableCell>
                  <TableCell className="text-xs max-w-[400px] truncate" title={it.summary}>
                    {it.summary}
                  </TableCell>
                  <TableCell className="text-right">
                    <div className="flex justify-end gap-1">
                      <Button
                        size="sm"
                        variant="outline"
                        className="h-7 px-2 text-xs"
                        onClick={() => setDetailId(it.id)}
                      >
                        <Eye className="h-3 w-3 mr-1" />
                        查看
                      </Button>
                      <Button
                        size="sm"
                        variant="ghost"
                        className="h-7 px-2 text-xs text-destructive"
                        onClick={() => deleteMut.mutate(it.id)}
                        disabled={deleteMut.isPending}
                      >
                        <Trash2 className="h-3 w-3" />
                      </Button>
                    </div>
                  </TableCell>
                </TableRow>
              ))
            )}
          </TableBody>
        </Table>
      </div>

      {/* 分页 */}
      <div className="mt-3 flex items-center justify-between text-sm text-muted-foreground">
        <div>共 {total} 条</div>
        <div className="flex items-center gap-2">
          <span>
            第 {page + 1} / {totalPages} 页
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={page === 0}
            onClick={() => setPage((p) => Math.max(0, p - 1))}
          >
            上一页
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={page + 1 >= totalPages}
            onClick={() => setPage((p) => p + 1)}
          >
            下一页
          </Button>
          <select
            className="h-8 rounded border bg-background px-2 text-xs"
            value={pageSize}
            onChange={(e) => {
              setPageSize(Number(e.target.value))
              setPage(0)
            }}
          >
            {[20, 50, 100, 200].map((s) => (
              <option key={s} value={s}>
                每页 {s}
              </option>
            ))}
          </select>
        </div>
      </div>

      <ErrorLogDetailDialog
        id={detailId}
        onClose={() => setDetailId(null)}
        onDelete={(id) => {
          deleteMut.mutate(id)
          setDetailId(null)
        }}
      />
    </>
  )
}

function ErrorLogDetailDialog({
  id,
  onClose,
  onDelete,
}: {
  id: number | null
  onClose: () => void
  onDelete: (id: number) => void
}) {
  const { data, isLoading } = useQuery({
    queryKey: ['error-log-detail', id],
    queryFn: () => getErrorLog(id!),
    enabled: id !== null,
  })

  return (
    <Dialog open={id !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-5xl max-h-[90vh] p-0 gap-0 flex flex-col">
        <DialogHeader className="px-6 pt-6 pb-3 border-b shrink-0">
          <DialogTitle className="pr-8">
            错误日志详情 #{id}
          </DialogTitle>
        </DialogHeader>
        <div className="flex-1 overflow-y-auto p-6 text-sm">
          {isLoading || !data ? (
            <div className="text-center text-muted-foreground py-8">加载中…</div>
          ) : (
            <div className="space-y-4">
              <div className="grid grid-cols-2 md:grid-cols-4 gap-3 text-xs">
                <Field label="时间">{fmtTime(data.at)}</Field>
                <Field label="状态码">
                  <Badge className={`${statusColor(data.statusCode)} text-xs`}>{data.statusCode}</Badge>
                </Field>
                <Field label="分类">{KIND_LABEL[data.errorKind] ?? data.errorKind}</Field>
                <Field label="模型">{data.model ?? '—'}</Field>
                <Field label="凭据">{data.credentialId ?? '—'}</Field>
                <Field label="路径">{data.requestPath ?? '—'}</Field>
                <Field label="方法">{data.requestMethod ?? '—'}</Field>
                <Field label="user_id">{data.userId ?? '—'}</Field>
              </div>

              <div>
                <SectionTitle>摘要</SectionTitle>
                <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all">
                  {data.summary}
                </pre>
              </div>

              <CodeBlock label="请求体（转发到上游）" content={data.requestBody} />
              <CodeBlock label="响应体（返回客户端）" content={data.responseBody} />
              {data.requestHeaders && (
                <CodeBlock label="请求 headers" content={data.requestHeaders} />
              )}
              {data.responseHeaders && (
                <CodeBlock label="响应 headers" content={data.responseHeaders} />
              )}
            </div>
          )}
        </div>
        <div className="px-6 py-3 border-t flex items-center justify-between bg-background/95">
          <Button
            variant="destructive"
            size="sm"
            onClick={() => id && onDelete(id)}
          >
            <Trash2 className="h-3 w-3 mr-1" />
            删除此条
          </Button>
          <Button size="sm" variant="outline" onClick={onClose}>
            关闭
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <div className="text-muted-foreground">{label}</div>
      <div className="font-mono">{children}</div>
    </div>
  )
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-1">
      {children}
    </h3>
  )
}

function CodeBlock({
  label,
  content,
}: {
  label: string
  content: string | null | undefined
}) {
  const handleCopy = () => {
    if (!content) return
    navigator.clipboard
      .writeText(content)
      .then(() => toast.success('已复制'))
      .catch(() => toast.error('复制失败'))
  }
  return (
    <div>
      <div className="flex items-center justify-between mb-1">
        <SectionTitle>{label}</SectionTitle>
        {content && (
          <Button size="sm" variant="ghost" className="h-6 px-2 text-xs" onClick={handleCopy}>
            复制
          </Button>
        )}
      </div>
      <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all max-h-[400px] overflow-y-auto">
        {content || <span className="text-muted-foreground">（空）</span>}
      </pre>
    </div>
  )
}

function toDatetimeLocal(d: Date): string {
  // 转换成 input[type=datetime-local] 接受的本地时区无 TZ 串
  const pad = (n: number) => String(n).padStart(2, '0')
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}` +
    `T${pad(d.getHours())}:${pad(d.getMinutes())}`
  )
}
