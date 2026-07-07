//! Kiro IDE 端点实现

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{
    KiroEndpoint, ListModelsParts, RequestContext, SetUserPreferenceParts, UsageRequestParts,
};
use crate::kiro::model::credentials::KiroCredentials;

pub const IDE_ENDPOINT_NAME: &str = "ide";

pub struct IdeEndpoint;

impl IdeEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn host(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "q.{}.amazonaws.com",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
            ctx.config.kiro_version, ctx.machine_id
        )
    }

    fn user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-{}",
            ctx.config.system_version,
            ctx.config.node_version,
            ctx.config.kiro_version,
            ctx.machine_id
        )
    }

    fn is_aws_sso_oidc_credentials(credentials: &KiroCredentials) -> bool {
        let auth_method = credentials.auth_method.as_deref();
        matches!(auth_method, Some("builder-id") | Some("idc"))
            || (credentials.client_id.is_some() && credentials.client_secret.is_some())
    }

    fn mcp_profile_arn_header_value(credentials: &KiroCredentials) -> Option<&str> {
        if Self::is_aws_sso_oidc_credentials(credentials) {
            return None;
        }

        credentials.profile_arn.as_deref()
    }

    fn inject_profile_arn(
        request_body: &str,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<String> {
        // 有 profileArn 就注入请求体，没有则移除。
        // 关键：Enterprise IdC / Q Developer 的数据面 generateAssistantResponse 必须带 profileArn，
        // 否则上游返回 403「User is not authorized to make this call.」。此前对所有 IdC/SSO 凭据
        // 一律剔除 profileArn（假设走默认 profile），对企业账号是错的。profileArn 由
        // token_manager 在 try_ensure_token 阶段通过 ListAvailableProfiles 解析并写回凭据。
        let profile_arn = credentials
            .profile_arn
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut request: serde_json::Value = serde_json::from_str(request_body)?;
        let obj = request
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("request body is not a JSON object"))?;
        match profile_arn {
            Some(arn) => {
                obj.insert(
                    "profileArn".to_string(),
                    serde_json::Value::String(arn.to_string()),
                );
            }
            None => {
                obj.remove("profileArn");
            }
        }
        Ok(serde_json::to_string(&request)?)
    }
}

impl Default for IdeEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for IdeEndpoint {
    fn name(&self) -> &'static str {
        IDE_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/generateAssistantResponse",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "https://q.{}.amazonaws.com/mcp",
            ctx.credentials.effective_api_region(ctx.config)
        )
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amzn-kiro-agent-mode", "vibe")
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
            // 真实 Kiro IDE 数据面请求带 Accept: */*（reqwest 默认不加），对齐客户端指纹
            .header("Accept", "*/*")
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        req
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(profile_arn) = Self::mcp_profile_arn_header_value(ctx.credentials) {
            req = req.header("x-amzn-kiro-profile-arn", profile_arn);
        }
        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        req
    }

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> anyhow::Result<String> {
        Self::inject_profile_arn(body, ctx.credentials)
    }

    fn usage_request_parts(&self, ctx: &RequestContext<'_>) -> anyhow::Result<UsageRequestParts> {
        let host = self.host(ctx);
        let mut url = format!(
            "https://{}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST&isEmailRequired=true",
            host
        );
        // getUsageLimits 的 profileArn 走 URL query（不是 x-amzn-kiro-profile-arn 头），Enterprise/
        // Q Developer 账号需要它，IdC/SSO 也应带（参考 Kiro-Go）。用凭据自身的 profileArn，不用
        // mcp_profile_arn_header_value（后者对 SSO 返回 None——那是给 MCP 头抑制用的）。
        if let Some(profile_arn) = ctx
            .credentials
            .profile_arn
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            url.push_str(&format!("&profileArn={}", urlencoding::encode(profile_arn)));
        }

        let mut headers = vec![
            (
                "x-amz-user-agent",
                format!(
                    "aws-sdk-js/1.0.0 KiroIDE-{}-{}",
                    ctx.config.kiro_version, ctx.machine_id
                ),
            ),
            (
                "user-agent",
                format!(
                    "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
                    ctx.config.system_version,
                    ctx.config.node_version,
                    ctx.config.kiro_version,
                    ctx.machine_id
                ),
            ),
            ("host", host),
            ("amz-sdk-invocation-id", Uuid::new_v4().to_string()),
            ("amz-sdk-request", "attempt=1; max=1".to_string()),
            ("Authorization", format!("Bearer {}", ctx.token)),
            ("Connection", "close".to_string()),
        ];

        if ctx.credentials.is_api_key_credential() {
            headers.push(("tokentype", "API_KEY".to_string()));
        }

        Ok(UsageRequestParts { url, headers })
    }

    fn set_user_preference_parts(
        &self,
        ctx: &RequestContext<'_>,
    ) -> anyhow::Result<SetUserPreferenceParts> {
        let host = self.host(ctx);
        let url = format!("https://{}/setUserPreference", host);

        // setUserPreference 必填 profileArn（与 MCP 头逻辑无关：IdC 凭据也要传）
        let profile_arn = ctx
            .credentials
            .profile_arn
            .as_deref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "setUserPreference 需要 profileArn，但当前凭据未保存 profileArn。\
                     请先点击「刷 Token」（刷新 Token 时会从上游同步 profileArn）"
                )
            })?
            .to_string();

        let headers = vec![
            ("content-type", "application/json".to_string()),
            ("x-amz-user-agent", self.x_amz_user_agent(ctx)),
            ("user-agent", self.user_agent(ctx)),
            ("host", host),
            ("amz-sdk-invocation-id", Uuid::new_v4().to_string()),
            ("amz-sdk-request", "attempt=1; max=1".to_string()),
            ("Authorization", format!("Bearer {}", ctx.token)),
            ("Connection", "close".to_string()),
        ];

        Ok(SetUserPreferenceParts {
            url,
            headers,
            profile_arn,
        })
    }

    fn list_models_parts(&self, ctx: &RequestContext<'_>) -> anyhow::Result<ListModelsParts> {
        let host = self.host(ctx);
        // maxResults=50 对齐真实客户端，避免只拿到默认页大小的模型列表
        let mut url = format!(
            "https://{}/ListAvailableModels?origin=AI_EDITOR&maxResults=50",
            host
        );
        if let Some(profile_arn) = Self::mcp_profile_arn_header_value(ctx.credentials) {
            url.push_str(&format!("&profileArn={}", urlencoding::encode(profile_arn)));
        }

        let mut headers = vec![
            ("x-amz-user-agent", self.x_amz_user_agent(ctx)),
            ("user-agent", self.user_agent(ctx)),
            ("host", host),
            ("x-amzn-codewhisperer-optout", "true".to_string()),
            ("amz-sdk-invocation-id", Uuid::new_v4().to_string()),
            ("amz-sdk-request", "attempt=1; max=1".to_string()),
            ("Authorization", format!("Bearer {}", ctx.token)),
            ("Connection", "close".to_string()),
        ];
        if ctx.credentials.is_api_key_credential() {
            headers.push(("tokentype", "API_KEY".to_string()));
        }
        Ok(ListModelsParts { url, headers })
    }
}
