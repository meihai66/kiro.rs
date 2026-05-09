//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};

use super::{
    handlers::{
        add_credential, batch_delete_proxies, batch_proxy_extend, batch_proxy_slots,
        batch_unbind_proxies, bind_credential_proxy, delete_credential, delete_proxy,
        batch_test_proxies, clear_error_logs, create_api_key, delete_api_key, delete_error_log,
        force_refresh_token, get_all_credentials, get_cached_balances, get_credential_balance,
        get_error_log, get_global_config, get_proxy_config, get_rpm_history,
        get_rpm_history_aggregate, get_stats_summary, import_proxies, import_token_json,
        list_api_keys, list_credential_models, list_error_logs, list_proxies, list_proxy_alerts,
        reset_failure_count, rotate_proxies_now,
        set_credential_allow_overuse, set_credential_disabled, set_credential_email,
        set_credential_endpoint, set_credential_priority, set_credential_region,
        set_credential_rpm, set_overage_preference, test_proxy, unbind_credential_proxy,
        update_api_key, update_global_config, update_proxy_config,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `POST /credentials/import-token-json` - 批量导入 token.json
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/:id/disabled` - 设置凭据禁用状态
/// - `POST /credentials/:id/priority` - 设置凭据优先级
/// - `POST /credentials/:id/reset` - 重置失败计数
/// - `GET /credentials/:id/balance` - 获取凭据余额
/// - `GET /credentials/balances/cached` - 获取所有凭据的缓存余额
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials/balances/cached", get(get_cached_balances))
        .route("/credentials/import-token-json", post(import_token_json))
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route("/credentials/{id}/region", post(set_credential_region))
        .route("/credentials/{id}/endpoint", post(set_credential_endpoint))
        .route("/credentials/{id}/email", post(set_credential_email))
        .route(
            "/credentials/{id}/allow-overuse",
            post(set_credential_allow_overuse),
        )
        .route("/credentials/{id}/rpm", post(set_credential_rpm))
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/credentials/{id}/rpm-history", get(get_rpm_history))
        .route("/stats/rpm-history", get(get_rpm_history_aggregate))
        .route("/stats/summary", get(get_stats_summary))
        .route(
            "/credentials/{id}/overage-preference",
            post(set_overage_preference),
        )
        .route(
            "/credentials/{id}/list-models",
            post(list_credential_models),
        )
        .route("/proxy", get(get_proxy_config).post(update_proxy_config))
        // 代理池
        .route("/proxies", get(list_proxies))
        .route("/proxies/import", post(import_proxies))
        .route("/proxies/alerts", get(list_proxy_alerts))
        .route("/proxies/rotate", post(rotate_proxies_now))
        .route("/proxies/batch/delete", post(batch_delete_proxies))
        .route("/proxies/batch/unbind", post(batch_unbind_proxies))
        .route("/proxies/batch/slots", post(batch_proxy_slots))
        .route("/proxies/batch/extend", post(batch_proxy_extend))
        .route("/proxies/batch/test", post(batch_test_proxies))
        .route("/proxies/{id}", delete(delete_proxy))
        .route("/proxies/{id}/test", post(test_proxy))
        .route(
            "/credentials/{id}/bind-proxy",
            post(bind_credential_proxy),
        )
        .route(
            "/credentials/{id}/unbind-proxy",
            post(unbind_credential_proxy),
        )
        .route(
            "/config/global",
            get(get_global_config).put(update_global_config),
        )
        // 错误日志
        .route("/error-logs", get(list_error_logs))
        .route("/error-logs/clear", post(clear_error_logs))
        .route(
            "/error-logs/{id}",
            get(get_error_log).delete(delete_error_log),
        )
        // API Keys
        .route("/api-keys", get(list_api_keys).post(create_api_key))
        .route(
            "/api-keys/{id}",
            axum::routing::patch(update_api_key).delete(delete_api_key),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}
