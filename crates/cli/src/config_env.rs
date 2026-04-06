use super::*;

const CCRUST_ENV_FILE_NAME: &str = ".env.ccrust";
pub(crate) const MANAGED_LOGIN_ENV_KEYS: &[&str] = &[
    "CLAUDE_CODE_API_PROVIDER",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "REASONING_MODEL_THINK",
    "COMPLETION_MODEL_THINK",
    "REASONING_MODEL",
    "COMPLETION_MODEL",
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ManagedLoginConfigState {
    pub(crate) tracked_path: Option<PathBuf>,
    pub(crate) active_values: BTreeMap<String, String>,
    pub(crate) original_env: BTreeMap<String, Option<String>>,
    pub(crate) provider_from_file: bool,
}

pub(crate) fn user_ccrust_env_path() -> PathBuf {
    claude_config_home_dir().join(CCRUST_ENV_FILE_NAME)
}

pub(crate) fn project_ccrust_env_path(cwd: &Path) -> PathBuf {
    cwd.join(".claude").join(CCRUST_ENV_FILE_NAME)
}

fn env_is_missing(name: &str) -> bool {
    env::var(name)
        .ok()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
}

fn parse_env_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        let mut output = String::new();
        let mut chars = trimmed[1..trimmed.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                output.push(ch);
                continue;
            }
            match chars.next() {
                Some('n') => output.push('\n'),
                Some('r') => output.push('\r'),
                Some('t') => output.push('\t'),
                Some('"') => output.push('"'),
                Some('\\') => output.push('\\'),
                Some(other) => {
                    output.push('\\');
                    output.push(other);
                }
                None => output.push('\\'),
            }
        }
        return output;
    }
    if trimmed.len() >= 2 && trimmed.starts_with('\'') && trimmed.ends_with('\'') {
        return trimmed[1..trimmed.len() - 1].to_owned();
    }
    trimmed.to_owned()
}

fn format_env_value(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
    {
        return value.to_owned();
    }

    let escaped = value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn parse_env_file(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (key, value) = trimmed.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_owned(), parse_env_value(value)))
        })
        .collect()
}

fn render_env_file(values: &BTreeMap<String, String>) -> String {
    if values.is_empty() {
        return String::new();
    }

    let mut output = values
        .iter()
        .map(|(key, value)| format!("{key}={}", format_env_value(value)))
        .collect::<Vec<_>>()
        .join("\n");
    output.push('\n');
    output
}

fn read_env_file(path: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(path)
        .ok()
        .map(|raw| parse_env_file(&raw))
        .unwrap_or_default()
}

pub(crate) fn existing_managed_login_env_path(cwd: &Path) -> Option<PathBuf> {
    for path in [project_ccrust_env_path(cwd), user_ccrust_env_path()] {
        let values = read_env_file(&path);
        if values
            .keys()
            .any(|key| MANAGED_LOGIN_ENV_KEYS.contains(&key.as_str()))
        {
            return Some(path);
        }
    }
    None
}

fn managed_env_snapshot() -> BTreeMap<String, Option<String>> {
    MANAGED_LOGIN_ENV_KEYS
        .iter()
        .map(|key| ((*key).to_owned(), env::var(key).ok()))
        .collect()
}

fn write_env_file(path: &Path, values: &BTreeMap<String, String>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, render_env_file(values))?;
    Ok(())
}

fn managed_env_candidate_paths(cwd: &Path, preferred_path: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = preferred_path {
        candidates.push(path.to_path_buf());
    }
    if let Some(path) = existing_managed_login_env_path(cwd) {
        candidates.push(path);
    }
    candidates.push(user_ccrust_env_path());
    candidates.push(project_ccrust_env_path(cwd));
    candidates.dedup();
    candidates
}

pub(crate) fn apply_managed_login_env(cwd: &Path) -> ManagedLoginConfigState {
    let user_path = user_ccrust_env_path();
    let project_path = project_ccrust_env_path(cwd);
    let user_values = read_env_file(&user_path);
    let project_values = read_env_file(&project_path);

    let mut merged = user_values.clone();
    merged.extend(project_values.clone());

    let mut source_by_key = BTreeMap::new();
    for key in user_values.keys() {
        source_by_key.insert(key.clone(), user_path.clone());
    }
    for key in project_values.keys() {
        source_by_key.insert(key.clone(), project_path.clone());
    }

    let original_env = managed_env_snapshot();
    let mut active_values = BTreeMap::new();
    for key in MANAGED_LOGIN_ENV_KEYS {
        if env_is_missing(key) {
            if let Some(value) = merged.get(*key).filter(|value| !value.trim().is_empty()) {
                env::set_var(key, value);
                active_values.insert((*key).to_owned(), value.clone());
            }
        }
    }

    let provider_from_file = active_values.contains_key("CLAUDE_CODE_API_PROVIDER");
    let tracked_path = if provider_from_file {
        source_by_key.get("CLAUDE_CODE_API_PROVIDER").cloned()
    } else {
        None
    };

    ManagedLoginConfigState {
        tracked_path,
        active_values,
        original_env,
        provider_from_file,
    }
}

pub(crate) fn apply_runtime_login_values(
    state: &mut ManagedLoginConfigState,
    tracked_path: PathBuf,
    values: BTreeMap<String, String>,
) {
    if state.original_env.is_empty() {
        state.original_env = managed_env_snapshot();
    }

    for key in MANAGED_LOGIN_ENV_KEYS {
        env::remove_var(key);
    }
    for (key, value) in &values {
        env::set_var(key, value);
    }

    state.tracked_path = Some(tracked_path);
    state.active_values = values;
    state.provider_from_file = true;
}

pub(crate) fn restore_runtime_login_values(state: &mut ManagedLoginConfigState) {
    for key in MANAGED_LOGIN_ENV_KEYS {
        match state.original_env.get(*key).cloned().flatten() {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }

    state.tracked_path = None;
    state.active_values.clear();
    state.original_env.clear();
    state.provider_from_file = false;
}

pub(crate) fn persist_managed_login_env(
    cwd: &Path,
    preferred_path: Option<&Path>,
    values: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    let candidates = managed_env_candidate_paths(cwd, preferred_path);
    let mut last_error = None;
    for path in candidates {
        let mut current = read_env_file(&path);
        for key in MANAGED_LOGIN_ENV_KEYS {
            current.remove(*key);
        }
        for (key, value) in values {
            if !value.trim().is_empty() {
                current.insert(key.clone(), value.clone());
            }
        }
        match write_env_file(&path, &current) {
            Ok(()) => return Ok(path),
            Err(error) => last_error = Some(error),
        }
    }

    Err(anyhow!(
        "failed to write login config: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no writable paths".to_owned())
    ))
}

pub(crate) fn persist_managed_env_updates(
    cwd: &Path,
    preferred_path: Option<&Path>,
    updates: &BTreeMap<String, Option<String>>,
) -> Result<PathBuf> {
    let candidates = managed_env_candidate_paths(cwd, preferred_path);
    let mut last_error = None;

    for path in candidates {
        let mut current = read_env_file(&path);
        for (key, value) in updates {
            match value.as_deref().filter(|value| !value.trim().is_empty()) {
                Some(value) => {
                    current.insert(key.clone(), value.to_owned());
                }
                None => {
                    current.remove(key);
                }
            }
        }

        match write_env_file(&path, &current) {
            Ok(()) => return Ok(path),
            Err(error) => last_error = Some(error),
        }
    }

    Err(anyhow!(
        "failed to write managed env updates: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no writable paths".to_owned())
    ))
}

pub(crate) fn apply_managed_env_updates(updates: &BTreeMap<String, Option<String>>) {
    for (key, value) in updates {
        match value.as_deref().filter(|value| !value.trim().is_empty()) {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }
}

pub(crate) fn clear_managed_login_env_file(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }

    let mut current = read_env_file(path);
    let mut removed = false;
    for key in MANAGED_LOGIN_ENV_KEYS {
        removed |= current.remove(*key).is_some();
    }
    if !removed {
        return Ok(false);
    }

    if current.is_empty() {
        fs::remove_file(path)?;
    } else {
        write_env_file(path, &current)?;
    }
    Ok(true)
}

pub(crate) fn managed_login_values_from_environment(
    provider: ApiProvider,
) -> BTreeMap<String, String> {
    let mut values =
        BTreeMap::from([("CLAUDE_CODE_API_PROVIDER".to_owned(), provider.to_string())]);

    match provider {
        ApiProvider::FirstParty => {
            if let Ok(value) = env::var("ANTHROPIC_API_KEY") {
                if !value.trim().is_empty() {
                    values.insert("ANTHROPIC_API_KEY".to_owned(), value);
                }
            }
        }
        ApiProvider::OpenAICompatible => {
            for key in [
                "OPENAI_API_KEY",
                "OPENAI_BASE_URL",
                "REASONING_MODEL_THINK",
                "COMPLETION_MODEL_THINK",
                "REASONING_MODEL",
                "COMPLETION_MODEL",
            ] {
                if let Ok(value) = env::var(key) {
                    if !value.trim().is_empty() {
                        values.insert(key.to_owned(), value);
                    }
                }
            }
        }
        ApiProvider::ChatGPTCodex
        | ApiProvider::Bedrock
        | ApiProvider::Vertex
        | ApiProvider::Foundry => {}
    }

    values
}
