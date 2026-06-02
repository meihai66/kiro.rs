//! Admin API HTTP 处理器

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};

use serde::Deserialize;

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, BatchProxyDeleteRequest, BatchProxyExtendRequest,
        BatchProxySlotsRequest, BatchProxyUnbindRequest, BindProxyRequest, ClearErrorLogsRequest,
        CreateApiKeyRequest, ExportCredentialsRequest, ImportProxiesRequest,
        ImportTokenJsonRequest, ListErrorLogsQuery, SetAllowOveruseRequest, SetCredentialRpmRequest,
        SetDisabledRequest, SetEmailRequest, SetEndpointRequest, SetOveragePreferenceRequest,
        SetPriorityRequest, SetRegionRequest, SuccessResponse, UpdateApiKeyRequest,
        UpdateProxyConfigRequest,
    },
};

/// GET /api/admin/credentials
/// 获取所有凭据状态
pub async fn get_all_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_all_credentials();
    Json(response)
}

/// POST /api/admin/credentials/:id/disabled
/// 设置凭据禁用状态
pub async fn set_credential_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    match state.service.set_disabled(id, payload.disabled) {
        Ok(_) => {
            let action = if payload.disabled { "禁用" } else { "启用" };
            Json(SuccessResponse::new(format!("凭据 #{} 已{}", id, action))).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/priority
/// 设置凭据优先级
pub async fn set_credential_priority(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetPriorityRequest>,
) -> impl IntoResponse {
    match state.service.set_priority(id, payload.priority) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 优先级已设置为 {}",
            id, payload.priority
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/region
/// 设置凭据 Region
pub async fn set_credential_region(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetRegionRequest>,
) -> impl IntoResponse {
    match state
        .service
        .set_region(id, payload.region, payload.api_region)
    {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} Region 已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/endpoint
/// 设置凭据 endpoint
pub async fn set_credential_endpoint(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetEndpointRequest>,
) -> impl IntoResponse {
    match state.service.set_endpoint(id, payload.endpoint) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} endpoint 已更新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/email
/// 设置凭据邮箱
pub async fn set_credential_email(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetEmailRequest>,
) -> impl IntoResponse {
    match state.service.set_email(id, payload.email) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 邮箱已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/allow-overuse
/// 设置凭据「允许超额使用」开关（开启后即使额度用尽也不主动禁用）
pub async fn set_credential_allow_overuse(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetAllowOveruseRequest>,
) -> impl IntoResponse {
    match state.service.set_allow_overuse(id, payload.allow) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 允许超额使用已{}",
            id,
            if payload.allow { "开启" } else { "关闭" }
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/rpm
/// 设置单凭据 RPM 上限（None/0 表示清除覆盖，沿用全局 credentialRpm）
pub async fn set_credential_rpm(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialRpmRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_rpm(id, payload.rpm) {
        Ok(_) => {
            let msg = match payload.rpm.filter(|&v| v > 0) {
                Some(v) => format!("凭据 #{} 已设置 RPM 上限为 {}", id, v),
                None => format!("凭据 #{} 已清除 RPM 覆盖（沿用全局值）", id),
            };
            Json(SuccessResponse::new(msg)).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/reset
/// 重置失败计数并重新启用
pub async fn reset_failure_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_and_enable(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 失败计数已重置并重新启用",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/list-models
/// 调上游 ListAvailableModels，返回该凭据支持的模型列表
pub async fn list_credential_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.list_credential_models(id).await {
        Ok(v) => Json(serde_json::json!({
            "ok": true,
            "id": id,
            "data": v,
        }))
        .into_response(),
        Err(e) => {
            // 与上游错误显示一致：不返回 4xx，让前端 ok=false 自行展示
            Json(serde_json::json!({
                "ok": false,
                "id": id,
                "error": e.to_string(),
            }))
            .into_response()
        }
    }
}

/// POST /api/admin/credentials/:id/overage-preference
/// 开关上游账号的超额计费（setUserPreference）
pub async fn set_overage_preference(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetOveragePreferenceRequest>,
) -> impl IntoResponse {
    let resp = state
        .service
        .set_overage_preference(id, &payload.overage_status)
        .await;
    Json(resp).into_response()
}

/// POST /api/admin/credentials/:id/refresh
/// 强制刷新指定凭据 Token
pub async fn force_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.force_refresh_token(id).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} Token 已强制刷新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的余额
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_balance(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/balances/cached
/// 获取所有凭据的缓存余额
pub async fn get_cached_balances(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_cached_balances())
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RpmHistoryQuery {
    /// 回看小时数（默认 24，最多 168）
    pub hours: Option<i64>,
}

/// GET /api/admin/credentials/:id/rpm-history?hours=24
pub async fn get_rpm_history(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    axum::extract::Query(q): axum::extract::Query<RpmHistoryQuery>,
) -> impl IntoResponse {
    let hours = q.hours.unwrap_or(24);
    match state.service.rpm_history(id, hours) {
        Ok(points) => Json(serde_json::json!({
            "id": id,
            "hours": hours,
            "points": points.iter().map(|(ts, c)| serde_json::json!({"minuteTs": ts, "count": c})).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/stats/summary
/// 全局摘要：启动时间、运行时长、总请求 / 成功 / 失败
pub async fn get_stats_summary(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.stats_summary() {
        Ok(v) => Json(v).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/stats/reset
/// 清空 api_keys success_count/fail_count + 每凭据 success_count/rate_limit_count
pub async fn reset_all_stats(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.reset_all_stats() {
        Ok(n) => Json(SuccessResponse::new(format!(
            "已清空统计（重置了 {} 个 API Key 的累计计数）",
            n
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/test-chat
/// 用 admin 凭据触发一次最小对话测试，返回模型回复文本和耗时。
pub async fn test_chat(
    State(state): State<AdminState>,
    Json(req): Json<super::types::TestChatRequest>,
) -> impl IntoResponse {
    match state.service.test_chat(req).await {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/stats/rpm-history?hours=24
/// 所有凭据汇总的 RPM 历史（用于全局仪表盘）
pub async fn get_rpm_history_aggregate(
    State(state): State<AdminState>,
    axum::extract::Query(q): axum::extract::Query<RpmHistoryQuery>,
) -> impl IntoResponse {
    let hours = q.hours.unwrap_or(24);
    match state.service.rpm_history_aggregate(hours) {
        Ok(points) => Json(serde_json::json!({
            "hours": hours,
            "points": points.iter().map(|(ts, c)| serde_json::json!({"minuteTs": ts, "count": c})).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/stats/rpm-analysis?hours=24
/// 「最佳 RPM」分析：每个凭据的 RPM 分桶 + 429 率（推荐值由前端按可调阈值计算）
pub async fn get_rpm_analysis(
    State(state): State<AdminState>,
    axum::extract::Query(q): axum::extract::Query<RpmHistoryQuery>,
) -> impl IntoResponse {
    let hours = q.hours.unwrap_or(24);
    match state.service.rpm_analysis(hours) {
        Ok(entries) => Json(serde_json::json!({
            "hours": hours,
            "entries": entries,
        }))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials
/// 添加新凭据
pub async fn add_credential(
    State(state): State<AdminState>,
    Json(payload): Json<AddCredentialRequest>,
) -> impl IntoResponse {
    match state.service.add_credential(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/import-token-json
/// 批量导入 token.json
pub async fn import_token_json(
    State(state): State<AdminState>,
    Json(payload): Json<ImportTokenJsonRequest>,
) -> impl IntoResponse {
    let response = state.service.import_token_json(payload).await;
    Json(response)
}

/// POST /api/admin/credentials/batch/export
/// 按 ID 批量导出凭据为 token.json 兼容 JSON，可直接喂回 import-token-json
pub async fn export_credentials(
    State(state): State<AdminState>,
    Json(payload): Json<ExportCredentialsRequest>,
) -> impl IntoResponse {
    let response = state.service.export_credentials(payload);
    Json(response)
}

/// GET /proxy - 获取全局代理配置
pub async fn get_proxy_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_proxy_config())
}

/// POST /proxy - 更新全局代理配置
pub async fn update_proxy_config(
    State(state): State<AdminState>,
    Json(req): Json<UpdateProxyConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_proxy_config(req).await {
        Ok(_) => Json(SuccessResponse::new("全局代理配置已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/global - 获取全局配置
pub async fn get_global_config(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_global_config();
    Json(response)
}

/// PUT /api/admin/config/global - 更新全局配置
pub async fn update_global_config(
    State(state): State<AdminState>,
    Json(req): Json<super::types::UpdateGlobalConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_global_config(req).await {
        Ok(_) => Json(SuccessResponse::new("全局配置已更新")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

// =================== API Keys handlers ===================

pub async fn list_api_keys(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.list_api_keys() {
        Ok(r) => Json(r).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn create_api_key(
    State(state): State<AdminState>,
    Json(req): Json<CreateApiKeyRequest>,
) -> impl IntoResponse {
    match state.service.create_api_key(req) {
        Ok(item) => Json(item).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn update_api_key(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateApiKeyRequest>,
) -> impl IntoResponse {
    match state.service.update_api_key(id, req) {
        Ok(_) => Json(SuccessResponse::new(format!("API Key #{} 已更新", id)))
            .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn delete_api_key(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match state.service.delete_api_key(id) {
        Ok(_) => Json(SuccessResponse::new(format!("API Key #{} 已删除", id)))
            .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

// =================== 代理池 handlers ===================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteProxyQuery {
    #[serde(default)]
    pub force: bool,
}

/// GET /api/admin/proxies
pub async fn list_proxies(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.list_proxies())
}

/// POST /api/admin/proxies/import
pub async fn import_proxies(
    State(state): State<AdminState>,
    Json(req): Json<ImportProxiesRequest>,
) -> impl IntoResponse {
    match state.service.import_proxies(req) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/proxies/:id?force=true
pub async fn delete_proxy(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<DeleteProxyQuery>,
) -> impl IntoResponse {
    match state.service.delete_proxy(&id, q.force) {
        Ok(freed) => Json(serde_json::json!({
            "success": true,
            "message": format!("代理 {} 已删除", id),
            "freedCredentialIds": freed,
        }))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxies/batch/delete
pub async fn batch_delete_proxies(
    State(state): State<AdminState>,
    Json(req): Json<BatchProxyDeleteRequest>,
) -> impl IntoResponse {
    match state.service.batch_delete_proxies(req) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxies/batch/unbind
pub async fn batch_unbind_proxies(
    State(state): State<AdminState>,
    Json(req): Json<BatchProxyUnbindRequest>,
) -> impl IntoResponse {
    match state.service.batch_unbind_proxies(req) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxies/batch/slots
pub async fn batch_proxy_slots(
    State(state): State<AdminState>,
    Json(req): Json<BatchProxySlotsRequest>,
) -> impl IntoResponse {
    match state.service.batch_set_proxy_slots(req) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxies/batch/extend
pub async fn batch_proxy_extend(
    State(state): State<AdminState>,
    Json(req): Json<BatchProxyExtendRequest>,
) -> impl IntoResponse {
    match state.service.batch_extend_proxies(req) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchTestProxyRequest {
    pub ids: Vec<String>,
}

/// POST /api/admin/proxies/:id/test
pub async fn test_proxy(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.service.test_proxy(&id).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxies/batch/test
pub async fn batch_test_proxies(
    State(state): State<AdminState>,
    Json(req): Json<BatchTestProxyRequest>,
) -> impl IntoResponse {
    match state.service.batch_test_proxies(&req.ids).await {
        Ok(results) => {
            let total = results.len();
            let ok_count = results.iter().filter(|r| r.ok).count();
            Json(serde_json::json!({
                "total": total,
                "okCount": ok_count,
                "failCount": total - ok_count,
                "results": results,
            }))
            .into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxies/rotate
pub async fn rotate_proxies_now(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.rotate_proxies_now().await {
        Ok(_) => Json(SuccessResponse::new("已触发一次轮换扫描")).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/proxies/alerts
pub async fn list_proxy_alerts(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.list_proxy_alerts())
}

/// POST /api/admin/credentials/:id/bind-proxy
pub async fn bind_credential_proxy(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(req): Json<BindProxyRequest>,
) -> impl IntoResponse {
    match state.service.bind_proxy_to_credential(id, req) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 已绑定代理槽",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

// =================== Error logs handlers ===================

/// GET /api/admin/error-logs?statusCodes=429,502&errorKinds=rate_limit&credentialId=1&since=...&until=...&limit=50&offset=0
pub async fn list_error_logs(
    State(state): State<AdminState>,
    axum::extract::Query(q): axum::extract::Query<ListErrorLogsQuery>,
) -> impl IntoResponse {
    match state.service.list_error_logs(&q) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/error-logs/:id
pub async fn get_error_log(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match state.service.get_error_log(id) {
        Ok(detail) => Json(detail).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/error-logs/:id
pub async fn delete_error_log(
    State(state): State<AdminState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    match state.service.delete_error_log(id) {
        Ok(_) => Json(SuccessResponse::new(format!("错误日志 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/error-logs/clear
/// body: `{ "before": "2026-05-01T00:00:00Z" }` 或 `{}` 清空全部
pub async fn clear_error_logs(
    State(state): State<AdminState>,
    Json(req): Json<ClearErrorLogsRequest>,
) -> impl IntoResponse {
    match state.service.clear_error_logs(&req) {
        Ok(resp) => Json(resp).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/unbind-proxy
pub async fn unbind_credential_proxy(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.unbind_proxy_from_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 已解绑代理槽（已强制禁用）",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}
