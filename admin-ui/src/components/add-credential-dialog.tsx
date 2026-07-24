import { useState } from 'react'
import { toast } from 'sonner'
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useAddCredential, useImportTokenJson } from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'

interface AddCredentialDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

type AuthMethod = 'social' | 'idc' | 'api_key'
type ProxyScheme = 'http' | 'https' | 'socks5'

interface ParsedApiKeyLine {
  key: string
  /** 内嵌代理 URL（可含 user:pass@，已百分号编码）；导入时自动入池并绑定 */
  proxyUrl?: string
  /** 代理来自 host:port[:user:pass] 列表格式（受协议选择器影响） */
  listProxy?: boolean
}

// 解析一行 API Key：`ksk_xxx`，或 `|` 拼接代理：
// `ksk_xxx|host:port:user:pass` / `ksk_xxx|host:port` / `ksk_xxx|scheme://user:pass@host:port`
function parseApiKeyLine(
  line: string,
  scheme: ProxyScheme
): { entry?: ParsedApiKeyLine; error?: string } {
  const sep = line.indexOf('|')
  const key = (sep === -1 ? line : line.slice(0, sep)).trim()
  const proxyRaw = sep === -1 ? '' : line.slice(sep + 1).trim()
  if (!key) return { error: '缺少 API Key' }
  if (!proxyRaw) return { entry: { key } }
  if (proxyRaw.includes('://')) return { entry: { key, proxyUrl: proxyRaw } }
  const segs = proxyRaw.split(':')
  const [host, port] = segs
  const portNum = Number(port)
  const badShape =
    !host || !Number.isInteger(portNum) || portNum <= 0 || portNum > 65535 ||
    (segs.length !== 2 && segs.length < 4)
  if (badShape) {
    return { error: `代理格式错误（期望 host:port:user:pass 或 host:port）: ${proxyRaw}` }
  }
  // user/pass 可能含特殊字符（pass 还可能含 ':'），编码进 URL，后端会解码
  const user = segs[2] ?? ''
  const pass = segs.slice(3).join(':')
  const auth = user || pass ? `${encodeURIComponent(user)}:${encodeURIComponent(pass)}@` : ''
  return { entry: { key, proxyUrl: `${scheme}://${auth}${host}:${portNum}`, listProxy: true } }
}

export function AddCredentialDialog({ open, onOpenChange }: AddCredentialDialogProps) {
  const [refreshToken, setRefreshToken] = useState('')
  const [kiroApiKey, setKiroApiKey] = useState('')
  const [authMethod, setAuthMethod] = useState<AuthMethod>('social')
  const [region, setRegion] = useState('')
  const [apiRegion, setApiRegion] = useState('')
  const [clientId, setClientId] = useState('')
  const [clientSecret, setClientSecret] = useState('')
  const [priority, setPriority] = useState('0')
  const [machineId, setMachineId] = useState('')
  const [endpoint, setEndpoint] = useState('')
  const [autoBindProxy, setAutoBindProxy] = useState(true)
  const [proxyScheme, setProxyScheme] = useState<ProxyScheme>('http')

  const { mutate, isPending: isAddPending } = useAddCredential()
  const { mutate: importMutate, isPending: isImportPending } = useImportTokenJson()
  const isPending = isAddPending || isImportPending
  const isApiKey = authMethod === 'api_key'
  // API Key 模式支持批量：每行一个 ksk_ Key，可 `|` 拼接代理（按 Key 去重，保留首次出现）
  const parsedLines = kiroApiKey
    .split('\n')
    .map((s) => s.trim())
    .filter(Boolean)
    .map((line) => ({ line, ...parseApiKeyLine(line, proxyScheme) }))
  const lineErrors = parsedLines.filter((p) => p.error)
  const apiKeyEntries: ParsedApiKeyLine[] = []
  {
    const seenKeys = new Set<string>()
    for (const p of parsedLines) {
      if (p.entry && !seenKeys.has(p.entry.key)) {
        seenKeys.add(p.entry.key)
        apiKeyEntries.push(p.entry)
      }
    }
  }
  const proxyCount = apiKeyEntries.filter((e) => e.proxyUrl).length
  const hasListProxy = apiKeyEntries.some((e) => e.listProxy)
  const isBatch = isApiKey && apiKeyEntries.length > 1
  // 批量或带内嵌代理时走 import-token-json 管线（add-credential API 不支持内嵌代理）
  const useImportPipeline = isApiKey && (isBatch || proxyCount > 0)

  const resetForm = () => {
    setRefreshToken('')
    setKiroApiKey('')
    setAuthMethod('social')
    setRegion('')
    setApiRegion('')
    setClientId('')
    setClientSecret('')
    setPriority('10')
    setMachineId('')
    setEndpoint('')
    setAutoBindProxy(true)
    setProxyScheme('http')
  }

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    if (isApiKey) {
      if (lineErrors.length > 0) {
        const detail = lineErrors
          .slice(0, 3)
          .map((p) => p.error)
          .join('\n')
        toast.error(`${lineErrors.length} 行格式错误\n${detail}`, { duration: 8000 })
        return
      }
      if (apiKeyEntries.length === 0) {
        toast.error('请输入 Kiro API Key')
        return
      }
      if (useImportPipeline) {
        // 批量/带代理走 import-token-json 管线（逐条验证 + 去重 + 内嵌代理入池强绑 / 自动绑代理）
        importMutate(
          {
            dryRun: false,
            items: apiKeyEntries.map((entry) => ({
              kiroApiKey: entry.key,
              authMethod: 'api_key',
              priority: Number.isFinite(parseInt(priority)) ? parseInt(priority) : 10,
              region: region.trim() || undefined,
              apiRegion: apiRegion.trim() || undefined,
              endpoint: endpoint.trim() || undefined,
              machineId: apiKeyEntries.length === 1 ? machineId.trim() || undefined : undefined,
              proxy: entry.proxyUrl ? { url: entry.proxyUrl } : undefined,
            })),
          },
          {
            onSuccess: (data) => {
              const { added, skipped, invalid } = data.summary
              if (added > 0) {
                toast.success(`导入完成：新增 ${added} 条${skipped > 0 ? `，跳过 ${skipped} 条（已存在）` : ''}`)
              } else {
                toast.warning(`没有新增凭据：跳过 ${skipped} 条，失败 ${invalid} 条`)
              }
              if (invalid > 0) {
                const reasons = data.items
                  .filter((it) => it.action === 'invalid')
                  .slice(0, 3)
                  .map((it) => `${it.fingerprint}: ${it.reason ?? '未知原因'}`)
                toast.error(`${invalid} 条导入失败\n${reasons.join('\n')}`, { duration: 8000 })
              }
              if (added > 0) {
                onOpenChange(false)
                resetForm()
              }
            },
            onError: (error: unknown) => {
              toast.error(`批量导入失败: ${extractErrorMessage(error)}`)
            },
          }
        )
        return
      }
    } else {
      if (!refreshToken.trim()) {
        toast.error('请输入 Refresh Token')
        return
      }

      if (authMethod === 'idc' && (!clientId.trim() || !clientSecret.trim())) {
        toast.error('IdC/Builder-ID/IAM 认证需要填写 Client ID 和 Client Secret')
        return
      }
    }

    mutate(
      {
        refreshToken: isApiKey ? undefined : refreshToken.trim(),
        kiroApiKey: isApiKey ? apiKeyEntries[0]?.key : undefined,
        authMethod,
        region: region.trim() || undefined,
        apiRegion: apiRegion.trim() || undefined,
        clientId: isApiKey ? undefined : clientId.trim() || undefined,
        clientSecret: isApiKey ? undefined : clientSecret.trim() || undefined,
        priority: Number.isFinite(parseInt(priority)) ? parseInt(priority) : 10,
        machineId: machineId.trim() || undefined,
        endpoint: endpoint.trim() || undefined,
        autoBindProxy,
      },
      {
        onSuccess: (data) => {
          toast.success(data.message)
          onOpenChange(false)
          resetForm()
        },
        onError: (error: unknown) => {
          toast.error(`添加失败: ${extractErrorMessage(error)}`)
        },
      }
    )
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg max-h-[85vh] flex flex-col">
        <DialogHeader>
          <DialogTitle>添加凭据</DialogTitle>
        </DialogHeader>

        <form onSubmit={handleSubmit} className="flex flex-col min-h-0 flex-1">
          <div className="space-y-4 py-4 overflow-y-auto flex-1 pr-1">
            {/* 凭据密钥 */}
            {isApiKey ? (
              <div className="space-y-2">
                <label htmlFor="kiroApiKey" className="text-sm font-medium">
                  Kiro API Key <span className="text-red-500">*</span>
                </label>
                <textarea
                  id="kiroApiKey"
                  rows={4}
                  placeholder={'格式: ksk_xxxxxxxx\n带代理: ksk_xxxx|host:port:user:pass\n每行一个 Key，多行即批量导入'}
                  value={kiroApiKey}
                  onChange={(e) => setKiroApiKey(e.target.value)}
                  disabled={isPending}
                  className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm font-mono ring-offset-background placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
                />
                <p className="text-xs text-muted-foreground">
                  {apiKeyEntries.length > 0
                    ? `共 ${apiKeyEntries.length} 个 Key` +
                      (proxyCount > 0 ? `，${proxyCount} 个带内嵌代理（自动入池并绑定）` : '') +
                      (isBatch ? '；逐条验证并去重导入，Machine ID 自动派生' : '')
                    : '每行一个 ksk_ Key；可用 | 拼接代理（host:port:user:pass / host:port / 完整 URL），导入时自动加入代理池并绑定'}
                </p>
                {lineErrors.length > 0 && (
                  <p className="text-xs text-red-500">
                    {lineErrors.length} 行格式错误：{lineErrors[0].error}
                  </p>
                )}
                {hasListProxy && (
                  <div className="flex items-center gap-2">
                    <label htmlFor="proxyScheme" className="text-xs text-muted-foreground shrink-0">
                      代理协议
                    </label>
                    <select
                      id="proxyScheme"
                      value={proxyScheme}
                      onChange={(e) => setProxyScheme(e.target.value as ProxyScheme)}
                      disabled={isPending}
                      className="h-8 rounded-md border border-input bg-background px-2 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
                    >
                      <option value="http">HTTP</option>
                      <option value="https">HTTPS</option>
                      <option value="socks5">SOCKS5</option>
                    </select>
                    <span className="text-xs text-muted-foreground">
                      应用于 host:port 列表格式的代理行
                    </span>
                  </div>
                )}
              </div>
            ) : (
              <div className="space-y-2">
                <label htmlFor="refreshToken" className="text-sm font-medium">
                  Refresh Token <span className="text-red-500">*</span>
                </label>
                <Input
                  id="refreshToken"
                  type="password"
                  placeholder="请输入 Refresh Token"
                  value={refreshToken}
                  onChange={(e) => setRefreshToken(e.target.value)}
                  disabled={isPending}
                />
              </div>
            )}

            {/* 认证方式 */}
            <div className="space-y-2">
              <label htmlFor="authMethod" className="text-sm font-medium">
                认证方式
              </label>
              <select
                id="authMethod"
                value={authMethod}
                onChange={(e) => setAuthMethod(e.target.value as AuthMethod)}
                disabled={isPending}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="social">Social</option>
                <option value="idc">IdC/Builder-ID/IAM</option>
                <option value="api_key">API Key</option>
              </select>
            </div>

            {/* Region 配置 */}
            <div className="space-y-2">
              <label className="text-sm font-medium">Region 配置</label>
              <div className="grid grid-cols-2 gap-2">
                <div>
                  <Input
                    id="region"
                    placeholder="Region"
                    value={region}
                    onChange={(e) => setRegion(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <div>
                  <Input
                    id="apiRegion"
                    placeholder="API Region（可选覆盖）"
                    value={apiRegion}
                    onChange={(e) => setApiRegion(e.target.value)}
                    disabled={isPending}
                  />
                </div>
              </div>
              <p className="text-xs text-muted-foreground">
                Region 用于 Token 刷新，留空使用全局配置。API Region 可单独覆盖 API 请求所用的 Region
              </p>
            </div>

            {/* IdC/Builder-ID/IAM 额外字段 */}
            {authMethod === 'idc' && !isApiKey && (
              <>
                <div className="space-y-2">
                  <label htmlFor="clientId" className="text-sm font-medium">
                    Client ID <span className="text-red-500">*</span>
                  </label>
                  <Input
                    id="clientId"
                    placeholder="请输入 Client ID"
                    value={clientId}
                    onChange={(e) => setClientId(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                <div className="space-y-2">
                  <label htmlFor="clientSecret" className="text-sm font-medium">
                    Client Secret <span className="text-red-500">*</span>
                  </label>
                  <Input
                    id="clientSecret"
                    type="password"
                    placeholder="请输入 Client Secret"
                    value={clientSecret}
                    onChange={(e) => setClientSecret(e.target.value)}
                    disabled={isPending}
                  />
                </div>
              </>
            )}

            {/* 优先级 */}
            <div className="space-y-2">
              <label htmlFor="priority" className="text-sm font-medium">
                优先级
              </label>
              <Input
                id="priority"
                type="number"
                min="0"
                placeholder="数字越小优先级越高"
                value={priority}
                onChange={(e) => setPriority(e.target.value)}
                disabled={isPending}
              />
              <p className="text-xs text-muted-foreground">
                数字越小优先级越高，默认为 0
              </p>
            </div>

            {/* Machine ID */}
            <div className="space-y-2">
              <label htmlFor="machineId" className="text-sm font-medium">
                Machine ID
              </label>
              <Input
                id="machineId"
                placeholder="留空使用配置中字段, 否则由刷新Token自动派生"
                value={machineId}
                onChange={(e) => setMachineId(e.target.value)}
                disabled={isPending || isBatch}
              />
              <p className="text-xs text-muted-foreground">
                可选，64 位十六进制字符串，留空使用配置中字段, 否则由刷新Token自动派生
              </p>
            </div>

            {/* Endpoint */}
            <div className="space-y-2">
              <label htmlFor="endpoint" className="text-sm font-medium">
                Endpoint
              </label>
              <select
                id="endpoint"
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                disabled={isPending}
                className="flex h-10 w-full rounded-md border border-input bg-background px-3 py-2 text-sm ring-offset-background focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <option value="">默认值</option>
                <option value="ide">ide</option>
                <option value="cli">cli</option>
              </select>
              <p className="text-xs text-muted-foreground">
                留空时回退到全局 defaultEndpoint
              </p>
            </div>

            {/* 代理池策略 */}
            <div className="space-y-2 rounded-md border bg-muted/30 p-3">
              <label className="flex items-center gap-2 text-sm font-medium">
                <input
                  type="checkbox"
                  checked={!autoBindProxy}
                  onChange={(e) => setAutoBindProxy(!e.target.checked)}
                  disabled={isPending || useImportPipeline}
                />
                暂不绑定代理（导入后手动指派）
                {useImportPipeline && (proxyCount > 0 ? '（带内嵌代理时固定绑定该代理）' : '（批量模式固定自动分配）')}
              </label>
              <p className="text-xs text-muted-foreground">
                启用代理池时：默认勾选将自动从池里分配最优代理；勾选此项则导入后凭据保持禁用，需到凭据列表手动绑定。代理池未启用时此选项无影响。
              </p>
            </div>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
              disabled={isPending}
            >
              取消
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending ? '添加中...' : isBatch ? `批量导入 ${apiKeyEntries.length} 条` : '添加'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
