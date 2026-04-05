fn input_string(input: &Value, key: &str) -> Result<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing string field '{key}'"))
}

fn input_string_or(input: &Value, key: &str, default: &str) -> String {
    input
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}

fn input_bool_or(input: &Value, key: &str, default: bool) -> bool {
    input.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn input_u64_or(input: &Value, key: &str, default: u64) -> u64 {
    input.get(key).and_then(Value::as_u64).unwrap_or(default)
}

fn optional_string(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn shell_command_input(input: &Value) -> Result<String> {
    optional_string(input, "command")
        .or_else(|| optional_string(input, "cmd"))
        .or_else(|| optional_string(input, "script"))
        .or_else(|| optional_string(input, "input"))
        .or_else(|| optional_string(input, "prompt"))
        .or_else(|| input.as_str().map(str::to_owned))
        .ok_or_else(|| anyhow!("missing string field 'command'"))
}

fn parse_tool_input<T>(input: Value) -> Result<T>
where
    T: DeserializeOwned,
{
    Ok(serde_json::from_value(input)?)
}

fn file_read_input(input: Value) -> Result<FileReadToolInput> {
    match input {
        Value::String(path) => Ok(FileReadToolInput { path }),
        other => parse_tool_input(other),
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
struct FileReadToolInput {
    #[serde(alias = "filePath")]
    path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
struct FileWriteToolInput {
    #[serde(alias = "filePath")]
    path: String,
    content: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
struct FileEditToolInput {
    #[serde(alias = "filePath")]
    path: String,
    #[serde(alias = "oldString", alias = "old_str")]
    old_string: String,
    #[serde(alias = "newString", alias = "new_str")]
    new_string: String,
    #[serde(default, alias = "replaceAll")]
    replace_all: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
struct ShellCommandToolInput {
    command: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
struct TerminalCaptureToolInput {
    action: Option<String>,
    command: Option<String>,
    id: Option<String>,
    shell: Option<String>,
}

fn string_list_field(input: &Value, key: &str) -> Result<Vec<String>> {
    let Some(value) = input.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("field '{key}' must be an array"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("field '{key}' entries must be strings"))
        })
        .collect()
}

fn string_map_field(input: &Value, key: &str) -> Result<BTreeMap<String, String>> {
    let Some(value) = input.get(key) else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("field '{key}' must be an object"))?;
    let mut map = BTreeMap::new();
    for (entry_key, entry_value) in object {
        let entry_value = entry_value
            .as_str()
            .ok_or_else(|| anyhow!("field '{key}.{entry_key}' must be a string"))?;
        map.insert(entry_key.clone(), entry_value.to_owned());
    }
    Ok(map)
}

fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn runtime_dir(cwd: &Path) -> PathBuf {
    cwd.join(".code-agent")
}

fn task_store(cwd: &Path) -> LocalTaskStore {
    LocalTaskStore::new(runtime_dir(cwd))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    Ok(())
}

fn append_jsonl(path: &Path, value: &Value) -> Result<()> {
    ensure_parent_dir(path)?;
    let mut content = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };
    content.push_str(&serde_json::to_string(value)?);
    content.push('\n');
    fs::write(path, content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn strip_html_tags(input: &str) -> String {
    let mut text = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn load_mcp_server_config(cwd: &Path, input: &Value) -> Result<McpServerConfig> {
    let root = input
        .get("plugin_root")
        .and_then(Value::as_str)
        .map(|value| resolve_path(cwd, value))
        .unwrap_or_else(|| cwd.to_path_buf());
    let server_name = input_string(input, "server")?;
    let runtime = OutOfProcessPluginRuntime;
    let loaded = runtime
        .load_manifest(&root)
        .await
        .with_context(|| format!("failed to load plugin manifest from {}", root.display()))?;
    let servers = parse_mcp_server_configs(&loaded.manifest.mcp_servers);
    servers
        .get(&server_name)
        .cloned()
        .ok_or_else(|| anyhow!("unknown MCP server '{server_name}' in {}", root.display()))
}

fn collect_files(base: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if base.is_file() {
        files.push(base.to_path_buf());
        return Ok(());
    }

    if !base.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(base).with_context(|| format!("failed to read {}", base.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn normalize_for_match(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }

    if pattern.starts_with(b"**") {
        let rest = &pattern[2..];
        if rest.is_empty() {
            return true;
        }
        for skip in 0..=text.len() {
            if wildcard_match(rest, &text[skip..]) {
                return true;
            }
        }
        return false;
    }

    match pattern[0] {
        b'*' => {
            if wildcard_match(&pattern[1..], text) {
                return true;
            }
            let mut idx = 0usize;
            while idx < text.len() && text[idx] != b'/' {
                idx += 1;
                if wildcard_match(&pattern[1..], &text[idx..]) {
                    return true;
                }
            }
            false
        }
        b'?' => !text.is_empty() && text[0] != b'/' && wildcard_match(&pattern[1..], &text[1..]),
        ch => !text.is_empty() && ch == text[0] && wildcard_match(&pattern[1..], &text[1..]),
    }
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    wildcard_match(pattern.as_bytes(), path.as_bytes())
}
