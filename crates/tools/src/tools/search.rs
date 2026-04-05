#[derive(Clone, Debug)]
struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "grep",
            "Search text in the workspace.",
            ToolKind::Search,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let pattern = input_string(&input, "pattern")?;
        let base = resolve_path(&context.cwd, &input_string_or(&input, "path", "."));
        let mut files = Vec::new();
        collect_files(&base, &mut files)?;

        let mut rendered = Vec::new();
        let mut matches = Vec::new();
        for path in files {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            for (index, line) in content.lines().enumerate() {
                if line.contains(&pattern) {
                    let relative = path.strip_prefix(&context.cwd).unwrap_or(&path);
                    rendered.push(format!("{}:{}:{}", relative.display(), index + 1, line));
                    matches.push(json!({
                        "path": relative,
                        "line": index + 1,
                    }));
                }
            }
        }

        Ok(ToolOutput {
            content: rendered.join("\n"),
            is_error: false,
            metadata: json!({
                "pattern": pattern,
                "match_count": matches.len(),
                "matches": matches,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "glob",
            "Expand glob patterns against the workspace.",
            ToolKind::Search,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let pattern = input_string(&input, "pattern")?;
        let base = resolve_path(&context.cwd, &input_string_or(&input, "base", "."));
        let mut files = Vec::new();
        collect_files(&base, &mut files)?;

        let mut matches = Vec::new();
        for path in files {
            let relative = path.strip_prefix(&context.cwd).unwrap_or(&path);
            let normalized = normalize_for_match(relative);
            if glob_matches(&pattern, &normalized) {
                matches.push(normalized);
            }
        }
        matches.sort();

        Ok(ToolOutput {
            content: matches.join("\n"),
            is_error: false,
            metadata: json!({
                "pattern": pattern,
                "match_count": matches.len(),
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "todo_write",
            "Write structured todo state for the current session.",
            ToolKind::Session,
            false,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let todos = input
            .get("items")
            .cloned()
            .or_else(|| input.get("todos").cloned())
            .unwrap_or_else(|| json!([]));
        if !todos.is_array() {
            bail!("todo_write expects an array field named 'items' or 'todos'");
        }
        let path = context.cwd.join(".code-agent").join("todos.json");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::write(&path, serde_json::to_vec_pretty(&todos)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(ToolOutput {
            content: format!("wrote {}", path.display()),
            is_error: false,
            metadata: json!({ "path": path, "count": todos.as_array().map(Vec::len).unwrap_or_default() }),
        })
    }
}

#[derive(Clone, Debug)]
struct MemoryTool;

#[async_trait]
impl Tool for MemoryTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "memory",
            "Read or update durable memory state.",
            ToolKind::Session,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = context.cwd.join(".code-agent").join("memory.json");
        let action = input_string_or(&input, "action", "read");
        match action.as_str() {
            "read" => {
                let content = match fs::read_to_string(&path) {
                    Ok(content) => content,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".to_owned(),
                    Err(error) => {
                        return Err(error)
                            .with_context(|| format!("failed to read {}", path.display()))
                    }
                };
                Ok(ToolOutput {
                    content,
                    is_error: false,
                    metadata: json!({ "path": path, "action": "read" }),
                })
            }
            "write" => {
                let value = input
                    .get("value")
                    .cloned()
                    .or_else(|| input.get("memory").cloned())
                    .unwrap_or_else(|| json!({}));
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                let content = serde_json::to_string_pretty(&value)?;
                fs::write(&path, content.as_bytes())
                    .with_context(|| format!("failed to write {}", path.display()))?;
                Ok(ToolOutput {
                    content: format!("wrote {}", path.display()),
                    is_error: false,
                    metadata: json!({ "path": path, "action": "write" }),
                })
            }
            other => bail!("unsupported memory action: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
struct McpTool;

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "mcp",
            "Call a registered MCP tool.",
            ToolKind::Mcp,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let tool_name = input_string(&input, "tool")?;
        let arguments = input.get("arguments").cloned().unwrap_or_else(|| json!({}));
        let result = call_tool_from_config(&config, &tool_name, arguments).await?;

        Ok(ToolOutput {
            content: result.content_text,
            is_error: result.is_error,
            metadata: json!({
                "server": config.name,
                "tool": tool_name,
                "raw": result.raw,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct ListMcpResourcesTool;

#[async_trait]
impl Tool for ListMcpResourcesTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "list_mcp_resources",
            "List resources exposed by MCP servers.",
            ToolKind::Mcp,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let resources = list_resources_from_config(&config).await?;
        let content = resources
            .iter()
            .map(|resource| resource.uri.clone())
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            content,
            is_error: false,
            metadata: serde_json::to_value(&resources)?,
        })
    }
}

#[derive(Clone, Debug)]
struct ReadMcpResourceTool;

#[async_trait]
impl Tool for ReadMcpResourceTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "read_mcp_resource",
            "Read a resource exposed by an MCP server.",
            ToolKind::Mcp,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let config = load_mcp_server_config(&context.cwd, &input).await?;
        let uri = input_string(&input, "uri")?;
        let result = read_resource_from_config(&config, &uri).await?;
        Ok(ToolOutput {
            content: result.content_text,
            is_error: false,
            metadata: json!({
                "server": config.name,
                "uri": uri,
                "raw": result.raw,
            }),
        })
    }
}
