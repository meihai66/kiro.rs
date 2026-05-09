// 凭据状态响应
export interface CredentialsStatusResponse {
  total: number
  available: number
  credentials: CredentialStatusItem[]
}

// 单个凭据状态
export interface CredentialStatusItem {
  id: number
  priority: number
  disabled: boolean
  failureCount: number
  refreshFailureCount: number
  disabledReason?: string | null
  expiresAt: string | null
  authMethod: string | null
  hasProfileArn: boolean
  accountEmail: string | null
  email?: string
  refreshTokenHash?: string
  subscriptionTitle?: string | null

  // ===== 统计（可持久化） =====
  callsTotal: number
  callsOk: number
  callsErr: number
  inputTokensTotal: number
  outputTokensTotal: number
  lastCallAt: string | null
  lastSuccessAt: string | null
  lastErrorAt: string | null
  lastError: string | null

  // ===== upstream 字段 =====
  successCount: number
  lastUsedAt: string | null
  /** 凭据级 Region（用于 Token 刷新） */
  region: string | null
  /** 凭据级 API Region（单独覆盖 API 请求） */
  apiRegion: string | null
  /** 凭据显式配置的 endpoint，null 表示回退默认值 */
  endpoint?: string | null
  /** 最终生效的 endpoint */
  effectiveEndpoint: string
  /** 当前绑定的代理槽 ID（启用代理池时；未绑定者不允许启用） */
  proxySlotId: string | null
  /** 已知最近的 overageStatus（"ENABLED" / "DISABLED"），可能为空 */
  overageStatus: string | null
  /** 当前并发请求数（实时） */
  inFlight: number
  /** 最近 60 秒 RPM（实时） */
  rpm: number
  /** 累计 429 触发次数（运行时统计；重启清零） */
  rateLimitCount: number
  /** 允许超额使用：开启后即使额度用尽也不主动禁用（凭据级开关，与上游账号 overageStatus 不同） */
  allowOveruse: boolean
  /** 当前冷却原因（None/缺省 表示不在冷却中） */
  cooldownReason?: string | null
  /** 当前冷却剩余时间（秒） */
  cooldownRemainingSecs?: number | null
  /** 凭据级 RPM 上限（缺省表示沿用全局 credentialRpm） */
  credentialRpm?: number | null
}

// ===== API Keys =====

export interface ApiKeyItem {
  id: number
  key: string
  keyMasked: string
  name: string
  description?: string | null
  enabled: boolean
  maxConcurrent: number
  cacheReadMinPct: number
  cacheReadMaxPct: number
  createdAt: string
  lastUsedAt?: string | null
  successCount: number
  failCount: number
  inFlight: number
}

export interface ApiKeyListResponse {
  total: number
  keys: ApiKeyItem[]
}

export interface CreateApiKeyRequest {
  name: string
  description?: string
  customKey?: string
  maxConcurrent?: number
  cacheReadMinPct?: number
  cacheReadMaxPct?: number
}

export interface UpdateApiKeyRequest {
  name?: string
  description?: string | null
  enabled?: boolean
  maxConcurrent?: number
  cacheReadMinPct?: number
  cacheReadMaxPct?: number
}

// ===== RPM 历史 =====

export interface RpmHistoryPoint {
  /** Unix 分钟时间戳（秒/60） */
  minuteTs: number
  count: number
}

export interface RpmHistoryResponse {
  id: number
  hours: number
  points: RpmHistoryPoint[]
}

export interface RpmHistoryAggregateResponse {
  hours: number
  points: RpmHistoryPoint[]
}

export interface StatsSummaryResponse {
  startedAt: string
  uptimeSecs: number
  totalRequests: number
  totalSuccess: number
  totalFail: number
}

// ===== 超额计费偏好 =====

export interface SetOveragePreferenceRequest {
  overage_status: 'ENABLED' | 'DISABLED'
}

export interface SetOveragePreferenceResponse {
  ok: boolean
  overage_status?: string | null
  error?: string | null
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
}

// 缓存余额信息
export interface CachedBalanceInfo {
  id: number
  remaining: number
  usageLimit: number
  usagePercentage: number
  subscriptionTitle: string | null
  cachedAt: number // Unix 毫秒时间戳
  ttlSecs: number
}

// 缓存余额响应
export interface CachedBalancesResponse {
  balances: CachedBalanceInfo[]
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// ===== 统计（可持久化） =====

export interface StatsBucket {
  // 按日：YYYY-MM-DD；按模型：model id
  key: string
  callsTotal: number
  callsOk: number
  callsErr: number
  inputTokensTotal: number
  outputTokensTotal: number
  lastCallAt: string | null
  lastSuccessAt: string | null
  lastErrorAt: string | null
  lastError: string | null
}

export interface CredentialStatsResponse {
  id: number
  callsTotal: number
  callsOk: number
  callsErr: number
  inputTokensTotal: number
  outputTokensTotal: number
  lastCallAt: string | null
  lastSuccessAt: string | null
  lastErrorAt: string | null
  lastError: string | null
  byDay: StatsBucket[]
  byModel: StatsBucket[]
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

export interface SetEndpointRequest {
  endpoint: string | null
}

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  kiroApiKey?: string
  authMethod?: 'social' | 'idc' | 'api_key'
  clientId?: string
  clientSecret?: string
  priority?: number
  /** Region（用于 Token 刷新及默认 API 请求），可被 apiRegion 单独覆盖 */
  region?: string
  /** 单独覆盖 API 请求使用的 region */
  apiRegion?: string
  machineId?: string
  endpoint?: string
  email?: string
  /** 是否自动从代理池为新凭据分配槽位（默认 true）；
   *  启用代理池且 false 时凭据导入后置 disabled，需手动 bind */
  autoBindProxy?: boolean
  /** 手动指定代理槽 ID（优先级最高） */
  proxySlotId?: string
}

// 添加凭据响应
export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

// ===== 账号信息（套餐/用量/邮箱等） =====

export interface CreditBonus {
  code: string
  name: string
  current: number
  limit: number
  expiresAt: string | null
}

export interface CreditsResourceDetail {
  displayName: string | null
  displayNamePlural: string | null
  resourceType: string | null
  currency: string | null
  unit: string | null
  overageRate: number | null
  overageCap: number | null
}

export interface CreditsUsageSummary {
  current: number
  limit: number
  baseCurrent: number
  baseLimit: number
  freeTrialCurrent: number
  freeTrialLimit: number
  freeTrialExpiry: string | null
  bonuses: CreditBonus[]
  nextResetDate: string | null
  overageEnabled: boolean | null
  resourceDetail: CreditsResourceDetail | null
}

export interface AccountSubscriptionDetails {
  rawType: string | null
  managementTarget: string | null
  upgradeCapability: string | null
  overageCapability: string | null
}

export interface ResourceUsageSummary {
  resourceType: string | null
  displayName: string | null
  unit: string | null
  currency: string | null
  current: number
  limit: number
}

export interface UsageAndLimitsResponse {
  userInfo: { email: string | null; userId: string | null } | null
  subscriptionInfo:
    | {
        type: string | null
        subscriptionTitle: string | null
        upgradeCapability: string | null
        overageCapability: string | null
        subscriptionManagementTarget: string | null
      }
    | null
  usageBreakdownList:
    | Array<{
        resourceType: string | null
        currentUsage: number | null
        currentUsageWithPrecision: number | null
        usageLimit: number | null
        usageLimitWithPrecision: number | null
        displayName: string | null
        displayNamePlural: string | null
        currency: string | null
        unit: string | null
        overageRate: number | null
        overageCap: number | null
        freeTrialInfo:
          | {
              usageLimit: number | null
              usageLimitWithPrecision: number | null
              currentUsage: number | null
              currentUsageWithPrecision: number | null
              freeTrialExpiry: string | null
              freeTrialStatus: string | null
            }
          | null
        bonuses:
          | Array<{
              bonusCode: string | null
              displayName: string | null
              usageLimit: number | null
              usageLimitWithPrecision: number | null
              currentUsage: number | null
              currentUsageWithPrecision: number | null
              status: string | null
              expiresAt: string | null
            }>
          | null
      }>
    | null
  nextDateReset: string | null
  overageConfiguration: { overageEnabled: boolean | null } | null
}

export interface AccountAggregateInfo {
  email: string | null
  userId: string | null
  idp: string | null
  status: string | null
  featureFlags: string[] | null
  subscriptionTitle: string | null
  subscriptionType: string
  subscription: AccountSubscriptionDetails
  usage: CreditsUsageSummary
  resources: ResourceUsageSummary[]
  rawUsage: UsageAndLimitsResponse
}

export interface CredentialAccountInfoResponse {
  id: number
  account: AccountAggregateInfo
}

// ============ 批量导入 token.json ============

// 官方 token.json 格式（用于解析导入）
export interface TokenJsonItem {
  provider?: string
  refreshToken?: string
  clientId?: string
  clientSecret?: string
  authMethod?: string
  priority?: number
  region?: string
  machineId?: string
  /** 邮箱（KAM v1.1+ account.email） */
  email?: string
  /** 内嵌代理（KAM v1.1+ account.proxy）；导入时会加入代理池并强制绑定 */
  proxy?: TokenJsonProxyItem
}

export interface TokenJsonProxyItem {
  url: string
  type?: string
  /** 接受 RFC3339 / YYYY-MM-DD / Unix 秒 / Unix 毫秒 */
  expires_at?: string | number
  expiresAt?: string | number
  label?: string
}

// 批量导入请求
export interface ImportTokenJsonRequest {
  dryRun?: boolean
  items: TokenJsonItem | TokenJsonItem[]
}

// 导入动作
export type ImportAction = 'added' | 'skipped' | 'invalid'

// 单项导入结果
export interface ImportItemResult {
  index: number
  fingerprint: string
  action: ImportAction
  reason?: string
  credentialId?: number
}

// 导入汇总
export interface ImportSummary {
  parsed: number
  added: number
  skipped: number
  invalid: number
}

// 批量导入响应
export interface ImportTokenJsonResponse {
  summary: ImportSummary
  items: ImportItemResult[]
}

// ============ 代理池 ============

export type ProxyStatus = 'active' | 'expiring' | 'expired' | 'full'

export interface ProxyEntryItem {
  id: string
  url: string
  username?: string | null
  expiresAt: string
  remainingSecs: number
  slots: number
  usedSlots: number
  boundCredentialIds: number[]
  status: ProxyStatus | string
  label?: string | null
  createdAt: string
  lastRotatedAt?: string | null
}

export interface ProxyListResponse {
  total: number
  proxies: ProxyEntryItem[]
  enabled: boolean
}

export interface ImportProxiesRequest {
  scheme: 'http' | 'https' | 'socks5' | 'socks5h'
  slotsPerProxy: number
  defaultExpiresAt: string
  lines: string[]
  label?: string
}

export interface ImportProxyItemResult {
  index: number
  line: string
  success: boolean
  proxyId?: string
  error?: string
}

export interface ImportProxiesResponse {
  total: number
  added: number
  failed: number
  items: ImportProxyItemResult[]
}

export interface BatchProxyDeleteRequest {
  ids: string[]
  force?: boolean
}

export interface BatchProxyUnbindRequest {
  ids: string[]
}

export interface BatchProxySlotsRequest {
  ids: string[]
  slots: number
  force?: boolean
}

export interface BatchProxyExtendRequest {
  ids: string[]
  expiresAt: string
}

export interface BatchProxyItemResult {
  id: string
  success: boolean
  error?: string
}

export interface BatchProxyResponse {
  total: number
  successCount: number
  failCount: number
  items: BatchProxyItemResult[]
}

export interface BindProxyRequest {
  proxyId: string
  /** 绑定后是否自动启用（默认 true） */
  autoEnable?: boolean
}

export interface ProxyTestResult {
  id: string
  ok: boolean
  elapsedMs: number
  ip?: string | null
  error?: string | null
}

export interface BatchProxyTestResponse {
  total: number
  okCount: number
  failCount: number
  results: ProxyTestResult[]
}

export interface ProxyAlertItem {
  at: string
  level: 'info' | 'warn' | 'error' | string
  message: string
}

export interface ProxyAlertsResponse {
  total: number
  alerts: ProxyAlertItem[]
}

// ============ 全局代理配置（保留兼容；UI 可不暴露）============

export interface ProxyConfigResponse {
  proxyUrl: string | null
  hasCredentials: boolean
}

export interface UpdateProxyConfigRequest {
  proxyUrl?: string | null
  proxyUsername?: string | null
  proxyPassword?: string | null
}

// ============ 全局配置 ============

export interface CompressionConfigResponse {
  enabled: boolean
  whitespaceCompression: boolean
  thinkingStrategy: string
  toolResultMaxChars: number
  toolResultHeadLines: number
  toolResultTailLines: number
  toolUseInputMaxChars: number
  toolDescriptionMaxChars: number
  maxHistoryTurns: number
  maxHistoryChars: number
  maxRequestBodyBytes: number
}

export interface GlobalConfigResponse {
  region: string
  credentialRpm: number | null
  promptCacheTtlSeconds: number
  promptCacheAccountingEnabled: boolean
  defaultEndpoint: string
  compression: CompressionConfigResponse
  autoDisablePatterns: string[]
  errorReplaceRules: string[]
  autoDisableUsageThresholdPct: number
  maxRetriesPerCredential: number
  maxTotalRetries: number
  allCredentialsCooldownBailThresholdSecs: number
  balanceAutoRefreshSecs: number
  errorLogEnabled: boolean
  errorLogMaxCount: number
  errorLogMaxAgeDays: number
  errorLogExcludedStatusCodes: number[]
}

export interface UpdateCompressionConfigRequest {
  enabled?: boolean
  whitespaceCompression?: boolean
  thinkingStrategy?: string
  toolResultMaxChars?: number
  toolResultHeadLines?: number
  toolResultTailLines?: number
  toolUseInputMaxChars?: number
  toolDescriptionMaxChars?: number
  maxHistoryTurns?: number
  maxHistoryChars?: number
  maxRequestBodyBytes?: number
}

export interface UpdateGlobalConfigRequest {
  region?: string
  credentialRpm?: number | null
  promptCacheTtlSeconds?: number
  promptCacheAccountingEnabled?: boolean
  defaultEndpoint?: string
  compression?: UpdateCompressionConfigRequest
  autoDisablePatterns?: string[]
  errorReplaceRules?: string[]
  autoDisableUsageThresholdPct?: number
  maxRetriesPerCredential?: number
  maxTotalRetries?: number
  allCredentialsCooldownBailThresholdSecs?: number
  balanceAutoRefreshSecs?: number
  errorLogEnabled?: boolean
  errorLogMaxCount?: number
  errorLogMaxAgeDays?: number
  errorLogExcludedStatusCodes?: number[]
}

// ===== 错误日志 =====

export interface ErrorLogSummaryItem {
  id: number
  at: string
  credentialId?: number | null
  endpoint?: string | null
  statusCode: number
  upstreamStatus?: number | null
  errorKind: string
  model?: string | null
  summary: string
}

export interface ErrorLogListResponse {
  total: number
  limit: number
  offset: number
  items: ErrorLogSummaryItem[]
}

export interface ErrorLogDetail extends ErrorLogSummaryItem {
  requestMethod?: string | null
  requestPath?: string | null
  requestHeaders?: string | null
  responseHeaders?: string | null
  requestBody?: string | null
  responseBody?: string | null
  userId?: string | null
  requestId?: string | null
}

export interface ListErrorLogsParams {
  statusCodes?: string  // CSV
  errorKinds?: string   // CSV
  credentialId?: number
  since?: string
  until?: string
  limit?: number
  offset?: number
}

export interface ClearErrorLogsRequest {
  before?: string
}

export interface ClearErrorLogsResponse {
  deleted: number
}

// ===== 对话测试 =====

export interface TestChatRequest {
  model: string
  message: string
  credentialId?: number
}

export interface TestChatResponse {
  credentialId: number
  text: string
  elapsedMs: number
  inputTokens: number
  outputTokens: number
}
