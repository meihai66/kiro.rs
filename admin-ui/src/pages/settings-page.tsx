import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  useGlobalConfig,
  useProxyConfig,
  useUpdateGlobalConfig,
  useUpdateProxyConfig,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import { storage } from '@/lib/storage'
import type {
  UpdateCompressionConfigRequest,
  UpdateGlobalConfigRequest,
} from '@/types/api'

export function SettingsPage() {
  const { data: globalConfig, isLoading: globalLoading } = useGlobalConfig()
  const { data: proxyConfig, isLoading: proxyLoading } = useProxyConfig()
  const { mutate: mutateGlobal, isPending: globalPending } =
    useUpdateGlobalConfig()
  const { mutate: mutateProxy, isPending: proxyPending } =
    useUpdateProxyConfig()

  // 基本设置
  const [region, setRegion] = useState('')
  const [credentialRpm, setCredentialRpm] = useState('')
  const [promptCacheTtlSeconds, setPromptCacheTtlSeconds] = useState('300')
  const [promptCacheAccountingEnabled, setPromptCacheAccountingEnabled] =
    useState(true)
  const [defaultEndpoint, setDefaultEndpoint] = useState('ide')

  // 全局代理（用于 count_tokens 等非凭据出站）
  const [proxyUrl, setProxyUrl] = useState('')
  const [proxyUsername, setProxyUsername] = useState('')
  const [proxyPassword, setProxyPassword] = useState('')

  // 表格自动刷新频率（仅前端 localStorage）
  const [refreshSecs, setRefreshSecs] = useState(() =>
    storage.getRefreshIntervalSecs()
  )
  const handleSaveRefresh = () => {
    storage.setRefreshIntervalSecs(refreshSecs)
    toast.success(`自动刷新频率已保存：${refreshSecs}s`)
  }

  // 自动禁用规则（textarea，每行一条）
  const [autoDisablePatternsText, setAutoDisablePatternsText] = useState('')
  // 错误内容替换规则（每行一条 pattern===replacement）
  const [errorReplaceRulesText, setErrorReplaceRulesText] = useState('')
  // 使用率自动禁用阈值
  const [autoDisableUsagePct, setAutoDisableUsagePct] = useState(0)

  // 重试相关
  const [maxRetriesPerCred, setMaxRetriesPerCred] = useState('2')
  const [maxTotalRetries, setMaxTotalRetries] = useState('3')
  const [allCoolingBailSecs, setAllCoolingBailSecs] = useState('2')

  // 导入凭据默认禁用
  const [importDisabledByDefault, setImportDisabledByDefault] = useState(true)

  // 余额自动刷新
  const [balanceAutoRefreshSecs, setBalanceAutoRefreshSecs] = useState('0')

  // 限流冷却
  const [rateLimitCooldownMin, setRateLimitCooldownMin] = useState('60')
  const [rateLimitCooldownMax, setRateLimitCooldownMax] = useState('300')
  const [capacityPressureCooldown, setCapacityPressureCooldown] = useState('8')
  const [rateLimitIgnoreRetryAfter, setRateLimitIgnoreRetryAfter] =
    useState(false)

  // 错误日志
  const [errorLogEnabled, setErrorLogEnabled] = useState(true)
  const [errorLogMaxCount, setErrorLogMaxCount] = useState('50000')
  const [errorLogMaxAgeDays, setErrorLogMaxAgeDays] = useState('7')
  const [errorLogExcludedCodes, setErrorLogExcludedCodes] = useState('')

  // 压缩配置
  const [cEnabled, setCEnabled] = useState(true)
  const [cWhitespace, setCWhitespace] = useState(true)
  const [cThinkingStrategy, setCThinkingStrategy] = useState('discard')
  const [cToolResultMaxChars, setCToolResultMaxChars] = useState('')
  const [cToolResultHeadLines, setCToolResultHeadLines] = useState('')
  const [cToolResultTailLines, setCToolResultTailLines] = useState('')
  const [cToolUseInputMaxChars, setCToolUseInputMaxChars] = useState('')
  const [cToolDescMaxChars, setCToolDescMaxChars] = useState('')
  const [cMaxHistoryTurns, setCMaxHistoryTurns] = useState('')
  const [cMaxHistoryChars, setCMaxHistoryChars] = useState('')
  const [cMaxRequestBodyBytes, setCMaxRequestBodyBytes] = useState('')

  const isLoading = globalLoading || proxyLoading
  const isPending = globalPending || proxyPending

  useEffect(() => {
    if (globalConfig) {
      setRegion(globalConfig.region || '')
      setCredentialRpm(globalConfig.credentialRpm?.toString() || '')
      setPromptCacheTtlSeconds(globalConfig.promptCacheTtlSeconds.toString())
      setPromptCacheAccountingEnabled(globalConfig.promptCacheAccountingEnabled)
      setDefaultEndpoint(globalConfig.defaultEndpoint || 'ide')
      setAutoDisablePatternsText(
        (globalConfig.autoDisablePatterns ?? []).join('\n')
      )
      setErrorReplaceRulesText(
        (globalConfig.errorReplaceRules ?? []).join('\n')
      )
      setAutoDisableUsagePct(globalConfig.autoDisableUsageThresholdPct ?? 0)
      setMaxRetriesPerCred(String(globalConfig.maxRetriesPerCredential ?? 2))
      setMaxTotalRetries(String(globalConfig.maxTotalRetries ?? 3))
      setAllCoolingBailSecs(
        String(globalConfig.allCredentialsCooldownBailThresholdSecs ?? 2)
      )
      setImportDisabledByDefault(globalConfig.importDisabledByDefault ?? true)
      setBalanceAutoRefreshSecs(String(globalConfig.balanceAutoRefreshSecs ?? 0))
      setRateLimitCooldownMin(String(globalConfig.rateLimitCooldownMinSecs ?? 60))
      setRateLimitCooldownMax(String(globalConfig.rateLimitCooldownMaxSecs ?? 300))
      setCapacityPressureCooldown(
        String(globalConfig.capacityPressureCooldownSecs ?? 8)
      )
      setRateLimitIgnoreRetryAfter(
        globalConfig.rateLimitIgnoreRetryAfter ?? false
      )
      setErrorLogEnabled(globalConfig.errorLogEnabled ?? true)
      setErrorLogMaxCount(String(globalConfig.errorLogMaxCount ?? 50000))
      setErrorLogMaxAgeDays(String(globalConfig.errorLogMaxAgeDays ?? 7))
      setErrorLogExcludedCodes(
        (globalConfig.errorLogExcludedStatusCodes ?? []).join(', ')
      )
      const c = globalConfig.compression
      setCEnabled(c.enabled)
      setCWhitespace(c.whitespaceCompression)
      setCThinkingStrategy(c.thinkingStrategy)
      setCToolResultMaxChars(c.toolResultMaxChars.toString())
      setCToolResultHeadLines(c.toolResultHeadLines.toString())
      setCToolResultTailLines(c.toolResultTailLines.toString())
      setCToolUseInputMaxChars(c.toolUseInputMaxChars.toString())
      setCToolDescMaxChars(c.toolDescriptionMaxChars.toString())
      setCMaxHistoryTurns(c.maxHistoryTurns.toString())
      setCMaxHistoryChars(c.maxHistoryChars.toString())
      setCMaxRequestBodyBytes(c.maxRequestBodyBytes.toString())
    }
    if (proxyConfig) {
      setProxyUrl(proxyConfig.proxyUrl || '')
      setProxyUsername('')
      setProxyPassword('')
    }
  }, [globalConfig, proxyConfig])

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()

    const globalPayload: UpdateGlobalConfigRequest = {}
    let hasGlobalChanges = false

    if (region.trim() !== (globalConfig?.region || '')) {
      globalPayload.region = region.trim()
      hasGlobalChanges = true
    }

    const newRpm = credentialRpm.trim() ? parseInt(credentialRpm.trim(), 10) : null
    if (newRpm !== (globalConfig?.credentialRpm ?? null)) {
      globalPayload.credentialRpm = newRpm
      hasGlobalChanges = true
    }

    const newTtl = parseInt(promptCacheTtlSeconds, 10)
    if (globalConfig && newTtl !== globalConfig.promptCacheTtlSeconds) {
      globalPayload.promptCacheTtlSeconds = newTtl
      hasGlobalChanges = true
    }

    if (
      globalConfig &&
      promptCacheAccountingEnabled !== globalConfig.promptCacheAccountingEnabled
    ) {
      globalPayload.promptCacheAccountingEnabled = promptCacheAccountingEnabled
      hasGlobalChanges = true
    }

    if (defaultEndpoint !== (globalConfig?.defaultEndpoint || 'ide')) {
      globalPayload.defaultEndpoint = defaultEndpoint
      hasGlobalChanges = true
    }

    // 自动禁用规则：每行一条；提交时一并替换
    const newPatterns = autoDisablePatternsText
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
    const oldPatterns = (globalConfig?.autoDisablePatterns ?? [])
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
    if (
      newPatterns.length !== oldPatterns.length ||
      newPatterns.some((p, i) => p !== oldPatterns[i])
    ) {
      globalPayload.autoDisablePatterns = newPatterns
      hasGlobalChanges = true
    }

    // 错误内容替换规则
    const newReplaceRules = errorReplaceRulesText
      .split(/\r?\n/)
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
    const oldReplaceRules = (globalConfig?.errorReplaceRules ?? [])
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
    if (
      newReplaceRules.length !== oldReplaceRules.length ||
      newReplaceRules.some((p, i) => p !== oldReplaceRules[i])
    ) {
      globalPayload.errorReplaceRules = newReplaceRules
      hasGlobalChanges = true
    }

    if (autoDisableUsagePct !== (globalConfig?.autoDisableUsageThresholdPct ?? 0)) {
      globalPayload.autoDisableUsageThresholdPct = autoDisableUsagePct
      hasGlobalChanges = true
    }

    const newMaxRetriesCred = Math.max(0, parseInt(maxRetriesPerCred, 10) || 0)
    if (newMaxRetriesCred !== (globalConfig?.maxRetriesPerCredential ?? 2)) {
      globalPayload.maxRetriesPerCredential = newMaxRetriesCred
      hasGlobalChanges = true
    }

    const newMaxTotalRetries = Math.max(0, parseInt(maxTotalRetries, 10) || 0)
    if (newMaxTotalRetries !== (globalConfig?.maxTotalRetries ?? 3)) {
      globalPayload.maxTotalRetries = newMaxTotalRetries
      hasGlobalChanges = true
    }

    const newCoolingBailSecs = Math.max(0, parseInt(allCoolingBailSecs, 10) || 0)
    if (
      newCoolingBailSecs !==
      (globalConfig?.allCredentialsCooldownBailThresholdSecs ?? 2)
    ) {
      globalPayload.allCredentialsCooldownBailThresholdSecs = newCoolingBailSecs
      hasGlobalChanges = true
    }

    if (
      importDisabledByDefault !==
      (globalConfig?.importDisabledByDefault ?? true)
    ) {
      globalPayload.importDisabledByDefault = importDisabledByDefault
      hasGlobalChanges = true
    }

    const newBalanceAutoRefreshSecs = Math.max(
      0,
      parseInt(balanceAutoRefreshSecs, 10) || 0
    )
    if (newBalanceAutoRefreshSecs !== (globalConfig?.balanceAutoRefreshSecs ?? 0)) {
      // 范围校验：合法值是 0 或 60~86400。后端拒收非法值；这里前置拦截避免静默 400。
      if (
        newBalanceAutoRefreshSecs !== 0 &&
        (newBalanceAutoRefreshSecs < 60 || newBalanceAutoRefreshSecs > 86_400)
      ) {
        toast.error('余额自动刷新周期应为 0（禁用）或 60~86400 秒')
        return
      }
      globalPayload.balanceAutoRefreshSecs = newBalanceAutoRefreshSecs
      hasGlobalChanges = true
    }

    const newRlMin = Math.max(1, parseInt(rateLimitCooldownMin, 10) || 60)
    if (newRlMin !== (globalConfig?.rateLimitCooldownMinSecs ?? 60)) {
      globalPayload.rateLimitCooldownMinSecs = newRlMin
      hasGlobalChanges = true
    }
    const newRlMax = Math.max(1, parseInt(rateLimitCooldownMax, 10) || 300)
    if (newRlMax !== (globalConfig?.rateLimitCooldownMaxSecs ?? 300)) {
      globalPayload.rateLimitCooldownMaxSecs = newRlMax
      hasGlobalChanges = true
    }
    const newCapCool = Math.max(1, parseInt(capacityPressureCooldown, 10) || 8)
    if (newCapCool !== (globalConfig?.capacityPressureCooldownSecs ?? 8)) {
      globalPayload.capacityPressureCooldownSecs = newCapCool
      hasGlobalChanges = true
    }

    if (
      rateLimitIgnoreRetryAfter !==
      (globalConfig?.rateLimitIgnoreRetryAfter ?? false)
    ) {
      globalPayload.rateLimitIgnoreRetryAfter = rateLimitIgnoreRetryAfter
      hasGlobalChanges = true
    }

    // 错误日志
    if (errorLogEnabled !== (globalConfig?.errorLogEnabled ?? true)) {
      globalPayload.errorLogEnabled = errorLogEnabled
      hasGlobalChanges = true
    }
    const newErrorLogMaxCount = Math.max(0, parseInt(errorLogMaxCount, 10) || 0)
    if (newErrorLogMaxCount !== (globalConfig?.errorLogMaxCount ?? 50000)) {
      globalPayload.errorLogMaxCount = newErrorLogMaxCount
      hasGlobalChanges = true
    }
    const newErrorLogMaxAgeDays = Math.max(0, parseInt(errorLogMaxAgeDays, 10) || 0)
    if (newErrorLogMaxAgeDays !== (globalConfig?.errorLogMaxAgeDays ?? 7)) {
      globalPayload.errorLogMaxAgeDays = newErrorLogMaxAgeDays
      hasGlobalChanges = true
    }
    const newExcludedCodes = errorLogExcludedCodes
      .split(',')
      .map((s) => parseInt(s.trim(), 10))
      .filter((n) => Number.isFinite(n) && n > 0)
    const oldExcludedCodes = globalConfig?.errorLogExcludedStatusCodes ?? []
    const codesChanged =
      newExcludedCodes.length !== oldExcludedCodes.length ||
      newExcludedCodes.some((c, i) => c !== oldExcludedCodes[i])
    if (codesChanged) {
      globalPayload.errorLogExcludedStatusCodes = newExcludedCodes
      hasGlobalChanges = true
    }

    if (globalConfig) {
      const oc = globalConfig.compression
      const comp: UpdateCompressionConfigRequest = {}
      let hasCompChanges = false
      const setIf = <K extends keyof UpdateCompressionConfigRequest>(
        key: K,
        newVal: UpdateCompressionConfigRequest[K],
        oldVal: UpdateCompressionConfigRequest[K]
      ) => {
        if (newVal !== oldVal) {
          comp[key] = newVal
          hasCompChanges = true
        }
      }
      setIf('enabled', cEnabled, oc.enabled)
      setIf('whitespaceCompression', cWhitespace, oc.whitespaceCompression)
      setIf('thinkingStrategy', cThinkingStrategy, oc.thinkingStrategy)
      setIf('toolResultMaxChars', parseInt(cToolResultMaxChars) || 0, oc.toolResultMaxChars)
      setIf('toolResultHeadLines', parseInt(cToolResultHeadLines) || 0, oc.toolResultHeadLines)
      setIf('toolResultTailLines', parseInt(cToolResultTailLines) || 0, oc.toolResultTailLines)
      setIf('toolUseInputMaxChars', parseInt(cToolUseInputMaxChars) || 0, oc.toolUseInputMaxChars)
      setIf('toolDescriptionMaxChars', parseInt(cToolDescMaxChars) || 0, oc.toolDescriptionMaxChars)
      setIf('maxHistoryTurns', parseInt(cMaxHistoryTurns) || 0, oc.maxHistoryTurns)
      setIf('maxHistoryChars', parseInt(cMaxHistoryChars) || 0, oc.maxHistoryChars)
      setIf(
        'maxRequestBodyBytes',
        parseInt(cMaxRequestBodyBytes) || 0,
        oc.maxRequestBodyBytes
      )
      if (hasCompChanges) {
        globalPayload.compression = comp
        hasGlobalChanges = true
      }
    }

    const proxyPayload: Record<string, string | null> = {
      proxyUrl: proxyUrl.trim() || null,
    }
    if (proxyUsername.trim() || proxyPassword.trim()) {
      proxyPayload.proxyUsername = proxyUsername.trim() || null
      proxyPayload.proxyPassword = proxyPassword.trim() || null
    }
    const hasProxyChanges =
      proxyPayload.proxyUrl !== (proxyConfig?.proxyUrl || null) ||
      proxyPayload.proxyUsername !== undefined ||
      proxyPayload.proxyPassword !== undefined

    if (hasGlobalChanges) {
      mutateGlobal(globalPayload, {
        onSuccess: () => toast.success('全局配置已保存'),
        onError: (e) => toast.error(`保存失败：${extractErrorMessage(e)}`),
      })
    }
    if (hasProxyChanges) {
      mutateProxy(proxyPayload, {
        onSuccess: () => toast.success('全局代理已保存'),
        onError: (e) => toast.error(`保存失败：${extractErrorMessage(e)}`),
      })
    }
    if (!hasGlobalChanges && !hasProxyChanges) {
      toast.info('没有变更')
    }
  }

  const numInput = (
    id: string,
    label: string,
    value: string,
    setter: (v: string) => void,
    hint?: string
  ) => (
    <div className="space-y-1">
      <label htmlFor={id} className="text-sm font-medium">
        {label}
      </label>
      <Input
        id={id}
        type="number"
        min={0}
        value={value}
        onChange={(e) => setter(e.target.value)}
        disabled={isPending}
      />
      {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
    </div>
  )

  return (
    <>
      <div className="flex items-center justify-between mb-4">
        <h1 className="text-2xl font-semibold">设置</h1>
      </div>

      {isLoading ? (
        <div className="py-8 text-center text-muted-foreground">加载中…</div>
      ) : (
        <form onSubmit={handleSubmit} className="space-y-4 max-w-3xl">
          {/* 基本 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">基本设置</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="space-y-1">
                <label className="text-sm font-medium">Region</label>
                <Input
                  placeholder="us-east-1"
                  value={region}
                  onChange={(e) => setRegion(e.target.value)}
                  disabled={isPending}
                />
              </div>
              {numInput(
                'rpm',
                'Credential RPM',
                credentialRpm,
                setCredentialRpm,
                '单凭据每分钟请求数上限，0 或留空使用默认策略'
              )}
              <div className="space-y-1">
                <label className="text-sm font-medium">Prompt Cache TTL</label>
                <select
                  className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm"
                  value={promptCacheTtlSeconds}
                  onChange={(e) => setPromptCacheTtlSeconds(e.target.value)}
                  disabled={isPending}
                >
                  <option value="300">5 分钟</option>
                  <option value="3600">1 小时</option>
                </select>
                <p className="text-xs text-muted-foreground">仅支持 5 分钟和 1 小时两档，保存后立即生效</p>
              </div>
              <div className="flex items-center justify-between">
                <div>
                  <label className="text-sm font-medium">Prompt Cache 记账</label>
                  <p className="text-xs text-muted-foreground">
                    关闭后立即停止输出和扣减本地 cache token
                  </p>
                </div>
                <Switch
                  checked={promptCacheAccountingEnabled}
                  onCheckedChange={setPromptCacheAccountingEnabled}
                  disabled={isPending}
                />
              </div>
              <div className="space-y-1">
                <label className="text-sm font-medium">默认 Endpoint</label>
                <select
                  className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm"
                  value={defaultEndpoint}
                  onChange={(e) => setDefaultEndpoint(e.target.value)}
                  disabled={isPending}
                >
                  <option value="ide">ide</option>
                  <option value="cli">cli</option>
                </select>
              </div>
              <div className="flex items-center justify-between">
                <div>
                  <label className="text-sm font-medium">导入默认禁用</label>
                  <p className="text-xs text-muted-foreground">
                    新添加 / 批量导入的凭据先置 disabled，验证后手动启用，避免未验证的号直接进调度
                  </p>
                </div>
                <Switch
                  checked={importDisabledByDefault}
                  onCheckedChange={setImportDisabledByDefault}
                  disabled={isPending}
                />
              </div>
            </CardContent>
          </Card>

          {/* 重试与冷却 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">重试与冷却</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-xs text-muted-foreground">
                控制单个请求在凭据间故障转移的重试预算，以及"所有凭据均处于冷却"
                时多快放弃并向客户端返回 <code>429 + Retry-After</code>。
              </p>
              <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                {numInput(
                  'maxRetriesCred',
                  '单凭据最多重试次数',
                  maxRetriesPerCred,
                  setMaxRetriesPerCred,
                  '默认 2；0 视为未配置'
                )}
                {numInput(
                  'maxTotalRetries',
                  '单请求总重试上限',
                  maxTotalRetries,
                  setMaxTotalRetries,
                  '默认 3；最终次数=min(凭据数×单凭据次数, 总上限)'
                )}
                {numInput(
                  'coolingBailSecs',
                  '全冷却 bail 阈值（秒）',
                  allCoolingBailSecs,
                  setAllCoolingBailSecs,
                  '默认 2；最短可用等待 ≤ 该值则短睡再试，> 则立即 429'
                )}
              </div>
            </CardContent>
          </Card>

          {/* 限流冷却 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">限流冷却（429）</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-xs text-muted-foreground">
                凭据收到上游 429 时进入冷却。下面三个值分别控制：
                <br />
                <strong>最短/最长</strong>：把上游 Retry-After clamp 到此区间；
                上游不带 Retry-After 时直接用最短值。
                <br />
                <strong>容量瓶颈</strong>：检测到 INSUFFICIENT_MODEL_CAPACITY /
                "high traffic"（不是单号被限流，是上游瞬时容量不够）时使用的短冷却。
              </p>
              <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                {numInput(
                  'rlMin',
                  '最短冷却（秒）',
                  rateLimitCooldownMin,
                  setRateLimitCooldownMin,
                  '默认 60，范围 1~3600'
                )}
                {numInput(
                  'rlMax',
                  '最长冷却（秒）',
                  rateLimitCooldownMax,
                  setRateLimitCooldownMax,
                  '默认 300，范围 1~86400'
                )}
                {numInput(
                  'capCool',
                  '容量瓶颈冷却（秒）',
                  capacityPressureCooldown,
                  setCapacityPressureCooldown,
                  '默认 8，范围 1~600'
                )}
              </div>
              <div className="flex items-center justify-between pt-2 border-t">
                <div>
                  <label className="text-sm font-medium">
                    忽略上游 Retry-After
                  </label>
                  <p className="text-xs text-muted-foreground">
                    开启后普通 429 直接在 [最短, 最长] 内随机出冷却时长，
                    不再尊重上游头。容量瓶颈型 429 仍走容量短冷却。
                  </p>
                </div>
                <Switch
                  checked={rateLimitIgnoreRetryAfter}
                  onCheckedChange={setRateLimitIgnoreRetryAfter}
                  disabled={isPending}
                />
              </div>
            </CardContent>
          </Card>

          {/* 余额自动刷新 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">余额自动刷新</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <p className="text-xs text-muted-foreground">
                后台每 30 秒滚动刷新最旧的凭据余额；按下面的目标周期把所有凭据均匀打散。
                <br />
                跳过：已禁用 / 在冷却 / 上次刷新还没到周期的凭据。
                设为 <code>0</code> 关闭后台任务（手动"查余额"不受影响）。
                推荐 600~900（10~15 分钟）。
              </p>
              <div className="flex items-center gap-2 max-w-xs">
                <Input
                  type="number"
                  min={0}
                  max={86400}
                  value={balanceAutoRefreshSecs}
                  onChange={(e) => setBalanceAutoRefreshSecs(e.target.value)}
                  disabled={isPending}
                />
                <span className="text-sm">秒</span>
              </div>
              <p className="text-[11px] text-muted-foreground">
                有效范围：0（关闭）或 60~86400 秒
              </p>
            </CardContent>
          </Card>

          {/* 错误日志 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">错误日志</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <p className="text-xs text-muted-foreground">
                只记录"最终返回给客户端的 4xx/5xx"，包含完整请求/响应体；详情页才会拉取大字段，
                列表页拉取很轻量。后台每小时按下面的限制清理旧记录。
              </p>
              <div className="flex items-center justify-between">
                <div>
                  <label className="text-sm font-medium">启用</label>
                  <p className="text-xs text-muted-foreground">关闭后不再写新日志（已有记录仍可查阅）</p>
                </div>
                <Switch
                  checked={errorLogEnabled}
                  onCheckedChange={setErrorLogEnabled}
                  disabled={isPending}
                />
              </div>
              <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
                {numInput(
                  'errorLogMaxCount',
                  '最大保留条数',
                  errorLogMaxCount,
                  setErrorLogMaxCount,
                  '默认 50000；0=不限'
                )}
                {numInput(
                  'errorLogMaxAgeDays',
                  '最大保留天数',
                  errorLogMaxAgeDays,
                  setErrorLogMaxAgeDays,
                  '默认 7；0=不限'
                )}
              </div>
              <div className="space-y-1">
                <label className="text-sm font-medium">不记录的状态码</label>
                <Input
                  placeholder="例如：400, 401"
                  value={errorLogExcludedCodes}
                  onChange={(e) => setErrorLogExcludedCodes(e.target.value)}
                  disabled={isPending}
                  className="text-xs font-mono"
                />
                <p className="text-xs text-muted-foreground">
                  逗号分隔；这些状态码不会写入日志数据库，但仍会正常返回给客户端
                </p>
              </div>
            </CardContent>
          </Card>

          {/* 错误响应自动禁用规则 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">错误响应自动禁用规则</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <p className="text-xs text-muted-foreground">
                上游请求返回错误时，若响应 body 包含其中任一字符串，对应凭据会被永久禁用。
                <br />
                每行一条规则；空行忽略；保存后立即生效（持久化到 config.json）。
              </p>
              <textarea
                className="w-full h-40 rounded-md border bg-background p-2 text-sm font-mono"
                placeholder={
                  '示例：\nreached the limit for overages\nAccount has been suspended\ninvalid_grant'
                }
                value={autoDisablePatternsText}
                onChange={(e) => setAutoDisablePatternsText(e.target.value)}
                disabled={isPending}
              />
              <div className="text-xs text-muted-foreground">
                当前已识别{' '}
                {
                  autoDisablePatternsText
                    .split(/\r?\n/)
                    .map((s) => s.trim())
                    .filter((s) => s.length > 0).length
                }{' '}
                条规则
              </div>
            </CardContent>
          </Card>

          {/* 前端：自动刷新频率（仅本浏览器） */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">UI 自动刷新频率</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <p className="text-xs text-muted-foreground">
                凭据列表 / 缓存余额轮询间隔，仅本浏览器生效（保存在
                localStorage）。最小 1s，最大 300s。
              </p>
              <div className="flex items-center gap-2 max-w-xs">
                <Input
                  type="number"
                  min={1}
                  max={300}
                  value={refreshSecs}
                  onChange={(e) =>
                    setRefreshSecs(
                      Math.max(1, Math.min(300, Number(e.target.value) || 2))
                    )
                  }
                />
                <span className="text-sm">秒</span>
                <Button
                  type="button"
                  size="sm"
                  variant="outline"
                  onClick={handleSaveRefresh}
                >
                  保存
                </Button>
              </div>
            </CardContent>
          </Card>

          {/* 使用率自动禁用 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">使用率自动禁用</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <p className="text-xs text-muted-foreground">
                当凭据查询余额时，若 <code>已用/额度 ≥ 阈值%</code> 自动禁用该凭据。
                阈值 0 表示不启用。
              </p>
              <div className="flex items-center gap-2 max-w-xs">
                <Input
                  type="number"
                  min={0}
                  max={100}
                  value={autoDisableUsagePct}
                  onChange={(e) =>
                    setAutoDisableUsagePct(
                      Math.max(0, Math.min(100, Number(e.target.value) || 0))
                    )
                  }
                  disabled={isPending}
                />
                <span className="text-sm">%</span>
              </div>
            </CardContent>
          </Card>

          {/* 错误内容替换规则 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">错误内容替换规则</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <p className="text-xs text-muted-foreground">
                响应错误体命中 <code>pattern</code> 时，优先把 JSON 里的错误原因字段（{' '}
                <code>message</code> / <code>error.message</code> 等）的值替换为{' '}
                <code>replacement</code>，保留外层 JSON 结构再返回给客户端；不是 JSON 或找不到字段则整段替换。
                <br />
                每行一条，格式：<code>pattern===replacement</code>；从上往下匹配，第一条命中即停止。
                仅影响返回给客户端的错误体，内部禁用/重试逻辑用原始体。
              </p>
              <textarea
                className="w-full h-32 rounded-md border bg-background p-2 text-sm font-mono"
                placeholder={
                  '示例：\nMONTHLY_REQUEST_COUNT===请稍后重试，月度请求次数已达上限\nbearer token included in the request is invalid===上游凭据失效，已切换其他凭据'
                }
                value={errorReplaceRulesText}
                onChange={(e) => setErrorReplaceRulesText(e.target.value)}
                disabled={isPending}
              />
              <div className="text-xs text-muted-foreground">
                当前已识别{' '}
                {
                  errorReplaceRulesText
                    .split(/\r?\n/)
                    .map((s) => s.trim())
                    .filter((s) => s.length > 0 && s.includes('===')).length
                }{' '}
                条有效规则
                {errorReplaceRulesText
                  .split(/\r?\n/)
                  .some((s) => s.trim() && !s.includes('===')) && (
                  <span className="text-yellow-600 ml-1">
                    （含格式不正确的行，将被忽略）
                  </span>
                )}
              </div>
            </CardContent>
          </Card>

          {/* 全局代理 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">全局代理（非代理池场景）</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="space-y-1">
                <label className="text-sm font-medium">代理 URL</label>
                <Input
                  placeholder="http://proxy:8080 或 socks5://proxy:1080"
                  value={proxyUrl}
                  onChange={(e) => setProxyUrl(e.target.value)}
                  disabled={isPending}
                />
                <p className="text-xs text-muted-foreground">
                  仅用于 count_tokens 等非凭据出站；启用代理池后凭据走代理槽，与此无关
                </p>
              </div>
              <div className="space-y-1">
                <label className="text-sm font-medium">代理认证（可选）</label>
                <div className="grid grid-cols-2 gap-2">
                  <Input
                    placeholder="用户名"
                    value={proxyUsername}
                    onChange={(e) => setProxyUsername(e.target.value)}
                    disabled={isPending}
                  />
                  <Input
                    type="password"
                    placeholder="密码"
                    value={proxyPassword}
                    onChange={(e) => setProxyPassword(e.target.value)}
                    disabled={isPending}
                  />
                </div>
                {proxyConfig?.hasCredentials && (
                  <p className="text-xs text-muted-foreground">
                    已配置认证，留空保持不变
                  </p>
                )}
              </div>
            </CardContent>
          </Card>

          {/* 压缩 */}
          <Card>
            <CardHeader>
              <CardTitle className="text-base">压缩配置</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium">启用压缩</label>
                <Switch
                  checked={cEnabled}
                  onCheckedChange={setCEnabled}
                  disabled={isPending}
                />
              </div>
              <div className="flex items-center justify-between">
                <label className="text-sm font-medium">空白压缩</label>
                <Switch
                  checked={cWhitespace}
                  onCheckedChange={setCWhitespace}
                  disabled={isPending}
                />
              </div>
              <div className="space-y-1">
                <label className="text-sm font-medium">Thinking 策略</label>
                <select
                  className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm"
                  value={cThinkingStrategy}
                  onChange={(e) => setCThinkingStrategy(e.target.value)}
                  disabled={isPending}
                >
                  <option value="discard">discard — 丢弃 thinking 内容（推荐，节省最多 token）</option>
                  <option value="truncate">truncate — 截断保留头/尾若干字符</option>
                  <option value="keep">keep — 完整保留（不压缩）</option>
                </select>
                <p className="text-[11px] text-muted-foreground">
                  对历史消息中的 thinking 块的处理：discard 完全移除；truncate 保留首尾摘要；keep 不压缩（最贵）
                </p>
              </div>
              {numInput('trMax', 'tool_result 截断阈值（字符）', cToolResultMaxChars, setCToolResultMaxChars)}
              <div className="grid grid-cols-2 gap-2">
                {numInput('trHead', 'tool_result 保留头部行数', cToolResultHeadLines, setCToolResultHeadLines)}
                {numInput('trTail', 'tool_result 保留尾部行数', cToolResultTailLines, setCToolResultTailLines)}
              </div>
              {numInput('tuMax', 'tool_use input 截断阈值（字符）', cToolUseInputMaxChars, setCToolUseInputMaxChars)}
              {numInput('tdMax', '工具描述截断阈值（字符）', cToolDescMaxChars, setCToolDescMaxChars)}
              <div className="grid grid-cols-2 gap-2">
                {numInput('mt', '历史最大轮数', cMaxHistoryTurns, setCMaxHistoryTurns, '0 = 不限')}
                {numInput('mc', '历史最大字符数', cMaxHistoryChars, setCMaxHistoryChars, '0 = 不限')}
              </div>
              {numInput(
                'mb',
                '请求体大小上限（字节）',
                cMaxRequestBodyBytes,
                setCMaxRequestBodyBytes,
                '超过此大小触发自适应压缩，0 = 不限'
              )}
            </CardContent>
          </Card>

          <div className="sticky bottom-0 bg-background py-3 flex justify-end gap-2 border-t">
            <Button type="submit" disabled={isPending}>
              {isPending ? '保存中…' : '保存'}
            </Button>
          </div>
        </form>
      )}
    </>
  )
}
