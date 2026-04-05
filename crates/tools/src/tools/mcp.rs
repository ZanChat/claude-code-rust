#[derive(Clone, Debug)]
struct McpAuthTool;

#[async_trait]
impl Tool for McpAuthTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "mcp_auth",
            "Authenticate or refresh MCP credentials.",
            ToolKind::Mcp,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let action = input_string_or(&input, "action", "status");
        match action.as_str() {
            "status" => {
                let cached = load_cached_auth_token(&config)?;
                let pending = load_pending_device_flow(&config)?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&json!({
                        "server": config.name,
                        "auth": config.auth,
                        "cached": cached,
                        "pending": pending,
                    }))?,
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "has_cached_token": cached.is_some(),
                        "has_pending_device_flow": pending.is_some(),
                    }),
                })
            }
            "set_token" => {
                let access_token = input_string(&input, "access_token")?;
                let cached = CachedMcpAuthToken {
                    access_token,
                    refresh_token: optional_string(&input, "refresh_token"),
                    token_type: optional_string(&input, "token_type"),
                    expires_at_unix_ms: input.get("expires_at_unix_ms").and_then(Value::as_i64),
                };
                let path = store_cached_auth_token(&config, &cached)?;
                Ok(ToolOutput {
                    content: format!("stored MCP auth token for {}", config.name),
                    is_error: false,
                    metadata: json!({ "server": config.name, "path": path }),
                })
            }
            "login" => {
                if input.get("access_token").is_some() {
                    let access_token = input_string(&input, "access_token")?;
                    let cached = CachedMcpAuthToken {
                        access_token,
                        refresh_token: optional_string(&input, "refresh_token"),
                        token_type: optional_string(&input, "token_type"),
                        expires_at_unix_ms: input.get("expires_at_unix_ms").and_then(Value::as_i64),
                    };
                    let path = store_cached_auth_token(&config, &cached)?;
                    return Ok(ToolOutput {
                        content: format!("stored MCP auth token for {}", config.name),
                        is_error: false,
                        metadata: json!({ "server": config.name, "path": path }),
                    });
                }
                match config.auth {
                    Some(McpAuthConfig::OAuthDevice { .. }) => {
                        let flow = start_oauth_device_flow(&config).await?;
                        Ok(ToolOutput {
                            content: serde_json::to_string_pretty(&flow)?,
                            is_error: false,
                            metadata: json!({
                                "server": config.name,
                                "device_code": flow.device_code,
                                "verification_uri": flow.verification_uri,
                                "verification_uri_complete": flow.verification_uri_complete,
                            }),
                        })
                    }
                    _ => bail!(
                        "mcp auth login requires an access_token unless the server uses oauth_device auth"
                    ),
                }
            }
            "poll" | "poll_device" => {
                let token = poll_oauth_device_flow(
                    &config,
                    optional_string(&input, "device_code").as_deref(),
                )
                .await?;
                Ok(ToolOutput {
                    content: format!("stored MCP auth token for {}", config.name),
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "access_token": token.access_token,
                        "refresh_token": token.refresh_token,
                        "token_type": token.token_type,
                        "expires_at_unix_ms": token.expires_at_unix_ms,
                    }),
                })
            }
            "refresh" => {
                if input.get("access_token").is_some() {
                    let access_token = input_string(&input, "access_token")?;
                    let cached = CachedMcpAuthToken {
                        access_token,
                        refresh_token: optional_string(&input, "refresh_token"),
                        token_type: optional_string(&input, "token_type"),
                        expires_at_unix_ms: input.get("expires_at_unix_ms").and_then(Value::as_i64),
                    };
                    let path = store_cached_auth_token(&config, &cached)?;
                    return Ok(ToolOutput {
                        content: format!("stored MCP auth token for {}", config.name),
                        is_error: false,
                        metadata: json!({ "server": config.name, "path": path }),
                    });
                }
                let token = refresh_oauth_device_token(&config).await?;
                Ok(ToolOutput {
                    content: format!("refreshed MCP auth token for {}", config.name),
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "access_token": token.access_token,
                        "refresh_token": token.refresh_token,
                        "token_type": token.token_type,
                        "expires_at_unix_ms": token.expires_at_unix_ms,
                    }),
                })
            }
            "clear" | "logout" => {
                let cleared = clear_cached_auth_token(&config)?;
                let pending = clear_pending_device_flow(&config)?;
                Ok(ToolOutput {
                    content: if cleared {
                        format!("cleared MCP auth token for {}", config.name)
                    } else {
                        format!("no MCP auth token cached for {}", config.name)
                    },
                    is_error: false,
                    metadata: json!({
                        "server": config.name,
                        "cleared": cleared,
                        "cleared_pending_device_flow": pending,
                    }),
                })
            }
            other => bail!("unsupported mcp_auth action: {other}"),
        }
    }
}

