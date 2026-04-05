#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderDescriptor {
    pub provider: ApiProvider,
    pub display_name: String,
    pub supports_streaming: bool,
    pub supports_tool_use: bool,
    pub supports_reasoning: bool,
    pub requires_cloud_auth: bool,
}

#[derive(Clone, Debug, Default)]
pub struct StaticModelCatalog {
    models: Vec<ModelMetadata>,
}

impl StaticModelCatalog {
    pub fn new(models: Vec<ModelMetadata>) -> Self {
        Self { models }
    }
}

impl ModelCatalog for StaticModelCatalog {
    fn list_models(&self) -> Vec<ModelMetadata> {
        self.models.clone()
    }

    fn get_model(&self, id: &str) -> Option<ModelMetadata> {
        self.models.iter().find(|model| model.id == id).cloned()
    }
}

#[derive(Clone, Debug, Default)]
pub struct DefaultUsageAccounting;

impl UsageAccounting for DefaultUsageAccounting {
    fn total_tokens(&self, usage: &TokenUsage) -> u64 {
        usage.input_tokens
            + usage.output_tokens
            + usage.cache_creation_input_tokens
            + usage.cache_read_input_tokens
    }
}

#[derive(Clone, Debug, Default)]
pub struct StaticContextWindowResolver {
    by_model: BTreeMap<String, u64>,
}

impl StaticContextWindowResolver {
    pub fn new(models: &[ModelMetadata]) -> Self {
        let by_model = models
            .iter()
            .filter_map(|model| {
                model
                    .context_window
                    .map(|window| (model.id.clone(), window))
            })
            .collect();
        Self { by_model }
    }
}

impl ContextWindowResolver for StaticContextWindowResolver {
    fn effective_context_window(&self, model: &str) -> Option<u64> {
        self.by_model.get(model).copied()
    }
}

impl Default for OpenAIAuthStatus {
    fn default() -> Self {
        Self {
            has_credentials: false,
            source: OpenAIAuthSource::None,
            api_key: None,
            bearer_token: None,
            email: None,
            account_id: None,
            auth_mode: None,
            token_freshness: OpenAITokenFreshness::Missing,
        }
    }
}

pub fn is_openai_provider(provider: ApiProvider) -> bool {
    matches!(
        provider,
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible
    )
}

pub fn read_codex_auth_file(path: Option<&std::path::Path>) -> Option<CodexAuthFile> {
    let path = path.map(PathBuf::from).unwrap_or_else(codex_auth_file_path);
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<CodexAuthFile>(&raw).ok()
}

pub fn read_auth_snapshot() -> Option<BTreeMap<String, AuthMaterial>> {
    let raw = fs::read_to_string(code_agent_auth_snapshot_path()).ok()?;
    serde_json::from_str::<AuthSnapshotFile>(&raw)
        .ok()
        .map(|snapshot| snapshot.providers)
}

pub fn read_provider_auth_snapshot(provider: ApiProvider) -> Option<AuthMaterial> {
    read_auth_snapshot()?.remove(provider.as_str())
}

fn openai_auth_url() -> String {
    env::var("OPENAI_AUTH_URL").unwrap_or_else(|_| OPENAI_AUTH_URL.to_owned())
}

fn decode_base64_url(input: &str) -> Option<Vec<u8>> {
    let mut sanitized = input.replace('-', "+").replace('_', "/");
    while !sanitized.len().is_multiple_of(4) {
        sanitized.push('=');
    }

    let mut output = Vec::with_capacity((sanitized.len() / 4) * 3);
    let mut chunk = [0u8; 4];
    let mut filled = 0usize;

    for byte in sanitized.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => 64,
            _ => return None,
        };

        chunk[filled] = value;
        filled += 1;

        if filled == 4 {
            output.push((chunk[0] << 2) | (chunk[1] >> 4));
            if chunk[2] != 64 {
                output.push((chunk[1] << 4) | (chunk[2] >> 2));
            }
            if chunk[3] != 64 {
                output.push((chunk[2] << 6) | chunk[3]);
            }
            filled = 0;
        }
    }

    Some(output)
}

pub fn decode_jwt_claims(token: Option<&str>) -> Option<TokenClaims> {
    let token = token?;
    let payload = token.split('.').nth(1)?;
    let decoded = decode_base64_url(payload)?;
    serde_json::from_slice::<TokenClaims>(&decoded).ok()
}

fn is_jwt_expired(token: Option<&str>, skew_seconds: u64) -> bool {
    let Some(exp) = decode_jwt_claims(token).and_then(|claims| claims.exp) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    exp <= now + skew_seconds
}

pub fn get_token_freshness(token: Option<&str>) -> OpenAITokenFreshness {
    let Some(token) = token else {
        return OpenAITokenFreshness::Missing;
    };
    if is_jwt_expired(Some(token), 0) {
        OpenAITokenFreshness::Expired
    } else if is_jwt_expired(Some(token), OPENAI_ACCESS_TOKEN_REFRESH_SKEW_SECONDS) {
        OpenAITokenFreshness::Stale
    } else {
        OpenAITokenFreshness::Fresh
    }
}

fn codex_auth_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn refresh_client_id(auth: &CodexAuthFile) -> Option<String> {
    if let Some(client_id) = decode_jwt_claims(auth.tokens.as_ref()?.access_token.as_deref())
        .and_then(|claims| claims.client_id)
    {
        return Some(client_id);
    }

    match decode_jwt_claims(auth.tokens.as_ref()?.id_token.as_deref()).and_then(|claims| claims.aud)
    {
        Some(Value::String(aud)) if !aud.trim().is_empty() => Some(aud),
        Some(Value::Array(audiences)) => audiences.into_iter().find_map(|aud| match aud {
            Value::String(value) if !value.trim().is_empty() => Some(value),
            _ => None,
        }),
        _ => None,
    }
}

fn write_codex_auth_file(path: &Path, auth: &CodexAuthFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        OffsetDateTime::now_utc().unix_timestamp_nanos()
    ));
    let mut payload = serde_json::to_vec_pretty(auth)?;
    payload.push(b'\n');
    fs::write(&tmp_path, payload)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

fn format_oauth_error_detail(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
        None => None,
    }
}

async fn refresh_codex_access_token(path: Option<&Path>) -> Result<Option<String>> {
    let path = path.map(PathBuf::from).unwrap_or_else(codex_auth_file_path);
    let _lock = acquire_codex_auth_file_lock(path.as_path()).await?;
    let Some(auth) = read_codex_auth_file(Some(path.as_path())) else {
        return Ok(None);
    };
    let Some(tokens) = auth.tokens.as_ref() else {
        return Ok(None);
    };
    let Some(refresh_token) = tokens.refresh_token.as_deref() else {
        return Ok(None);
    };

    if tokens.access_token.is_some() && !is_jwt_expired(tokens.access_token.as_deref(), 10) {
        return Ok(tokens.access_token.clone());
    }

    let client_id = refresh_client_id(&auth)
        .ok_or_else(|| anyhow!("unable to determine the OpenAI OAuth client_id"))?;
    let client = reqwest::Client::new();
    let response = client
        .post(openai_auth_url())
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    let value: Option<Value> = serde_json::from_str(&body).ok();
    let access_token = value
        .as_ref()
        .and_then(|json| json.get("access_token"))
        .and_then(Value::as_str);
    if !status.is_success() || access_token.is_none() {
        let detail = format_oauth_error_detail(
            value
                .as_ref()
                .and_then(|json| json.get("error_description")),
        )
        .or_else(|| format_oauth_error_detail(value.as_ref().and_then(|json| json.get("error"))))
        .unwrap_or_else(|| compact_error_body(&body));
        return Err(anyhow!("failed to refresh Codex auth token: {detail}"));
    }

    let mut next_auth = auth;
    if let Some(tokens) = next_auth.tokens.as_mut() {
        tokens.access_token = access_token.map(str::to_owned);
        if let Some(refresh_token) = value
            .as_ref()
            .and_then(|json| json.get("refresh_token"))
            .and_then(Value::as_str)
        {
            tokens.refresh_token = Some(refresh_token.to_owned());
        }
        if let Some(id_token) = value
            .as_ref()
            .and_then(|json| json.get("id_token"))
            .and_then(Value::as_str)
        {
            tokens.id_token = Some(id_token.to_owned());
        }
    }
    next_auth.last_refresh =
        Some(OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?);
    write_codex_auth_file(path.as_path(), &next_auth)?;
    Ok(access_token.map(str::to_owned))
}

pub fn get_openai_auth_status(provider: ApiProvider) -> OpenAIAuthStatus {
    let auth = read_codex_auth_file(None);
    let auth_mode = auth.as_ref().and_then(|entry| entry.auth_mode.clone());

    if provider == ApiProvider::ChatGPTCodex {
        if auth.as_ref().and_then(|entry| entry.auth_mode.as_deref()) != Some("chatgpt") {
            return OpenAIAuthStatus {
                auth_mode,
                ..OpenAIAuthStatus::default()
            };
        }

        let access_token = auth
            .as_ref()
            .and_then(|entry| entry.tokens.as_ref())
            .and_then(|tokens| tokens.access_token.clone());
        let refresh_token = auth
            .as_ref()
            .and_then(|entry| entry.tokens.as_ref())
            .and_then(|tokens| tokens.refresh_token.clone());

        if access_token.is_none() || refresh_token.is_none() {
            return OpenAIAuthStatus {
                auth_mode,
                token_freshness: get_token_freshness(access_token.as_deref()),
                ..OpenAIAuthStatus::default()
            };
        }

        let claims = decode_jwt_claims(access_token.as_deref());
        return OpenAIAuthStatus {
            has_credentials: true,
            source: OpenAIAuthSource::CodexAuthToken,
            api_key: None,
            bearer_token: access_token.clone(),
            email: claims.and_then(|value| value.email),
            account_id: auth
                .as_ref()
                .and_then(|entry| entry.tokens.as_ref())
                .and_then(|tokens| tokens.account_id.clone()),
            auth_mode,
            token_freshness: get_token_freshness(access_token.as_deref()),
        };
    }

    if let Ok(api_key) = env::var("OPENAI_API_KEY") {
        if !api_key.trim().is_empty() {
            return OpenAIAuthStatus {
                has_credentials: true,
                source: OpenAIAuthSource::OpenAiApiKey,
                api_key: Some(api_key.clone()),
                bearer_token: Some(api_key),
                email: None,
                account_id: None,
                auth_mode,
                token_freshness: OpenAITokenFreshness::Fresh,
            };
        }
    }

    if let Some(api_key) = auth.as_ref().and_then(|entry| entry.openai_api_key.clone()) {
        return OpenAIAuthStatus {
            has_credentials: true,
            source: OpenAIAuthSource::CodexAuthApiKey,
            api_key: Some(api_key.clone()),
            bearer_token: Some(api_key),
            email: None,
            account_id: auth
                .as_ref()
                .and_then(|entry| entry.tokens.as_ref())
                .and_then(|tokens| tokens.account_id.clone()),
            auth_mode,
            token_freshness: OpenAITokenFreshness::Fresh,
        };
    }

    let access_token = auth
        .as_ref()
        .and_then(|entry| entry.tokens.as_ref())
        .and_then(|tokens| tokens.access_token.clone());
    if access_token.is_some() && provider != ApiProvider::OpenAICompatible {
        return OpenAIAuthStatus {
            has_credentials: true,
            source: OpenAIAuthSource::CodexAuthToken,
            api_key: None,
            bearer_token: access_token.clone(),
            email: decode_jwt_claims(access_token.as_deref()).and_then(|claims| claims.email),
            account_id: auth
                .as_ref()
                .and_then(|entry| entry.tokens.as_ref())
                .and_then(|tokens| tokens.account_id.clone()),
            auth_mode,
            token_freshness: get_token_freshness(access_token.as_deref()),
        };
    }

    OpenAIAuthStatus {
        auth_mode,
        ..OpenAIAuthStatus::default()
    }
}

