import { useEffect, useMemo, useRef, useState } from 'react'
import {
  Activity,
  CheckCircle2,
  Clock,
  Gauge,
  Loader2,
  Plus,
  TrendingUp,
  Upload,
  Wallet,
  Zap,
} from 'lucide-react'
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
import { getStatsSummary } from '@/api/credentials'

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
  useGlobalConfig,
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

/** 额度使用条：分段刻度 + 渐变填充，比纯色单条更清晰；
 * 100% 满格时仍能看见底色，超额时整条饱和深红。 */
function UsageBar({
  pct,
  tone,
}: {
  pct: number
  tone: 'red' | 'yellow' | 'emerald'
}) {
  const safe = Math.max(0, Math.min(100, pct))
  const fillCls =
    tone === 'red'
      ? // 满额红色：用 rose-400→red-500（更柔和不刺眼）+ /85 整体降亮度
        'bg-gradient-to-r from-rose-400 to-red-500 opacity-85 dark:opacity-90'
      : tone === 'yellow'
        ? 'bg-gradient-to-r from-yellow-400 to-orange-500'
        : 'bg-gradient-to-r from-emerald-500 to-teal-500'
  return (
    <div
      className="relative h-1.5 rounded-full overflow-hidden bg-muted/60"
      role="progressbar"
      aria-valuenow={safe}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <div
        className={`absolute inset-y-0 left-0 rounded-full ${fillCls} transition-all duration-300`}
        style={{ width: `${safe}%` }}
      />
      {/* 25/50/75 刻度线，仅在 muted 上方淡淡显示 */}
      <div className="pointer-events-none absolute inset-0 flex">
        <div className="flex-1 border-r border-background/40" />
        <div className="flex-1 border-r border-background/40" />
        <div className="flex-1 border-r border-background/40" />
        <div className="flex-1" />
      </div>
    </div>
  )
}

/** 单字符图标徽章：方形大写首字母，用于 auth/订阅一体化展示 */
function LetterIcon({
  letter,
  cls,
  title,
}: {
  letter: string
  cls: string
  title: string
}) {
  return (
    <span
      className={`inline-flex h-5 w-5 items-center justify-center rounded text-[11px] font-bold leading-none ring-1 ${cls}`}
      title={title}
    >
      {letter}
    </span>
  )
}

/** 认证方式 → 单字符图标。social=S(蓝), idc=I(紫), api_key=K(灰) */
function AuthBadge({ method }: { method: string }) {
  const lower = method.toLowerCase()
  if (lower === 'social') {
    return (
      <LetterIcon
        letter="S"
        cls="bg-blue-500/15 text-blue-700 dark:text-blue-400 ring-blue-500/30"
        title={`认证：social`}
      />
    )
  }
  if (lower === 'idc' || lower === 'builder-id' || lower === 'iam') {
    return (
      <LetterIcon
        letter="I"
        cls="bg-purple-500/15 text-purple-700 dark:text-purple-400 ring-purple-500/30"
        title={`认证：${method}`}
      />
    )
  }
  if (lower === 'api_key' || lower === 'apikey') {
    return (
      <LetterIcon
        letter="K"
        cls="bg-slate-500/15 text-slate-700 dark:text-slate-400 ring-slate-500/30"
        title={`认证：API Key`}
      />
    )
  }
  return (
    <LetterIcon
      letter={method.charAt(0).toUpperCase() || '?'}
      cls="bg-muted text-muted-foreground ring-muted-foreground/30"
      title={`认证：${method}`}
    />
  )
}

/** 订阅 → 单字符图标。PRO+/PRO=P(绿), Solo=S(蓝), Indie=I(紫), Team=T(橙), Free=F(黄) */
function SubscriptionBadge({ title }: { title: string }) {
  const upper = title.toUpperCase()
  let letter = '?'
  let cls = 'bg-muted text-muted-foreground ring-muted-foreground/30'
  if (upper.includes('PRO')) {
    // PRO+ 和 PRO 共用绿色 P，title 区分
    letter = 'P'
    cls =
      'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 ring-emerald-500/30'
  } else if (upper.includes('SOLO')) {
    letter = 'S'
    cls = 'bg-blue-500/15 text-blue-700 dark:text-blue-400 ring-blue-500/30'
  } else if (upper.includes('INDIE') || upper.includes('INDIVIDUAL')) {
    letter = 'I'
    cls =
      'bg-purple-500/15 text-purple-700 dark:text-purple-400 ring-purple-500/30'
  } else if (upper.includes('TEAM') || upper.includes('BUSINESS')) {
    letter = 'T'
    cls = 'bg-orange-500/15 text-orange-700 dark:text-orange-400 ring-orange-500/30'
  } else if (upper.includes('FREE') || upper.includes('TRIAL')) {
    letter = 'F'
    cls = 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 ring-yellow-500/30'
  }
  return <LetterIcon letter={letter} cls={cls} title={`套餐：${title}`} />
}

/** 优先级图标：与 LetterIcon 同尺寸（5×5 圆角方块），里面显示数字。
 * 颜色按数值取色——0 红（最高优先），数字越大越淡。 */
function PriorityChip({ priority }: { priority: number }) {
  const cls =
    priority === 0
      ? 'bg-red-500/15 text-red-700 dark:text-red-400 ring-red-500/30'
      : priority === 1
        ? 'bg-orange-500/15 text-orange-700 dark:text-orange-400 ring-orange-500/30'
        : priority === 2
          ? 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 ring-yellow-500/30'
          : priority === 3
            ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 ring-emerald-500/30'
            : priority === 4
              ? 'bg-blue-500/15 text-blue-700 dark:text-blue-400 ring-blue-500/30'
              : 'bg-muted text-muted-foreground ring-muted-foreground/30'
  return (
    <span
      className={`inline-flex h-5 w-5 items-center justify-center rounded text-[11px] font-bold leading-none ring-1 ${cls}`}
      title={`优先级 ${priority}（数字越小越优先）`}
    >
      {priority}
    </span>
  )
}

/** 顶部紧凑状态条单元（图标 + label + 大数字 + 备注） */
function StatCell({
  icon: Icon,
  label,
  value,
  hint,
  tone = 'default',
}: {
  icon: React.ComponentType<{ className?: string }>
  label: string
  value: string | number
  hint?: string
  tone?: 'default' | 'ok' | 'warn'
}) {
  const valueCls =
    tone === 'ok'
      ? 'text-emerald-600 dark:text-emerald-400'
      : tone === 'warn'
        ? 'text-yellow-600 dark:text-yellow-400'
        : 'text-foreground'
  return (
    <div className="flex items-center gap-2.5 px-3 py-1.5 first:pl-0 last:pr-0">
      <Icon className="h-4 w-4 text-muted-foreground shrink-0" />
      <div className="flex flex-col leading-tight">
        <span className="text-[10px] uppercase tracking-wide text-muted-foreground">
          {label}
        </span>
        <span className={`text-sm font-semibold font-mono ${valueCls}`}>
          {value}
          {hint && (
            <span className="ml-1 text-[10px] font-normal text-muted-foreground">
              {hint}
            </span>
          )}
        </span>
      </div>
    </div>
  )
}

function formatUptime(secs: number): string {
  if (secs < 60) return `${Math.max(0, Math.floor(secs))}s`
  const m = Math.floor(secs / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  const mm = m % 60
  if (h < 24) return mm > 0 ? `${h}h ${mm}m` : `${h}h`
  const d = Math.floor(h / 24)
  const hh = h % 24
  return hh > 0 ? `${d}d ${hh}h` : `${d}d`
}

/** 把数字按 K/M 单位格式化：1100 → 1.1k；12_500 → 12.5k；2_000_000 → 2.0M */
function formatK(n: number): string {
  const abs = Math.abs(n)
  if (abs >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (abs >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  // < 1000：整数取整、小数保留 1 位
  if (Number.isInteger(n)) return String(n)
  return n.toFixed(1)
}

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
  /** 全局 credentialRpm（凭据级未覆盖时作为"上限"展示） */
  globalCredentialRpm: number | null
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
          <div
            className="inline-flex items-center gap-1.5 text-xs w-[170px]"
            title={
              `优先级 ${c.priority}` +
              (c.email ? ` · ${c.email}` : '') +
              (c.allowOveruse ? ' · 已开启允许超额使用' : '')
            }
          >
            <PriorityChip priority={c.priority} />
            {c.allowOveruse && (
              <span
                className="inline-block h-2 w-2 rounded-full bg-orange-500 shrink-0"
                title="允许超额使用：开"
              />
            )}
            <span className="truncate min-w-0 flex-1">
              {c.email || <span className="text-muted-foreground">—</span>}
            </span>
          </div>
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
      cell: ({ row }) => {
        const c = row.original
        return (
          <div className="flex items-center gap-1">
            {c.authMethod ? (
              <AuthBadge method={c.authMethod} />
            ) : (
              <span className="text-xs text-muted-foreground">—</span>
            )}
            {c.subscriptionTitle && (
              <SubscriptionBadge title={c.subscriptionTitle} />
            )}
          </div>
        )
      },
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
        const remaining = live?.remaining ?? cached?.remaining ?? 0
        const baseUsed = Math.min(limit, Math.max(0, limit - remaining))
        const overage = remaining < 0 ? -remaining : 0
        const totalUsed = baseUsed + overage
        const pct = (totalUsed / limit) * 100
        const tone: 'red' | 'yellow' | 'emerald' =
          overage > 0 || pct >= 90
            ? 'red'
            : pct >= 70
              ? 'yellow'
              : 'emerald'
        const textCls =
          tone === 'red'
            ? 'text-red-600 dark:text-red-400'
            : tone === 'yellow'
              ? 'text-yellow-600 dark:text-yellow-400'
              : 'text-emerald-600 dark:text-emerald-400'
        // 显示规则：常规值原样（保留 1 位小数）；只有超额量用 K 简写
        const fmt = (n: number) =>
          Number.isInteger(n) ? String(n) : n.toFixed(1)
        return (
          <div
            className="flex flex-col gap-1 min-w-[150px]"
            title={`已用 ${totalUsed.toFixed(2)}（含超额 ${overage.toFixed(2)}），合计 ${pct.toFixed(1)}%`}
          >
            <div className={`text-xs font-mono whitespace-nowrap ${textCls}`}>
              {fmt(totalUsed)} / {fmt(limit)}
              {overage > 0 && (
                <span className="ml-1 text-[11px] font-semibold text-red-600">
                  +{formatK(overage)}
                </span>
              )}
            </div>
            <UsageBar pct={pct} tone={tone} />
          </div>
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
      accessorKey: 'rpm',
      header: 'RPM',
      cell: ({ row }) => {
        const c = row.original
        const credCap = c.credentialRpm ?? null
        const globalCap = ctx.globalCredentialRpm
        const cap = credCap ?? globalCap ?? null
        const capSource = credCap != null ? '凭据级' : globalCap ? '全局' : '未设置'
        const pct = cap && cap > 0 ? Math.min(100, (c.rpm / cap) * 100) : 0
        const tone: 'red' | 'yellow' | 'emerald' =
          cap == null
            ? 'emerald'
            : pct >= 100 || pct >= 90
              ? 'red'
              : pct >= 70
                ? 'yellow'
                : 'emerald'
        const textCls =
          cap == null
            ? 'text-muted-foreground'
            : tone === 'red'
              ? 'text-red-600 dark:text-red-400'
              : tone === 'yellow'
                ? 'text-yellow-600 dark:text-yellow-400'
                : 'text-emerald-600 dark:text-emerald-400'
        return (
          <div
            className="flex flex-col gap-1 min-w-[110px]"
            title={
              cap
                ? `实时 RPM ${c.rpm}；上限 ${cap}/min（${capSource}）`
                : `实时 RPM ${c.rpm}；未设上限`
            }
          >
            <div className={`text-xs font-mono whitespace-nowrap ${textCls}`}>
              {c.rpm}
              <span
                className={
                  'ml-0.5 ' +
                  (credCap != null
                    ? 'text-emerald-600 dark:text-emerald-400 font-semibold'
                    : 'text-muted-foreground')
                }
              >
                /{cap ?? '—'}
              </span>
            </div>
            {cap != null && cap > 0 && <UsageBar pct={pct} tone={tone} />}
          </div>
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
  const { data: globalConfig } = useGlobalConfig()
  const globalCredentialRpm = globalConfig?.credentialRpm ?? null
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
  const [priorityFilters, setPriorityFilters] = useState<Set<number>>(
    () => new Set()
  )
  const [authFilters, setAuthFilters] = useState<Set<string>>(() => new Set())
  const [overuseFilter, setOveruseFilter] = useState<'' | 'on' | 'off'>('')

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
  const togglePriorityFilter = (v: number) => {
    setPriorityFilters((prev) => {
      const next = new Set(prev)
      if (next.has(v)) next.delete(v)
      else next.add(v)
      return next
    })
  }
  const toggleAuthFilter = (v: string) => {
    setAuthFilters((prev) => {
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

  // 顶部状态条数据：服务级摘要（运行时间/总请求/成功率）+ 凭据汇总（in-flight/RPM）
  const { data: statsSummary } = useQuery({
    queryKey: ['stats-summary'],
    queryFn: getStatsSummary,
    refetchInterval: 30_000,
  })
  const totalInFlight = allCredentials.reduce(
    (s, c) => s + (c.inFlight ?? 0),
    0
  )
  const totalRpm = allCredentials.reduce((s, c) => s + (c.rpm ?? 0), 0)

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

  // 优先级取值集合（动态）+ 计数
  const priorityOptions = useMemo(() => {
    const set = new Set<number>()
    allCredentials.forEach((c) => set.add(c.priority))
    return Array.from(set).sort((a, b) => a - b)
  }, [allCredentials])
  const priorityCountMap = useMemo(() => {
    const m = new Map<number, number>()
    allCredentials.forEach((c) => m.set(c.priority, (m.get(c.priority) ?? 0) + 1))
    return m
  }, [allCredentials])

  // 认证类型集合（动态）+ 计数
  const authOptions = useMemo(() => {
    const set = new Set<string>()
    allCredentials.forEach((c) => {
      if (c.authMethod) set.add(c.authMethod)
    })
    return Array.from(set).sort()
  }, [allCredentials])
  const authCountMap = useMemo(() => {
    const m = new Map<string, number>()
    allCredentials.forEach((c) => {
      const k = c.authMethod ?? '—'
      m.set(k, (m.get(k) ?? 0) + 1)
    })
    return m
  }, [allCredentials])

  // 允许超额计数
  const overuseCountMap = useMemo(() => {
    let on = 0
    let off = 0
    allCredentials.forEach((c) => (c.allowOveruse ? on++ : off++))
    return { on, off }
  }, [allCredentials])

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
      // 优先级：选中的任一档位命中即通过
      if (priorityFilters.size > 0 && !priorityFilters.has(c.priority)) {
        return false
      }
      // 认证：选中的任一类型命中即通过
      if (authFilters.size > 0 && !authFilters.has(c.authMethod ?? '')) {
        return false
      }
      // 允许超额：on/off 单选
      if (overuseFilter === 'on' && !c.allowOveruse) return false
      if (overuseFilter === 'off' && c.allowOveruse) return false
      return true
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    allCredentials,
    statusFilters,
    usageLimitFilters,
    priorityFilters,
    authFilters,
    overuseFilter,
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
        globalCredentialRpm,
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
      globalCredentialRpm,
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
      <div className="flex items-center justify-between mb-3">
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

      {/* 状态条：服务级 + 凭据汇总 */}
      <div className="mb-3 flex items-stretch flex-wrap divide-x divide-border rounded-md border bg-card overflow-hidden">
        <StatCell
          icon={Clock}
          label="运行时间"
          value={statsSummary ? formatUptime(statsSummary.uptimeSecs) : '—'}
          hint={
            statsSummary
              ? `自 ${new Date(statsSummary.startedAt).toLocaleString('zh-CN', { hour12: false })}`
              : undefined
          }
        />
        <StatCell
          icon={TrendingUp}
          label="总请求"
          value={statsSummary?.totalRequests ?? 0}
        />
        <StatCell
          icon={CheckCircle2}
          label="成功"
          value={statsSummary?.totalSuccess ?? 0}
          tone="ok"
          hint={
            statsSummary && statsSummary.totalRequests > 0
              ? `${((statsSummary.totalSuccess / statsSummary.totalRequests) * 100).toFixed(1)}%`
              : undefined
          }
        />
        <StatCell
          icon={Activity}
          label="并发"
          value={totalInFlight}
          tone={totalInFlight > 0 ? 'ok' : 'default'}
        />
        <StatCell icon={Gauge} label="实时 RPM" value={totalRpm} />
        {(statsSummary?.totalFail ?? 0) > 0 && (
          <StatCell
            icon={Zap}
            label="失败"
            value={statsSummary?.totalFail ?? 0}
            tone="warn"
          />
        )}
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

        {/* 优先级 */}
        {priorityOptions.length > 0 && (
          <div className="flex items-center gap-1">
            <span className="text-muted-foreground mr-1">优先级</span>
            <div className="inline-flex rounded-md border overflow-hidden">
              {priorityOptions.map((p, i) => {
                const active = priorityFilters.has(p)
                const count = priorityCountMap.get(p) ?? 0
                return (
                  <button
                    key={p}
                    type="button"
                    onClick={() => togglePriorityFilter(p)}
                    className={
                      'h-7 px-3 text-xs transition-colors ' +
                      (i > 0 ? 'border-l ' : '') +
                      (active
                        ? 'bg-primary text-primary-foreground'
                        : 'bg-background hover:bg-muted')
                    }
                  >
                    {p}
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
        )}

        {/* 认证类型 */}
        {authOptions.length > 0 && (
          <div className="flex items-center gap-1">
            <span className="text-muted-foreground mr-1">认证</span>
            <div className="inline-flex rounded-md border overflow-hidden">
              {authOptions.map((m, i) => {
                const active = authFilters.has(m)
                const count = authCountMap.get(m) ?? 0
                return (
                  <button
                    key={m}
                    type="button"
                    onClick={() => toggleAuthFilter(m)}
                    className={
                      'h-7 px-3 text-xs font-mono transition-colors ' +
                      (i > 0 ? 'border-l ' : '') +
                      (active
                        ? 'bg-primary text-primary-foreground'
                        : 'bg-background hover:bg-muted')
                    }
                  >
                    {m}
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
        )}

        {/* 允许超额 */}
        <div className="flex items-center gap-1">
          <span className="text-muted-foreground mr-1">允许超额</span>
          <div className="inline-flex rounded-md border overflow-hidden">
            {(
              [
                { value: 'on' as const, label: '开', count: overuseCountMap.on },
                {
                  value: 'off' as const,
                  label: '关',
                  count: overuseCountMap.off,
                },
              ]
            ).map((opt, i) => {
              const active = overuseFilter === opt.value
              return (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() =>
                    setOveruseFilter(active ? '' : (opt.value as 'on' | 'off'))
                  }
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
                    ({opt.count})
                  </span>
                </button>
              )
            })}
          </div>
        </div>

        <span className="text-muted-foreground">
          匹配 {credentials.length} / 共 {allCredentials.length}
        </span>
        {(statusFilters.size > 0 ||
          usageLimitFilters.size > 0 ||
          priorityFilters.size > 0 ||
          authFilters.size > 0 ||
          overuseFilter !== '') && (
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-xs"
            onClick={() => {
              setStatusFilters(new Set())
              setUsageLimitFilters(new Set())
              setPriorityFilters(new Set())
              setAuthFilters(new Set())
              setOveruseFilter('')
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
