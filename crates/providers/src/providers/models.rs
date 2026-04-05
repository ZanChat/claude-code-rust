#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigMigrationReport {
    pub provider: ApiProvider,
    pub env: BTreeMap<String, String>,
    pub auth_snapshot_path: Option<PathBuf>,
    pub codex_auth_path: Option<PathBuf>,
}

pub fn config_migration_report(provider: ApiProvider) -> ConfigMigrationReport {
    let mut env = BTreeMap::new();
    if let Ok(value) = env::var("CLAUDE_CODE_API_PROVIDER") {
        if !value.trim().is_empty() {
            env.insert("CLAUDE_CODE_API_PROVIDER".to_owned(), value);
        }
    }
    for key in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
        "CLAUDE_CODE_SKIP_FOUNDRY_AUTH",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_ORG_ID",
        "AZURE_OPENAI_API_KEY",
        "AWS_REGION",
        "AWS_ACCESS_KEY_ID",
        "VERTEXAI_PROJECT",
        "VERTEXAI_LOCATION",
    ] {
        if let Ok(value) = env::var(key) {
            if !value.trim().is_empty() {
                env.insert(key.to_owned(), value);
            }
        }
    }

    let auth_snapshot_path = code_agent_auth_snapshot_path();
    let codex_auth_path = codex_auth_file_path();
    ConfigMigrationReport {
        provider,
        env,
        auth_snapshot_path: auth_snapshot_path.exists().then_some(auth_snapshot_path),
        codex_auth_path: codex_auth_path.exists().then_some(codex_auth_path),
    }
}

pub fn get_openai_credential_hint(provider: ApiProvider) -> String {
    match provider {
        ApiProvider::ChatGPTCodex => format!(
            "Run official 'codex login' so {} contains auth_mode=chatgpt and refreshable tokens.",
            codex_auth_file_path().display()
        ),
        ApiProvider::OpenAICompatible => "Set OPENAI_API_KEY and OPENAI_BASE_URL.".to_owned(),
        ApiProvider::OpenAI => format!(
            "Set OPENAI_API_KEY or sign in with Codex so {} exists.",
            codex_auth_file_path().display()
        ),
        _ => "Provider does not use OpenAI-family credentials.".to_owned(),
    }
}

pub fn get_anthropic_auth_material(provider: ApiProvider) -> Option<AuthMaterial> {
    if provider != ApiProvider::FirstParty {
        return None;
    }

    if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
        if !api_key.trim().is_empty() {
            return Some(AuthMaterial {
                api_key: Some(api_key),
                source: Some("ANTHROPIC_API_KEY".to_owned()),
                ..AuthMaterial::default()
            });
        }
    }

    for token_var in ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"] {
        if let Ok(token) = env::var(token_var) {
            if !token.trim().is_empty() {
                return Some(AuthMaterial {
                    bearer_token: Some(token),
                    source: Some(token_var.to_owned()),
                    ..AuthMaterial::default()
                });
            }
        }
    }

    None
}

pub fn get_anthropic_credential_hint(provider: ApiProvider) -> String {
    match provider {
        ApiProvider::FirstParty => {
            "Set ANTHROPIC_API_KEY or provide CLAUDE_CODE_OAUTH_TOKEN.".to_owned()
        }
        ApiProvider::Bedrock => "Provide AWS credentials and region configuration.".to_owned(),
        ApiProvider::Vertex => {
            "Provide Google Cloud credentials and Vertex region configuration.".to_owned()
        }
        ApiProvider::Foundry => {
            "Provide Azure Foundry credentials and deployment configuration.".to_owned()
        }
        _ => "Provider does not use Anthropic-family credentials.".to_owned(),
    }
}

pub fn provider_descriptor(provider: ApiProvider) -> ProviderDescriptor {
    match provider {
        ApiProvider::FirstParty => ProviderDescriptor {
            provider,
            display_name: "Anthropic".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::Bedrock => ProviderDescriptor {
            provider,
            display_name: "Amazon Bedrock".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::Vertex => ProviderDescriptor {
            provider,
            display_name: "Google Vertex AI".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::Foundry => ProviderDescriptor {
            provider,
            display_name: "Microsoft Foundry".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::OpenAI => ProviderDescriptor {
            provider,
            display_name: "OpenAI".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: false,
        },
        ApiProvider::ChatGPTCodex => ProviderDescriptor {
            provider,
            display_name: "ChatGPT Codex".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: true,
        },
        ApiProvider::OpenAICompatible => ProviderDescriptor {
            provider,
            display_name: "OpenAI-Compatible".to_owned(),
            supports_streaming: true,
            supports_tool_use: true,
            supports_reasoning: true,
            requires_cloud_auth: false,
        },
    }
}

pub fn compatibility_models_for(provider: ApiProvider) -> Vec<ModelMetadata> {
    match provider {
        ApiProvider::FirstParty
        | ApiProvider::Bedrock
        | ApiProvider::Vertex
        | ApiProvider::Foundry => vec![
            ModelMetadata {
                id: "claude-sonnet-4-6".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
            ModelMetadata {
                id: "claude-opus-4-1".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(32_000),
                supports_tool_use: true,
                supports_reasoning: true,
            },
            ModelMetadata {
                id: "claude-haiku-4-5".to_owned(),
                provider: provider.to_string(),
                context_window: Some(200_000),
                max_output_tokens: Some(16_000),
                supports_tool_use: true,
                supports_reasoning: false,
            },
        ],
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
            openai_family_compatibility_models(provider)
        }
    }
}

fn openai_family_compatibility_models(provider: ApiProvider) -> Vec<ModelMetadata> {
    let provider_name = provider.to_string();
    let mut models = Vec::new();
    let mut push_unique = |model: ModelMetadata| {
        if models
            .iter()
            .any(|existing: &ModelMetadata| existing.id == model.id)
        {
            return;
        }
        models.push(model);
    };

    push_unique(ModelMetadata {
        id: get_openai_reasoning_model(),
        provider: provider_name.clone(),
        context_window: Some(200_000),
        max_output_tokens: Some(32_000),
        supports_tool_use: true,
        supports_reasoning: true,
    });
    push_unique(ModelMetadata {
        id: get_openai_completion_model(),
        provider: provider_name.clone(),
        context_window: Some(128_000),
        max_output_tokens: Some(16_000),
        supports_tool_use: true,
        supports_reasoning: false,
    });
    push_unique(ModelMetadata {
        id: DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        provider: provider_name.clone(),
        context_window: Some(200_000),
        max_output_tokens: Some(32_000),
        supports_tool_use: true,
        supports_reasoning: true,
    });
    push_unique(ModelMetadata {
        id: DEFAULT_OPENAI_COMPLETION_MODEL.to_owned(),
        provider: provider_name.clone(),
        context_window: Some(128_000),
        max_output_tokens: Some(16_000),
        supports_tool_use: true,
        supports_reasoning: false,
    });
    push_unique(ModelMetadata {
        id: "codex-mini-latest".to_owned(),
        provider: provider_name,
        context_window: Some(128_000),
        max_output_tokens: Some(16_000),
        supports_tool_use: true,
        supports_reasoning: true,
    });

    models
}

pub fn compatibility_model_catalog(provider: ApiProvider) -> StaticModelCatalog {
    StaticModelCatalog::new(compatibility_models_for(provider))
}

// ---------------------------------------------------------------------------
// Model overrides and thinking configuration (parity with TS)
// ---------------------------------------------------------------------------

/// Default OpenAI reasoning model (used for thinking-enabled turns).
pub const DEFAULT_OPENAI_REASONING_MODEL: &str = "gpt-5.4";

/// Default OpenAI completion model (used for standard/utility turns).
pub const DEFAULT_OPENAI_COMPLETION_MODEL: &str = "gpt-5.3-codex";

/// Default max thinking token budget for Claude models.
pub const DEFAULT_MAX_THINKING_TOKENS: u64 = 10_000;

/// Valid think-level values for OpenAI reasoning effort.
pub const OPENAI_THINK_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

/// Returns the OpenAI reasoning model, honouring the `REASONING_MODEL` env var.
pub fn get_openai_reasoning_model() -> String {
    env::var("REASONING_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_REASONING_MODEL.to_owned())
}

/// Returns the OpenAI completion model, honouring the `COMPLETION_MODEL` env var.
pub fn get_openai_completion_model() -> String {
    env::var("COMPLETION_MODEL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_COMPLETION_MODEL.to_owned())
}

/// Parse a think-level env var value into a validated level string.
fn parse_think_level(value: Option<String>, fallback: &str) -> String {
    match value
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
    {
        Some(v) if OPENAI_THINK_LEVELS.contains(&v.as_str()) => v,
        Some(_) => fallback.to_owned(),
        None => fallback.to_owned(),
    }
}

/// Returns the reasoning think level from `REASONING_MODEL_THINK` (default: `xhigh`).
pub fn get_openai_reasoning_think_level() -> String {
    parse_think_level(env::var("REASONING_MODEL_THINK").ok(), "xhigh")
}

/// Returns the completion think level from `COMPLETION_MODEL_THINK` (default: `xhigh`).
pub fn get_openai_completion_think_level() -> String {
    parse_think_level(env::var("COMPLETION_MODEL_THINK").ok(), "xhigh")
}

// ---------------------------------------------------------------------------
// Claude-specific thinking helpers (parity with TS thinking.ts)
// ---------------------------------------------------------------------------

/// Check if a Claude model supports thinking at all.
/// Claude 4+ models support thinking. Claude 3.x models do not.
pub fn model_supports_thinking(model: &str, provider: ApiProvider) -> bool {
    let lower = model.to_lowercase();
    match provider {
        // OpenAI family always supports "thinking" via reasoning_effort
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => true,
        // 1P and Foundry: all Claude 4+ models
        ApiProvider::FirstParty | ApiProvider::Foundry => !lower.contains("claude-3-"),
        // 3P (Bedrock / Vertex): only Opus 4+ and Sonnet 4+
        ApiProvider::Bedrock | ApiProvider::Vertex => {
            lower.contains("sonnet-4") || lower.contains("opus-4")
        }
    }
}

/// Check if a Claude model supports adaptive thinking (newer 4.6+ models).
pub fn model_supports_adaptive_thinking(model: &str, provider: ApiProvider) -> bool {
    match provider {
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => true,
        _ => {
            let lower = model.to_lowercase();
            if lower.contains("opus-4-6") || lower.contains("sonnet-4-6") {
                return true;
            }
            // Known legacy models do not support adaptive
            if lower.contains("opus") || lower.contains("sonnet") || lower.contains("haiku") {
                return false;
            }
            // Default: true for 1P/Foundry (newer models), false for 3P
            matches!(provider, ApiProvider::FirstParty | ApiProvider::Foundry)
        }
    }
}

/// Resolve the `ThinkingConfig` for a Claude request.
///
/// Reads `CLAUDE_CODE_DISABLE_THINKING`, `CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING`,
/// and `MAX_THINKING_TOKENS` to determine the correct thinking config.
pub fn resolve_claude_thinking_config(
    model: &str,
    provider: ApiProvider,
    max_output_tokens: u64,
) -> ThinkingConfig {
    if env_flag_truthy("CLAUDE_CODE_DISABLE_THINKING") {
        return ThinkingConfig::Disabled;
    }
    if !model_supports_thinking(model, provider) {
        return ThinkingConfig::Disabled;
    }

    if !env_flag_truthy("CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING")
        && model_supports_adaptive_thinking(model, provider)
    {
        return ThinkingConfig::Adaptive;
    }

    // Budget-based thinking for older models
    let budget = env::var("MAX_THINKING_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_THINKING_TOKENS);
    let clamped = budget.min(max_output_tokens.saturating_sub(1));
    ThinkingConfig::Enabled {
        budget_tokens: clamped,
    }
}

/// Resolve the model to use for a given request.
///
/// If the user has explicitly set `--model`, that value is always used.
/// For OpenAI-family providers (when no explicit model is set), the model is
/// split based on whether thinking is enabled:
/// - thinking enabled → `REASONING_MODEL` (default `gpt-5.4`)
/// - thinking disabled → `COMPLETION_MODEL` (default `gpt-5.3-codex`)
///
/// For Claude providers, `REASONING_MODEL`/`COMPLETION_MODEL` are also respected
/// if set, but no split is applied by default (Claude uses a single model).
pub fn resolve_active_model(
    provider: ApiProvider,
    model: &str,
    user_specified_model: bool,
    thinking_enabled: bool,
) -> String {
    if user_specified_model {
        return model.to_owned();
    }

    match provider {
        ApiProvider::OpenAI | ApiProvider::ChatGPTCodex | ApiProvider::OpenAICompatible => {
            // OpenAI providers split between reasoning and completion models
            if thinking_enabled {
                get_openai_reasoning_model()
            } else {
                get_openai_completion_model()
            }
        }
        _ => {
            // Claude providers: honour REASONING_MODEL/COMPLETION_MODEL if
            // explicitly set, otherwise use the catalog default.
            if thinking_enabled {
                if let Ok(v) = env::var("REASONING_MODEL") {
                    if !v.trim().is_empty() {
                        return v;
                    }
                }
            } else if let Ok(v) = env::var("COMPLETION_MODEL") {
                if !v.trim().is_empty() {
                    return v;
                }
            }
            model.to_owned()
        }
    }
}

/// Returns the OpenAI reasoning effort level for a resolved model.
pub async fn collect_provider_response(
    provider: &dyn Provider,
    request: ProviderRequest,
) -> Result<CollectedProviderResponse> {
    let mut stream = provider.start_stream(request).await?;
    let mut collected = CollectedProviderResponse::default();

    while let Some(event) = stream.next_event().await? {
        match event {
            ProviderEvent::MessageDelta { text } => collected.text.push_str(&text),
            ProviderEvent::ToolCall { call } => collected.tool_calls.push(call),
            ProviderEvent::Usage { usage } => collected.usage = Some(usage),
            ProviderEvent::Stop { reason } => {
                collected.stop_reason = Some(reason);
                break;
            }
            ProviderEvent::ToolCallBoundary { .. } | ProviderEvent::Error { .. } => {}
        }
    }

    Ok(collected)
}

pub async fn collect_provider_text(
    provider: &dyn Provider,
    request: ProviderRequest,
) -> Result<(String, Option<TokenUsage>)> {
    let collected = collect_provider_response(provider, request).await?;
    Ok((collected.text, collected.usage))
}

