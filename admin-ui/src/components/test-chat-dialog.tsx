import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
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
import { listCredentialModels } from '@/api/model-coverage'
import { useCredentials } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

// 拉取失败时的兜底列表
const FALLBACK_MODELS = [
  { value: 'claude-opus-4-7', label: 'Opus 4.7' },
  { value: 'claude-opus-4-6', label: 'Opus 4.6' },
  { value: 'claude-sonnet-4-6', label: 'Sonnet 4.6' },
  { value: 'claude-opus-4-5-20251101', label: 'Opus 4.5' },
  { value: 'claude-sonnet-4-5-20250929', label: 'Sonnet 4.5' },
]

// Kiro ListAvailableModels 响应里没有固定的「积分倍率」字段；
// 若上游某天确实下发了倍率（任一常见字段名），这里自动识别并标注。
const RATE_FIELDS = [
  'creditMultiplier',
  'multiplier',
  'creditsPerRequest',
  'creditRate',
  'tokenMultiplier',
  'unitCredits',
  'credits',
  'credit',
  'rate',
  'weight',
  'cost',
]
function extractRate(m: Record<string, unknown>): string | null {
  for (const k of RATE_FIELDS) {
    const v = m[k]
    if (typeof v === 'number' && Number.isFinite(v)) return String(v)
    if (typeof v === 'string' && v.trim() !== '' && !Number.isNaN(Number(v)))
      return v
  }
  return null
}

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
  const [customMode, setCustomMode] = useState(false)
  const [message, setMessage] = useState('hi')

  // 模型列表：从上游 Kiro ListAvailableModels 获取（复用「模型检测」接口）。
  // 指定了凭据就用该凭据，否则挑一个可用凭据去拉。
  const { data: credsResp } = useCredentials()
  const fetchCredId =
    credentialId ??
    credsResp?.credentials.find((c) => !c.disabled)?.id ??
    null
  const { data: modelsResp, isLoading: modelsLoading } = useQuery({
    queryKey: ['test-chat-models', fetchCredId],
    queryFn: () => listCredentialModels(fetchCredId as number),
    enabled: open && fetchCredId != null,
    staleTime: 5 * 60 * 1000,
  })
  const upstreamModels =
    modelsResp?.ok && Array.isArray(modelsResp.data?.models)
      ? modelsResp.data!.models!.filter((m) => m.modelId)
      : []

  // 当前选中模型的倍率（上游若下发倍率字段则展示）
  const selectedUpstream = upstreamModels.find((m) => m.modelId === model)
  const selectedRate = selectedUpstream
    ? extractRate(selectedUpstream as Record<string, unknown>)
    : null

  // 用户手动改过模型后，不再自动覆盖（支持自己输入任意模型）
  const userTouchedRef = useRef(false)
  useEffect(() => {
    if (open) {
      userTouchedRef.current = false
      setCustomMode(false)
    }
  }, [open])

  // 上游列表加载成功后，把默认模型切到上游 defaultModel / 第一个（未手输时）
  useEffect(() => {
    if (!open || userTouchedRef.current || upstreamModels.length === 0) return
    const ids = upstreamModels.map((m) => m.modelId as string)
    if (ids.includes(model)) return
    const dft = modelsResp?.data?.defaultModel?.modelId
    setModel(dft && ids.includes(dft) ? dft : ids[0])
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, modelsResp])

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
              <label className="text-xs text-muted-foreground">
                模型
                {modelsLoading && (
                  <span className="ml-1 text-[11px]">（拉取中…）</span>
                )}
                {upstreamModels.length > 0 && (
                  <span className="ml-1 text-[11px] text-emerald-600">
                    · 上游 {upstreamModels.length} 个
                  </span>
                )}
              </label>
              <select
                value={customMode ? '__custom__' : model}
                onChange={(e) => {
                  const v = e.target.value
                  userTouchedRef.current = true
                  if (v === '__custom__') {
                    setCustomMode(true)
                  } else {
                    setCustomMode(false)
                    setModel(v)
                  }
                }}
                className="flex h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
                disabled={mut.isPending}
              >
                {upstreamModels.length > 0
                  ? upstreamModels.map((m) => {
                      const id = m.modelId as string
                      const rate = extractRate(m as Record<string, unknown>)
                      return (
                        <option key={id} value={id}>
                          {(m.modelName || id) +
                            '（' +
                            id +
                            '）' +
                            (rate ? ` · 倍率 ${rate}×` : '')}
                        </option>
                      )
                    })
                  : FALLBACK_MODELS.map((m) => (
                      <option key={m.value} value={m.value}>
                        {m.label}（{m.value}）
                      </option>
                    ))}
                <option value="__custom__">✏️ 自定义输入…</option>
              </select>
              {customMode && (
                <Input
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                  placeholder="输入模型 id，如 claude-sonnet-4.6"
                  disabled={mut.isPending}
                  className="mt-1"
                />
              )}
              <p className="text-[11px] text-muted-foreground">
                {modelsLoading
                  ? '上游模型拉取中…'
                  : upstreamModels.length > 0
                    ? `上游共 ${upstreamModels.length} 个模型`
                    : '上游模型拉取失败，使用内置列表（可选「自定义输入」）'}
                {credentialId != null ? ` · 仅用凭据 #${credentialId}` : ''}
              </p>
              <p className="text-[11px]">
                <span className="text-muted-foreground">积分倍率：</span>
                {customMode ? (
                  <span className="text-muted-foreground">自定义模型，未知</span>
                ) : selectedRate ? (
                  <span className="text-emerald-600 font-medium">
                    {selectedRate}×
                  </span>
                ) : (
                  <span className="text-muted-foreground">未知（上游未提供）</span>
                )}
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
