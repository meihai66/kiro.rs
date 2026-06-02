mod admin;
mod admin_ui;
mod anthropic;
mod api_key_manager;
mod common;
mod http_client;
pub mod image;
mod kiro;
mod model;
mod storage;
pub mod token;

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use kiro::endpoint::{CliEndpoint, IdeEndpoint, KiroEndpoint};
use kiro::model::credentials::KiroCredentials;
use kiro::provider::KiroProvider;
use kiro::proxy_pool::ProxyPool;
use kiro::proxy_rotation;
use api_key_manager::ApiKeyManager;
use kiro::token_manager::MultiTokenManager;
use std::sync::OnceLock;

pub static SERVICE_STARTED_AT: OnceLock<chrono::DateTime<chrono::Utc>> = OnceLock::new();
use model::arg::Args;
use model::config::Config;
use parking_lot::RwLock;
use std::time::Duration;

#[tokio::main]
async fn main() {
    // 记录启动时间（统计页"运行时间"用）
    let _ = SERVICE_STARTED_AT.set(chrono::Utc::now());

    // 解析命令行参数
    let args = Args::parse();

    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 加载配置
    let config_path = args
        .config
        .unwrap_or_else(|| Config::default_config_path().to_string());
    let config = Config::load(&config_path).unwrap_or_else(|e| {
        tracing::error!("加载配置失败: {}", e);
        std::process::exit(1);
    });
    let config = Arc::new(RwLock::new(config));

    // ============ 打开 SQLite + 一次性 JSON 迁移 ============
    let db_path = args
        .db
        .clone()
        .or_else(|| config.read().db_path.clone())
        .unwrap_or_else(|| "kiro.db".to_string());
    let store = storage::Store::open(&db_path).unwrap_or_else(|e| {
        tracing::error!("打开 SQLite 数据库失败 ({}): {}", db_path, e);
        std::process::exit(1);
    });
    tracing::info!("SQLite 数据库就绪: {}", db_path);

    // 凭据/代理/余额 JSON 一次性迁移（仅 DB 为空时执行；导入后 .json → .json.migrated）
    let credentials_path = args
        .credentials
        .clone()
        .unwrap_or_else(|| KiroCredentials::default_credentials_path().to_string());
    let proxies_path_for_migration = args
        .proxies
        .clone()
        .or_else(|| config.read().proxy_pool_path.clone())
        .unwrap_or_else(|| "proxies.json".to_string());
    let balance_cache_path = std::path::Path::new(&credentials_path)
        .parent()
        .map(|d| d.join("kiro_balance_cache.json"));
    match storage::migration::migrate_json_if_needed(
        &store,
        Some(std::path::Path::new(&credentials_path)),
        Some(std::path::Path::new(&proxies_path_for_migration)),
        balance_cache_path.as_deref(),
    ) {
        Ok(report) => {
            if !report.skipped
                && (report.credentials_imported > 0
                    || report.proxies_imported > 0
                    || report.balances_imported > 0)
            {
                tracing::info!(
                    "JSON → SQLite 迁移完成：凭据 {} / 代理 {} / 余额 {}",
                    report.credentials_imported,
                    report.proxies_imported,
                    report.balances_imported
                );
            }
        }
        Err(e) => {
            tracing::error!("JSON → SQLite 迁移失败: {}", e);
            std::process::exit(1);
        }
    }

    // 始终从 SQLite 加载凭据（启用 sqlite 后 JSON 仅是迁移源）
    let mut credentials_list = store.list_credentials().unwrap_or_else(|e| {
        tracing::error!("从 SQLite 加载凭据失败: {}", e);
        std::process::exit(1);
    });
    let is_multiple_format = true; // SQLite 始终多凭据格式

    if let Ok(kiro_api_key) = std::env::var("KIRO_API_KEY")
        && !kiro_api_key.trim().is_empty()
    {
        tracing::info!("检测到 KIRO_API_KEY 环境变量，添加 API Key 凭据（最高优先级）");
        credentials_list.insert(
            0,
            KiroCredentials {
                kiro_api_key: Some(kiro_api_key),
                auth_method: Some("api_key".to_string()),
                priority: u32::MIN,
                runtime_only: true,
                ..Default::default()
            },
        );
    }

    tracing::info!("已加载 {} 个凭据配置", credentials_list.len());

    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    {
        let ide: Arc<dyn KiroEndpoint> = Arc::new(IdeEndpoint::new());
        endpoints.insert(ide.name().to_string(), ide);
        let cli: Arc<dyn KiroEndpoint> = Arc::new(CliEndpoint::new());
        endpoints.insert(cli.name().to_string(), cli);
    }
    let endpoint_names: Vec<String> = endpoints.keys().cloned().collect();

    let default_endpoint = config.read().default_endpoint.clone();
    if !endpoints.contains_key(&default_endpoint) {
        tracing::error!(
            "第一阶段仅支持已注册 endpoint，当前 defaultEndpoint={} 不受支持，已注册: {:?}",
            default_endpoint,
            endpoint_names
        );
        std::process::exit(1);
    }
    for cred in &credentials_list {
        let endpoint = cred.effective_endpoint_name(Some(&default_endpoint));
        if !endpoints.contains_key(endpoint) {
            tracing::error!(
                "第一阶段仅支持已注册 endpoint，凭据 id={:?} 指定了未支持 endpoint={}，已注册: {:?}",
                cred.id,
                endpoint,
                endpoint_names
            );
            std::process::exit(1);
        }
    }

    // ============ 代理池（启用时从 SQLite 加载）============
    let proxy_pool: Option<Arc<ProxyPool>> = if config.read().proxy_pool_enabled {
        match ProxyPool::from_store(store.clone()) {
            Ok(p) => {
                tracing::info!("代理池已启用，从 SQLite 加载 {} 个代理", p.len());
                Some(p)
            }
            Err(e) => {
                tracing::error!("加载代理池失败: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // 启用代理池时，对未绑代理槽的凭据强制 disabled，等管理员手动分配
    if proxy_pool.is_some() {
        let mut unbound_count = 0;
        for cred in credentials_list.iter_mut() {
            if cred.proxy_slot_id.is_none() && !cred.disabled {
                tracing::warn!(
                    cred_id = ?cred.id,
                    "代理池启用但凭据未绑代理槽，已强制 disabled"
                );
                cred.disabled = true;
                unbound_count += 1;
            }
        }
        if unbound_count > 0 {
            tracing::warn!(
                "{} 个凭据因未绑定代理槽被强制禁用，请通过 Admin API/UI 手动分配代理后再启用",
                unbound_count
            );
        }
    }

    // 获取第一个凭据用于日志显示
    let first_credentials = credentials_list.first().cloned().unwrap_or_default();
    #[cfg(feature = "sensitive-logs")]
    tracing::debug!("主凭证: {:?}", first_credentials);
    #[cfg(not(feature = "sensitive-logs"))]
    tracing::debug!(
        id = ?first_credentials.id,
        priority = first_credentials.priority,
        has_profile_arn = first_credentials.profile_arn.is_some(),
        has_expires_at = first_credentials.expires_at.is_some(),
        auth_method = ?first_credentials.auth_method.as_deref(),
        "主凭证摘要"
    );

    // 获取 API Key
    let api_key = config.read().api_key.clone().unwrap_or_else(|| {
        tracing::error!("配置文件中未设置 apiKey");
        std::process::exit(1);
    });

    // 构建代理配置
    let proxy_config = {
        let cfg = config.read();
        cfg.proxy_url.as_ref().map(|url| {
            let mut proxy = http_client::ProxyConfig::new(url);
            if let (Some(username), Some(password)) = (&cfg.proxy_username, &cfg.proxy_password) {
                proxy = proxy.with_auth(username, password);
            }
            proxy
        })
    };

    if proxy_config.is_some() {
        tracing::info!(
            "已配置 HTTP 代理: {}",
            config.read().proxy_url.as_ref().unwrap()
        );
    }

    // 创建 MultiTokenManager 和 KiroProvider
    let token_manager = MultiTokenManager::new(
        config.read().clone(),
        credentials_list,
        proxy_config.clone(),
        Some(credentials_path.into()),
        is_multiple_format,
    )
    .unwrap_or_else(|e| {
        tracing::error!("创建 Token 管理器失败: {}", e);
        std::process::exit(1);
    });
    let token_manager = Arc::new(token_manager);

    // 给 token_manager 注入 SQLite store（凭据写入走 SQL）
    token_manager.set_store(store.clone());

    // 注入代理池（启用时）并启动后台轮换任务
    if let Some(pool) = proxy_pool.as_ref() {
        token_manager.set_proxy_pool(pool.clone());
        let warning_hours = config.read().proxy_expiry_warning_hours;
        let interval_secs = config.read().proxy_rotation_interval_seconds.max(15);
        let _handle = proxy_rotation::start_rotation_task(
            pool.clone(),
            token_manager.clone(),
            warning_hours,
            Duration::from_secs(interval_secs),
        );
    }

    // 启动 RPM 历史采样任务：每分钟把当前 60s rpm 写入 SQLite
    {
        let store_for_rpm = store.clone();
        let tm_for_rpm = token_manager.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            ticker.tick().await; // 跳过首个 tick（避免 0 数据点）
            // 记录每个凭据上次采样时的累计 429 数，用于差分出「该分钟新增 429」
            let mut last_rl: std::collections::HashMap<u64, u32> = std::collections::HashMap::new();
            loop {
                ticker.tick().await;
                let snapshot = tm_for_rpm.snapshot();
                let minute_ts = chrono::Utc::now().timestamp() / 60;
                for entry in snapshot.entries {
                    let cur = entry.rate_limit_count;
                    // 首次见到该号 / 计数回退（重启或清空统计）→ 增量记 0
                    let delta = match last_rl.get(&entry.id) {
                        Some(&prev) => cur.saturating_sub(prev),
                        None => 0,
                    };
                    last_rl.insert(entry.id, cur);
                    if let Err(e) =
                        store_for_rpm.record_rpm(entry.id, minute_ts, entry.rpm, delta)
                    {
                        tracing::warn!(credential_id = entry.id, "RPM 历史写入失败: {}", e);
                        break;
                    }
                }
                // 保留 7 天
                let _ = store_for_rpm.purge_old_rpm(7);
            }
        });
    }

    // 错误日志后台清理：每小时按 (max_count, max_age_days) prune
    {
        let store_for_logs = store.clone();
        let cfg_for_logs = config.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            ticker.tick().await; // 跳过首个 tick（启动直接 prune 没必要）
            loop {
                ticker.tick().await;
                let (max_count, max_age_days) = {
                    let cfg = cfg_for_logs.read();
                    (cfg.error_log_max_count as u64, cfg.error_log_max_age_days)
                };
                if max_count == 0 && max_age_days == 0 {
                    continue;
                }
                match store_for_logs.prune_error_logs(max_count, max_age_days) {
                    Ok(n) if n > 0 => tracing::info!(deleted = n, "错误日志后台清理完成"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("错误日志清理失败: {}", e),
                }
            }
        });
    }

    // 余额缓存：启动期不再主动调上游 getUsageLimits（避免 N×500ms 串行拖慢启动）。
    // - SQLite balance_cache 已经在 AdminService::new 中恢复，前端查询直接命中
    // - token 仅在剩余有效期 < 10 分钟时刷新（持久化到 SQLite）
    // - 余额按需刷新：admin "查询余额" 按钮 / 凭据被实际调度时延迟刷
    tracing::info!(
        "启动期跳过余额预热（{} 个凭据），余额从 SQLite 缓存恢复，按需刷新",
        token_manager.total_count()
    );

    let kiro_provider = KiroProvider::with_proxy(
        token_manager.clone(),
        proxy_config.clone(),
        endpoints,
        default_endpoint.clone(),
    );
    let kiro_provider = Arc::new(kiro_provider);

    // 初始化 count_tokens 配置
    {
        let cfg = config.read();
        token::init_config(token::CountTokensConfig {
            api_url: cfg.count_tokens_api_url.clone(),
            api_key: cfg.count_tokens_api_key.clone(),
            auth_type: cfg.count_tokens_auth_type.clone(),
            proxy: proxy_config,
            tls_backend: cfg.tls_backend,
        });
    }

    // 创建共享的压缩配置（供 Anthropic 路由和 Admin API 共用，支持热更新）
    let compression_config = Arc::new(RwLock::new(config.read().compression.clone()));
    // 同步初始化 token 估算用的图片配置（与 compression_config 同源）
    crate::token::init_image_config(compression_config.read().clone());
    let prompt_cache_runtime = Arc::new(RwLock::new(anthropic::PromptCacheRuntime::new(
        config.read().prompt_cache_ttl_seconds,
        config.read().prompt_cache_accounting_enabled,
        config.read().prompt_cache_sim_scale_hit,
    )));

    // 启动期 seed：把 config.api_key 一次性导入 api_keys 表（如尚未存在）
    // 这样升级到 0.0.8 后旧 config.api_key 仍可用，但中间件不再走"双路径兜底"。
    if !api_key.trim().is_empty() {
        match store.list_api_keys() {
            Ok(existing) => {
                if !existing.iter().any(|r| r.key == api_key) {
                    match store.create_api_key(&storage::ApiKeyCreate {
                        key: api_key.clone(),
                        name: "config-default".to_string(),
                        description: Some(
                            "由 config.json 中 apiKey 自动 seed（升级路径）".to_string(),
                        ),
                        enabled: true,
                        max_concurrent: 0,
                        cache_read_min_pct: 0,
                        cache_read_max_pct: 0,
                    }) {
                        Ok(row) => {
                            tracing::info!(
                                "已 seed config.apiKey 到 api_keys 表（id={}, name={}）",
                                row.id,
                                row.name
                            );
                        }
                        Err(e) => {
                            tracing::warn!("seed config.apiKey 失败（可能已存在）: {}", e);
                        }
                    }
                }
            }
            Err(e) => tracing::warn!("查询 api_keys 表失败: {}", e),
        }
    }

    // 加载 API Key 管理器（从 SQLite）
    let api_key_manager = match ApiKeyManager::load(store.clone()) {
        Ok(m) => {
            tracing::info!("API Key 管理器已加载");
            Some(m)
        }
        Err(e) => {
            tracing::error!("加载 API Key 管理器失败: {}", e);
            std::process::exit(1);
        }
    };

    // 构建 Anthropic API 路由（从第一个凭据获取 profile_arn）
    let anthropic_app = anthropic::create_router_with_provider(
        &api_key,
        Some(kiro_provider.clone()),
        first_credentials.profile_arn.clone(),
        compression_config.clone(),
        prompt_cache_runtime.clone(),
        api_key_manager.clone(),
        Some(store.clone()),
        Some(config.clone()),
    );

    // 构建 Admin API 路由（如果配置了非空的 admin_api_key）
    // 安全检查：空字符串被视为未配置，防止空 key 绕过认证
    let admin_key_valid = config
        .read()
        .admin_api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);

    let app = {
        let cfg = config.read();
        if let Some(admin_key) = &cfg.admin_api_key {
            if admin_key.trim().is_empty() {
                tracing::warn!("admin_api_key 配置为空，Admin API 未启用");
                anthropic_app
            } else {
                let mut admin_service = admin::AdminService::new(
                    token_manager.clone(),
                    Some(kiro_provider.clone()),
                    config.clone(),
                    compression_config.clone(),
                    prompt_cache_runtime.clone(),
                    endpoint_names.clone(),
                );
                if let Some(pool) = proxy_pool.as_ref() {
                    admin_service = admin_service.with_proxy_pool(pool.clone());
                }
                admin_service = admin_service.with_store(store.clone());
                if let Some(mgr) = api_key_manager.clone() {
                    admin_service = admin_service.with_api_key_manager(mgr);
                }
                let admin_service = Arc::new(admin_service);

                // 余额自动刷新后台任务
                {
                    let svc = admin_service.clone();
                    let tm = token_manager.clone();
                    let cfg = config.clone();
                    tokio::spawn(async move {
                        // 30s 一个 tick；每个 tick 找出"年龄 > target"的凭据，
                        // 顺序刷新最多 batch_max 个，每次间隔 0.3s 避免上游瞬时峰值。
                        let mut ticker = tokio::time::interval(Duration::from_secs(30));
                        ticker.tick().await; // 跳过首个 tick（启动时让其他任务先 settle）
                        loop {
                            ticker.tick().await;
                            let target_secs = cfg.read().balance_auto_refresh_secs as u64;
                            if target_secs == 0 {
                                continue; // 关闭则什么都不做
                            }
                            // 候选：未禁用、不在冷却、年龄 >= target
                            let snapshot = tm.snapshot();
                            let mut candidates: Vec<(u64, u64)> = snapshot
                                .entries
                                .into_iter()
                                .filter(|e| !e.disabled && e.cooldown_remaining_secs.is_none())
                                .filter_map(|e| {
                                    let age = svc.balance_cache_age_secs(e.id).unwrap_or(u64::MAX);
                                    if age >= target_secs {
                                        Some((e.id, age))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            // 最旧的先刷
                            candidates.sort_by(|a, b| b.1.cmp(&a.1));
                            // 每 tick 最多刷 ceil(N * 30 / target)，避免长时间持续打上游
                            let n_total = candidates.len();
                            if n_total == 0 {
                                continue;
                            }
                            let batch_max = ((n_total as u64 * 30 / target_secs).max(1)
                                .min(n_total as u64))
                                as usize;
                            let concurrency = (cfg.read().balance_refresh_concurrency.max(1)
                                as usize)
                                .min(256);
                            tracing::debug!(
                                target_secs,
                                candidates = n_total,
                                batch_max,
                                concurrency,
                                "余额自动刷新 tick"
                            );
                            // 有界并发刷新：每个凭据走各自代理出口，并发不会撞同一 IP 的上游限流。
                            use futures::stream::StreamExt;
                            let ids: Vec<u64> = candidates
                                .into_iter()
                                .take(batch_max)
                                .map(|(id, _)| id)
                                .collect();
                            futures::stream::iter(ids)
                                .for_each_concurrent(concurrency, |id| {
                                    let svc = svc.clone();
                                    async move {
                                        if let Err(e) = svc.refresh_balance(id).await {
                                            tracing::debug!(
                                                credential_id = id,
                                                "余额自动刷新失败（忽略，后续 tick 重试）: {}",
                                                e
                                            );
                                        }
                                    }
                                })
                                .await;
                        }
                    });
                }

                let admin_state = admin::AdminState::from_arc(admin_key, admin_service);
                let admin_app = admin::create_admin_router(admin_state);

                // 创建 Admin UI 路由
                let admin_ui_app = admin_ui::create_admin_ui_router();

                tracing::info!("Admin API 已启用");
                tracing::info!("Admin UI 已启用: /admin");
                anthropic_app
                    .nest("/api/admin", admin_app)
                    .nest("/admin", admin_ui_app)
            }
        } else {
            anthropic_app
        }
    };

    // 启动服务器
    let addr = {
        let cfg = config.read();
        format!("{}:{}", cfg.host, cfg.port)
    };
    tracing::info!("启动 Anthropic API 端点: {}", addr);
    #[cfg(feature = "sensitive-logs")]
    tracing::debug!("API Key: {}***", &api_key[..(api_key.len() / 2)]);
    #[cfg(not(feature = "sensitive-logs"))]
    tracing::info!(
        "API Key: ***{} (长度: {})",
        &api_key[api_key.len().saturating_sub(4)..],
        api_key.len()
    );
    tracing::info!("可用 API:");
    tracing::info!("  GET  /v1/models");
    tracing::info!("  POST /v1/messages");
    tracing::info!("  POST /v1/messages/count_tokens");
    if admin_key_valid {
        tracing::info!("Admin API:");
        tracing::info!("  GET  /api/admin/credentials");
        tracing::info!("  POST /api/admin/credentials/:index/disabled");
        tracing::info!("  POST /api/admin/credentials/:index/priority");
        tracing::info!("  POST /api/admin/credentials/:index/reset");
        tracing::info!("  GET  /api/admin/credentials/:index/balance");
        tracing::info!("Admin UI:");
        tracing::info!("  GET  /admin");
    }

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            tracing::error!("绑定监听地址失败 ({}): {}", addr, e);
            std::process::exit(1);
        });
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("HTTP 服务异常退出: {}", e);
        std::process::exit(1);
    }
}
