use super::*;

use std::collections::BTreeMap;

use std::path::{Path, PathBuf};

use std::env;

use std::fs;

use std::time::Duration;

use serde::{Deserialize, Serialize};

use anyhow::{anyhow, Result};

use async_trait::async_trait;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthRequest {
    pub provider: ApiProvider,
    pub profile: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AuthMaterial {
    pub api_key: Option<String>,
    pub bearer_token: Option<String>,
    pub extra_headers: BTreeMap<String, String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenAIAuthStatus {
    pub has_credentials: bool,
    pub source: OpenAIAuthSource,
    pub api_key: Option<String>,
    pub bearer_token: Option<String>,
    pub email: Option<String>,
    pub account_id: Option<String>,
    pub auth_mode: Option<String>,
    pub token_freshness: OpenAITokenFreshness,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeminiAuthStatus {
    pub has_credentials: bool,
    pub source: GeminiAuthSource,
    pub api_key: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexAuthTokens {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CodexAuthFile {
    pub auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    pub tokens: Option<CodexAuthTokens>,
    pub last_refresh: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenClaims {
    pub exp: Option<u64>,
    pub email: Option<String>,
    pub aud: Option<Value>,
    pub client_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct CodexAuthFileLock {
    path: PathBuf,
}

impl Drop for CodexAuthFileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenAIAuthSource {
    #[serde(rename = "OPENAI_API_KEY")]
    OpenAiApiKey,
    #[serde(rename = "codex_auth_api_key")]
    CodexAuthApiKey,
    #[serde(rename = "codex_auth_token")]
    CodexAuthToken,
    #[serde(rename = "none")]
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenAITokenFreshness {
    Fresh,
    Stale,
    Expired,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeminiAuthSource {
    #[serde(rename = "GEMINI_API_KEY")]
    GeminiApiKey,
    #[serde(rename = "GOOGLE_API_KEY")]
    GoogleApiKey,
    #[serde(rename = "none")]
    None,
}

pub fn get_gemini_auth_status() -> GeminiAuthStatus {
    if let Ok(api_key) = env::var("GEMINI_API_KEY") {
        if !api_key.trim().is_empty() {
            return GeminiAuthStatus {
                has_credentials: true,
                source: GeminiAuthSource::GeminiApiKey,
                api_key: Some(api_key),
            };
        }
    }

    if let Ok(api_key) = env::var("GOOGLE_API_KEY") {
        if !api_key.trim().is_empty() {
            return GeminiAuthStatus {
                has_credentials: true,
                source: GeminiAuthSource::GoogleApiKey,
                api_key: Some(api_key),
            };
        }
    }

    GeminiAuthStatus {
        has_credentials: false,
        source: GeminiAuthSource::None,
        api_key: None,
    }
}

pub fn get_gemini_credential_hint() -> String {
    "Set GEMINI_API_KEY or GOOGLE_API_KEY.".to_owned()
}

#[async_trait]
impl AuthResolver for EnvironmentAuthResolver {
    async fn resolve_auth(&self, request: AuthRequest) -> Result<AuthMaterial> {
        match request.provider {
            ApiProvider::Gemini => {
                let status = get_gemini_auth_status();
                if status.has_credentials {
                    return Ok(AuthMaterial {
                        api_key: status.api_key,
                        source: Some(
                            match status.source {
                                GeminiAuthSource::GeminiApiKey => "GEMINI_API_KEY",
                                GeminiAuthSource::GoogleApiKey => "GOOGLE_API_KEY",
                                GeminiAuthSource::None => "none",
                            }
                            .to_owned(),
                        ),
                        ..AuthMaterial::default()
                    });
                }

                read_provider_auth_snapshot(request.provider)
                    .ok_or_else(|| anyhow!(get_gemini_credential_hint()))
            }
            ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
                let status = get_openai_auth_status(request.provider);
                if status.has_credentials {
                    let mut api_key = status.api_key.clone();
                    let mut bearer_token = status.bearer_token.clone();
                    let source = status.source.clone();

                    if source == OpenAIAuthSource::CodexAuthToken {
                        let needs_refresh = matches!(
                            status.token_freshness,
                            OpenAITokenFreshness::Stale | OpenAITokenFreshness::Expired
                        );
                        if needs_refresh {
                            bearer_token = refresh_codex_access_token(None).await?;
                        }
                    }

                    if request.provider == ApiProvider::ChatGPTCodex
                        && source != OpenAIAuthSource::CodexAuthToken
                    {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }

                    if request.provider == ApiProvider::OpenAICompatible
                        && !matches!(
                            source,
                            OpenAIAuthSource::OpenAiApiKey
                                | OpenAIAuthSource::CodexAuthApiKey
                                | OpenAIAuthSource::CodexAuthToken
                        )
                    {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }
                    if request.provider == ApiProvider::OpenAICompatible
                        && source == OpenAIAuthSource::CodexAuthToken
                        && !openai_compatible_uses_official_base_url()
                    {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }

                    if source == OpenAIAuthSource::CodexAuthToken && bearer_token.is_none() {
                        return Err(anyhow!(get_openai_credential_hint(request.provider)));
                    }

                    return Ok(AuthMaterial {
                        api_key: api_key.take(),
                        bearer_token,
                        source: Some(
                            match source {
                                OpenAIAuthSource::OpenAiApiKey => "OPENAI_API_KEY",
                                OpenAIAuthSource::CodexAuthApiKey => "codex_auth_api_key",
                                OpenAIAuthSource::CodexAuthToken => "codex_auth_token",
                                OpenAIAuthSource::None => "none",
                            }
                            .to_owned(),
                        ),
                        ..AuthMaterial::default()
                    });
                }
                read_provider_auth_snapshot(request.provider)
                    .ok_or_else(|| anyhow!(get_openai_credential_hint(request.provider)))
            }
            ApiProvider::FirstParty => get_anthropic_auth_material(request.provider)
                .or_else(|| read_provider_auth_snapshot(request.provider))
                .ok_or_else(|| anyhow!(get_anthropic_credential_hint(request.provider))),
            ApiProvider::Bedrock | ApiProvider::Vertex | ApiProvider::Foundry => Ok(AuthMaterial {
                source: Some("ambient_cloud_auth".to_owned()),
                ..AuthMaterial::default()
            }),
        }
    }
}

pub fn codex_home_dir() -> PathBuf {
    if let Some(home) = env::var_os("CODEX_HOME") {
        return PathBuf::from(home);
    }

    match env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".codex"),
        None => PathBuf::from(".codex"),
    }
}

pub fn codex_auth_file_path() -> PathBuf {
    codex_home_dir().join("auth.json")
}

pub fn code_agent_auth_snapshot_path() -> PathBuf {
    codex_home_dir().join("code-agent-auth.json")
}

pub fn clear_auth_snapshot(provider: ApiProvider) -> Result<bool> {
    let path = code_agent_auth_snapshot_path();
    if !path.exists() {
        return Ok(false);
    }
    let mut providers = read_auth_snapshot().unwrap_or_default();
    let removed = match provider {
        ApiProvider::OpenAICompatible => {
            providers.remove(provider.as_str()).is_some() | providers.remove("openai").is_some()
        }
        _ => providers.remove(provider.as_str()).is_some(),
    };
    fs::write(
        &path,
        serde_json::to_vec_pretty(&AuthSnapshotFile { providers })?,
    )?;
    Ok(removed)
}

pub fn write_auth_snapshot(provider: ApiProvider, auth: &AuthMaterial) -> Result<PathBuf> {
    let path = code_agent_auth_snapshot_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut providers = read_auth_snapshot().unwrap_or_default();
    if provider == ApiProvider::OpenAICompatible {
        providers.remove("openai");
    }
    providers.insert(provider.as_str().to_owned(), auth.clone());
    fs::write(
        &path,
        serde_json::to_vec_pretty(&AuthSnapshotFile { providers })?,
    )?;
    Ok(path)
}

pub(crate) async fn acquire_codex_auth_file_lock(path: &Path) -> Result<CodexAuthFileLock> {
    let lock_path = codex_auth_lock_path(path);
    for attempt in 0..=CODEX_AUTH_LOCK_RETRIES {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => return Ok(CodexAuthFileLock { path: lock_path }),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if attempt == CODEX_AUTH_LOCK_RETRIES {
                    break;
                }
                let delay_ms =
                    (CODEX_AUTH_LOCK_MIN_DELAY_MS << attempt).min(CODEX_AUTH_LOCK_MAX_DELAY_MS);
                sleep(Duration::from_millis(delay_ms)).await;
            }
            Err(error) => {
                return Err(anyhow!(
                    "failed to acquire Codex auth lock {}: {}",
                    lock_path.display(),
                    error
                ));
            }
        }
    }

    Err(anyhow!(
        "timed out acquiring Codex auth lock {}",
        lock_path.display()
    ))
}

pub(crate) fn env_flag(name: &str) -> bool {
    matches!(
        env::var(name).ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON")
    )
}

pub(crate) fn env_flag_truthy(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}
