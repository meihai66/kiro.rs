import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { Loader2, Send } from 'lucide-react'
import { toast } from 'sonner'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { testChat } from '@/api/credentials'
import { extractErrorMessage } from '@/lib/utils'

const MODEL_OPTIONS = [
  { value: 'claude-opus-4-7', label: 'Opus 4.7' },
  { value: 'claude-opus-4-7-thinking', label: 'Opus 4.7 (thinking)' },
  { value: 'claude-opus-4-6', label: 'Opus 4.6' },
  { value: 'claude-opus-4-6-thinking', label: 'Opus 4.6 (thinking)' },
  { value: 'claude-sonnet-4-6', label: 'Sonnet 4.6' },
  { value: 'claude-sonnet-4-6-thinking', label: 'Sonnet 4.6 (thinking)' },
  { value: 'claude-opus-4-5-20251101', label: 'Opus 4.5' },
  { value: 'claude-sonnet-4-5-20250929', label: 'Sonnet 4.5' },
] as const

interface TestChatDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** 限定使用某个凭据（来自凭据详情时）；不传则走默认调度 */
  credentialId?: number
}

export function TestChatDialog({
  open,
  onOpenChange,
  credentialId,
}: TestChatDialogProps) {
  const [model, setModel] = useState<string>('claude-opus-4-7')
  const [message, setMessage] = useState('hi')

  const mut = useMutation({
    mutationFn: () => testChat({ model, message, credentialId }),
    onError: (e) => toast.error('测试失败: ' + extractErrorMessage(e)),
  })

  const handleSend = () => {
    if (!model.trim()) {
      toast.error('请选择模型')
      return
    }
    mut.mutate()
  }

  const handleReset = () => {
    setMessage('hi')
    mut.reset()
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl max-h-[90vh] flex flex-col gap-0 p-0">
        <DialogHeader className="px-6 pt-6 pb-3 border-b shrink-0">
          <DialogTitle className="pr-8">
            对话测试
            {credentialId != null && (
              <span className="ml-2 text-xs font-normal text-muted-foreground">
                · 凭据 #{credentialId}（强制使用）
              </span>
            )}
          </DialogTitle>
        </DialogHeader>
        <div className="flex-1 overflow-y-auto p-6 space-y-4">
          <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground">模型</label>
              <select
                value={model}
                onChange={(e) => setModel(e.target.value)}
                className="flex h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
                disabled={mut.isPending}
              >
                {MODEL_OPTIONS.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label}（{m.value}）
                  </option>
                ))}
              </select>
              <p className="text-[11px] text-muted-foreground">
                {credentialId != null
                  ? `仅用凭据 #${credentialId}，不走故障转移`
                  : '使用任意可用凭据走默认调度策略'}
              </p>
            </div>
            <div className="space-y-1">
              <label className="text-xs text-muted-foreground">输入</label>
              <Input
                value={message}
                onChange={(e) => setMessage(e.target.value)}
                placeholder="hi"
                disabled={mut.isPending}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && !e.shiftKey && !mut.isPending) {
                    e.preventDefault()
                    handleSend()
                  }
                }}
              />
              <p className="text-[11px] text-muted-foreground">
                留空默认 "hi"。回车快速发送
              </p>
            </div>
          </div>

          {/* 结果 */}
          {mut.isPending && (
            <div className="rounded-md border bg-muted/30 p-4 flex items-center gap-2 text-sm text-muted-foreground">
              <Loader2 className="h-4 w-4 animate-spin" />
              请求中…（含上游往返，可能 1-30s）
            </div>
          )}
          {mut.isSuccess && mut.data && (
            <div className="space-y-2">
              <div className="flex flex-wrap items-center gap-2 text-xs">
                <span className="rounded bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 px-2 py-0.5 font-mono">
                  凭据 #{mut.data.credentialId}
                </span>
                <span className="rounded bg-muted px-2 py-0.5 font-mono">
                  {mut.data.elapsedMs}ms
                </span>
                <span className="rounded bg-muted px-2 py-0.5 font-mono">
                  in {mut.data.inputTokens} · out {mut.data.outputTokens}
                </span>
              </div>
              <div>
                <div className="text-xs text-muted-foreground mb-1">
                  模型回复
                </div>
                <pre className="rounded-md border bg-muted/40 p-3 text-sm whitespace-pre-wrap break-all max-h-[400px] overflow-y-auto">
                  {mut.data.text || (
                    <span className="text-muted-foreground">（空响应）</span>
                  )}
                </pre>
              </div>
            </div>
          )}
        </div>
        <div className="px-6 py-3 border-t flex items-center justify-between bg-background/95">
          <Button
            variant="ghost"
            size="sm"
            onClick={handleReset}
            disabled={mut.isPending}
          >
            重置
          </Button>
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => onOpenChange(false)}
            >
              关闭
            </Button>
            <Button size="sm" onClick={handleSend} disabled={mut.isPending}>
              {mut.isPending ? (
                <Loader2 className="h-3.5 w-3.5 mr-1 animate-spin" />
              ) : (
                <Send className="h-3.5 w-3.5 mr-1" />
              )}
              发送
            </Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
