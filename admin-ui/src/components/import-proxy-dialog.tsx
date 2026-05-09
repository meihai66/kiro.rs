import { useMemo, useState } from 'react'
import { useMutation, useQueryClient } from '@tanstack/react-query'
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
import { Input } from '@/components/ui/input'
import { importProxies } from '@/api/proxies'
import { extractErrorMessage } from '@/lib/utils'

interface ImportProxyDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

const SCHEME_OPTIONS = [
  { value: 'http', label: 'HTTP' },
  { value: 'https', label: 'HTTPS' },
  { value: 'socks5', label: 'SOCKS5' },
] as const

function defaultExpires(): string {
  // 默认 30 天后到期，YYYY-MM-DDTHH:mm 格式（datetime-local input）
  const d = new Date(Date.now() + 30 * 24 * 3600 * 1000)
  d.setSeconds(0, 0)
  const pad = (n: number) => n.toString().padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`
}

export function ImportProxyDialog({ open, onOpenChange }: ImportProxyDialogProps) {
  const queryClient = useQueryClient()
  const [scheme, setScheme] = useState<'http' | 'https' | 'socks5'>('http')
  const [slots, setSlots] = useState(1)
  const [expiresAt, setExpiresAt] = useState(defaultExpires())
  const [label, setLabel] = useState('')
  const [text, setText] = useState('')

  const lines = useMemo(
    () =>
      text
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter((l) => l.length > 0),
    [text]
  )

  const preview = useMemo(() => {
    return lines.slice(0, 5).map((line) => {
      const parts = line.split(':')
      if (parts.length < 4) return { line, ok: false, info: '格式错误' }
      const host = parts[0]
      const port = parts[1]
      return {
        line,
        ok: true,
        info: `${scheme}://${host}:${port}`,
      }
    })
  }, [lines, scheme])

  const mutation = useMutation({
    mutationFn: async () => {
      // 转 expiresAt 为 ISO，加上 :00.000Z 由本地时区转 UTC
      const isoExpires = new Date(expiresAt).toISOString()
      return importProxies({
        scheme,
        slotsPerProxy: slots,
        defaultExpiresAt: isoExpires,
        lines,
        label: label.trim() || undefined,
      })
    },
    onSuccess: (resp) => {
      queryClient.invalidateQueries({ queryKey: ['proxies'] })
      if (resp.failed === 0) {
        toast.success(`成功导入 ${resp.added} 个代理`)
      } else {
        toast.warning(`导入完成：成功 ${resp.added}，失败 ${resp.failed}`)
        // eslint-disable-next-line no-console
        console.warn('导入失败明细', resp.items.filter((i) => !i.success))
      }
      onOpenChange(false)
      setText('')
    },
    onError: (e) => {
      toast.error(`导入失败：${extractErrorMessage(e)}`)
    },
  })

  const canSubmit = lines.length > 0 && slots >= 1 && expiresAt.length > 0

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>批量导入代理</DialogTitle>
          <DialogDescription>
            每行一条 <code>host:port:user:pass</code>（user/pass 可留空作 host:port::）
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 max-h-[60vh] overflow-y-auto">
          <div className="grid grid-cols-3 gap-4">
            <div>
              <label className="text-sm font-medium">协议</label>
              <select
                className="mt-1 h-9 w-full rounded-md border bg-background px-3 text-sm"
                value={scheme}
                onChange={(e) =>
                  setScheme(e.target.value as 'http' | 'https' | 'socks5')
                }
                disabled={mutation.isPending}
              >
                {SCHEME_OPTIONS.map((s) => (
                  <option key={s.value} value={s.value}>
                    {s.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="text-sm font-medium">每代理槽位</label>
              <Input
                type="number"
                min={1}
                value={slots}
                onChange={(e) =>
                  setSlots(Math.max(1, Number(e.target.value) || 1))
                }
                disabled={mutation.isPending}
              />
            </div>
            <div>
              <label className="text-sm font-medium">统一到期时间</label>
              <Input
                type="datetime-local"
                value={expiresAt}
                onChange={(e) => setExpiresAt(e.target.value)}
                disabled={mutation.isPending}
              />
            </div>
          </div>

          <div>
            <label className="text-sm font-medium">标签（可选）</label>
            <Input
              placeholder="如 ipr-batch-2026-05"
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              disabled={mutation.isPending}
            />
          </div>

          <div>
            <label className="text-sm font-medium">
              代理列表（每行 host:port:user:pass）
            </label>
            <textarea
              className="mt-1 w-full h-48 rounded-md border bg-background p-2 text-sm font-mono"
              placeholder={
                '168.143.72.123:35123:JFkinzgf:LdSmlpgbv3\n149.119.186.233:48501:CixjQyKF:w2Kc9rmIde'
              }
              value={text}
              onChange={(e) => setText(e.target.value)}
              disabled={mutation.isPending}
            />
            <div className="mt-1 text-xs text-muted-foreground">
              已识别 {lines.length} 行
            </div>
          </div>

          {preview.length > 0 && (
            <div className="rounded border bg-muted/30 p-2">
              <div className="text-xs text-muted-foreground mb-1">
                预览（前 {preview.length} 行）
              </div>
              <ul className="space-y-1 text-xs font-mono">
                {preview.map((p, i) => (
                  <li
                    key={i}
                    className={p.ok ? 'text-foreground' : 'text-destructive'}
                  >
                    {p.line} → {p.info}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => onOpenChange(false)}
            disabled={mutation.isPending}
          >
            取消
          </Button>
          <Button
            onClick={() => mutation.mutate()}
            disabled={!canSubmit || mutation.isPending}
          >
            {mutation.isPending ? '导入中…' : `导入 ${lines.length} 条`}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
