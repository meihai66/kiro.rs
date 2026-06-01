import { useEffect, useMemo, useState } from 'react'
import { toast } from 'sonner'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  RefreshCw,
  Wallet,
  Trash2,
  Loader2,
  MessageSquare,
  Shield,
  ShieldAlert,
  Link as LinkIcon,
  Save,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Switch } from '@/components/ui/switch'
import { Input } from '@/components/ui/input'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type {
  CredentialStatusItem,
  CachedBalanceInfo,
  BalanceResponse,
} from '@/types/api'
import { unbindCredentialProxy } from '@/api/proxies'
import { RpmSparkline } from '@/components/rpm-sparkline'
import { TestChatDialog } from '@/components/test-chat-dialog'
import { UsageStatsDialog } from '@/components/usage-stats-dialog'
import {
  useSetDisabled,
  useSetPriority,
  useSetRegion,
  useSetEndpoint,
  useSetEmail,
  useSetAllowOveruse,
  useSetCredentialRpm,
  useResetFailure,
  useForceRefreshToken,
  useDeleteCredential,
} from '@/hooks/use-credentials'

interface CredentialCardProps {
  credential: CredentialStatusItem
  cachedBalance?: CachedBalanceInfo
  onViewBalance: (id: number, forceRefresh: boolean) => void
  selected: boolean
  onToggleSelect: () => void
  balance: BalanceResponse | null
  loadingBalance: boolean
  onBindProxy?: (credentialId: number) => void
}

function formatLastUsed(lastUsedAt: string | null): string {
  if (!lastUsedAt) return '从未使用'
  const date = new Date(lastUsedAt)
  const diff = Date.now() - date.getTime()
  if (diff < 0) return '刚刚'
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds} 秒前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  return `${Math.floor(hours / 24)} 天前`
}

/** 美元金额：>=1 保留 2 位，<1 保留 4 位 */
function formatUsd(value: number): string {
  const v = value ?? 0
  return `$${v >= 1 ? v.toFixed(2) : v.toFixed(4)}`
}

/** 积分：大数转 k，小数保留 2 位 */
function formatCredit(value: number): string {
  const v = value ?? 0
  if (v >= 1000) return `${(v / 1000).toFixed(1)}k`
  if (v === 0) return '0'
  return v.toFixed(2).replace(/\.?0+$/, '')
}

function formatCacheAge(cachedAt: number): string {
  const diff = Date.now() - cachedAt
  const seconds = Math.floor(diff / 1000)
  if (seconds < 60) return `${seconds}s 前`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m 前`
  return `${Math.floor(minutes / 60)}h 前`
}

/** 字段标签 + 表单控件容器 */
function Field({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <label className="flex flex-col gap-1">
      <span className="text-xs text-muted-foreground">{label}</span>
      {children}
    </label>
  )
}

/** 区块标题 */
function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
      {children}
    </h3>
  )
}

export function CredentialCard({
  credential,
  cachedBalance,
  onViewBalance,
  balance,
  loadingBalance,
  onBindProxy,
}: CredentialCardProps) {
  const queryClient = useQueryClient()
  const unbindProxyMutation = useMutation({
    mutationFn: () => unbindCredentialProxy(credential.id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      toast.success('已解绑代理（凭据已禁用）')
    },
    onError: (err) => toast.error('解绑失败: ' + (err as Error).message),
  })

  const setDisabled = useSetDisabled()
  const setPriority = useSetPriority()
  const setRegion = useSetRegion()
  const setEndpoint = useSetEndpoint()
  const setEmail = useSetEmail()
  const setAllowOveruse = useSetAllowOveruse()
  const setCredentialRpmMut = useSetCredentialRpm()
  const resetFailure = useResetFailure()
  const forceRefreshToken = useForceRefreshToken()
  const deleteCredential = useDeleteCredential()

  // 表单可编辑字段（受控）；在 credential prop 变化时同步
  const [email, setEmailVal] = useState(credential.email ?? '')
  const [priority, setPriorityVal] = useState(String(credential.priority))
  const [region, setRegionVal] = useState(credential.region ?? '')
  const [apiRegion, setApiRegionVal] = useState(credential.apiRegion ?? '')
  const [endpoint, setEndpointVal] = useState(credential.endpoint ?? '')
  const [rpmCap, setRpmCap] = useState(
    credential.credentialRpm != null ? String(credential.credentialRpm) : ''
  )
  const [showDeleteDialog, setShowDeleteDialog] = useState(false)
  const [showTestChat, setShowTestChat] = useState(false)
  const [showUsageStats, setShowUsageStats] = useState(false)

  useEffect(() => {
    setEmailVal(credential.email ?? '')
    setPriorityVal(String(credential.priority))
    setRegionVal(credential.region ?? '')
    setApiRegionVal(credential.apiRegion ?? '')
    setEndpointVal(credential.endpoint ?? '')
    setRpmCap(
      credential.credentialRpm != null ? String(credential.credentialRpm) : ''
    )
  }, [
    credential.id,
    credential.email,
    credential.priority,
    credential.region,
    credential.apiRegion,
    credential.endpoint,
    credential.credentialRpm,
  ])

  const currentRpmCapValue: number | null =
    credential.credentialRpm != null && credential.credentialRpm > 0
      ? credential.credentialRpm
      : null
  const parsedRpmCap: number | null = (() => {
    const trimmed = rpmCap.trim()
    if (!trimmed) return null
    const n = parseInt(trimmed, 10)
    return Number.isFinite(n) && n > 0 ? n : null
  })()

  const dirty = useMemo(() => {
    return (
      (email.trim() || '') !== (credential.email ?? '') ||
      priority !== String(credential.priority) ||
      (region.trim() || '') !== (credential.region ?? '') ||
      (apiRegion.trim() || '') !== (credential.apiRegion ?? '') ||
      (endpoint || '') !== (credential.endpoint ?? '') ||
      parsedRpmCap !== currentRpmCapValue
    )
  }, [
    email,
    priority,
    region,
    apiRegion,
    endpoint,
    parsedRpmCap,
    currentRpmCapValue,
    credential.email,
    credential.priority,
    credential.region,
    credential.apiRegion,
    credential.endpoint,
  ])

  const saving =
    setEmail.isPending ||
    setPriority.isPending ||
    setRegion.isPending ||
    setEndpoint.isPending ||
    setCredentialRpmMut.isPending

  const handleSaveAll = async () => {
    const tasks: Promise<unknown>[] = []
    if ((email.trim() || '') !== (credential.email ?? '')) {
      tasks.push(
        setEmail.mutateAsync({
          id: credential.id,
          email: email.trim() || null,
        })
      )
    }
    if (priority !== String(credential.priority)) {
      const n = parseInt(priority, 10)
      if (isNaN(n) || n < 0) {
        toast.error('优先级必须是非负整数')
        return
      }
      tasks.push(setPriority.mutateAsync({ id: credential.id, priority: n }))
    }
    if (
      (region.trim() || '') !== (credential.region ?? '') ||
      (apiRegion.trim() || '') !== (credential.apiRegion ?? '')
    ) {
      tasks.push(
        setRegion.mutateAsync({
          id: credential.id,
          region: region.trim() || null,
          apiRegion: apiRegion.trim() || null,
        })
      )
    }
    if ((endpoint || '') !== (credential.endpoint ?? '')) {
      tasks.push(
        setEndpoint.mutateAsync({ id: credential.id, endpoint: endpoint || null })
      )
    }
    if (parsedRpmCap !== currentRpmCapValue) {
      tasks.push(
        setCredentialRpmMut.mutateAsync({
          id: credential.id,
          rpm: parsedRpmCap,
        })
      )
    }
    if (tasks.length === 0) return
    try {
      await Promise.all(tasks)
      toast.success('已保存')
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
    } catch (err) {
      toast.error('保存失败: ' + (err as Error).message)
    }
  }

  const handleToggleDisabled = () => {
    setDisabled.mutate(
      { id: credential.id, disabled: !credential.disabled },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error('操作失败: ' + (err as Error).message),
      }
    )
  }

  const handleToggleAllowOveruse = (allow: boolean) => {
    setAllowOveruse.mutate(
      { id: credential.id, allow },
      {
        onSuccess: (res) => toast.success(res.message),
        onError: (err) => toast.error('操作失败: ' + (err as Error).message),
      }
    )
  }

  const handleReset = () => {
    resetFailure.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error('操作失败: ' + (err as Error).message),
    })
  }

  const handleForceRefresh = () => {
    forceRefreshToken.mutate(credential.id, {
      onSuccess: (res) => toast.success(res.message),
      onError: (err) => toast.error('刷新失败: ' + (err as Error).message),
    })
  }

  const handleDelete = () => {
    if (!credential.disabled) {
      toast.error('请先禁用凭据再删除')
      setShowDeleteDialog(false)
      return
    }
    deleteCredential.mutate(credential.id, {
      onSuccess: (res) => {
        toast.success(res.message)
        setShowDeleteDialog(false)
      },
      onError: (err) => toast.error('删除失败: ' + (err as Error).message),
    })
  }

  const isCacheStale = () => {
    if (!cachedBalance) return true
    const ageMs = Date.now() - cachedBalance.cachedAt
    const ttlMs = (cachedBalance.ttlSecs ?? 60) * 1000
    return ageMs > ttlMs
  }

  // 余额展示（live > 缓存 > 未知）
  const balanceDisplay = (() => {
    if (loadingBalance) {
      return (
        <span className="inline-flex items-center gap-1 text-sm text-muted-foreground">
          <Loader2 className="w-3 h-3 animate-spin" />
          加载中...
        </span>
      )
    }
    if (balance) {
      return (
        <span className="font-medium">
          {balance.remaining.toFixed(2)} / {balance.usageLimit.toFixed(2)}
          <span className="text-xs text-muted-foreground ml-1">
            ({(100 - balance.usagePercentage).toFixed(1)}% 剩余)
          </span>
        </span>
      )
    }
    if (cachedBalance && cachedBalance.usageLimit > 0) {
      return (
        <span className="font-medium">
          {cachedBalance.remaining.toFixed(2)} /{' '}
          {cachedBalance.usageLimit.toFixed(2)}
          <span className="text-xs text-muted-foreground ml-1">
            ({(100 - cachedBalance.usagePercentage).toFixed(1)}% 剩余 ·{' '}
            {formatCacheAge(cachedBalance.cachedAt)}缓存)
          </span>
        </span>
      )
    }
    return <span className="text-sm text-muted-foreground">未知</span>
  })()

  return (
    <>
      <div className="space-y-5">
        {/* 顶部状态条 */}
        <div className="flex flex-wrap items-center justify-between gap-3 pb-3 border-b">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-sm text-muted-foreground">
              #{credential.id}
            </span>
            {credential.disabled ? (
              <Badge variant="destructive">已禁用</Badge>
            ) : (
              <Badge className="bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
                正常
              </Badge>
            )}
            {credential.allowOveruse && (
              <Badge className="bg-orange-500/15 text-orange-700 dark:text-orange-400">
                允许超额
              </Badge>
            )}
            {credential.authMethod && (
              <Badge variant="outline" className="font-mono text-xs">
                {credential.authMethod}
              </Badge>
            )}
            {credential.subscriptionTitle && (
              <Badge variant="secondary" className="text-xs">
                {credential.subscriptionTitle}
              </Badge>
            )}
          </div>

          <div className="flex items-center gap-3">
            <span className="text-sm text-muted-foreground">启用</span>
            <Switch
              checked={!credential.disabled}
              onCheckedChange={handleToggleDisabled}
              disabled={setDisabled.isPending}
            />
          </div>
        </div>

        {/* 基本信息 */}
        <section className="space-y-3">
          <SectionTitle>基本信息</SectionTitle>
          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            <Field label="邮箱">
              <Input
                value={email}
                onChange={(e) => setEmailVal(e.target.value)}
                placeholder="user@example.com"
                className="h-9"
              />
            </Field>
            <Field label="优先级（数字越小越优先）">
              <Input
                type="number"
                min={0}
                value={priority}
                onChange={(e) => setPriorityVal(e.target.value)}
                className="h-9"
              />
            </Field>
          </div>
        </section>

        {/* 路由配置 */}
        <section className="space-y-3">
          <SectionTitle>路由配置</SectionTitle>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            <Field label="Region">
              <Input
                value={region}
                onChange={(e) => setRegionVal(e.target.value)}
                placeholder="留空使用全局"
                className="h-9"
              />
            </Field>
            <Field label="API Region">
              <Input
                value={apiRegion}
                onChange={(e) => setApiRegionVal(e.target.value)}
                placeholder="留空使用 Region"
                className="h-9"
              />
            </Field>
            <Field label="Endpoint">
              <select
                value={endpoint}
                onChange={(e) => setEndpointVal(e.target.value)}
                className="flex h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
              >
                <option value="">默认（{credential.effectiveEndpoint}）</option>
                <option value="ide">ide</option>
                <option value="cli">cli</option>
              </select>
            </Field>
          </div>
          <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
            <Field label="RPM 上限（每分钟）">
              <Input
                type="number"
                min={0}
                inputMode="numeric"
                value={rpmCap}
                onChange={(e) => setRpmCap(e.target.value)}
                placeholder="留空 / 0 沿用全局"
                className="h-9"
              />
              <p className="text-[11px] text-muted-foreground">
                覆盖全局 credentialRpm；保存后立即生效（重启不丢）
              </p>
            </Field>
          </div>
        </section>

        {/* 控制开关 */}
        <section className="space-y-3">
          <SectionTitle>控制开关</SectionTitle>
          <div className="rounded-md border divide-y">
            <div className="flex items-center justify-between px-3 py-2.5 gap-3">
              <div className="space-y-0.5">
                <div className="text-sm font-medium">允许超额使用</div>
                <div className="text-xs text-muted-foreground">
                  开启后即使额度用尽也不主动禁用，由上游决定是否实际放行
                </div>
              </div>
              <button
                type="button"
                onClick={() => handleToggleAllowOveruse(!credential.allowOveruse)}
                disabled={setAllowOveruse.isPending}
                title={
                  credential.allowOveruse
                    ? '点击关闭：额度用尽自动禁用'
                    : '点击开启：额度用尽不主动禁用'
                }
                className={
                  'inline-flex h-7 shrink-0 items-center justify-center rounded-md px-3 text-xs font-medium text-white whitespace-nowrap transition-colors disabled:opacity-60 ' +
                  (credential.allowOveruse
                    ? 'bg-emerald-500 hover:bg-emerald-600'
                    : 'bg-red-500 hover:bg-red-600')
                }
              >
                超额{credential.allowOveruse ? '开' : '关'}
              </button>
            </div>
          </div>
        </section>

        {/* 状态信息 */}
        <section className="space-y-3">
          <SectionTitle>运行状态</SectionTitle>
          <div className="grid grid-cols-2 md:grid-cols-3 gap-x-4 gap-y-2 text-sm">
            <div>
              <div className="text-xs text-muted-foreground">成功 / 错误(累计)</div>
              <div className="font-medium">
                {credential.successCount}
                <span className="text-muted-foreground"> / </span>
                <span
                  className={
                    credential.errorCount > 0
                      ? 'text-red-500 font-medium'
                      : 'font-medium'
                  }
                >
                  {credential.errorCount}
                </span>
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">连续失败 / 刷新失败</div>
              <div>
                <span
                  className={
                    credential.failureCount > 0
                      ? 'text-red-500 font-medium'
                      : 'font-medium'
                  }
                >
                  {credential.failureCount}
                </span>
                <span className="text-muted-foreground"> / </span>
                <span
                  className={
                    credential.refreshFailureCount > 0
                      ? 'text-amber-600 font-medium'
                      : 'font-medium'
                  }
                >
                  {credential.refreshFailureCount}
                </span>
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">429 限流次数</div>
              <div
                className={
                  credential.rateLimitCount > 0
                    ? 'text-yellow-600 font-medium'
                    : 'font-medium'
                }
              >
                {credential.rateLimitCount}
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">调用状态</div>
              <div className="font-medium leading-tight">
                <div>{formatLastUsed(credential.lastUsedAt)}</div>
                <div className="text-xs text-muted-foreground">
                  {formatCredit(credential.creditUsageTotal)}
                </div>
                <button
                  type="button"
                  className="text-xs text-emerald-600 hover:underline cursor-pointer"
                  onClick={() => setShowUsageStats(true)}
                  title="查看调用统计"
                >
                  {formatUsd(credential.totalValueUsd)}
                </button>
              </div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">并发 / RPM</div>
              <div className="font-medium">
                {credential.inFlight} / {credential.rpm}
              </div>
            </div>
            <div className="col-span-2 md:col-span-1">
              <div className="text-xs text-muted-foreground">余额</div>
              <div>{balanceDisplay}</div>
            </div>
          </div>
          {credential.disabledReason && (
            <div className="rounded-md bg-destructive/10 px-3 py-2 text-xs text-destructive">
              禁用原因：{credential.disabledReason}
            </div>
          )}
        </section>

        {/* 代理槽 */}
        <section className="space-y-2">
          <SectionTitle>代理槽</SectionTitle>
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {credential.proxySlotId ? (
              <>
                <Shield className="h-4 w-4 text-emerald-600" />
                <span className="font-mono text-xs">
                  {credential.proxySlotId}
                </span>
                {onBindProxy && (
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 px-2 text-xs"
                    onClick={() => onBindProxy(credential.id)}
                  >
                    重绑
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  className="h-7 px-2 text-xs text-destructive"
                  onClick={() => unbindProxyMutation.mutate()}
                  disabled={unbindProxyMutation.isPending}
                >
                  解绑
                </Button>
              </>
            ) : (
              <>
                <Badge className="bg-yellow-500/15 text-yellow-700 dark:text-yellow-400">
                  <ShieldAlert className="h-3 w-3 mr-1" />
                  未绑定代理
                </Badge>
                {onBindProxy && (
                  <Button
                    size="sm"
                    variant="outline"
                    className="h-7 px-2 text-xs"
                    onClick={() => onBindProxy(credential.id)}
                  >
                    <LinkIcon className="h-3 w-3 mr-1" />
                    绑定
                  </Button>
                )}
              </>
            )}
          </div>
        </section>

        {/* RPM 历史 */}
        <section className="space-y-2">
          <SectionTitle>RPM 24h</SectionTitle>
          <RpmSparkline credentialId={credential.id} hours={24} />
        </section>

        {/* 底部操作栏 */}
        <div className="sticky bottom-0 -mx-6 -mb-6 px-6 py-3 border-t bg-background/95 backdrop-blur flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={handleReset}
              disabled={resetFailure.isPending || credential.failureCount === 0}
            >
              <RefreshCw className="h-3.5 w-3.5 mr-1" />
              重置失败
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={handleForceRefresh}
              disabled={forceRefreshToken.isPending}
            >
              <RefreshCw
                className={`h-3.5 w-3.5 mr-1 ${
                  forceRefreshToken.isPending ? 'animate-spin' : ''
                }`}
              />
              刷新 Token
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => setShowTestChat(true)}
              title="用此凭据发送一条最小测试请求"
            >
              <MessageSquare className="h-3.5 w-3.5 mr-1" />
              对话测试
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => onViewBalance(credential.id, isCacheStale())}
            >
              <Wallet className="h-3.5 w-3.5 mr-1" />
              查询余额
            </Button>
            <Button
              size="sm"
              variant="destructive"
              onClick={() => setShowDeleteDialog(true)}
              disabled={!credential.disabled}
              title={!credential.disabled ? '需要先禁用凭据才能删除' : undefined}
            >
              <Trash2 className="h-3.5 w-3.5 mr-1" />
              删除
            </Button>
          </div>

          <Button
            size="sm"
            onClick={handleSaveAll}
            disabled={!dirty || saving}
            className="min-w-[88px]"
          >
            {saving ? (
              <Loader2 className="h-3.5 w-3.5 mr-1 animate-spin" />
            ) : (
              <Save className="h-3.5 w-3.5 mr-1" />
            )}
            保存修改
          </Button>
        </div>
      </div>

      <Dialog open={showDeleteDialog} onOpenChange={setShowDeleteDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>确认删除凭据</DialogTitle>
            <DialogDescription>
              您确定要删除凭据 #{credential.id} 吗？此操作无法撤销。
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowDeleteDialog(false)}
              disabled={deleteCredential.isPending}
            >
              取消
            </Button>
            <Button
              variant="destructive"
              onClick={handleDelete}
              disabled={deleteCredential.isPending || !credential.disabled}
            >
              确认删除
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <TestChatDialog
        open={showTestChat}
        onOpenChange={setShowTestChat}
        credentialId={credential.id}
      />
      <UsageStatsDialog
        credential={credential}
        open={showUsageStats}
        onOpenChange={setShowUsageStats}
      />
    </>
  )
}
