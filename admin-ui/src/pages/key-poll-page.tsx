import { useEffect, useState } from 'react'
import { keepPreviousData, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Eye, Loader2, PlayCircle } from 'lucide-react'

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
  clearKeyOnboardLogs,
  clearKeyPollLogs,
  clearKeySeen,
  deleteKeySeen,
  getKeyPollConfig,
  getKeyPollLog,
  listKeyOnboardLogs,
  listKeyPollLogs,
  listKeySeen,
  runKeyPoll,
  updateKeyPollConfig,
} from '@/api/key-poll'
import type { PollOutcome } from '@/types/api'
import { extractErrorMessage } from '@/lib/utils'

function fmtTime(t: string): string {
  return new Date(t).toLocaleString('zh-CN', { hour12: false })
}

function triggerLabel(kind: string): string {
  return kind === 'manual' ? '手动' : '自动'
}

export function KeyPollPage() {
  const queryClient = useQueryClient()
  const [detailId, setDetailId] = useState<number | null>(null)
  const [showKey, setShowKey] = useState(false)
  // 表单本地态（跟随后端值初始化，编辑中不被轮询覆盖）
  const [apiUrl, setApiUrl] = useState('')
  const [apiKey, setApiKey] = useState('')
  const [intervalSecs, setIntervalSecs] = useState('300')
  const [priority, setPriority] = useState('10')
  const [retryInvalidMax, setRetryInvalidMax] = useState('3')
  const [dirty, setDirty] = useState(false)
  const [lastRun, setLastRun] = useState<PollOutcome | null>(null)

  const { data: config, isLoading: configLoading } = useQuery({
    queryKey: ['key-poll-config'],
    queryFn: getKeyPollConfig,
  })

  const {
    data: onboardLogs,
    isFetching: onboardFetching,
    refetch: refetchOnboard,
  } = useQuery({
    queryKey: ['key-onboard-logs'],
    queryFn: () => listKeyOnboardLogs({ limit: 100 }),
    placeholderData: keepPreviousData,
    refetchInterval: 15_000,
  })

  const {
    data: pollLogs,
    isFetching: pollFetching,
    refetch: refetchPolls,
  } = useQuery({
    queryKey: ['key-poll-logs'],
    queryFn: () => listKeyPollLogs({ limit: 50 }),
    placeholderData: keepPreviousData,
    refetchInterval: 15_000,
  })

  const { data: seenList, isFetching: seenFetching, refetch: refetchSeen } = useQuery({
    queryKey: ['key-poll-seen'],
    queryFn: () => listKeySeen({ limit: 100 }),
    placeholderData: keepPreviousData,
    refetchInterval: 30_000,
  })

  useEffect(() => {
    if (!config || dirty) return
    setApiUrl(config.apiUrl)
    setApiKey(config.apiKey)
    setIntervalSecs(String(config.intervalSecs))
    setPriority(String(config.priority))
    setRetryInvalidMax(String(config.retryInvalidMax))
  }, [config, dirty])

  const configMut = useMutation({
    mutationFn: updateKeyPollConfig,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['key-poll-config'] })
    },
    onError: (e) => toast.error('保存失败：' + extractErrorMessage(e)),
  })

  const runMut = useMutation({
    mutationFn: (dryRun: boolean) => runKeyPoll(dryRun),
    onSuccess: (outcome) => {
      setLastRun(outcome)
      if (!outcome.ok) {
        toast.error(`上号失败：${outcome.summary}`, { duration: 8000 })
      } else if (outcome.dryRun) {
        toast.success(`试运行完成：${outcome.summary}`)
      } else if (outcome.added > 0) {
        toast.success(`已上号 ${outcome.added} 个：${outcome.summary}`)
      } else {
        toast.info(outcome.summary)
      }
      queryClient.invalidateQueries({ queryKey: ['key-poll-logs'] })
      queryClient.invalidateQueries({ queryKey: ['key-onboard-logs'] })
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    },
    onError: (e) => toast.error('执行失败：' + extractErrorMessage(e)),
  })

  const clearPollMut = useMutation({
    mutationFn: clearKeyPollLogs,
    onSuccess: (r) => {
      toast.success(`已清空 ${r.deleted} 条轮询记录`)
      queryClient.invalidateQueries({ queryKey: ['key-poll-logs'] })
    },
    onError: (e) => toast.error('清空失败：' + extractErrorMessage(e)),
  })

  const clearOnboardMut = useMutation({
    mutationFn: clearKeyOnboardLogs,
    onSuccess: (r) => {
      toast.success(`已清空 ${r.deleted} 条上号记录`)
      queryClient.invalidateQueries({ queryKey: ['key-onboard-logs'] })
    },
    onError: (e) => toast.error('清空失败：' + extractErrorMessage(e)),
  })

  const deleteSeenMut = useMutation({
    mutationFn: deleteKeySeen,
    onSuccess: () => {
      toast.success('已移除，该 Key 下次轮询会重新尝试')
      queryClient.invalidateQueries({ queryKey: ['key-poll-seen'] })
    },
    onError: (e) => toast.error('操作失败：' + extractErrorMessage(e)),
  })

  const clearSeenMut = useMutation({
    mutationFn: clearKeySeen,
    onSuccess: (r) => {
      toast.success(`已移除 ${r.deleted} 条去重记录`)
      queryClient.invalidateQueries({ queryKey: ['key-poll-seen'] })
    },
    onError: (e) => toast.error('清空失败：' + extractErrorMessage(e)),
  })

  const saveForm = () => {
    const secs = parseInt(intervalSecs)
    const prio = parseInt(priority)
    const min = config?.minIntervalSecs ?? 30
    if (!Number.isFinite(secs) || secs < min) {
      toast.error(`轮询间隔至少 ${min} 秒`)
      return
    }
    if (!Number.isFinite(prio) || prio < 0) {
      toast.error('优先级需为非负整数')
      return
    }
    const retry = parseInt(retryInvalidMax)
    if (!Number.isFinite(retry) || retry < 0 || retry > 100) {
      toast.error('失败重试上限需为 0~100 的整数')
      return
    }
    configMut.mutate(
      {
        apiUrl: apiUrl.trim(),
        apiKey: apiKey.trim(),
        intervalSecs: secs,
        priority: prio,
        retryInvalidMax: retry,
      },
      {
        onSuccess: () => {
          setDirty(false)
          toast.success('已保存')
        },
      }
    )
  }

  const onboardItems = onboardLogs?.items ?? []
  const pollItems = pollLogs?.items ?? []
  const isPending = configMut.isPending

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-3">
          <h1 className="text-2xl font-semibold">自动上号</h1>
          {config &&
            (config.enabled ? (
              <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
                轮询中 · 每 {config.intervalSecs}s
              </Badge>
            ) : (
              <Badge variant="secondary">已停止</Badge>
            ))}
          {onboardLogs && (
            <Badge variant="secondary" title="累计成功上号次数（最多留存记录数见配置）">
              累计上号 {onboardLogs.total}
            </Badge>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            disabled={runMut.isPending}
            onClick={() => runMut.mutate(true)}
            title="只拉取并比对，不导入"
          >
            {runMut.isPending && <Loader2 className="h-3 w-3 mr-1 animate-spin" />}
            试运行
          </Button>
          <Button size="sm" disabled={runMut.isPending} onClick={() => runMut.mutate(false)}>
            <PlayCircle className="h-3 w-3 mr-1" />
            立即上号
          </Button>
        </div>
      </div>

      <div className="space-y-4">
        {/* 配置 */}
        <Card>
          <CardHeader>
            <CardTitle className="text-base">轮询配置</CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <p className="text-xs text-muted-foreground">
              定时请求发卡站接口拉取自己名下的 Kiro API Key，发现本地没有的新 Key 就自动入池：
              逐条验证 → 分配并绑定代理 → 直接启用参与调度。已存在的 Key 自动跳过，
              配置修改后立即生效，无需重启。
            </p>

            <div className="flex items-center justify-between">
              <div>
                <label className="text-sm font-medium">启用自动轮询</label>
                <p className="text-xs text-muted-foreground">
                  关闭后仅可手动点「立即上号」
                </p>
              </div>
              <Switch
                checked={config?.enabled ?? false}
                disabled={configLoading || isPending}
                onCheckedChange={(v) =>
                  configMut.mutate(
                    { enabled: v },
                    { onSuccess: () => toast.success(v ? '已开启自动轮询' : '已停止自动轮询') }
                  )
                }
              />
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium">接口地址</label>
              <Input
                className="font-mono text-xs"
                placeholder="https://key.example.com/api/my/keys"
                value={apiUrl}
                onChange={(e) => {
                  setApiUrl(e.target.value)
                  setDirty(true)
                }}
                disabled={isPending}
              />
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium">接口密钥（X-API-Key）</label>
              <div className="flex gap-2">
                <Input
                  type={showKey ? 'text' : 'password'}
                  className="font-mono text-xs"
                  placeholder="usr-xxxxxxxx"
                  value={apiKey}
                  onChange={(e) => {
                    setApiKey(e.target.value)
                    setDirty(true)
                  }}
                  disabled={isPending}
                />
                <Button type="button" variant="outline" size="sm" onClick={() => setShowKey((v) => !v)}>
                  {showKey ? '隐藏' : '显示'}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                发卡站个人中心的 API Key，请求时放在 <code>X-API-Key</code> 头里。留空即无法轮询。
              </p>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
              <div className="space-y-2">
                <label className="text-sm font-medium">轮询间隔（秒）</label>
                <Input
                  type="number"
                  min={config?.minIntervalSecs ?? 30}
                  value={intervalSecs}
                  onChange={(e) => {
                    setIntervalSecs(e.target.value)
                    setDirty(true)
                  }}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">
                  最小 {config?.minIntervalSecs ?? 30} 秒，推荐 300（5 分钟）
                </p>
              </div>
              <div className="space-y-2">
                <label className="text-sm font-medium">导入优先级</label>
                <Input
                  type="number"
                  min={0}
                  value={priority}
                  onChange={(e) => {
                    setPriority(e.target.value)
                    setDirty(true)
                  }}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">数字越小越优先被调度</p>
              </div>
            </div>

            <div className="space-y-2">
              <label className="text-sm font-medium">失败重试上限</label>
              <Input
                type="number"
                min={0}
                max={100}
                value={retryInvalidMax}
                onChange={(e) => {
                  setRetryInvalidMax(e.target.value)
                  setDirty(true)
                }}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                验证失败的 Key 最多重试几次后不再尝试（0 = 每轮都重试）。
                已成功上号的 Key 永久跳过，不受此项影响。
              </p>
            </div>

            <div className="flex items-center justify-between">
              <div>
                <label className="text-sm font-medium">只导入 active 状态</label>
                <p className="text-xs text-muted-foreground">
                  忽略响应里 status 非 active 的 Key（过期/封禁等）
                </p>
              </div>
              <Switch
                checked={config?.onlyActive ?? true}
                disabled={configLoading || isPending}
                onCheckedChange={(v) => configMut.mutate({ onlyActive: v })}
              />
            </div>

            <div className="flex items-center justify-between">
              <div>
                <label className="text-sm font-medium">记录原始响应</label>
                <p className="text-xs text-muted-foreground">
                  轮询记录里保存上游返回的原始 JSON（最多留存 {config?.pollLogMaxCount ?? 300} 条）
                </p>
              </div>
              <Switch
                checked={config?.logEnabled ?? true}
                disabled={configLoading || isPending}
                onCheckedChange={(v) => configMut.mutate({ logEnabled: v })}
              />
            </div>

            <div className="flex items-center gap-2 pt-1">
              <Button size="sm" onClick={saveForm} disabled={isPending || !dirty}>
                {isPending ? '保存中…' : dirty ? '保存修改' : '已保存'}
              </Button>
              {dirty && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    setDirty(false)
                    if (config) {
                      setApiUrl(config.apiUrl)
                      setApiKey(config.apiKey)
                      setIntervalSecs(String(config.intervalSecs))
                      setPriority(String(config.priority))
                    }
                  }}
                >
                  放弃修改
                </Button>
              )}
            </div>
          </CardContent>
        </Card>

        {/* 最近一次手动执行结果 */}
        {lastRun && (
          <Card>
            <CardHeader>
              <CardTitle className="text-base">
                最近一次{lastRun.dryRun ? '试运行' : '手动上号'}结果
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 text-sm">
              <div className="flex flex-wrap gap-2 text-xs">
                <Badge variant={lastRun.ok ? 'success' : 'destructive'}>
                  {lastRun.ok ? '成功' : '失败'}
                </Badge>
                <Badge variant="secondary">上游 {lastRun.total} 个</Badge>
                <Badge variant="secondary">active {lastRun.active}</Badge>
                <Badge variant="secondary" title="已在凭据池里，跳过">
                  已在池 {lastRun.alreadyInPool}
                </Badge>
                <Badge variant="secondary" title="上号过或失败次数超限，跳过">
                  已处理 {lastRun.skippedSeen}
                </Badge>
                <Badge variant="secondary">待上号 {lastRun.candidates}</Badge>
                <Badge variant="secondary">新增 {lastRun.added}</Badge>
                {lastRun.invalid > 0 && (
                  <Badge variant="destructive">失败 {lastRun.invalid}</Badge>
                )}
              </div>
              <p className="text-xs text-muted-foreground">{lastRun.summary}</p>
              {lastRun.errors && lastRun.errors.length > 0 && (
                <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all max-h-40 overflow-y-auto">
                  {lastRun.errors.join('\n')}
                </pre>
              )}
            </CardContent>
          </Card>
        )}

        {/* 上号记录 */}
        <Card>
          <CardHeader className="flex-row items-center justify-between space-y-0">
            <CardTitle className="text-base">
              上号记录
              <span className="ml-2 text-xs font-normal text-muted-foreground">
                每个成功上号的 Key 一条，共 {onboardLogs?.total ?? 0} 条
              </span>
            </CardTitle>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => refetchOnboard()}
                disabled={onboardFetching}
              >
                {onboardFetching && <Loader2 className="h-3 w-3 mr-1 animate-spin" />}
                刷新
              </Button>
              <Button
                variant="destructive"
                size="sm"
                disabled={clearOnboardMut.isPending || onboardItems.length === 0}
                onClick={() => {
                  if (!confirm('清空全部上号记录？此操作不可恢复（不影响已上号的凭据）。')) return
                  clearOnboardMut.mutate()
                }}
              >
                清空
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            <div className="rounded-md border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="whitespace-nowrap">时间</TableHead>
                    <TableHead>凭据</TableHead>
                    <TableHead>Key</TableHead>
                    <TableHead>订单号</TableHead>
                    <TableHead>绑定代理</TableHead>
                    <TableHead>状态</TableHead>
                    <TableHead>来源</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {onboardItems.length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={7} className="h-24 text-center text-muted-foreground">
                        还没有成功上号的记录
                      </TableCell>
                    </TableRow>
                  ) : (
                    onboardItems.map((it) => (
                      <TableRow key={it.id}>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {fmtTime(it.at)}
                        </TableCell>
                        <TableCell className="font-mono text-xs">#{it.credentialId}</TableCell>
                        <TableCell className="font-mono text-xs">{it.keyMasked}</TableCell>
                        <TableCell className="font-mono text-xs">
                          {it.orderId ?? <span className="text-muted-foreground">—</span>}
                        </TableCell>
                        <TableCell
                          className="font-mono text-xs max-w-[240px] truncate"
                          title={it.proxyUrl ?? it.proxyId ?? ''}
                        >
                          {it.proxyUrl ?? it.proxyId ?? (
                            <span className="text-muted-foreground">未绑定</span>
                          )}
                        </TableCell>
                        <TableCell>
                          {it.enabled ? (
                            <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 text-xs">
                              已启用
                            </Badge>
                          ) : (
                            <Badge variant="warning" className="text-xs" title={it.note ?? ''}>
                              未启用
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell className="text-xs">{triggerLabel(it.triggerKind)}</TableCell>
                      </TableRow>
                    ))
                  )}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>

        {/* 轮询记录 */}
        <Card>
          <CardHeader className="flex-row items-center justify-between space-y-0">
            <CardTitle className="text-base">
              轮询记录
              <span className="ml-2 text-xs font-normal text-muted-foreground">
                每次请求上游一条（含原始响应），共 {pollLogs?.total ?? 0} 条
              </span>
            </CardTitle>
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => refetchPolls()}
                disabled={pollFetching}
              >
                {pollFetching && <Loader2 className="h-3 w-3 mr-1 animate-spin" />}
                刷新
              </Button>
              <Button
                variant="destructive"
                size="sm"
                disabled={clearPollMut.isPending || pollItems.length === 0}
                onClick={() => {
                  if (!confirm('清空全部轮询记录？此操作不可恢复。')) return
                  clearPollMut.mutate()
                }}
              >
                清空
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            <div className="rounded-md border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className="whitespace-nowrap">时间</TableHead>
                    <TableHead>来源</TableHead>
                    <TableHead>结果</TableHead>
                    <TableHead className="text-right">上游</TableHead>
                    <TableHead className="text-right">active</TableHead>
                    <TableHead className="text-right">新增</TableHead>
                    <TableHead className="text-right">失败</TableHead>
                    <TableHead>摘要</TableHead>
                    <TableHead className="text-right">操作</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {pollItems.length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={9} className="h-24 text-center text-muted-foreground">
                        还没有轮询记录
                      </TableCell>
                    </TableRow>
                  ) : (
                    pollItems.map((it) => (
                      <TableRow key={it.id}>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {fmtTime(it.at)}
                        </TableCell>
                        <TableCell className="text-xs">{triggerLabel(it.triggerKind)}</TableCell>
                        <TableCell>
                          {it.ok ? (
                            <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 text-xs">
                              OK{it.httpStatus ? ` ${it.httpStatus}` : ''}
                            </Badge>
                          ) : (
                            <Badge variant="destructive" className="text-xs">
                              {it.httpStatus ?? '网络错误'}
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs">{it.total}</TableCell>
                        <TableCell className="text-right font-mono text-xs">{it.active}</TableCell>
                        <TableCell className="text-right font-mono text-xs">
                          {it.added > 0 ? (
                            <span className="text-emerald-600 dark:text-emerald-400">{it.added}</span>
                          ) : (
                            it.added
                          )}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs">
                          {it.invalid > 0 ? (
                            <span className="text-orange-600 dark:text-orange-400">{it.invalid}</span>
                          ) : (
                            it.invalid
                          )}
                        </TableCell>
                        <TableCell className="text-xs max-w-[280px] truncate" title={it.summary}>
                          {it.summary}
                        </TableCell>
                        <TableCell className="text-right">
                          <Button
                            size="sm"
                            variant="outline"
                            className="h-7 px-2 text-xs"
                            onClick={() => setDetailId(it.id)}
                          >
                            <Eye className="h-3 w-3 mr-1" />
                            查看
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))
                  )}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>

        {/* 已处理 Key（去重表）*/}
        <Card>
          <CardHeader className="flex-row items-center justify-between space-y-0">
            <CardTitle className="text-base">
              已处理 Key
              <span className="ml-2 text-xs font-normal text-muted-foreground">
                用于防止重复上号，共 {seenList?.total ?? 0} 条
              </span>
            </CardTitle>
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => refetchSeen()} disabled={seenFetching}>
                {seenFetching && <Loader2 className="h-3 w-3 mr-1 animate-spin" />}
                刷新
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={clearSeenMut.isPending}
                onClick={() => {
                  if (!confirm('移除所有「失败」记录？这些 Key 下次轮询会重新尝试上号（已上号的记录保留）。'))
                    return
                  clearSeenMut.mutate(true)
                }}
              >
                重试全部失败
              </Button>
              <Button
                variant="destructive"
                size="sm"
                disabled={clearSeenMut.isPending}
                onClick={() => {
                  if (
                    !confirm(
                      '清空全部去重记录？包括已上号的——之后若上游仍返回这些 Key，且本地凭据已被删除，会被重新上号一次。'
                    )
                  )
                    return
                  clearSeenMut.mutate(false)
                }}
              >
                全部清空
              </Button>
            </div>
          </CardHeader>
          <CardContent>
            <p className="text-xs text-muted-foreground mb-2">
              上号成功的 Key 永久记在这里，即使之后把凭据删了也不会被重复上号；
              验证失败的 Key 累计到「失败重试上限」后也不再尝试。移除某条即可让它重新参与上号。
            </p>
            <div className="rounded-md border">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Key</TableHead>
                    <TableHead>结果</TableHead>
                    <TableHead>凭据</TableHead>
                    <TableHead className="text-right">尝试次数</TableHead>
                    <TableHead className="whitespace-nowrap">首次</TableHead>
                    <TableHead className="whitespace-nowrap">最近</TableHead>
                    <TableHead>备注</TableHead>
                    <TableHead className="text-right">操作</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {(seenList?.items ?? []).length === 0 ? (
                    <TableRow>
                      <TableCell colSpan={8} className="h-24 text-center text-muted-foreground">
                        还没有处理过任何 Key
                      </TableCell>
                    </TableRow>
                  ) : (
                    (seenList?.items ?? []).map((it) => (
                      <TableRow key={it.keyHash}>
                        <TableCell className="font-mono text-xs">{it.keyMasked}</TableCell>
                        <TableCell>
                          {it.outcome === 'onboarded' ? (
                            <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 text-xs">
                              已上号
                            </Badge>
                          ) : it.outcome === 'invalid' ? (
                            <Badge variant="destructive" className="text-xs">
                              验证失败
                            </Badge>
                          ) : (
                            <Badge variant="secondary" className="text-xs">
                              已存在
                            </Badge>
                          )}
                        </TableCell>
                        <TableCell className="font-mono text-xs">
                          {it.credentialId != null ? (
                            `#${it.credentialId}`
                          ) : (
                            <span className="text-muted-foreground">—</span>
                          )}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs">{it.attempts}</TableCell>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {fmtTime(it.firstSeen)}
                        </TableCell>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {fmtTime(it.lastSeen)}
                        </TableCell>
                        <TableCell className="text-xs max-w-[240px] truncate" title={it.note ?? ''}>
                          {it.note ?? <span className="text-muted-foreground">—</span>}
                        </TableCell>
                        <TableCell className="text-right">
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-7 px-2 text-xs"
                            disabled={deleteSeenMut.isPending}
                            title="移除后该 Key 下次轮询会重新尝试上号"
                            onClick={() => deleteSeenMut.mutate(it.keyHash)}
                          >
                            允许重上
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))
                  )}
                </TableBody>
              </Table>
            </div>
          </CardContent>
        </Card>
      </div>

      <PollLogDetailDialog id={detailId} onClose={() => setDetailId(null)} />
    </>
  )
}

function PollLogDetailDialog({ id, onClose }: { id: number | null; onClose: () => void }) {
  const { data, isLoading } = useQuery({
    queryKey: ['key-poll-log-detail', id],
    queryFn: () => getKeyPollLog(id!),
    enabled: id !== null,
  })

  const copy = (text: string) => {
    navigator.clipboard
      .writeText(text)
      .then(() => toast.success('已复制'))
      .catch(() => toast.error('复制失败'))
  }

  return (
    <Dialog open={id !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-4xl max-h-[90vh] p-0 gap-0 flex flex-col">
        <DialogHeader className="px-6 pt-6 pb-3 border-b shrink-0">
          <DialogTitle className="pr-8">轮询详情 #{id}</DialogTitle>
        </DialogHeader>
        <div className="flex-1 overflow-y-auto p-6 text-sm">
          {isLoading || !data ? (
            <div className="text-center text-muted-foreground py-8">加载中…</div>
          ) : (
            <div className="space-y-4">
              <div className="grid grid-cols-2 md:grid-cols-4 gap-3 text-xs">
                <Field label="时间">{fmtTime(data.at)}</Field>
                <Field label="来源">{triggerLabel(data.triggerKind)}</Field>
                <Field label="HTTP">{data.httpStatus ?? '—'}</Field>
                <Field label="结果">{data.ok ? 'OK' : '失败'}</Field>
                <Field label="上游总数">{data.total}</Field>
                <Field label="active">{data.active}</Field>
                <Field label="新增">{data.added}</Field>
                <Field label="失败">{data.invalid}</Field>
              </div>
              <div>
                <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground mb-1">
                  摘要
                </div>
                <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all">
                  {data.summary}
                </pre>
              </div>
              <div>
                <div className="flex items-center justify-between mb-1">
                  <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                    上游原始响应
                  </div>
                  {data.responseBody && (
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-6 px-2 text-xs"
                      onClick={() => copy(data.responseBody!)}
                    >
                      复制
                    </Button>
                  )}
                </div>
                <pre className="rounded bg-muted/40 p-2 text-xs whitespace-pre-wrap break-all max-h-[420px] overflow-y-auto">
                  {data.responseBody || (
                    <span className="text-muted-foreground">（未记录，可在配置里开启）</span>
                  )}
                </pre>
              </div>
            </div>
          )}
        </div>
        <div className="px-6 py-3 border-t flex justify-end bg-background/95">
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
