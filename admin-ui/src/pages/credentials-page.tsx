import { memo, useEffect, useMemo, useRef, useState } from 'react'
import {
  Activity,
  BarChart3,
  CheckCircle2,
  CheckSquare,
  Clock,
  Database,
  Gauge,
  Loader2,
  ListFilter,
  Percent,
  PlayCircle,
  Plus,
  Square,
  SquareDot,
  Target,
  TrendingUp,
  Upload,
  Wallet,
  XCircle,
  Zap,
} from 'lucide-react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { ColumnDef, RowSelectionState } from '@tanstack/react-table'
import { toast } from 'sonner'

import { AddCredentialDialog } from '@/components/add-credential-dialog'
import { BalanceDialog } from '@/components/balance-dialog'
import { UsageStatsDialog } from '@/components/usage-stats-dialog'
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
  exportCredentials as exportCredentialsApi,
  forceRefreshToken as forceRefreshTokenApi,
  getCredentialBalance,
  setCredentialAllowOveruse as setCredentialAllowOveruseApi,
  setCredentialApiRegionOnly as setCredentialApiRegionOnlyApi,
  setCredentialDisabled as setCredentialDisabledApi,
  setCredentialPriority as setCredentialPriorityApi,
  setCredentialRegionOnly as setCredentialRegionOnlyApi,
  setCredentialRpm as setCredentialRpmApi,
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
  | 'setRpm'
  | 'setRegion'
  | 'setApiRegion'
  | 'overageOn'
  | 'overageOff'
  | 'allowOveruseOn'
  | 'allowOveruseOff'
  | 'verify'
  | 'refreshToken'
  | 'queryBalance'
  | 'export'
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
  // 满额"危险"色：用 dusty rose 暗底 + 斜纹叠加，类似 CI/CD danger zone 风格
  // 不再用纯红渐变（太亮/刺眼），但仍保持"警示性"足够明显
  const fillStyle: React.CSSProperties =
    tone === 'red'
      ? {
          backgroundColor: 'rgb(244 63 94 / 0.55)', // rose-500 @ 55% 不透明
          backgroundImage:
            'repeating-linear-gradient(' +
            '45deg, ' +
            'rgb(190 18 60 / 0.35) 0 4px, ' + // rose-700 暗斜纹
            'transparent 4px 8px)',
        }
      : {}
  const fillCls =
    tone === 'red'
      ? '' // 用 inline style 控制
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
        style={{ width: `${safe}%`, ...fillStyle }}
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

/** 订阅 → 单字符图标。Power=⚡(玫红), PRO+/PRO=P(绿), Solo=S(蓝), Indie=I(紫), Team=T(橙), Free=F(黄) */
function SubscriptionBadge({ title }: { title: string }) {
  const upper = title.toUpperCase()
  let letter = '?'
  let cls = 'bg-muted text-muted-foreground ring-muted-foreground/30'
  if (upper.includes('POWER')) {
    letter = '⚡'
    cls = 'bg-rose-500/15 text-rose-700 dark:text-rose-400 ring-rose-500/30'
  } else if (upper.includes('PRO')) {
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

function formatUsdShort(value: number): string {
  const v = value ?? 0
  return `$${v >= 1 ? v.toFixed(2) : v.toFixed(4)}`
}

function formatCreditShort(value: number): string {
  const v = value ?? 0
  if (v >= 1000) return `${(v / 1000).toFixed(1)}k`
  if (v === 0) return '0'
  return v.toFixed(2).replace(/\.?0+$/, '')
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
  onForceRefresh: (id: number) => void
  onBindProxy: (id: number) => void
  onUnbindProxy: (id: number) => void
  onTestProxy: (slotId: string) => void
  onToggleOverage: (cred: CredentialStatusItem) => void
  onToggleAllowOveruse: (id: number, allow: boolean) => void
  onShowDetail: (cred: CredentialStatusItem) => void
  onShowUsage: (cred: CredentialStatusItem) => void
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

const EMPTY_FILL = 'var(--color-muted)'
/** 全成功时的纯绿（emerald-500 基调），与渐变起点稍有区分，让“出现失败”一眼可见 */
const PERFECT_GREEN = 'hsl(158, 75%, 40%)'
/** 失败率→颜色的容忍度指数：>1 时低失败率更偏绿，需接近全失败才转红（数值越大越容忍） */
const FAIL_TOLERANCE = 1.3

/** 每格按“失败率”在 绿→黄→红 之间连续取色（失败与 429 合并计入失败）。
 *
 * - 全成功 = 纯绿（PERFECT_GREEN）；
 * - 一旦出现失败/429，从黄绿开始，随失败率升高经黄色过渡到红色（走 hue 而非 RGB 直混，
 *   避免绿红直接混成脏褐色）；
 * - 失败率经 `^FAIL_TOLERANCE` 映射：少量失败仍明显偏绿，接近全失败才转纯红。
 *
 * 每格 100 次请求 → 失败率天然是 0%~100% 的百分比刻度，不同失败数对应不同颜色。 */
function outcomeCellColor(s: number, e: number, r: number): string {
  const total = s + e + r
  if (total <= 0) return EMPTY_FILL
  const bad = e + r // 失败与 429 合并计入
  if (bad === 0) return PERFECT_GREEN
  const failRate = Math.min(bad / total, 1)
  // hue：约 100°(黄绿) → 0°(红)，经 60°(黄) 过渡；容忍度曲线让低失败率停留在绿端
  const hue = 100 * (1 - Math.pow(failRate, FAIL_TOLERANCE))
  return `hsl(${hue.toFixed(0)}, 82%, 46%)`
}

/** 最近 1000 次请求分布条：一行 10 格（每格 = 100 次请求的一块，左旧右新），
 * 格色 = 该块内 失败率（失败与 429 合并）在 绿→黄→红 连续渐变上取色（全成功纯绿，
 * 失败率越高越偏红，容忍度较大：少量失败仍偏绿）；
 * 最新一格未满 100 次时按次数部分填充，其余灰底。
 * 共 10 个节点 + 组件级 memo，渲染开销可忽略；高度 7px 不撑高表格行。 */
const RecentOutcomeStrip = memo(function RecentOutcomeStrip({
  outcomes,
}: {
  outcomes: string
}) {
  const SLOTS = 10
  const PER_SLOT = 100
  // 旧 → 新按 100 次切块；不足 10 块时左侧补空格，让最新一块始终在右侧
  const chunks: (string | null)[] = []
  for (let i = 0; i < outcomes.length; i += PER_SLOT) {
    chunks.push(outcomes.slice(i, i + PER_SLOT))
  }
  while (chunks.length < SLOTS) chunks.unshift(null)
  const slots = chunks.slice(-SLOTS)

  return (
    <div
      className="mt-0.5 flex h-[7px] items-stretch gap-px"
      title="最近 1000 次请求（每格 100 次，左旧右新）：格色 = 失败率(失败+429合并) 在 绿→黄→红 上取色，全成功=绿，越红失败越多"
    >
      {slots.map((chunk, i) => {
        if (chunk === null) {
          return <div key={i} className="w-[9px] rounded-[1px] bg-muted opacity-60" />
        }
        const counts = { s: 0, e: 0, r: 0 }
        for (const ch of chunk) {
          if (ch === 's' || ch === 'e' || ch === 'r') counts[ch]++
        }
        const fill = outcomeCellColor(counts.s, counts.e, counts.r)
        const pct = (chunk.length / PER_SLOT) * 100
        return (
          <div
            key={i}
            className="flex w-[9px] items-stretch overflow-hidden rounded-[1px] bg-muted/60"
            title={`成功 ${counts.s} / 失败 ${counts.e} / 429 ${counts.r}`}
          >
            <div style={{ width: `${pct}%`, backgroundColor: fill }} />
          </div>
        )
      })}
    </div>
  )
})

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
      id: 'priority',
      accessorFn: (row) => row.priority,
      header: '优先级 / 邮箱',
      sortDescFirst: false,
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
      // 排序：异常优先（disabled=0、failing=1、rateLimited=2、normal=3）
      accessorFn: (row) => {
        const cls = classifyCredential(row)
        return cls === 'disabled'
          ? 0
          : cls === 'failing'
            ? 1
            : cls === 'rateLimited'
              ? 2
              : 3
      },
      sortDescFirst: false,
      cell: ({ row }) => {
        const c = row.original
        const cls = classifyCredential(c)
        // 状态列固定窄宽，长 reason 截断、悬浮看 title
        const wrap = 'inline-block max-w-[112px] truncate align-middle'
        if (cls === 'disabled') {
          return (
            <Badge
              variant="destructive"
              className={`text-xs ${wrap}`}
              title={c.disabledReason ?? '已禁用'}
            >
              已禁用{c.disabledReason ? ` · ${c.disabledReason}` : ''}
            </Badge>
          )
        }
        if (cls === 'rateLimited') {
          const remaining = c.cooldownRemainingSecs ?? 0
          return (
            <Badge
              className={`bg-yellow-500/15 text-yellow-700 dark:text-yellow-400 text-xs ${wrap}`}
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
              className={`bg-red-500/15 text-red-700 dark:text-red-400 text-xs ${wrap}`}
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
      enableSorting: false,
      cell: ({ row }) => {
        const c = row.original
        return (
          <div className="w-fit">
            <span
              className="font-mono text-xs whitespace-nowrap"
              title="成功 / 累计错误 / 429 限流（均为累计值）"
            >
              <span className="text-emerald-600">{c.successCount}</span>
              <span className="text-muted-foreground mx-1">/</span>
              <span
                className={c.errorCount > 0 ? 'text-red-500' : 'text-muted-foreground'}
              >
                {c.errorCount}
              </span>
              <span className="text-muted-foreground mx-1">/</span>
              <span
                className={c.rateLimitCount > 0 ? 'text-yellow-600' : 'text-muted-foreground'}
              >
                {c.rateLimitCount}
              </span>
            </span>
            <RecentOutcomeStrip outcomes={c.recentOutcomes ?? ''} />
          </div>
        )
      },
    },
    {
      accessorKey: 'authMethod',
      header: '认证',
      enableSorting: false,
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
      // 排序：按已用绝对值（含超额）；无额度数据视为 -1 排在最后
      accessorFn: (row) => {
        const live = ctx.liveBalanceMap.get(row.id)
        const cached = ctx.cachedBalanceMap.get(row.id)
        const limit = live?.usageLimit ?? cached?.usageLimit ?? 0
        if (limit <= 0) return -1
        const remaining = live?.remaining ?? cached?.remaining ?? 0
        const baseUsed = Math.min(limit, Math.max(0, limit - remaining))
        const overage = remaining < 0 ? -remaining : 0
        return baseUsed + overage
      },
      sortUndefined: 'last',
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
      enableSorting: false,
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
            className="flex flex-col gap-1 min-w-[74px]"
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
              <span
                className={
                  'ml-1 ' +
                  (c.inFlight > 0
                    ? 'text-blue-600 dark:text-blue-400 font-semibold'
                    : 'text-muted-foreground')
                }
                title={`当前并发（in-flight）${c.inFlight}`}
              >
                [{c.inFlight}]
              </span>
            </div>
            {cap != null && cap > 0 && <UsageBar pct={pct} tone={tone} />}
          </div>
        )
      },
    },
    {
      id: 'lastUsed',
      header: '调用状态',
      accessorFn: (row) =>
        row.lastUsedAt ? new Date(row.lastUsedAt).getTime() : 0,
      cell: ({ row }) => (
        <div className="text-xs whitespace-nowrap leading-tight">
          <div className="text-muted-foreground">
            {formatLastUsed(row.original.lastUsedAt)}
          </div>
          <div className="text-muted-foreground">
            {formatCreditShort(row.original.creditUsageTotal)}
          </div>
          <button
            type="button"
            className="text-emerald-600 hover:underline cursor-pointer"
            onClick={() => ctx.onShowUsage(row.original)}
            title="查看调用统计"
          >
            {formatUsdShort(row.original.totalValueUsd)}
          </button>
        </div>
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
                ? '关超额'
                : c.overageStatus === 'DISABLED'
                  ? '开超额'
                  : '开超额'}
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
  // 批量 RPM 输入：空 / 0 都视为「清除覆盖（沿用全局）」
  const [batchRpmValue, setBatchRpmValue] = useState('')
  // 批量 Region / API Region 输入：空字符串视为「清除（回退全局/默认）」
  const [batchRegionValue, setBatchRegionValue] = useState('')
  const [batchApiRegionValue, setBatchApiRegionValue] = useState('')
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
  const [overuseFilter, setOveruseFilter] = useState<'' | 'on' | 'off'>('')
  const [searchQuery, setSearchQuery] = useState('')
  // 上游账号侧 overageStatus 筛选：ENABLED / DISABLED / unknown
  // 桶简化为 ENABLED / DISABLED；DISABLED 包含 null/未知（按用户意愿）
  const [accountOverageFilter, setAccountOverageFilter] = useState<
    Set<'ENABLED' | 'DISABLED'>
  >(() => new Set())

  // 任何筛选条件变更都会清空批量选中，避免操作打到被过滤掉的行
  const clearSelectionOnFilterChange = () => setRowSelection({})

  const toggleStatusFilter = (v: StatusFilter) => {
    clearSelectionOnFilterChange()
    setStatusFilters((prev) => {
      const next = new Set(prev)
      if (next.has(v)) next.delete(v)
      else next.add(v)
      return next
    })
  }
  const toggleUsageLimitFilter = (v: string) => {
    clearSelectionOnFilterChange()
    setUsageLimitFilters((prev) => {
      const next = new Set(prev)
      if (next.has(v)) next.delete(v)
      else next.add(v)
      return next
    })
  }
  const togglePriorityFilter = (v: number) => {
    clearSelectionOnFilterChange()
    setPriorityFilters((prev) => {
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
  const [usageCred, setUsageCred] = useState<CredentialStatusItem | null>(null)
  const [usageDialogOpen, setUsageDialogOpen] = useState(false)
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

  // 允许超额计数
  const overuseCountMap = useMemo(() => {
    let on = 0
    let off = 0
    allCredentials.forEach((c) => (c.allowOveruse ? on++ : off++))
    return { on, off }
  }, [allCredentials])

  // 上游账号 overageStatus 计数（unknown 与 DISABLED 合并到"关"）
  const accountOverageCountMap = useMemo(() => {
    let enabled = 0
    let disabled = 0 // 含 unknown
    allCredentials.forEach((c) => {
      if (c.overageStatus === 'ENABLED') enabled++
      else disabled++
    })
    return { enabled, disabled }
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
      // 允许超额：on/off 单选
      if (overuseFilter === 'on' && !c.allowOveruse) return false
      if (overuseFilter === 'off' && c.allowOveruse) return false
      // 上游账号 overageStatus：选中的任一桶命中即通过；unknown 归为 "关(DISABLED)"
      if (accountOverageFilter.size > 0) {
        const bucket: 'ENABLED' | 'DISABLED' =
          c.overageStatus === 'ENABLED' ? 'ENABLED' : 'DISABLED'
        if (!accountOverageFilter.has(bucket)) return false
      }
      // 关键字模糊搜索：id / email / accountEmail / subscriptionTitle 任一子串命中
      const q = searchQuery.trim().toLowerCase()
      if (q) {
        const haystack = [
          String(c.id),
          c.email ?? '',
          c.accountEmail ?? '',
          c.subscriptionTitle ?? '',
        ]
          .join('\n')
          .toLowerCase()
        if (!haystack.includes(q)) return false
      }
      return true
    })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    allCredentials,
    statusFilters,
    usageLimitFilters,
    priorityFilters,
    overuseFilter,
    accountOverageFilter,
    searchQuery,
    cachedBalanceMap,
    liveBalances,
  ])

  // 批量操作的勾选必须限制在「当前页可见行」内：
  // 跨页选中虽可保留 rowSelection，但执行时只会作用到当前可见行，
  // 避免操作到看不见的行造成误删/误改。
  const selectedIds = useMemo(() => {
    const visibleIdSet = new Set(visibleRows.map((r) => String(r.id)))
    return Object.keys(rowSelection)
      .filter((k) => rowSelection[k] && visibleIdSet.has(k))
      .map(Number)
  }, [rowSelection, visibleRows])

  // 已选凭据的「调用状态」价值合计（totalValueUsd）与积分合计（creditUsageTotal）
  const selectedTotals = useMemo(() => {
    if (selectedIds.length === 0) return { valueUsd: 0, credit: 0 }
    const idSet = new Set(selectedIds)
    return allCredentials.reduce(
      (acc, c) => {
        if (idSet.has(c.id)) {
          acc.valueUsd += c.totalValueUsd ?? 0
          acc.credit += c.creditUsageTotal ?? 0
        }
        return acc
      },
      { valueUsd: 0, credit: 0 },
    )
  }, [selectedIds, allCredentials])
  // 1 积分约等于多少价值（USD）
  const usdPerCredit =
    selectedTotals.credit > 0 ? selectedTotals.valueUsd / selectedTotals.credit : 0

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
        onForceRefresh: handleForceRefresh,
        onBindProxy: (id) => setBindProxyId(id),
        onUnbindProxy: handleUnbindProxy,
        onTestProxy: handleTestProxy,
        onToggleOverage: handleToggleOverage,
        onToggleAllowOveruse: handleToggleAllowOveruse,
        onShowDetail: (c) => setDetailCred(c),
        onShowUsage: (c) => {
          setUsageCred(c)
          setUsageDialogOpen(true)
        },
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

  // 批量改 RPM 上限（rpm = null 表示清除覆盖，沿用全局）
  const handleBatchSetRpm = async (rpm: number | null) => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      setCredentialRpmApi(id, rpm)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    const label = rpm && rpm > 0 ? `RPM=${rpm}` : '清除 RPM 覆盖'
    if (fail === 0) toast.success(`已对 ${ok} 个凭据${label === '清除 RPM 覆盖' ? '' : '设置'}${label}`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  // 批量改 Region：region=null 表示清除（回退到全局 region），不会动 apiRegion
  const handleBatchSetRegion = async (region: string | null) => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      setCredentialRegionOnlyApi(id, region)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    const label = region ? `Region=${region}` : '清除 Region 覆盖'
    if (fail === 0) toast.success(`已对 ${ok} 个凭据${region ? '设置' : ''}${label}`)
    else toast.warning(`成功 ${ok}，失败 ${fail}`)
  }

  // 批量改 API Region：apiRegion=null 表示清除（回退到 region），不会动 region
  const handleBatchSetApiRegion = async (apiRegion: string | null) => {
    if (selectedIds.length === 0) return
    const { ok, fail } = await runBatchConcurrent(selectedIds, (id) =>
      setCredentialApiRegionOnlyApi(id, apiRegion)
    )
    queryClient.invalidateQueries({ queryKey: ['credentials'] })
    const label = apiRegion ? `API Region=${apiRegion}` : '清除 API Region 覆盖'
    if (fail === 0) toast.success(`已对 ${ok} 个凭据${apiRegion ? '设置' : ''}${label}`)
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
        case 'setRpm': {
          const trimmed = batchRpmValue.trim()
          const n = trimmed ? parseInt(trimmed, 10) : 0
          await handleBatchSetRpm(Number.isFinite(n) && n > 0 ? n : null)
          break
        }
        case 'setRegion':
          await handleBatchSetRegion(batchRegionValue.trim() || null)
          break
        case 'setApiRegion':
          await handleBatchSetApiRegion(batchApiRegionValue.trim() || null)
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
        case 'export':
          await handleBatchExport()
          break
        case 'delete':
          await handleBatchDelete()
          break
      }
    } finally {
      setBatchRunning(false)
    }
  }

  // 批量导出：服务端按勾选 ID 返回可重新导入的 JSON，前端直接触发下载
  const handleBatchExport = async () => {
    if (selectedIds.length === 0) return
    try {
      const resp = await exportCredentialsApi({ credentialIds: selectedIds })
      if (resp.items.length === 0) {
        const reason = resp.skipped[0]?.reason ?? '所选凭据都无法导出'
        toast.error(`导出失败：${reason}`)
        return
      }
      // 文件顶层用数组，与 import-token-json 接受的格式一致
      const blob = new Blob([JSON.stringify(resp.items, null, 2)], {
        type: 'application/json',
      })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      const ts = new Date()
        .toISOString()
        .replace(/[-:T]/g, '')
        .replace(/\..+$/, '')
      a.href = url
      a.download = `kiro-credentials-${ts}.json`
      document.body.appendChild(a)
      a.click()
      a.remove()
      URL.revokeObjectURL(url)

      if (resp.skipped.length > 0) {
        toast.warning(
          `已导出 ${resp.items.length} 个，跳过 ${resp.skipped.length} 个（${resp.skipped[0].reason}）`
        )
      } else {
        toast.success(`已导出 ${resp.items.length} 个凭据到 JSON 文件`)
      }
    } catch (e) {
      toast.error('导出失败：' + extractErrorMessage(e))
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
    <div className="flex flex-col flex-1 min-h-0">
      {/* 顶栏 */}
      <div className="flex items-center justify-between mb-3 gap-3 flex-wrap shrink-0">
        <div className="flex items-center gap-2 flex-wrap">
          {/* 统计框：总数 / 可用 / 成功率 / 成功 / 失败 */}
          {(() => {
            const totalReq = statsSummary?.totalRequests ?? 0
            const totalOk = statsSummary?.totalSuccess ?? 0
            const totalErr = statsSummary?.totalFail ?? 0
            const successRate = totalReq > 0 ? (totalOk / totalReq) * 100 : null
            const rateColor =
              successRate === null
                ? 'text-muted-foreground'
                : successRate >= 99
                  ? 'text-emerald-700 dark:text-emerald-400'
                  : successRate >= 95
                    ? 'text-sky-700 dark:text-sky-400'
                    : successRate >= 80
                      ? 'text-amber-700 dark:text-amber-400'
                      : 'text-rose-700 dark:text-rose-400'
            return (
              <div className="inline-flex items-center gap-4 rounded-md border bg-card px-4 py-2 text-sm shadow-sm">
                <span className="inline-flex items-center gap-1.5" title="凭据总数">
                  <Database className="h-3.5 w-3.5 text-muted-foreground" />
                  <span className="text-muted-foreground text-xs">总数</span>
                  <span className="font-mono font-semibold tabular-nums text-base leading-none">
                    {data?.total ?? 0}
                  </span>
                </span>
                <span className="h-5 w-px bg-border" />
                <span
                  className="inline-flex items-center gap-1.5 text-emerald-700 dark:text-emerald-400"
                  title="可用凭据数"
                >
                  <CheckCircle2 className="h-3.5 w-3.5" />
                  <span className="text-muted-foreground text-xs">可用</span>
                  <span className="font-mono font-semibold tabular-nums text-base leading-none">
                    {data?.available ?? 0}
                  </span>
                </span>
                <span className="h-5 w-px bg-border" />
                <span
                  className={`inline-flex items-center gap-1.5 ${rateColor}`}
                  title={`成功率（成功 ${totalOk} / 总 ${totalReq}）`}
                >
                  <Percent className="h-3.5 w-3.5" />
                  <span className="text-muted-foreground text-xs">成功率</span>
                  <span className="font-mono font-semibold tabular-nums text-base leading-none">
                    {successRate === null ? '—' : `${successRate.toFixed(1)}%`}
                  </span>
                </span>
                <span className="h-5 w-px bg-border" />
                <span
                  className="inline-flex items-center gap-1.5 text-emerald-700 dark:text-emerald-400"
                  title="累计成功请求"
                >
                  <TrendingUp className="h-3.5 w-3.5" />
                  <span className="text-muted-foreground text-xs">成功</span>
                  <span className="font-mono font-semibold tabular-nums text-base leading-none">
                    {totalOk.toLocaleString()}
                  </span>
                </span>
                <span className="h-5 w-px bg-border" />
                <span
                  className={
                    'inline-flex items-center gap-1.5 ' +
                    (totalErr > 0
                      ? 'text-rose-700 dark:text-rose-400'
                      : 'text-muted-foreground')
                  }
                  title="累计失败请求"
                >
                  <XCircle className="h-3.5 w-3.5" />
                  <span className="text-muted-foreground text-xs">失败</span>
                  <span className="font-mono font-semibold tabular-nums text-base leading-none">
                    {totalErr.toLocaleString()}
                  </span>
                </span>
              </div>
            )
          })()}
          {/* 内联实时指标 pill —— 紧凑、与 badge 同一行，避免多占一行 */}
          <span
            className="inline-flex items-center gap-1 rounded-md border bg-card px-2 py-0.5 text-xs"
            title="服务运行时长"
          >
            <Clock className="h-3 w-3 text-muted-foreground" />
            <span className="text-muted-foreground">运行</span>
            <span className="font-mono font-medium">
              {statsSummary ? formatUptime(statsSummary.uptimeSecs) : '—'}
            </span>
          </span>
          <span
            className="inline-flex items-center gap-1 rounded-md border bg-card px-2 py-0.5 text-xs"
            title="累计请求数"
          >
            <BarChart3 className="h-3 w-3 text-muted-foreground" />
            <span className="text-muted-foreground">请求</span>
            <span className="font-mono font-medium">
              {(statsSummary?.totalRequests ?? 0).toLocaleString()}
            </span>
          </span>
          <span
            className={
              'inline-flex items-center gap-1 rounded-md border bg-card px-2 py-0.5 text-xs ' +
              (totalInFlight > 0
                ? 'text-emerald-600 dark:text-emerald-400 border-emerald-500/40 bg-emerald-50/40 dark:bg-emerald-950/20'
                : '')
            }
            title="当前并发请求数"
          >
            <Activity
              className={
                'h-3 w-3 ' +
                (totalInFlight > 0
                  ? 'text-emerald-600 dark:text-emerald-400 animate-pulse'
                  : 'text-muted-foreground')
              }
            />
            <span className="text-muted-foreground">并发</span>
            <span className="font-mono font-medium">{totalInFlight}</span>
          </span>
          <span
            className={
              'inline-flex items-center gap-1 rounded-md border bg-card px-2 py-0.5 text-xs ' +
              (totalRpm > 0
                ? 'text-sky-700 dark:text-sky-400 border-sky-500/40'
                : '')
            }
            title="实时 RPM 总和"
          >
            {totalRpm > 0 ? (
              <Zap className="h-3 w-3 text-sky-600 dark:text-sky-400" />
            ) : (
              <Gauge className="h-3 w-3 text-muted-foreground" />
            )}
            <span className="text-muted-foreground">RPM</span>
            <span className="font-mono font-medium">{totalRpm}</span>
          </span>
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
      <div className="mb-3 flex flex-wrap items-center gap-3 text-xs shrink-0">
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


        {/* 允许超额（凭据级开关） */}
        <div className="flex items-center gap-1">
          <span
            className="text-muted-foreground mr-1"
            title="本服务是否在余额耗尽时主动禁用（凭据级开关）"
          >
            允许超额
          </span>
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
                  onClick={() => {
                    clearSelectionOnFilterChange()
                    setOveruseFilter(active ? '' : (opt.value as 'on' | 'off'))
                  }}
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

        {/* 账号超额（上游账号侧 overageStatus） */}
        <div className="flex items-center gap-1">
          <span
            className="text-muted-foreground mr-1"
            title="上游账号 overageStatus（Kiro 侧账号是否允许产生超额账单）"
          >
            账号超额
          </span>
          <div className="inline-flex rounded-md border overflow-hidden">
            {(
              [
                {
                  value: 'ENABLED' as const,
                  label: '开',
                  count: accountOverageCountMap.enabled,
                },
                {
                  value: 'DISABLED' as const,
                  label: '关',
                  count: accountOverageCountMap.disabled,
                },
              ]
            ).map((opt, i) => {
              const active = accountOverageFilter.has(opt.value)
              return (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => {
                    clearSelectionOnFilterChange()
                    setAccountOverageFilter((prev) => {
                      const next = new Set(prev)
                      if (next.has(opt.value)) next.delete(opt.value)
                      else next.add(opt.value)
                      return next
                    })
                  }}
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
          overuseFilter !== '' ||
          accountOverageFilter.size > 0 ||
          searchQuery.trim() !== '') && (
          <Button
            size="sm"
            variant="ghost"
            className="h-7 px-2 text-xs"
            onClick={() => {
              setStatusFilters(new Set())
              setUsageLimitFilters(new Set())
              setPriorityFilters(new Set())
              setOveruseFilter('')
              setAccountOverageFilter(new Set())
              setSearchQuery('')
            }}
          >
            清除筛选
          </Button>
        )}
      </div>

      {/* 快捷选择 + 选中工具栏 */}
      <div className="mb-3 flex flex-wrap items-center gap-3 rounded-md border bg-muted/30 px-3 py-2 shrink-0">
        {/* === 选择组：全选 / 反选 / 取消（连续按钮 + 范围勾选 + 已选计数） === */}
        <div className="inline-flex h-8 items-stretch rounded-md border bg-background overflow-hidden shadow-sm">
          <button
            type="button"
            className="inline-flex items-center gap-1 px-2.5 text-xs hover:bg-muted transition-colors"
            title="勾选当前页全部行"
            onClick={() => {
              const next: RowSelectionState = { ...rowSelection }
              visibleRows.forEach((c) => {
                next[String(c.id)] = true
              })
              setRowSelection(next)
            }}
          >
            <CheckSquare className="h-3.5 w-3.5" />
            全选本页
          </button>
          <span className="w-px bg-border" />
          <button
            type="button"
            className="inline-flex items-center gap-1 px-2.5 text-xs hover:bg-muted transition-colors"
            title="反转当前页选择状态"
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
            <SquareDot className="h-3.5 w-3.5" />
            反选
          </button>
          <span className="w-px bg-border" />
          <button
            type="button"
            className="inline-flex items-center gap-1 px-2.5 text-xs hover:bg-muted transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            title="清空所有勾选"
            onClick={() => setRowSelection({})}
            disabled={selectedIds.length === 0}
          >
            <Square className="h-3.5 w-3.5" />
            清空
          </button>
        </div>

        {/* 当前页范围勾选（独立小盒） */}
        <div className="inline-flex h-8 items-center gap-1.5 rounded-md border bg-background px-2 text-xs shadow-sm">
          <Target className="h-3.5 w-3.5 text-muted-foreground" />
          <span className="text-muted-foreground">本页第</span>
          <Input
            type="number"
            min={1}
            max={visibleRows.length || undefined}
            placeholder="起"
            value={rangeStart}
            onChange={(e) => setRangeStart(e.target.value)}
            className="h-6 w-12 px-1.5 text-xs border-0 bg-muted/40 focus-visible:ring-1"
          />
          <span className="text-muted-foreground">~</span>
          <Input
            type="number"
            min={1}
            max={visibleRows.length || undefined}
            placeholder="止"
            value={rangeEnd}
            onChange={(e) => setRangeEnd(e.target.value)}
            className="h-6 w-12 px-1.5 text-xs border-0 bg-muted/40 focus-visible:ring-1"
            onKeyDown={(e) => {
              if (e.key === 'Enter') applyIdRange()
            }}
          />
          <span className="text-muted-foreground">行</span>
          <button
            type="button"
            onClick={applyIdRange}
            className="ml-0.5 h-6 px-1.5 text-xs rounded hover:bg-muted text-primary font-medium"
          >
            勾选
          </button>
        </div>

        {/* 已选计数 */}
        {selectedIds.length > 0 ? (
          <Badge
            variant="secondary"
            className="h-7 gap-1 text-xs px-2.5 font-mono tabular-nums"
          >
            <CheckCircle2 className="h-3 w-3 text-emerald-600 dark:text-emerald-400" />
            已选 {selectedIds.length}
            <span className="text-muted-foreground">
              · {formatCreditShort(selectedTotals.credit)} 积分
            </span>
            <span className="text-emerald-600 dark:text-emerald-400">
              · {formatUsdShort(selectedTotals.valueUsd)}
            </span>
            {selectedTotals.credit > 0 && (
              <span className="text-muted-foreground">
                · 1积分≈${usdPerCredit.toFixed(6)}
              </span>
            )}
          </Badge>
        ) : (
          <span className="text-xs text-muted-foreground">未选中</span>
        )}

        <span className="flex-1" />

        {/* === 执行组：操作类型 + 参数 + 执行按钮（视觉聚拢，分隔感强） === */}
        <div className="inline-flex h-8 items-stretch rounded-md border bg-background overflow-hidden shadow-sm">
          <div className="inline-flex items-center pl-2 pr-1 text-xs text-muted-foreground gap-1 border-r bg-muted/30">
            <ListFilter className="h-3.5 w-3.5" />
            批量
          </div>
          <select
            value={batchAction}
            onChange={(e) => setBatchAction(e.target.value as BatchAction)}
            className="h-full bg-background px-2 text-xs border-0 focus:outline-none focus:ring-1 focus:ring-ring"
          >
            <option value="enable">启用</option>
            <option value="disable">禁用</option>
            <option value="setPriority">改优先级</option>
            <option value="setRpm">改 RPM 上限</option>
            <option value="setRegion">改 Region</option>
            <option value="setApiRegion">改 API Region</option>
            <option value="overageOn">账号超额开</option>
            <option value="overageOff">账号超额关</option>
            <option value="allowOveruseOn">允许超额:开</option>
            <option value="allowOveruseOff">允许超额:关</option>
            <option value="verify">验活</option>
            <option value="refreshToken">刷新</option>
            <option value="queryBalance">查余额</option>
            <option value="export">导出 JSON</option>
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
              className="h-full w-20 text-xs border-0 border-l rounded-none focus-visible:ring-1"
            />
          )}
          {batchAction === 'setRpm' && (
            <Input
              type="number"
              min={0}
              placeholder="留空=清除"
              value={batchRpmValue}
              onChange={(e) => setBatchRpmValue(e.target.value)}
              className="h-full w-24 text-xs border-0 border-l rounded-none focus-visible:ring-1"
              title="RPM 上限；留空 / 0 表示清除覆盖，沿用全局值"
            />
          )}
          {batchAction === 'setRegion' && (
            <Input
              type="text"
              placeholder="us-east-1，留空=清除"
              value={batchRegionValue}
              onChange={(e) => setBatchRegionValue(e.target.value)}
              className="h-full w-40 text-xs border-0 border-l rounded-none focus-visible:ring-1"
              title="凭据级 Region；留空表示清除覆盖，沿用全局 region。不影响 API Region。"
            />
          )}
          {batchAction === 'setApiRegion' && (
            <Input
              type="text"
              placeholder="us-west-2，留空=清除"
              value={batchApiRegionValue}
              onChange={(e) => setBatchApiRegionValue(e.target.value)}
              className="h-full w-40 text-xs border-0 border-l rounded-none focus-visible:ring-1"
              title="凭据级 API Region；留空表示清除覆盖，回退到 region。不影响 Region。"
            />
          )}
        </div>
        <Button
          size="sm"
          variant={batchAction === 'delete' ? 'destructive' : 'default'}
          className="h-8 px-3 text-xs gap-1 shadow-sm"
          onClick={() => runBatchAction()}
          disabled={batchRunning || selectedIds.length === 0}
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
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
              执行中
            </>
          ) : (
            <>
              <PlayCircle className="h-3.5 w-3.5" />
              执行
              {selectedIds.length > 0 && (
                <span className="ml-0.5 px-1 rounded bg-primary-foreground/20 text-[10px] tabular-nums font-mono">
                  {selectedIds.length}
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
        headerSlot={
          <Input
            type="search"
            placeholder="搜索 邮箱 / ID / 订阅"
            value={searchQuery}
            onChange={(e) => {
              clearSelectionOnFilterChange()
              setSearchQuery(e.target.value)
            }}
            className="ml-2 h-8 w-56 text-xs"
          />
        }
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

      {/* 调用统计对话框 */}
      <UsageStatsDialog
        credential={usageCred}
        open={usageDialogOpen}
        onOpenChange={setUsageDialogOpen}
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
    </div>
  )
}
