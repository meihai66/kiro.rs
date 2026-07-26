import { useEffect, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Eye, Loader2, RefreshCw, Trash2 } from 'lucide-react'

import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import {
  clearWebhookLogs,
  deleteWebhookLog,
  getWebhookConfig,
  getWebhookLog,
  listWebhookLogs,
  updateWebhookConfig,
} from '@/api/webhook'
import { extractErrorMessage } from '@/lib/utils'

function fmtTime(t: string): string {
  return new Date(t).toLocaleString('zh-CN', { hour12: false })
}

function statusColor(code: number): string {
  if (code >= 500) return 'bg-red-500/15 text-red-700 dark:text-red-400'
  if (code === 401 || code === 403) return 'bg-rose-500/15 text-rose-700 dark:text-rose-400'
  if (code >= 400) return 'bg-orange-500/15 text-orange-700 dark:text-orange-400'
  return 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
}

export function WebhookPage() {
  const queryClient = useQueryClient()
  const [page, setPage] = useState(0)
  const [pageSize, setPageSize] = useState(50)
  const [detailId, setDetailId] = useState<number | null>(null)
  const [keyInput, setKeyInput] = useState('')
  const [showKey, setShowKey] = useState(false)

  const { data: config, isLoading: configLoading } = useQuery({
    queryKey: ['webhook-config'],
    queryFn: getWebhookConfig,
  })

  const {
    data: logs,
    isLoading: logsLoading,
    isFetching,
    refetch,
  } = useQuery({
    queryKey: ['webhook-logs', { limit: pageSize, offset: page * pageSize }],
    queryFn: () => listWebhookLogs({ limit: pageSize, offset: page * pageSize }),
    placeholderData: keepPreviousData,
    refetchInterval: 10_000,
  })

  // 密钥输入框跟随后端值初始化（用户编辑中不覆盖）
  useEffect(() => {
    if (config && !showKey) setKeyInput(config.apiKey)
  }, [config, showKey])

  const configMut = useMutation({
    mutationFn: updateWebhookConfig,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['webhook-config'] })
    },
    onError: (e) => toast.error('保存失败：' + extractErrorMessage(e)),
  })

  const deleteMut = useMutation({
    mutationFn: deleteWebhookLog,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['webhook-logs'] })
    },
    onError: (e) => toast.error('删除失败：' + extractErrorMessage(e)),
  })

  const clearMut = useMutation({
    mutationFn: clearWebhookLogs,
    onSuccess: (r) => {
      toast.success(`已清空 ${r.deleted} 条`)
      queryClient.invalidateQueries({ queryKey: ['webhook-logs'] })
    },
    onError: (e) => toast.error('清空失败：' + extractErrorMessage(e)),
  })

  const items = logs?.items ?? []
  const total = logs?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / pageSize))
  const endpointUrl = config ? `${window.location.origin}${config.endpointPath}` : ''

  const copy = (text: string, label: string) => {
    navigator.clipboard
      .writeText(text)
      .then(() => toast.success(`${label}已复制`))
      .catch(() => toast.error('复制失败'))
  }

  // 密钥隐藏时示例里也不显示明文，避免录屏/截图外泄；点「显示」后带上真实密钥
  const exampleKey = config?.apiKey ? (showKey ? config.apiKey : '<webhook 密钥>') : '<webhook 密钥>'
  const curlExample = config
    ? `curl -X POST ${endpointUrl} \\
  -H "x-api-key: ${exampleKey}" \\
  -H "Content-Type: application/json" \\
  -d '{"keys": ["ksk_xxx|1.2.3.4:8080:user:pass", "ksk_yyy"]}'`
    : ''

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold">Webhook</h1>
          {config &&
            (config.enabled && config.hasApiKey ? (
              <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
                已启用
              </Badge>
            ) : (
              <Badge variant="secondary">
                {config.hasApiKey ? '已关闭' : '未配置密钥'}
              </Badge>
            ))}
        </div>
      </div>

      <div className="space-y-4">
        {/* 接口配置 */}
        <Card>
          <CardHeader>
            <CardTitle className="text-base">接口配置</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <p className="text-xs text-muted-foreground">
              外部系统向此接口推送 Kiro API Key（<code>ksk_*</code>），服务会解析行内代理并自动入池绑定、
              逐条验证去重，导入成功即直接启用参与调度（绑定代理失败的凭据保持禁用）。
              开关与密钥修改后立即生效，无需重启。
            </p>

            <div className="flex items-center justify-between">
              <div>
                <label className="text-sm font-medium">启用接口</label>
                <p className="text-xs text-muted-foreground">
                  关闭后接口返回 403，已导入的凭据不受影响
                </p>
              </div>
              <Switch
                checked={config?.enabled ?? false}
                disabled={configLoading || configMut.isPending || !config?.hasApiKey}
                onCheckedChange={(v) =>
                  configMut.mutate(
                    { enabled: v },
                    { onSuccess: () => toast.success(v ? '接口已启用' : '接口已关闭') }
                  )
                }
              />
            </div>

            <div className="flex items-center justify-between">
              <div>
                <label className="text-sm font-medium">记录原始请求</label>
                <p className="text-xs text-muted-foreground">
                  保存推送方发来的原始请求体与返回体（最多留存 {config?.logMaxCount ?? 500} 条）
                </p>
              </div>
              <Switch
                checked={config?.logEnabled ?? false}
                disabled={configLoading || configMut.isPending}
                onCheckedChange={(v) =>
                  configMut.mutate(
                    { logEnabled: v },
                    { onSuccess: () => toast.success(v ? '已开启记录' : '已关闭记录') }
                  )
                }
              />
            </div>

            {!config?.hasApiKey && (
              <p className="text-xs text-orange-600 dark:text-orange-400">
                尚未配置密钥，接口不可用。请填写或生成一个密钥后再启用。
              </p>
            )}

            {/* 密钥 */}
            <div className="space-y-2">
              <label className="text-sm font-medium">Webhook 密钥</label>
              <div className="flex gap-2">
                <Input
                  type={showKey ? 'text' : 'password'}
                  className="font-mono text-xs"
                  placeholder="填写自定义密钥，或点「生成」随机创建"
                  value={keyInput}
                  onChange={(e) => setKeyInput(e.target.value)}
                  disabled={configMut.isPending}
                />
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => setShowKey((v) => !v)}
                >
                  {showKey ? '隐藏' : '显示'}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!config?.apiKey}
                  onClick={() => config?.apiKey && copy(config.apiKey, '密钥')}
                >
                  复制
                </Button>
              </div>
              <div className="flex items-center gap-2">
                <Button
                  size="sm"
                  disabled={configMut.isPending || keyInput === (config?.apiKey ?? '')}
                  onClick={() =>
                    configMut.mutate(
                      { apiKey: keyInput.trim() },
                      {
                        onSuccess: (r) => {
                          setKeyInput(r.apiKey)
                          toast.success(r.hasApiKey ? '密钥已保存' : '密钥已清除，接口不可用')
                        },
                      }
                    )
                  }
                >
                  保存密钥
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={configMut.isPending}
                  onClick={() => {
                    if (config?.hasApiKey && !confirm('重新生成会立即失效旧密钥，推送方需同步更新。继续？'))
                      return
                    configMut.mutate(
                      { regenerateApiKey: true },
                      {
                        onSuccess: (r) => {
                          setKeyInput(r.apiKey)
                          setShowKey(true)
                          toast.success('已生成新密钥，请同步给推送方')
                        },
                      }
                    )
                  }}
                >
                  <RefreshCw className="h-3 w-3 mr-1" />
                  生成
                </Button>
                {configMut.isPending && (
                  <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
                )}
              </div>
              <p className="text-xs text-muted-foreground">
                认证支持 <code>x-api-key</code> 或 <code>Authorization: Bearer &lt;密钥&gt;</code>；
                与 Admin 密钥相互独立，可单独发给自动化系统。留空保存即清除密钥。
              </p>
            </div>

            {/* 调用示例 */}
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium">调用示例</label>
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-6 px-2 text-xs"
                  onClick={() => copy(curlExample, '示例')}
                >
                  复制
                </Button>
              </div>
              <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all">
                {curlExample}
              </pre>
              <p className="text-xs text-muted-foreground">
                <code>keys</code> 支持字符串数组或多行字符串，每行 <code>ksk_xxx</code> 或
                {' '}<code>ksk_xxx|host:port:user:pass</code>（也支持 <code>host:port</code> 与完整代理 URL）。
                可选字段：<code>priority</code>、<code>region</code>、<code>apiRegion</code>、
                <code>endpoint</code>、<code>proxyScheme</code>。
              </p>
            </div>
          </CardContent>
        </Card>

        {/* 接收记录 */}
        <Card>
          <CardHeader className="flex-row items-center justify-between space-y-0">
            <CardTitle className="text-base">
              接收记录
              <span className="ml-2 text-xs font-normal text-muted-foreground">共 {total} 条</span>
            </CardTitle>
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => refetch()} disabled={isFetching}>
                {isFetching && <Loader2 className="h-3 w-3 mr-1 animate-spin" />}
                刷新
              </Button>
              <Button
                variant="destructive"
                size="sm"
                disabled={clearMut.isPending || total === 0}
                onClick={() => {
                  if (!confirm('清空全部接收记录？此操作不可恢复。')) return
                  clearMut.mutate()
                }}
              >
                清空全部
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            <div className="rounded-md border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="whitespace-nowrap">时间</TableHead>
                    <TableHead>来源 IP</TableHead>
                    <TableHead>状态码</TableHead>
                    <TableHead className="text-right">收到</TableHead>
                    <TableHead className="text-right">新增</TableHead>
                    <TableHead className="text-right">跳过</TableHead>
                    <TableHead className="text-right">失败</TableHead>
                    <TableHead>摘要</TableHead>
                    <TableHead className="text-right">操作</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {logsLoading ? (
                    <TableRow>
                      <TableCell colSpan={9} className="h-24 text-center text-muted-foreground">
                        加载中…
                      </TableCell>
                    </TableRow>
                  ) : items.length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={9} className="h-24 text-center text-muted-foreground">
                        {config?.logEnabled
                          ? '还没有收到推送'
                          : '记录原始请求已关闭，开启后新的推送会被记录'}
                      </TableCell>
                    </TableRow>
                  ) : (
                    items.map((it) => (
                      <TableRow key={it.id}>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {fmtTime(it.at)}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {it.sourceIp ?? <span className="text-muted-foreground">—</span>}
                        </TableCell>
                        <TableCell>
                          <Badge className={`text-xs ${statusColor(it.statusCode)}`}>
                            {it.statusCode}
                          </Badge>
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs">{it.received}</TableCell>
                        <TableCell className="text-right font-mono text-xs">
                          {it.added > 0 ? (
                            <span className="text-emerald-600 dark:text-emerald-400">{it.added}</span>
                          ) : (
                            it.added
                          )}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs">{it.skipped}</TableCell>
                        <TableCell className="text-right font-mono text-xs">
                          {it.invalid > 0 ? (
                            <span className="text-orange-600 dark:text-orange-400">{it.invalid}</span>
                          ) : (
                            it.invalid
                          )}
                        </TableCell>
                        <TableCell className="text-xs max-w-[320px] truncate" title={it.summary}>
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
          </CardContent>
        </Card>
      </div>

      <WebhookLogDetailDialog
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

function WebhookLogDetailDialog({
  id,
  onClose,
  onDelete,
}: {
  id: number | null
  onClose: () => void
  onDelete: (id: number) => void
}) {
  const { data, isLoading } = useQuery({
    queryKey: ['webhook-log-detail', id],
    queryFn: () => getWebhookLog(id!),
    enabled: id !== null,
  })

  return (
    <Dialog open={id !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-5xl max-h-[90vh] p-0 gap-0 flex flex-col">
        <DialogHeader className="px-6 pt-6 pb-3 border-b shrink-0">
          <DialogTitle className="pr-8">接收详情 #{id}</DialogTitle>
        </DialogHeader>
        <div className="flex-1 overflow-y-auto p-6 text-sm">
          {isLoading || !data ? (
            <div className="text-center text-muted-foreground py-8">加载中…</div>
          ) : (
            <div className="space-y-4">
              <div className="grid grid-cols-2 md:grid-cols-4 gap-3 text-xs">
                <Field label="时间">{fmtTime(data.at)}</Field>
                <Field label="来源 IP">{data.sourceIp ?? '—'}</Field>
                <Field label="状态码">
                  <Badge className={`${statusColor(data.statusCode)} text-xs`}>
                    {data.statusCode}
                  </Badge>
                </Field>
                <Field label="收到行数">{data.received}</Field>
                <Field label="新增">{data.added}</Field>
                <Field label="跳过">{data.skipped}</Field>
                <Field label="失败">{data.invalid}</Field>
              </div>

              <div>
                <SectionTitle>摘要</SectionTitle>
                <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all">
                  {data.summary}
                </pre>
              </div>

              <CodeBlock label="原始请求体（推送方发来的内容）" content={data.requestBody} />
              <CodeBlock label="响应体（返回给推送方）" content={data.responseBody} />
              {data.requestHeaders && (
                <CodeBlock label="请求 headers" content={data.requestHeaders} />
              )}
            </div>
          )}
        </div>
        <div className="px-6 py-3 border-t flex items-center justify-between bg-background/95">
          <Button variant="destructive" size="sm" onClick={() => id && onDelete(id)}>
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

function CodeBlock({ label, content }: { label: string; content: string | null | undefined }) {
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
