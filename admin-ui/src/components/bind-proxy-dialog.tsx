import { useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Badge } from '@/components/ui/badge'
import { bindCredentialProxy, getProxies } from '@/api/proxies'
import { extractErrorMessage } from '@/lib/utils'
import type { ProxyEntryItem } from '@/types/api'

interface BindProxyDialogProps {
  credentialId: number | null
  open: boolean
  onOpenChange: (open: boolean) => void
}

function formatRemaining(secs: number): string {
  if (secs <= 0) return '已过期'
  const days = Math.floor(secs / 86400)
  const hours = Math.floor((secs % 86400) / 3600)
  if (days > 0) return `${days}d ${hours}h`
  return `${hours}h`
}

export function BindProxyDialog({
  credentialId,
  open,
  onOpenChange,
}: BindProxyDialogProps) {
  const queryClient = useQueryClient()
  const [selected, setSelected] = useState<string | null>(null)
  const [autoEnable, setAutoEnable] = useState(true)

  const { data, isLoading } = useQuery({
    queryKey: ['proxies'],
    queryFn: getProxies,
    enabled: open,
  })

  // 仅显示有空闲槽位且未过期的候选
  const candidates: ProxyEntryItem[] = useMemo(() => {
    const list = data?.proxies ?? []
    return list
      .filter(
        (p) =>
          p.usedSlots < p.slots &&
          p.status !== 'expired' &&
          !p.boundCredentialIds.includes(credentialId ?? -1)
      )
      .sort((a, b) => {
        const slotsCmp = b.slots - b.usedSlots - (a.slots - a.usedSlots)
        if (slotsCmp !== 0) return slotsCmp
        return b.remainingSecs - a.remainingSecs
      })
  }, [data, credentialId])

  const mutation = useMutation({
    mutationFn: async () => {
      if (!credentialId || !selected)
        throw new Error('未选择代理或凭据 ID 缺失')
      return bindCredentialProxy(credentialId, {
        proxyId: selected,
        autoEnable,
      })
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['credentials'] })
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      toast.success('已绑定代理槽')
      onOpenChange(false)
      setSelected(null)
    },
    onError: (e) => {
      toast.error(`绑定失败：${extractErrorMessage(e)}`)
    },
  })

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>
            为凭据 #{credentialId ?? '?'} 绑定代理
          </DialogTitle>
          <DialogDescription>
            从代理池中选择一个空闲槽位绑定。建议选择剩余有效期较长的代理。
          </DialogDescription>
        </DialogHeader>

        <div className="max-h-[50vh] overflow-y-auto rounded border">
          {isLoading ? (
            <div className="py-8 text-center text-sm text-muted-foreground">
              加载代理列表中…
            </div>
          ) : candidates.length === 0 ? (
            <div className="py-8 text-center text-sm text-muted-foreground">
              暂无可用代理（无空闲槽位 / 全部到期），请先到代理池页面导入新代理
            </div>
          ) : (
            <ul className="divide-y">
              {candidates.map((p) => {
                const active = selected === p.id
                const expiring = p.remainingSecs < 86400
                return (
                  <li
                    key={p.id}
                    className={`flex items-center justify-between px-3 py-2 cursor-pointer hover:bg-muted/50 ${active ? 'bg-muted' : ''}`}
                    onClick={() => setSelected(p.id)}
                  >
                    <div className="flex items-center gap-2 text-sm">
                      <input
                        type="radio"
                        readOnly
                        checked={active}
                      />
                      <span className="font-mono text-xs">{p.id}</span>
                      <span className="font-mono text-xs text-muted-foreground">
                        {p.url}
                      </span>
                    </div>
                    <div className="flex items-center gap-2">
                      <span className="font-mono text-xs">
                        槽位 {p.usedSlots}/{p.slots}
                      </span>
                      <Badge
                        className={
                          expiring
                            ? 'bg-yellow-500/15 text-yellow-700 dark:text-yellow-400'
                            : 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400'
                        }
                      >
                        剩余 {formatRemaining(p.remainingSecs)}
                      </Badge>
                    </div>
                  </li>
                )
              })}
            </ul>
          )}
        </div>

        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={autoEnable}
            onChange={(e) => setAutoEnable(e.target.checked)}
          />
          绑定后自动启用凭据
        </label>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            取消
          </Button>
          <Button
            onClick={() => mutation.mutate()}
            disabled={!selected || mutation.isPending}
          >
            {mutation.isPending ? '绑定中…' : '绑定'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
