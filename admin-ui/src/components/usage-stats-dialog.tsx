import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import type { CredentialStatusItem } from '@/types/api'

interface UsageStatsDialogProps {
  credential: CredentialStatusItem | null
  open: boolean
  onOpenChange: (open: boolean) => void
}

function fmtUsd(v: number): string {
  const n = v ?? 0
  return `$${n >= 1 ? n.toFixed(2) : n.toFixed(4)}`
}

function fmtCredit(v: number): string {
  const n = v ?? 0
  if (n === 0) return '0'
  return n.toLocaleString('zh-CN', { maximumFractionDigits: 2 })
}

function fmtTokens(v: number): string {
  const n = v ?? 0
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`
  return String(n)
}

export function UsageStatsDialog({
  credential,
  open,
  onOpenChange,
}: UsageStatsDialogProps) {
  const stats = [...(credential?.modelStats ?? [])].sort(
    (a, b) => (b.costUsd ?? 0) - (a.costUsd ?? 0)
  )
  const totalCalls = stats.reduce((s, m) => s + (m.calls ?? 0), 0)

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>凭据 #{credential?.id} 调用统计</DialogTitle>
        </DialogHeader>

        {stats.length === 0 ? (
          <div className="py-8 text-center text-sm text-muted-foreground">
            暂无调用记录
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-xs text-muted-foreground border-b">
                  <th className="text-left py-1 pr-2">模型</th>
                  <th className="text-right px-2">调用</th>
                  <th className="text-right px-2">输入</th>
                  <th className="text-right px-2">输出</th>
                  <th className="text-right px-2">缓存读</th>
                  <th className="text-right px-2">缓存写</th>
                  <th className="text-right px-2">积分</th>
                  <th className="text-right pl-2">价值</th>
                </tr>
              </thead>
              <tbody>
                {stats.map((m) => (
                  <tr key={m.model} className="border-b last:border-0">
                    <td className="py-1 pr-2 font-mono text-xs">{m.model}</td>
                    <td className="text-right px-2 tabular-nums">{m.calls}</td>
                    <td className="text-right px-2 tabular-nums">
                      {fmtTokens(m.inputTokens)}
                    </td>
                    <td className="text-right px-2 tabular-nums">
                      {fmtTokens(m.outputTokens)}
                    </td>
                    <td className="text-right px-2 tabular-nums">
                      {fmtTokens(m.cacheReadTokens)}
                    </td>
                    <td className="text-right px-2 tabular-nums">
                      {fmtTokens(m.cacheWriteTokens)}
                    </td>
                    <td className="text-right px-2 tabular-nums">
                      {fmtCredit(m.creditUsage)}
                    </td>
                    <td className="text-right pl-2 tabular-nums text-emerald-600">
                      {fmtUsd(m.costUsd)}
                    </td>
                  </tr>
                ))}
              </tbody>
              <tfoot>
                <tr className="border-t font-medium">
                  <td className="py-1 pr-2">合计</td>
                  <td className="text-right px-2 tabular-nums">{totalCalls}</td>
                  <td colSpan={4} />
                  <td className="text-right px-2 tabular-nums">
                    {fmtCredit(credential?.creditUsageTotal ?? 0)}
                  </td>
                  <td className="text-right pl-2 tabular-nums text-emerald-600">
                    {fmtUsd(credential?.totalValueUsd ?? 0)}
                  </td>
                </tr>
              </tfoot>
            </table>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
