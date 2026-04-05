#[derive(Clone, Debug, Default)]
struct PromptFrontmatter {
    argument_names: Vec<String>,
}

#[derive(Clone, Debug)]
struct ResolvedPromptCommand {
    content: String,
    base_dir: Option<PathBuf>,
    plugin_root: PathBuf,
    argument_names: Vec<String>,
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

fn parse_prompt_frontmatter(markdown: &str) -> (PromptFrontmatter, String) {
    let normalized = markdown.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return (PromptFrontmatter::default(), normalized);
    };
    let Some(end_index) = rest.find("\n---\n") else {
        return (PromptFrontmatter::default(), normalized);
    };

    let frontmatter_text = &rest[..end_index];
    let body = rest[end_index + "\n---\n".len()..].to_owned();
    let mut frontmatter = PromptFrontmatter::default();
    let mut list_key: Option<&str> = None;

    for raw_line in frontmatter_text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some(item) = line.strip_prefix("- ") {
            match list_key {
                Some("arguments") => frontmatter.argument_names.push(strip_matching_quotes(item)),
                _ => {}
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

        match key {
            "arguments" => {
                if value.is_empty() {
                    list_key = Some("arguments");
                } else {
                    frontmatter.argument_names = parse_argument_names(value);
                }
            }
            _ => {}
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

fn substitute_prompt_arguments(content: &str, args: Option<&str>, argument_names: &[String]) -> String {
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
        result = result.replace(&format!("${name}"), parsed_args.get(index).map(String::as_str).unwrap_or(""));
    }

    let replaced = result.replace("$ARGUMENTS", args);
    if replaced == content && !args.trim().is_empty() {
        format!("{replaced}\n\nARGUMENTS: {args}")
    } else {
        replaced
    }
}

fn invocation_argument_string(invocation: &CommandInvocation) -> Option<String> {
    let raw = invocation.raw_input.trim();
    let without_slash = raw.strip_prefix('/')?;
    let split_index = without_slash.find(char::is_whitespace)?;
    let rest = &without_slash[split_index..];
    let rest = rest.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

fn load_prompt_command_from_path(path: &Path, plugin_root: &Path) -> Option<ResolvedPromptCommand> {
    let content = safe_read_text(path)?;
    let (frontmatter, body) = parse_prompt_frontmatter(&content);
    let base_dir = (path.file_name().and_then(|value| value.to_str()) == Some(SKILL_FILE_NAME))
        .then(|| path.parent().map(Path::to_path_buf))
        .flatten();

    Some(ResolvedPromptCommand {
        content: body,
        base_dir,
        plugin_root: plugin_root.to_path_buf(),
        argument_names: frontmatter.argument_names,
    })
}

fn resolve_inline_manifest_prompt_command(root: &Path, command_name: &str) -> Option<ResolvedPromptCommand> {
    let manifest = load_plugin_manifest_sync(root)?;
    let CommandDefinitions::Mapping(entries) = manifest.commands? else {
        return None;
    };
    let metadata = entries.get(command_name)?;
    let content = metadata.content.as_ref()?;
    let (frontmatter, body) = parse_prompt_frontmatter(content);

    Some(ResolvedPromptCommand {
        content: body,
        base_dir: None,
        plugin_root: root.to_path_buf(),
        argument_names: frontmatter.argument_names,
    })
}

fn resolve_prompt_command_definition(
    spec: &CommandSpec,
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
) -> Option<ResolvedPromptCommand> {
    if spec.source == CommandSource::BuiltIn {
        return None;
    }

    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);

    if let Some(origin) = spec.origin.as_deref() {
        let path = PathBuf::from(origin);
        if path.is_file() {
            return load_prompt_command_from_path(&path, &root);
        }
    }

    resolve_inline_manifest_prompt_command(&root, &spec.name)
}

fn expand_prompt_command(
    command: ResolvedPromptCommand,
    args: Option<&str>,
    session_id: SessionId,
) -> String {
    let mut content = command.content;

    if let Some(base_dir) = &command.base_dir {
        content = format!("Base directory for this skill: {}\n\n{content}", base_dir.display());
        content = content.replace("${CLAUDE_SKILL_DIR}", &base_dir.display().to_string());
    }

    content = content.replace(
        "${CLAUDE_PLUGIN_ROOT}",
        &command.plugin_root.display().to_string(),
    );
    content = content.replace(
        "${CLAUDE_SESSION_ID}",
        &session_id.to_string(),
    );

    substitute_prompt_arguments(&content, args, &command.argument_names)
}

fn resolve_prompt_command_prompt(
    registry: &CommandRegistry,
    invocation: &CommandInvocation,
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
    session_id: SessionId,
) -> Result<Option<String>> {
    let Some(spec) = registry.resolve(&invocation.name) else {
        return Ok(None);
    };
    let Some(command) = resolve_prompt_command_definition(spec, cwd, plugin_root) else {
        return Ok(None);
    };

    Ok(Some(expand_prompt_command(
        command,
        invocation_argument_string(invocation).as_deref(),
        session_id,
    )))
}