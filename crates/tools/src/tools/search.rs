#[derive(Clone, Debug)]
struct GrepTool;

#[derive(Clone, Debug)]
struct GrepCompatTool;

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

#[async_trait]
impl Tool for GrepCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Grep".to_owned(),
            description: "Search text in the workspace.".to_owned(),
            kind: ToolKind::Search,
            input_schema: schemars::schema_for!(GrepToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("grep", input, context).await
    }
}

#[derive(Clone, Debug)]
struct GlobTool;

#[derive(Clone, Debug)]
struct GlobCompatTool;

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

#[async_trait]
impl Tool for GlobCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Glob".to_owned(),
            description: "Expand glob patterns against the workspace.".to_owned(),
            kind: ToolKind::Search,
            input_schema: schemars::schema_for!(GlobToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("glob", input, context).await
    }
}

#[derive(Clone, Debug)]
struct TodoWriteTool;

#[derive(Clone, Debug)]
struct TodoWriteCompatTool;

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
        let path = context.cwd.join(".claude").join("todos.json");
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

#[async_trait]
impl Tool for TodoWriteCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TodoWrite".to_owned(),
            description: "Write structured todo state for the current session.".to_owned(),
            kind: ToolKind::Session,
            input_schema: schemars::schema_for!(TodoWriteCompatInput),
            read_only: false,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("todo_write", input, context).await
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
        let path = context.cwd.join(".claude").join("memory.json");
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
        let config = load_mcp_server_config(context, &input).await?;
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

#[derive(Clone, Debug)]
struct ListMcpResourcesCompatTool;

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
        let config = load_mcp_server_config(context, &input).await?;
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

#[async_trait]
impl Tool for ListMcpResourcesCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ListMcpResourcesTool".to_owned(),
            description: "List resources exposed by MCP servers.".to_owned(),
            kind: ToolKind::Mcp,
            input_schema: schemars::schema_for!(McpResourceListToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("list_mcp_resources", input, context).await
    }
}

#[derive(Clone, Debug)]
struct ReadMcpResourceTool;

#[derive(Clone, Debug)]
struct ReadMcpResourceCompatTool;

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
        let config = load_mcp_server_config(context, &input).await?;
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

#[async_trait]
impl Tool for ReadMcpResourceCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ReadMcpResourceTool".to_owned(),
            description: "Read a resource exposed by an MCP server.".to_owned(),
            kind: ToolKind::Mcp,
            input_schema: schemars::schema_for!(McpResourceReadToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("read_mcp_resource", input, context).await
    }
}

#[derive(Clone, Debug)]
struct ToolSearchTool;

#[async_trait]
impl Tool for ToolSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ToolSearch".to_owned(),
            description: "Search the compatibility tool surface by name or description.".to_owned(),
            kind: ToolKind::Search,
            input_schema: schemars::schema_for!(ToolSearchToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, _context: &ToolContext) -> Result<ToolOutput> {
        let ToolSearchToolInput { query, max_results } = parse_tool_input(input)?;
        let query = query.trim().to_owned();
        let normalized_query = query.to_ascii_lowercase();
        let specs = compatibility_tool_registry().specs();

        if let Some(selection) = normalized_query.strip_prefix("select:") {
            let requested = selection
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            let matches = requested
                .into_iter()
                .filter_map(|requested_name| {
                    specs.iter()
                        .find(|spec| spec.name.eq_ignore_ascii_case(requested_name))
                        .map(|spec| spec.name.clone())
                })
                .collect::<Vec<_>>();

            return Ok(ToolOutput {
                content: matches.join("\n"),
                is_error: false,
                metadata: json!({
                    "query": query,
                    "matches": matches,
                    "total_deferred_tools": 0,
                }),
            });
        }

        let mut matches = specs
            .into_iter()
            .filter(|spec| spec.name != "ToolSearch")
            .filter_map(|spec| {
                let score = if spec.name.eq_ignore_ascii_case(&normalized_query) {
                    0
                } else if spec.name.to_ascii_lowercase().contains(&normalized_query) {
                    1
                } else if spec
                    .description
                    .to_ascii_lowercase()
                    .contains(&normalized_query)
                {
                    2
                } else {
                    return None;
                };
                Some((score, spec.name))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.cmp(right));
        matches.truncate(max_results as usize);
        let names = matches
            .into_iter()
            .map(|(_, name)| name)
            .collect::<Vec<_>>();

        Ok(ToolOutput {
            content: names.join("\n"),
            is_error: false,
            metadata: json!({
                "query": query,
                "matches": names,
                "total_deferred_tools": 0,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct SkillTool;

#[derive(Clone, Debug, Default)]
struct SkillPromptFrontmatter {
    argument_names: Vec<String>,
}

fn resolve_skill_tool_root(cwd: &Path, plugin_root: Option<&str>) -> PathBuf {
    match plugin_root {
        Some(value) if !value.trim().is_empty() => resolve_path(cwd, value),
        _ => cwd.to_path_buf(),
    }
}

fn skill_tool_project_roots(cwd: &Path, plugin_root: Option<&str>) -> Vec<PathBuf> {
    if plugin_root.is_some_and(|value| !value.trim().is_empty()) {
        vec![resolve_skill_tool_root(cwd, plugin_root)]
    } else {
        ccrust_plugins::legacy_skill_search_roots(cwd, &ccrust_session::claude_config_home_dir())
    }
}

fn extend_unique_skill_entries(
    skills: &mut Vec<ccrust_plugins::SkillEntry>,
    seen: &mut std::collections::BTreeSet<PathBuf>,
    entries: Vec<ccrust_plugins::SkillEntry>,
) {
    for entry in entries {
        if seen.insert(entry.path.clone()) {
            skills.push(entry);
        }
    }
}

async fn resolved_skill_tool_entries(
    cwd: &Path,
    plugin_root: Option<&str>,
) -> Result<Vec<ccrust_plugins::SkillEntry>> {
    let runtime = OutOfProcessPluginRuntime;
    let root = resolve_skill_tool_root(cwd, plugin_root);
    let home = ccrust_session::claude_config_home_dir();
    let project_roots = skill_tool_project_roots(cwd, plugin_root);
    let mut seen = std::collections::BTreeSet::new();
    let mut skills = Vec::new();

    if home != root {
        extend_unique_skill_entries(
            &mut skills,
            &mut seen,
            ccrust_plugins::discover_legacy_skill_entries(&home, "skills", "commands")
                .unwrap_or_default(),
        );
    }

    extend_unique_skill_entries(
        &mut skills,
        &mut seen,
        ccrust_plugins::discover_legacy_skill_entries(
            &root,
            ccrust_plugins::LEGACY_SKILLS_DIR,
            ccrust_plugins::LEGACY_COMMANDS_DIR,
        )
        .unwrap_or_default(),
    );

    for search_root in project_roots.into_iter().skip(1) {
        extend_unique_skill_entries(
            &mut skills,
            &mut seen,
            ccrust_plugins::discover_legacy_skill_entries(
                &search_root,
                ccrust_plugins::LEGACY_SKILLS_DIR,
                ccrust_plugins::LEGACY_COMMANDS_DIR,
            )
            .unwrap_or_default(),
        );
    }

    extend_unique_skill_entries(&mut skills, &mut seen, runtime.discover_skills(&root).await?);

    Ok(skills)
}

fn strip_matching_quotes(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let quoted = (trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\''));
        if quoted {
            return trimmed[1..trimmed.len() - 1].trim().to_owned();
        }
    }
    trimmed.to_owned()
}

fn parse_inline_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let raw = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);

    raw.split(',')
        .map(strip_matching_quotes)
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_argument_names(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let items = if trimmed.starts_with('[') {
        parse_inline_list(trimmed)
    } else {
        trimmed
            .split_whitespace()
            .map(strip_matching_quotes)
            .collect::<Vec<_>>()
    };

    items
        .into_iter()
        .filter(|value| !value.is_empty() && !value.chars().all(|ch| ch.is_ascii_digit()))
        .collect()
}

fn parse_skill_prompt_frontmatter(markdown: &str) -> (SkillPromptFrontmatter, String) {
    let normalized = markdown.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return (SkillPromptFrontmatter::default(), normalized);
    };
    let Some(end_index) = rest.find("\n---\n") else {
        return (SkillPromptFrontmatter::default(), normalized);
    };

    let frontmatter_text = &rest[..end_index];
    let body = rest[end_index + "\n---\n".len()..].to_owned();
    let mut frontmatter = SkillPromptFrontmatter::default();
    let mut list_key: Option<&str> = None;

    for raw_line in frontmatter_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(item) = line.strip_prefix("- ") {
            if list_key == Some("arguments") {
                frontmatter.argument_names.push(strip_matching_quotes(item));
            }
            continue;
        }

        let Some((key, value)) = line.split_once(':') else {
            list_key = None;
            continue;
        };

        let key = key.trim();
        let value = value.trim();
        list_key = None;

        if key == "arguments" {
            if value.is_empty() {
                list_key = Some("arguments");
            } else {
                frontmatter.argument_names = parse_argument_names(value);
            }
        }
    }

    frontmatter.argument_names.retain(|value| !value.is_empty());
    (frontmatter, body)
}

fn parse_shell_like_arguments(args: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in args.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' | '\'' => {
                if let Some(active) = quote {
                    if active == ch {
                        quote = None;
                    } else {
                        current.push(ch);
                    }
                } else {
                    quote = Some(ch);
                }
            }
            ch if ch.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    values.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }

    if escaped || quote.is_some() {
        return args
            .split_whitespace()
            .map(str::to_owned)
            .filter(|value| !value.is_empty())
            .collect();
    }

    if !current.is_empty() {
        values.push(current);
    }

    values
}

fn substitute_skill_arguments(content: &str, args: Option<&str>, argument_names: &[String]) -> String {
    let Some(args) = args else {
        return content.to_owned();
    };

    let parsed_args = parse_shell_like_arguments(args);
    let mut result = content.to_owned();

    for (index, value) in parsed_args.iter().enumerate() {
        result = result.replace(&format!("$ARGUMENTS[{index}]"), value);
        result = result.replace(&format!("${index}"), value);
    }

    for (index, name) in argument_names.iter().enumerate() {
        result = result.replace(
            &format!("${name}"),
            parsed_args.get(index).map(String::as_str).unwrap_or(""),
        );
    }

    let replaced = result.replace("$ARGUMENTS", args);
    if replaced == content && !args.trim().is_empty() {
        format!("{replaced}\n\nARGUMENTS: {args}")
    } else {
        replaced
    }
}

fn rendered_skill_prompt(
    entry: &ccrust_plugins::SkillEntry,
    root: &Path,
    args: Option<&str>,
    session_id: Option<SessionId>,
) -> Result<String> {
    let content = fs::read_to_string(&entry.path)
        .with_context(|| format!("failed to read {}", entry.path.display()))?;
    let (frontmatter, mut body) = parse_skill_prompt_frontmatter(&content);

    if entry.path.file_name().and_then(|value| value.to_str()) == Some(ccrust_plugins::SKILL_FILE_NAME)
    {
        if let Some(base_dir) = entry.path.parent() {
            let base_dir_display = base_dir.display().to_string();
            body = format!("Base directory for this skill: {base_dir_display}\n\n{body}");
            body = body.replace("${CLAUDE_SKILL_DIR}", &base_dir_display);
        }
    }

    body = body.replace("${CLAUDE_PLUGIN_ROOT}", &root.display().to_string());
    body = body.replace(
        "${CLAUDE_SESSION_ID}",
        &session_id.map(|value| value.to_string()).unwrap_or_default(),
    );

    Ok(substitute_skill_arguments(
        &body,
        args.filter(|value| !value.trim().is_empty()),
        &frontmatter.argument_names,
    ))
}

#[async_trait]
impl Tool for SkillTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Skill".to_owned(),
            description: "Load a discovered skill prompt by name and return its contents.".to_owned(),
            kind: ToolKind::Agent,
            input_schema: schemars::schema_for!(SkillToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let SkillToolInput {
            skill,
            args,
            plugin_root,
        } = parse_tool_input(input)?;
        let root = resolve_skill_tool_root(&context.cwd, plugin_root.as_deref());
        let normalized = skill.trim().trim_start_matches('/');
        let skills = resolved_skill_tool_entries(&context.cwd, plugin_root.as_deref()).await?;
        let entry = skills
            .into_iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(normalized))
            .ok_or_else(|| anyhow!("unknown skill: {normalized}"))?;
        let content = rendered_skill_prompt(&entry, &root, args.as_deref(), context.session_id)?;

        Ok(ToolOutput {
            content,
            is_error: false,
            metadata: json!({
                "skill": entry.name,
                "path": entry.path,
                "source": format!("{:?}", entry.source),
            }),
        })
    }
}
