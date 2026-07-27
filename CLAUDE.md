# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 项目概述

kiro-rs 是一个用 Rust 编写的 Anthropic Claude API 兼容代理服务，将 Anthropic API 请求转换为 Kiro API 请求。支持多凭据管理、自动故障转移、流式响应和 Web 管理界面。

**技术栈**: Rust (Axum 0.8 + Tokio) + React 18 + TypeScript + Tailwind CSS

## 常用命令

```bash
# 使用 Makefile（推荐）
make help          # 查看所有可用命令
make release       # 构建前端 + 后端（release）
make dev           # 开发运行（自动构建前端，启用 sensitive-logs）
make check         # 格式化 + lint + 测试
make ui            # 仅构建前端
make ui-dev        # 前端开发服务器

# 手动构建（必须先构建前端）
cd admin-ui && pnpm install && pnpm build
cargo build --release

# 开发运行
cargo run -- -c config.json --credentials credentials.json

# 测试
cargo test
cargo test <test_name>           # 运行单个测试

# 代码检查
cargo fmt          # 格式化
cargo clippy       # lint

# 启用敏感日志构建（排障用，输出 token 用量等诊断信息）
cargo run --features sensitive-logs -- -c config.json --credentials credentials.json

# 前端开发
cd admin-ui
pnpm install
pnpm dev           # 开发服务器
pnpm build         # 生产构建

# 排障工具
python tools/test_400_improperly_formed.py  # 测试上游 400 错误场景
```

## 请求处理流程

```
POST /v1/messages (Anthropic 格式)
  → auth_middleware: 验证 x-api-key / Bearer token（subtle 常量时间比较）
  → post_messages handler:
      1. 判断 WebSearch 触发条件，决定本地处理或剔除后转发
      2. converter::convert_request() 转换为 Kiro 请求格式
      3. provider.call_api() 发送请求（含重试和故障转移）
      4. stream.rs 解析 AWS Event Stream → 转换为 Anthropic SSE 格式返回
```

## 核心设计模式

1. **Provider Pattern** - `kiro/provider.rs`: 统一的 API 提供者接口，处理请求转发和重试。支持凭据级代理（每个凭据可配独立 HTTP/SOCKS5 代理，缓存对应 HTTP Client 避免重复创建）
2. **Multi-Token Manager** - `kiro/token_manager.rs`: 多凭据管理，按优先级故障转移，后台异步刷新 Token（支持 Social 和 IdC 两种认证方式）。余额缓存动态 TTL：高频用户 10 分钟、低频用户 30 分钟、低余额用户 24 小时，过期时异步刷新不阻塞请求
3. **Protocol Converter** - `anthropic/converter.rs`: Anthropic ↔ Kiro 双向协议转换，包括模型映射（sonnet/opus/haiku → Kiro 模型 ID）、JSON Schema 规范化（修复 MCP 工具的 `required: null` / `properties: null`）、工具占位符生成、图片格式转换
4. **Event Stream Parser** - `kiro/parser/`: AWS Event Stream 二进制协议解析（header + payload + CRC32C 校验）
5. **Streaming Response** - `anthropic/stream.rs`: 使用 `StreamContext` 实时将 Kiro 事件转换为 Anthropic SSE，最终 usage 采用本地估算口径并透传上游 `meteringEvent` 诊断信息
6. **Input Compressor** - `anthropic/compressor.rs`: 多层压缩管道（空白压缩 → thinking 截断 → tool_result 截断 → tool_use input 截断 → 历史截断），自动修复 tool_use/tool_result 配对以避免上游 400 错误
7. **Image Processor** - `image.rs`: 图片处理（缩放、GIF 抽帧、token 计算）。GIF 抽帧策略：最多 20 帧、最多 5fps、按时长自适应采样间隔，输出为 JPEG 静态帧序列

## 共享状态

```rust
AppState {
    api_key: String,                          // Anthropic API 认证密钥
    kiro_provider: Option<Arc<KiroProvider>>,  // 核心 API 提供者（Arc 线程安全共享）
    profile_arn: Option<String>,               // AWS Profile ARN
    compression_config: CompressionConfig,     // 输入压缩配置
}
```

通过 Axum `State` extractor 注入到所有 handler 中。

## 凭据故障转移与冷却

- 凭据按 `priority` 字段排序，优先使用高优先级凭据
- 请求失败时 `report_failure()` 触发故障转移到下一个可用凭据
- 冷却分类管理：`FailureLimit` / `InsufficientBalance` / `ModelUnavailable` / `QuotaExceeded`
- `MODEL_TEMPORARILY_UNAVAILABLE` 触发全局熔断，禁用所有凭据

## 代理池故障切换

- 启用代理池时，同一代理连续 N 次网络层请求失败（未收到上游响应，`proxyFailureThreshold` 配置，默认 3，0=关闭）→ 该代理被标记不可用（类别 `network_failure`，持久化），其所绑凭据自动换绑到可用代理；池中无可用代理时凭据被禁用（`DisableReason::ProxyUnavailable`）
- 经代理成功收到上游响应（无论状态码）即清零该代理的连续失败计数
- 管理员可手动禁用代理（类别 `manual`，同样触发所绑凭据换绑），并可单个/批量重置不可用状态（`POST /proxies/{id}/set-disabled`、`POST /proxies/batch/reset-disabled`）

## API 端点

**代理端点**:
- `GET /v1/models` - 获取可用模型列表
- `POST /v1/messages` - 创建消息（Anthropic 格式）
- `POST /v1/messages/count_tokens` - Token 计数

**Admin API** (需配置 `adminApiKey`):
- 凭据 CRUD、状态监控、余额查询
- 自动上号：`GET|POST /key-poll/config`、`POST /key-poll/run`（`{"dryRun":true}` 试运行）、`GET /key-poll/logs`、`GET /key-poll/logs/{id}`、`POST /key-poll/logs/clear`、`GET /key-poll/onboard-logs`、`POST /key-poll/onboard-logs/clear`、`GET /key-poll/seen`、`DELETE /key-poll/seen/{hash}`、`POST /key-poll/seen/clear`

## 重要注意事项

1. **构建顺序**: 必须先构建前端 `admin-ui`，再编译 Rust 后端（静态文件通过 `rust-embed` 嵌入，derive 宏为 `#[derive(Embed)]`）
2. **凭据格式**: 支持单凭据（向后兼容）和多凭据（数组格式，支持 priority 字段）；`authMethod` 支持 `social` / `idc` / `api_key`（Kiro API Key 凭据：`kiroApiKey` 填 `ksk_*`，直接作 Bearer Token 免刷新，也可用环境变量 `KIRO_API_KEY` 注入）
3. **重试策略**: 单凭据最多重试 2 次，单请求最多重试 3 次
4. **WebSearch 工具**: 仅当请求明确触发 WebSearch（`tool_choice` 强制 / 仅提供 `web_search` 单工具 / 消息前缀匹配）时走本地 WebSearch；否则从 `tools` 中剔除 `web_search` 后转发上游（避免误路由）
5. **安全**: 使用 `subtle` 库进行常量时间比较防止时序攻击；Admin API Key 空字符串视为未配置
6. **Prefill 处理**: Claude 4.x 已弃用 assistant prefill，末尾 assistant 消息被静默丢弃
7. **sensitive-logs 特性**: 编译时 feature flag，启用后输出 token 用量诊断日志和请求体大小（默认关闭，仅用于排障）
8. **网络错误分类**: 连接关闭/重置、发送失败等网络错误被归类为瞬态上游错误，返回 502（不记录请求体）
9. **Rust edition**: 项目使用 Rust 2024 edition
10. **图片处理**: GIF 会被抽帧并重编码为 JPEG 静态帧序列（最多 20 帧、最多 5fps），以降低请求体大小并提升内容识别效果。图片缩放规则：长边超过 4000px 或总像素超过 400 万时等比缩放
11. **输入压缩**: 当请求体接近上游限制（约 5MB）时，自动执行多层压缩（空白压缩 → thinking 截断 → tool_result 截断 → tool_use input 截断 → 历史截断），并自动修复 tool_use/tool_result 配对以避免上游 400 错误
12. **上游 400 排障**: 若遇到 `Improperly formed request` 错误，参考 `docs/troubleshooting/400-improperly-formed-request.md` 和 `tools/test_400_improperly_formed.py` 进行诊断
13. **自动上号**: `src/key_poll.rs` 定时轮询发卡站接口（`config.keyPoll`），拉到本地没有的 `ksk_*` 就走 `import_token_json_with_options(force_enable=true)` 管线（验证 → 绑代理 → 直接启用，绕过 `importDisabledByDefault`）。后台任务常驻，每 10s tick 按当前配置判断是否到期，开关/间隔热更新无需重启。落三张表：`key_poll_logs`（每次轮询一条，含原始响应，留存 300）、`key_onboard_logs`（每个成功上号的 Key 一条，留存 1000，带 `died_at`/`death_reason` 记存活时长——死亡时刻取 error_logs 里**上号之后第一条**致命 `credential_disabled` 事件——取最后一条会被重复事件带偏，不加 since 会翻出 id 复用/上一轮的旧事件。注意 DB 只存 disabled 布尔不存原因，重启后所有禁用凭据的内存 disable_reason 都是 `Manual`，所以 Manual/None 必须靠事件兜底判定；快照为空时跳过判定避免误标全删）、`key_poll_seen`（去重表，只存 key 的 sha256）。去重规则：`onboarded` 是终态永久跳过（凭据被删也不重复上号，SQL upsert 里显式不许降级），`invalid` 按 `retryInvalidMax` 计次跳过，环境类失败（无可用代理槽/上游网络/限流，由 `ImportItemResult.retryable` 标记）记为 `retrying` 且不计次。代理槽不足时按 `reclaimDisabledProxies` 从**已禁用**凭据回收槽（`death_rank` 排序，启用中的凭据不动，冷却/禁用代理上的槽跳过）；前端页面 `admin-ui/src/pages/key-poll-page.tsx`
