use super::*;

use std::collections::BTreeMap;

use std::env;

use std::time::Duration;

use tokio::time::sleep;

use tokio::process::Command;

use code_agent_core::{ContentBlock, Message, MessageRole};

use async_trait::async_trait;

use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};

use serde_json::{json, Value};

use anyhow::{anyhow, Result};

use time::OffsetDateTime;

use hmac::Mac;

use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct HttpProvider {
    provider: ApiProvider,
    auth: AuthMaterial,
    client: reqwest::Client,
    base_url: String,
}

impl HttpProvider {
    pub fn new(provider: ApiProvider, auth: AuthMaterial) -> Self {
        let base_url = provider_base_url(provider);
        Self {
            provider,
            auth,
            client: reqwest::Client::new(),
            base_url,
        }
    }

    pub fn with_base_url(provider: ApiProvider, auth: AuthMaterial, base_url: String) -> Self {
        Self {
            provider,
            auth,
            client: reqwest::Client::new(),
            base_url,
        }
    }

    async fn start_anthropic_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let url = join_api_path(&self.base_url, "messages", "v1");
        let mut payload = json!({
            "model": request.model,
            "stream": false,
            "max_tokens": request.max_output_tokens.unwrap_or(4_096),
            "messages": anthropic_messages(&request.messages),
        });

        if let Some(system) = anthropic_system_prompt(&request.messages) {
            payload["system"] = Value::String(system);
        }
        // Wire Claude thinking config into the API payload.
        match &request.thinking {
            ThinkingConfig::Adaptive => {
                payload["thinking"] = json!({ "type": "adaptive" });
            }
            ThinkingConfig::Enabled { budget_tokens } => {
                payload["thinking"] = json!({
                    "type": "enabled",
                    "budget_tokens": budget_tokens,
                });
            }
            ThinkingConfig::Disabled => {}
        }
        if !request.tools.is_empty() {
            payload["tools"] = Value::Array(
                request
                    .tools
                    .iter()
                    .map(|tool| {
                        json!({
                            "name": tool.name,
                            "description": tool.description,
                            "input_schema": tool.input_schema,
                        })
                    })
                    .collect(),
            );
        }

        let response = self
            .client
            .post(url)
            .headers(self.anthropic_headers(&request.extra_headers)?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "anthropic request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    async fn start_openai_responses_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        if matches!(self.provider, ApiProvider::OpenAICompatible)
            && openai_compatible_uses_chat_completions(&self.base_url)
        {
            let url = join_if_missing(&self.base_url, "chat/completions");
            return self
                .start_openai_chat_completions_stream(url, request)
                .await;
        }
        let url = join_if_missing(&self.base_url, "responses");
        self.send_openai_responses_request(url, request, true, true)
            .await
    }

    async fn start_openai_chat_completions_stream(
        &self,
        url: String,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let payload = build_openai_chat_completions_payload(&request);
        let response = self
            .post_openai_json_with_retry(&url, &request.extra_headers, &payload)
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "{} request failed with status {}: {}",
                openai_request_failure_label(self.provider),
                status,
                compact_error_body(&body)
            ));
        }

        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_openai_chat_response(&value)?,
        )))
    }

    async fn start_chatgpt_codex_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let url = join_if_missing(&self.base_url, "codex/responses");
        let mut request = request;
        let mut supports_reasoning_summaries = false;
        let mut supports_verbosity = false;

        if let Some(cache) = read_chatgpt_codex_models_cache() {
            if let Some(etag) = cache.etag {
                request
                    .extra_headers
                    .entry("x-models-etag".to_owned())
                    .or_insert(etag);
            }

            if let Some(model) = cache
                .models
                .iter()
                .find(|entry| entry.slug == request.model)
            {
                supports_reasoning_summaries = model.supports_reasoning_summaries;
                supports_verbosity = model.support_verbosity;
                if !model.supported_in_api {
                    return Err(anyhow!(
                        "ChatGPT Codex model '{}' is not available through the Codex API",
                        request.model
                    ));
                }
            }
        }

        self.send_openai_responses_request(
            url,
            request,
            supports_reasoning_summaries,
            supports_verbosity,
        )
        .await
    }

    async fn send_openai_responses_request(
        &self,
        url: String,
        request: ProviderRequest,
        supports_reasoning_summaries: bool,
        supports_verbosity: bool,
    ) -> Result<Box<dyn ProviderStream>> {
        let payload = build_openai_responses_payload(
            &request,
            supports_reasoning_summaries,
            supports_verbosity,
        );

        let response = self
            .post_openai_json_with_retry(&url, &request.extra_headers, &payload)
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await?;
            return Err(anyhow!(
                "{} request failed with status {}: {}",
                openai_request_failure_label(self.provider),
                status,
                compact_error_body(&body)
            ));
        }

        Ok(Box::new(OpenAIResponsesSseStream::new(response)))
    }

    async fn post_openai_json_with_retry(
        &self,
        url: &str,
        extra_headers: &BTreeMap<String, String>,
        payload: &Value,
    ) -> Result<reqwest::Response> {
        let headers = self.openai_headers(extra_headers)?;
        let max_retries = openai_responses_max_retries();
        let mut attempt = 0usize;

        loop {
            attempt += 1;
            match self
                .client
                .post(url)
                .headers(headers.clone())
                .json(payload)
                .send()
                .await
            {
                Ok(response) => {
                    let status = response.status();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned);

                    if should_retry_openai_responses_status(status) && attempt <= max_retries {
                        let _ = response.bytes().await;
                        sleep(openai_responses_retry_delay(
                            attempt,
                            retry_after.as_deref(),
                        ))
                        .await;
                        continue;
                    }

                    return Ok(response);
                }
                Err(error) => {
                    if should_retry_openai_send_error(&error) && attempt <= max_retries {
                        sleep(openai_responses_retry_delay(attempt, None)).await;
                        continue;
                    }

                    return Err(anyhow!(
                        "{} request failed while sending request to {} after {} attempt{}: {}",
                        openai_request_failure_label(self.provider),
                        url,
                        attempt,
                        if attempt == 1 { "" } else { "s" },
                        error
                    ));
                }
            }
        }
    }

    async fn start_bedrock_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let model = resolve_provider_model(ApiProvider::Bedrock, &request.model);
        let mut payload = build_anthropic_payload(
            &request,
            Some((
                "anthropic_version",
                Value::String("bedrock-2023-05-31".to_owned()),
            )),
        );
        payload.as_object_mut().map(|object| object.remove("model"));
        let body = serde_json::to_vec(&payload)?;
        let url = bedrock_invoke_url(&self.base_url, &model);
        let headers = self
            .bedrock_headers(&url, &body, &request.extra_headers)
            .await?;

        let response = self
            .client
            .post(url)
            .headers(headers)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "bedrock request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    async fn start_vertex_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let model = resolve_provider_model(ApiProvider::Vertex, &request.model);
        let url = vertex_predict_url(&self.base_url, &model)?;
        let mut payload = build_anthropic_payload(
            &request,
            Some((
                "anthropic_version",
                Value::String("vertex-2023-10-16".to_owned()),
            )),
        );
        payload.as_object_mut().map(|object| object.remove("model"));

        let response = self
            .client
            .post(url)
            .headers(self.vertex_headers(&request.extra_headers).await?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "vertex request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    async fn start_foundry_stream(
        &self,
        request: ProviderRequest,
    ) -> Result<Box<dyn ProviderStream>> {
        let model = resolve_provider_model(ApiProvider::Foundry, &request.model);
        let url = foundry_messages_url(&self.base_url)?;
        let mut request = request;
        request.model = model;
        let payload = build_anthropic_payload(
            &request,
            Some(("anthropic_version", Value::String("2023-06-01".to_owned()))),
        );

        let response = self
            .client
            .post(url)
            .headers(self.foundry_headers(&request.extra_headers).await?)
            .json(&payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(anyhow!(
                "foundry request failed with status {}: {}",
                status,
                compact_error_body(&body)
            ));
        }
        let value: Value = serde_json::from_str(&body)?;
        Ok(Box::new(StaticProviderStream::new(
            events_from_anthropic_response(&value)?,
        )))
    }

    fn anthropic_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        if let Some(api_key) = self.auth.api_key.as_deref() {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(api_key)?,
            );
        } else if let Some(token) = self.auth.bearer_token.as_deref() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        } else {
            return Err(anyhow!("missing Anthropic auth material"));
        }

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    async fn bedrock_headers(
        &self,
        url: &str,
        payload: &[u8],
        extra_headers: &BTreeMap<String, String>,
    ) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        if let Some(token) = self
            .auth
            .bearer_token
            .clone()
            .or_else(|| env_value(["AWS_BEARER_TOKEN_BEDROCK"]))
        {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
            insert_extra_headers(&mut headers, extra_headers)?;
            return Ok(headers);
        }

        if env_flag("CLAUDE_CODE_SKIP_BEDROCK_AUTH") {
            insert_extra_headers(&mut headers, extra_headers)?;
            return Ok(headers);
        }

        let access_key = env_value(["AWS_ACCESS_KEY_ID"])
            .ok_or_else(|| anyhow!("missing AWS_ACCESS_KEY_ID for Bedrock runtime"))?;
        let secret_key = env_value(["AWS_SECRET_ACCESS_KEY"])
            .ok_or_else(|| anyhow!("missing AWS_SECRET_ACCESS_KEY for Bedrock runtime"))?;
        let session_token = env_value(["AWS_SESSION_TOKEN"]);
        let region = bedrock_region();
        let url = reqwest::Url::parse(url)?;
        let host = url
            .host_str()
            .ok_or_else(|| anyhow!("bedrock base URL is missing a host"))?;
        let path = url.path();
        let signed = sign_bedrock_request(
            "POST",
            host,
            path,
            payload,
            &region,
            &access_key,
            &secret_key,
            session_token.as_deref(),
            OffsetDateTime::now_utc(),
        )?;

        headers.insert(
            HeaderName::from_static("x-amz-date"),
            HeaderValue::from_str(&signed.amz_date)?,
        );
        headers.insert(
            HeaderName::from_static("x-amz-content-sha256"),
            HeaderValue::from_str(&signed.payload_sha256)?,
        );
        if let Some(token) = signed.session_token {
            headers.insert(
                HeaderName::from_static("x-amz-security-token"),
                HeaderValue::from_str(&token)?,
            );
        }
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&signed.authorization)?);
        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    async fn vertex_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        if !env_flag("CLAUDE_CODE_SKIP_VERTEX_AUTH") {
            let token = self
                .auth
                .bearer_token
                .clone()
                .or_else(|| env_value(["VERTEX_ACCESS_TOKEN", "GOOGLE_OAUTH_ACCESS_TOKEN"]))
                .or(get_command_stdout(&[
                    ("gcloud", &["auth", "application-default", "print-access-token"][..]),
                    ("gcloud", &["auth", "print-access-token"][..]),
                ])
                .await)
                .ok_or_else(|| anyhow!("missing Vertex bearer token; set VERTEX_ACCESS_TOKEN or sign in with gcloud"))?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    async fn foundry_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );

        if let Some(api_key) = self
            .auth
            .api_key
            .clone()
            .or_else(|| env_value(["ANTHROPIC_FOUNDRY_API_KEY", "AZURE_API_KEY"]))
        {
            headers.insert(
                HeaderName::from_static("x-api-key"),
                HeaderValue::from_str(&api_key)?,
            );
        } else if !env_flag("CLAUDE_CODE_SKIP_FOUNDRY_AUTH") {
            let token = self
                .auth
                .bearer_token
                .clone()
                .or_else(|| env_value(["AZURE_AUTH_TOKEN", "FOUNDRY_AUTH_TOKEN"]))
                .or(get_command_stdout(&[
                    (
                        "az",
                        &[
                            "account",
                            "get-access-token",
                            "--resource",
                            "https://ai.azure.com",
                            "--query",
                            "accessToken",
                            "-o",
                            "tsv",
                        ][..],
                    ),
                ])
                .await)
                .ok_or_else(|| {
                    anyhow!(
                        "missing Foundry auth; set ANTHROPIC_FOUNDRY_API_KEY/AZURE_API_KEY or sign in with az"
                    )
                })?;
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
        }

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }

    fn openai_headers(&self, extra_headers: &BTreeMap<String, String>) -> Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("code-agent-rust/", env!("CARGO_PKG_VERSION"))),
        );

        let bearer = self
            .auth
            .bearer_token
            .as_deref()
            .or(self.auth.api_key.as_deref())
            .ok_or_else(|| anyhow!("missing OpenAI auth material"))?;
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {bearer}"))?,
        );

        insert_extra_headers(&mut headers, extra_headers)?;
        Ok(headers)
    }
}

#[async_trait]
impl Provider for HttpProvider {
    fn api_provider(&self) -> ApiProvider {
        self.provider
    }

    async fn start_stream(&self, request: ProviderRequest) -> Result<Box<dyn ProviderStream>> {
        match self.provider {
            ApiProvider::FirstParty => self.start_anthropic_stream(request).await,
            ApiProvider::OpenAI | ApiProvider::OpenAICompatible => {
                self.start_openai_responses_stream(request).await
            }
            ApiProvider::ChatGPTCodex => self.start_chatgpt_codex_stream(request).await,
            ApiProvider::Bedrock => self.start_bedrock_stream(request).await,
            ApiProvider::Vertex => self.start_vertex_stream(request).await,
            ApiProvider::Foundry => self.start_foundry_stream(request).await,
        }
    }
}

pub fn provider_base_url(provider: ApiProvider) -> String {
    match provider {
        ApiProvider::FirstParty => env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_owned()),
        ApiProvider::OpenAI => "https://api.openai.com/v1".to_owned(),
        ApiProvider::ChatGPTCodex => CHATGPT_CODEX_BASE_URL.to_owned(),
        ApiProvider::OpenAICompatible => env::var("OPENAI_BASE_URL")
            .map(|value| value.trim_end_matches('/').to_owned())
            .unwrap_or_else(|_| "https://openai-compatible.invalid".to_owned()),
        ApiProvider::Bedrock => env_value(["ANTHROPIC_BEDROCK_BASE_URL", "BEDROCK_BASE_URL"])
            .unwrap_or_else(|| format!("https://bedrock-runtime.{}.amazonaws.com", bedrock_region())),
        ApiProvider::Vertex => env_value(["ANTHROPIC_VERTEX_BASE_URL", "VERTEX_BASE_URL"]).unwrap_or_else(
            || {
                let location = vertex_region_for_model(None);
                let project = vertex_project_id().unwrap_or_else(|_| "unset-project".to_owned());
                format!(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}"
                )
            },
        ),
        ApiProvider::Foundry => env_value(["ANTHROPIC_FOUNDRY_BASE_URL", "FOUNDRY_BASE_URL"])
            .or_else(|| {
                env_value(["ANTHROPIC_FOUNDRY_RESOURCE"])
                    .map(|resource| format!("https://{resource}.services.ai.azure.com/anthropic"))
            })
            .unwrap_or_else(|| "https://foundry.unconfigured.local/anthropic".to_owned()),
    }
}

pub(crate) fn build_anthropic_payload(
    request: &ProviderRequest,
    extra_body_field: Option<(&str, Value)>,
) -> Value {
    let mut payload = json!({
        "model": request.model,
        "stream": false,
        "max_tokens": request.max_output_tokens.unwrap_or(4_096),
        "messages": anthropic_messages(&request.messages),
    });

    if let Some(system) = anthropic_system_prompt(&request.messages) {
        payload["system"] = Value::String(system);
    }
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.input_schema,
                    })
                })
                .collect(),
        );
    }
    if let Some((key, value)) = extra_body_field {
        payload[key] = value;
    }

    payload
}

pub(crate) fn bedrock_region() -> String {
    env_value(["AWS_REGION", "AWS_DEFAULT_REGION"]).unwrap_or_else(|| "us-east-1".to_owned())
}

pub(crate) fn vertex_region_for_model(model: Option<&str>) -> String {
    if let Some(model) = model {
        let normalized = model.trim();
        if !normalized.is_empty() {
            for (prefix, env_name) in [
                ("claude-haiku-4-5", "VERTEX_REGION_CLAUDE_HAIKU_4_5"),
                ("claude-3-5-haiku", "VERTEX_REGION_CLAUDE_3_5_HAIKU"),
                ("claude-3-5-sonnet", "VERTEX_REGION_CLAUDE_3_5_SONNET"),
                ("claude-3-7-sonnet", "VERTEX_REGION_CLAUDE_3_7_SONNET"),
                ("claude-opus-4-1", "VERTEX_REGION_CLAUDE_4_1_OPUS"),
                ("claude-opus-4", "VERTEX_REGION_CLAUDE_4_0_OPUS"),
                ("claude-sonnet-4-6", "VERTEX_REGION_CLAUDE_4_6_SONNET"),
                ("claude-sonnet-4-5", "VERTEX_REGION_CLAUDE_4_5_SONNET"),
                ("claude-sonnet-4", "VERTEX_REGION_CLAUDE_4_0_SONNET"),
            ] {
                if normalized.starts_with(prefix) {
                    if let Some(region) = env_value([env_name]) {
                        return region;
                    }
                }
            }
        }
    }

    env_value(["CLOUD_ML_REGION", "VERTEX_REGION"]).unwrap_or_else(|| "us-east5".to_owned())
}

pub(crate) fn vertex_project_id() -> Result<String> {
    env_value([
        "GCLOUD_PROJECT",
        "GOOGLE_CLOUD_PROJECT",
        "gcloud_project",
        "google_cloud_project",
        "ANTHROPIC_VERTEX_PROJECT_ID",
    ])
    .ok_or_else(|| {
        anyhow!(
            "missing Vertex project id; set GOOGLE_CLOUD_PROJECT or ANTHROPIC_VERTEX_PROJECT_ID"
        )
    })
}

pub(crate) fn bedrock_invoke_url(base_url: &str, model: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.contains("/model/") && trimmed.ends_with("/invoke") {
        return trimmed.to_owned();
    }
    let encoded_model = utf8_percent_encode(model, URI_COMPONENT_ENCODE_SET).to_string();
    format!("{trimmed}/model/{encoded_model}/invoke")
}

pub(crate) fn vertex_predict_url(base_url: &str, model: &str) -> Result<String> {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(":rawPredict") || trimmed.ends_with(":streamRawPredict") {
        return Ok(trimmed.replace(":streamRawPredict", ":rawPredict"));
    }
    if trimmed.contains("/publishers/anthropic/models/") {
        return Ok(format!("{trimmed}:rawPredict"));
    }
    if trimmed.ends_with("/publishers/anthropic/models") {
        return Ok(format!("{trimmed}/{model}:rawPredict"));
    }
    if trimmed.contains("/projects/") && trimmed.contains("/locations/") {
        return Ok(format!(
            "{trimmed}/publishers/anthropic/models/{model}:rawPredict"
        ));
    }

    let location = vertex_region_for_model(Some(model));
    let project = vertex_project_id()?;
    Ok(format!(
        "https://{location}-aiplatform.googleapis.com/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{model}:rawPredict"
    ))
}

pub(crate) fn foundry_messages_url(base_url: &str) -> Result<String> {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1/messages") {
        return Ok(trimmed.to_owned());
    }
    if trimmed.ends_with("/anthropic") {
        return Ok(join_api_path(trimmed, "messages", "v1"));
    }
    if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        return Ok(join_if_missing(trimmed, "anthropic/v1/messages"));
    }
    Err(anyhow!("invalid Foundry base URL: {trimmed}"))
}

#[derive(Clone, Debug)]
pub(crate) struct SignedBedrockRequest {
    pub(crate) authorization: String,
    pub(crate) amz_date: String,
    pub(crate) payload_sha256: String,
    pub(crate) session_token: Option<String>,
}

pub(crate) fn hash_hex(payload: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(payload);
    hex::encode(digest.finalize())
}

pub(crate) fn hmac_sha256(key: &[u8], message: &str) -> Result<Vec<u8>> {
    let mut mac =
        HmacSha256::new_from_slice(key).map_err(|_| anyhow!("invalid HMAC signing key"))?;
    mac.update(message.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_bedrock_request(
    method: &str,
    host: &str,
    path: &str,
    payload: &[u8],
    region: &str,
    access_key: &str,
    secret_key: &str,
    session_token: Option<&str>,
    now: OffsetDateTime,
) -> Result<SignedBedrockRequest> {
    let amz_date = now.format(&format_description!(
        "[year][month][day]T[hour][minute][second]Z"
    ))?;
    let date_stamp = now.format(&format_description!("[year][month][day]"))?;
    let payload_sha256 = hash_hex(payload);

    let mut canonical_headers = vec![
        format!("accept:application/json\n"),
        format!("content-type:application/json\n"),
        format!("host:{host}\n"),
        format!("x-amz-content-sha256:{payload_sha256}\n"),
        format!("x-amz-date:{amz_date}\n"),
    ];
    let mut signed_headers = vec![
        "accept",
        "content-type",
        "host",
        "x-amz-content-sha256",
        "x-amz-date",
    ];
    let session_token = session_token.map(str::to_owned);
    if let Some(token) = session_token.as_deref() {
        canonical_headers.push(format!("x-amz-security-token:{token}\n"));
        signed_headers.push("x-amz-security-token");
    }

    let canonical_request = format!(
        "{method}\n{path}\n\n{}{}\n{}",
        canonical_headers.join(""),
        signed_headers.join(";"),
        payload_sha256
    );
    let canonical_request_hash = hash_hex(canonical_request.as_bytes());
    let service = env::var("BEDROCK_SIGNING_NAME").unwrap_or_else(|_| "bedrock".to_owned());
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign =
        format!("AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{canonical_request_hash}");

    let k_date = hmac_sha256(format!("AWS4{secret_key}").as_bytes(), &date_stamp)?;
    let k_region = hmac_sha256(&k_date, region)?;
    let k_service = hmac_sha256(&k_region, &service)?;
    let k_signing = hmac_sha256(&k_service, "aws4_request")?;
    let signature = hex::encode(hmac_sha256(&k_signing, &string_to_sign)?);
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={}, Signature={signature}",
        signed_headers.join(";")
    );

    Ok(SignedBedrockRequest {
        authorization,
        amz_date,
        payload_sha256,
        session_token,
    })
}

pub(crate) fn compact_error_body(body: &str) -> String {
    if let Some(message) = extract_error_message(body) {
        return truncate_error_text(&message, 600);
    }

    let trimmed = body.trim();
    let compact = trimmed.replace('\n', " ");
    truncate_error_text(&compact, 600)
}

pub(crate) fn truncate_error_text(text: &str, max_len: usize) -> String {
    let mut compact = text.trim().to_owned();
    if compact.len() > max_len {
        compact.truncate(max_len);
        compact.push_str("...");
    }
    compact
}

pub(crate) fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let mut fragments = Vec::new();

    for pointer in [
        "/detail",
        "/message",
        "/error/message",
        "/response/error/message",
        "/error/errors",
        "/error",
        "/response/error",
    ] {
        if let Some(target) = value.pointer(pointer) {
            collect_error_fragments(target, &mut fragments);
        }
    }

    if fragments.is_empty() {
        collect_error_fragments(&value, &mut fragments);
    }

    (!fragments.is_empty()).then(|| fragments.join(" "))
}

pub(crate) fn join_api_path(base_url: &str, suffix: &str, version_segment: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(&format!("/{version_segment}")) {
        format!("{trimmed}/{suffix}")
    } else {
        format!("{trimmed}/{version_segment}/{suffix}")
    }
}

pub(crate) fn join_if_missing(base_url: &str, suffix: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        trimmed.to_owned()
    } else {
        format!("{trimmed}/{suffix}")
    }
}

pub(crate) fn openai_compatible_uses_chat_completions(base_url: &str) -> bool {
    if env_flag_truthy("OPENAI_COMPAT_CHAT_COMPLETIONS") {
        return true;
    }

    base_url
        .to_ascii_lowercase()
        .contains("generativelanguage.googleapis.com")
}

pub(crate) fn resolve_provider_model(provider: ApiProvider, model: &str) -> String {
    let normalized = model.trim();
    if normalized.is_empty() {
        return model.to_owned();
    }

    let mapped = match provider {
        ApiProvider::Bedrock if normalized.starts_with("arn:aws:bedrock:") => normalized,
        ApiProvider::Bedrock if normalized.contains(".anthropic.") || normalized.contains(":") => {
            normalized
        }
        ApiProvider::Vertex if normalized.contains('@') => normalized,
        ApiProvider::Foundry if !normalized.starts_with("claude-") => normalized,
        ApiProvider::FirstParty
        | ApiProvider::OpenAI
        | ApiProvider::ChatGPTCodex
        | ApiProvider::OpenAICompatible => normalized,
        ApiProvider::Bedrock => match normalized {
            "claude-3-7-sonnet-20250219" => "us.anthropic.claude-3-7-sonnet-20250219-v1:0",
            "claude-3-5-sonnet-20241022" => "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "claude-3-5-haiku-20241022" => "us.anthropic.claude-3-5-haiku-20241022-v1:0",
            "claude-haiku-4-5" => "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "claude-sonnet-4" => "us.anthropic.claude-sonnet-4-20250514-v1:0",
            "claude-sonnet-4-5" => "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
            "claude-sonnet-4-6" => "us.anthropic.claude-sonnet-4-6",
            "claude-opus-4" => "us.anthropic.claude-opus-4-20250514-v1:0",
            "claude-opus-4-1" => "us.anthropic.claude-opus-4-1-20250805-v1:0",
            "claude-opus-4-5" => "us.anthropic.claude-opus-4-5-20251101-v1:0",
            "claude-opus-4-6" => "us.anthropic.claude-opus-4-6-v1",
            _ => normalized,
        },
        ApiProvider::Vertex => match normalized {
            "claude-3-7-sonnet-20250219" => "claude-3-7-sonnet@20250219",
            "claude-3-5-sonnet-20241022" => "claude-3-5-sonnet-v2@20241022",
            "claude-3-5-haiku-20241022" => "claude-3-5-haiku@20241022",
            "claude-haiku-4-5" => "claude-haiku-4-5@20251001",
            "claude-sonnet-4" => "claude-sonnet-4@20250514",
            "claude-sonnet-4-5" => "claude-sonnet-4-5@20250929",
            "claude-sonnet-4-6" => "claude-sonnet-4-6",
            "claude-opus-4" => "claude-opus-4@20250514",
            "claude-opus-4-1" => "claude-opus-4-1@20250805",
            "claude-opus-4-5" => "claude-opus-4-5@20251101",
            "claude-opus-4-6" => "claude-opus-4-6",
            _ => normalized,
        },
        ApiProvider::Foundry => normalized,
    };

    mapped.to_owned()
}

async fn get_command_stdout(candidates: &[(&str, &[&str])]) -> Option<String> {
    for (program, args) in candidates {
        let output = match Command::new(program).args(args.iter()).output().await {
            Ok(output) => output,
            Err(_) => continue,
        };
        if !output.status.success() {
            continue;
        }
        let stdout = match String::from_utf8(output.stdout) {
            Ok(stdout) => stdout,
            Err(_) => continue,
        };
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

pub(crate) fn collect_error_fragments(value: &Value, fragments: &mut Vec<String>) {
    match value {
        Value::String(message) => push_error_fragment(fragments, message),
        Value::Array(items) => {
            for item in items {
                collect_error_fragments(item, fragments);
            }
        }
        Value::Object(object) => {
            let location = object.get("loc").and_then(format_error_location);
            if let Some(message) = object
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| object.get("msg").and_then(Value::as_str))
                .or_else(|| object.get("reason").and_then(Value::as_str))
                .or_else(|| object.get("error_description").and_then(Value::as_str))
            {
                if let Some(location) = location {
                    push_error_fragment(fragments, format!("{location}: {message}"));
                } else {
                    push_error_fragment(fragments, message);
                }
            }

            for key in ["detail", "error", "errors"] {
                if let Some(child) = object.get(key) {
                    collect_error_fragments(child, fragments);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn format_error_location(value: &Value) -> Option<String> {
    let location = value
        .as_array()?
        .iter()
        .filter_map(|part| match part {
            Value::String(text) => Some(text.trim().to_owned()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (!location.is_empty()).then(|| location.join("."))
}

pub(crate) fn push_error_fragment(fragments: &mut Vec<String>, fragment: impl AsRef<str>) {
    let fragment = fragment.as_ref().trim();
    if fragment.is_empty() {
        return;
    }
    if !fragments.iter().any(|existing| existing == fragment) {
        fragments.push(fragment.to_owned());
    }
}

pub(crate) fn openai_request_failure_label(provider: ApiProvider) -> &'static str {
    match provider {
        ApiProvider::ChatGPTCodex => "ChatGPT Codex",
        ApiProvider::OpenAICompatible => "OpenAI-compatible",
        _ => "OpenAI",
    }
}

pub(crate) fn openai_responses_max_retries() -> usize {
    env::var("CLAUDE_CODE_MAX_RETRIES")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(OPENAI_RESPONSES_DEFAULT_MAX_RETRIES)
}

pub(crate) fn should_retry_openai_responses_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::REQUEST_TIMEOUT
            | reqwest::StatusCode::CONFLICT
            | reqwest::StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

pub(crate) fn should_retry_openai_send_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || (error.is_request() && !error.is_body())
}

pub(crate) fn openai_responses_retry_delay(
    attempt: usize,
    retry_after_header: Option<&str>,
) -> Duration {
    if let Some(retry_after_header) = retry_after_header {
        if let Ok(seconds) = retry_after_header.trim().parse::<u64>() {
            return Duration::from_secs(seconds);
        }
    }

    let shift = attempt.saturating_sub(1).min(16) as u32;
    let delay_ms = OPENAI_RESPONSES_BASE_DELAY_MS
        .saturating_mul(1u64 << shift)
        .min(OPENAI_RESPONSES_MAX_DELAY_MS);
    Duration::from_millis(delay_ms)
}

pub(crate) fn insert_extra_headers(
    headers: &mut HeaderMap,
    extra_headers: &BTreeMap<String, String>,
) -> Result<()> {
    for (key, value) in extra_headers {
        headers.insert(
            HeaderName::from_bytes(key.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(())
}

pub(crate) fn message_text(message: &Message) -> String {
    message
        .blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn parse_tool_input(input_json: &str) -> Value {
    serde_json::from_str(input_json).unwrap_or_else(|_| json!({}))
}

pub(crate) fn anthropic_system_prompt(messages: &[Message]) -> Option<String> {
    let parts = messages
        .iter()
        .filter(|message| matches!(message.role, MessageRole::System))
        .map(message_text)
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>();

    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub(crate) fn anthropic_messages(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        let role = match message.role {
            MessageRole::System | MessageRole::Attachment => continue,
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "user",
        };

        let content = anthropic_content_blocks(message);
        if content.is_empty() {
            continue;
        }

        encoded.push(json!({
            "role": role,
            "content": content,
        }));
    }

    encoded
}

pub(crate) fn anthropic_content_blocks(message: &Message) -> Vec<Value> {
    let mut content = Vec::new();

    for block in &message.blocks {
        match block {
            ContentBlock::Text { text } => {
                if !text.is_empty() {
                    content.push(json!({
                        "type": "text",
                        "text": text,
                    }));
                }
            }
            ContentBlock::ToolCall { call } => {
                content.push(json!({
                    "type": "tool_use",
                    "id": call.id,
                    "name": call.name,
                    "input": parse_tool_input(&call.input_json),
                }));
            }
            ContentBlock::ToolResult { result } => {
                content.push(json!({
                    "type": "tool_result",
                    "tool_use_id": result.tool_call_id,
                    "content": result.output_text,
                    "is_error": result.is_error,
                }));
            }
            ContentBlock::Attachment { .. } | ContentBlock::Boundary { .. } => {}
        }
    }

    content
}

pub(crate) fn build_openai_responses_payload(
    request: &ProviderRequest,
    supports_reasoning_summaries: bool,
    supports_verbosity: bool,
) -> Value {
    let mut payload = json!({
        "model": request.model,
        "instructions": anthropic_system_prompt(&request.messages)
            .unwrap_or_else(|| "You are Codex.".to_owned()),
        "input": openai_responses_input(&request.messages),
        "stream": true,
        "store": false,
    });

    if let Some(max_output_tokens) = request.max_output_tokens {
        payload["max_output_tokens"] = Value::Number(max_output_tokens.into());
    }
    if request.thinking != ThinkingConfig::Disabled {
        let mut reasoning = json!({
            "effort": resolve_reasoning_effort(&request.model),
        });
        if supports_reasoning_summaries {
            reasoning["summary"] = Value::String("auto".to_owned());
        }
        payload["reasoning"] = reasoning;
    }
    if supports_verbosity {
        payload["text"] = json!({
            "verbosity": "medium",
        });
    }
    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    })
                })
                .collect(),
        );
        payload["tool_choice"] = Value::String("auto".to_owned());
    }

    payload
}

pub(crate) fn build_openai_chat_completions_payload(request: &ProviderRequest) -> Value {
    let mut payload = json!({
        "model": request.model,
        "messages": openai_chat_messages(&request.messages),
        "stream": false,
    });

    if request.thinking != ThinkingConfig::Disabled {
        payload["reasoning_effort"] = Value::String(resolve_reasoning_effort(&request.model));
    }

    if let Some(max_output_tokens) = request.max_output_tokens {
        payload["max_completion_tokens"] = Value::Number(max_output_tokens.into());
    }

    if !request.tools.is_empty() {
        payload["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        }
                    })
                })
                .collect(),
        );
        payload["tool_choice"] = Value::String("auto".to_owned());
    }

    payload
}

pub(crate) fn openai_chat_messages(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        match message.role {
            MessageRole::System => {
                let text = message_text(message);
                if !text.trim().is_empty() {
                    encoded.push(json!({
                        "role": "system",
                        "content": text,
                    }));
                }
            }
            MessageRole::User => {
                let text = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.clone()),
                        ContentBlock::Attachment { attachment } => {
                            Some(format!("[Attachment omitted: {}]", attachment.name))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    encoded.push(json!({
                        "role": "user",
                        "content": text,
                    }));
                }
            }
            MessageRole::Assistant => {
                let mut item = json!({
                    "role": "assistant",
                });
                let text = message_text(message);
                if !text.trim().is_empty() {
                    item["content"] = Value::String(text);
                }
                let tool_calls = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolCall { call } => Some(openai_chat_tool_call(call)),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !tool_calls.is_empty() {
                    item["tool_calls"] = Value::Array(tool_calls);
                }
                if item.get("content").is_some() || item.get("tool_calls").is_some() {
                    encoded.push(item);
                }
            }
            MessageRole::Tool => {
                for result in message.blocks.iter().filter_map(|block| match block {
                    ContentBlock::ToolResult { result } => Some(result),
                    _ => None,
                }) {
                    encoded.push(json!({
                        "role": "tool",
                        "tool_call_id": result.tool_call_id,
                        "content": result.output_text,
                    }));
                }
            }
            MessageRole::Attachment => {}
        }
    }

    encoded
}

pub(crate) fn resolve_reasoning_effort(model: &str) -> String {
    if model == get_openai_completion_model() {
        get_openai_completion_think_level()
    } else {
        get_openai_reasoning_think_level()
    }
}

pub(crate) fn openai_responses_input(messages: &[Message]) -> Vec<Value> {
    let mut encoded = Vec::new();

    for message in messages {
        match message.role {
            MessageRole::System => {}
            MessageRole::User => {
                let content = message
                    .blocks
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } if !text.is_empty() => {
                            Some(json!({ "type": "input_text", "text": text }))
                        }
                        ContentBlock::Attachment { attachment } => Some(json!({
                            "type": "input_text",
                            "text": format!("[Attachment omitted: {}]", attachment.name),
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if !content.is_empty() {
                    encoded.push(json!({
                        "role": "user",
                        "content": content,
                    }));
                }
            }
            MessageRole::Assistant => {
                let text = message_text(message);
                if !text.is_empty() {
                    encoded.push(json!({
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": text,
                        }],
                    }));
                }
                encoded.extend(message.blocks.iter().filter_map(|block| match block {
                    ContentBlock::ToolCall { call } => Some(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.input_json,
                    })),
                    _ => None,
                }));
            }
            MessageRole::Tool => {
                encoded.extend(message.blocks.iter().filter_map(|block| match block {
                    ContentBlock::ToolResult { result } => Some(json!({
                        "type": "function_call_output",
                        "call_id": result.tool_call_id,
                        "output": result.output_text,
                    })),
                    _ => None,
                }));
            }
            MessageRole::Attachment => {}
        }
    }

    encoded
}
