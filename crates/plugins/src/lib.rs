use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use code_agent_core::{CommandCategory, CommandKind, CommandSource, CommandSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use tokio::process::{Child, Command};

pub const PLUGIN_MANIFEST_PATH: &str = ".claude-plugin/plugin.json";
pub const LEGACY_SKILLS_DIR: &str = ".claude/skills";
pub const LEGACY_COMMANDS_DIR: &str = ".claude/commands";
pub const SKILL_FILE_NAME: &str = "SKILL.md";

fn command_spec(
    name: impl Into<String>,
    description: impl Into<String>,
    aliases: Vec<String>,
    source: CommandSource,
    origin: Option<String>,
) -> CommandSpec {
    CommandSpec {
        name: name.into(),
        description: description.into(),
        aliases,
        category: CommandCategory::Tooling,
        kind: CommandKind::Prompt,
        interactive: true,
        supports_non_interactive: true,
        requires_provider: true,
        source,
        hidden: false,
        remote_safe: false,
        bridge_safe: true,
        origin,
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginAuthor {
    pub name: String,
    pub email: Option<String>,
    pub url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PluginDependency {
    Name(String),
    Detailed {
        name: String,
        marketplace: Option<String>,
        version: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PathListOrSingle {
    Single(String),
    List(Vec<String>),
}

impl PathListOrSingle {
    pub fn values(&self) -> Vec<&str> {
        match self {
            Self::Single(path) => vec![path.as_str()],
            Self::List(paths) => paths.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandMetadata {
    pub source: Option<String>,
    pub content: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "argumentHint")]
    pub argument_hint: Option<String>,
    pub model: Option<String>,
    #[serde(rename = "allowedTools")]
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum CommandDefinitions {
    Single(String),
    List(Vec<String>),
    Mapping(BTreeMap<String, CommandMetadata>),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub author: Option<PluginAuthor>,
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub license: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub dependencies: Option<Vec<PluginDependency>>,
    pub commands: Option<CommandDefinitions>,
    pub agents: Option<PathListOrSingle>,
    pub skills: Option<PathListOrSingle>,
    #[serde(rename = "outputStyles")]
    pub output_styles: Option<PathListOrSingle>,
    pub hooks: Option<Value>,
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: BTreeMap<String, Value>,
    #[serde(default, rename = "lspServers")]
    pub lsp_servers: BTreeMap<String, Value>,
    #[serde(default, rename = "userConfig")]
    pub user_config: BTreeMap<String, Value>,
}

#[derive(Clone, Debug)]
pub struct LoadedPlugin {
    pub root: PathBuf,
    pub manifest: PluginManifest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PluginComponent {
    Commands,
    Agents,
    Skills,
    Hooks,
    OutputStyles,
    McpServers,
    LspServers,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BridgeLaunchRequest {
    pub plugin_root: PathBuf,
    pub component: Option<String>,
    pub executable: Option<PathBuf>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginBridgeDescriptor {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginBridgeStatus {
    pub component: Option<String>,
    pub descriptor: Option<PluginBridgeDescriptor>,
    pub running: bool,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SkillSource {
    Manifest,
    LegacySkillsDir,
    LegacyCommandsDir,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub path: PathBuf,
    pub source: SkillSource,
}

#[async_trait]
pub trait PluginRuntime: Send + Sync {
    async fn load_manifest(&self, root: &Path) -> Result<LoadedPlugin>;
    async fn discover_skills(&self, root: &Path) -> Result<Vec<SkillEntry>>;
    async fn discover_commands(&self, root: &Path) -> Result<Vec<CommandSpec>>;
    async fn prepare_bridge(&self, request: BridgeLaunchRequest) -> Result<PluginBridgeDescriptor>;
    async fn start_bridge(&self, request: BridgeLaunchRequest) -> Result<PluginBridgeStatus>;
    async fn stop_bridge(&self, root: &Path, component: Option<&str>)
        -> Result<PluginBridgeStatus>;
    async fn bridge_status(
        &self,
        root: &Path,
        component: Option<&str>,
    ) -> Result<PluginBridgeStatus>;
}

#[derive(Clone, Debug, Default)]
pub struct OutOfProcessPluginRuntime;

#[derive(Debug)]
struct RunningPluginBridge {
    component: Option<String>,
    descriptor: PluginBridgeDescriptor,
    child: Child,
}

fn bridge_registry() -> &'static Mutex<BTreeMap<String, RunningPluginBridge>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, RunningPluginBridge>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn bridge_key(root: &Path, component: Option<&str>) -> String {
    let component = component.unwrap_or("default");
    format!("{}::{component}", root.display())
}

fn is_relative_plugin_path(path: &str) -> bool {
    path.starts_with("./")
}

fn validate_relative_path(path: &str, field: &str) -> Result<()> {
    if is_relative_plugin_path(path) {
        return Ok(());
    }
    bail!("{field} must be a relative path starting with './': {path}");
}

fn validate_markdown_path(path: &str, field: &str) -> Result<()> {
    validate_relative_path(path, field)?;
    if path.ends_with(".md") {
        return Ok(());
    }
    bail!("{field} must point to a markdown file: {path}");
}

fn validate_json_path(path: &str, field: &str) -> Result<()> {
    validate_relative_path(path, field)?;
    if path.ends_with(".json") {
        return Ok(());
    }
    bail!("{field} must point to a json file: {path}");
}

fn validate_path_list(paths: Option<&PathListOrSingle>, field: &str) -> Result<()> {
    let Some(paths) = paths else {
        return Ok(());
    };

    for path in paths.values() {
        validate_relative_path(path, field)?;
    }
    Ok(())
}

fn validate_agent_paths(paths: Option<&PathListOrSingle>) -> Result<()> {
    let Some(paths) = paths else {
        return Ok(());
    };

    for path in paths.values() {
        validate_markdown_path(path, "agents")?;
    }
    Ok(())
}

fn validate_hooks(value: Option<&Value>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };

    match value {
        Value::String(path) => validate_json_path(path, "hooks"),
        Value::Array(entries) => {
            for entry in entries {
                match entry {
                    Value::String(path) => validate_json_path(path, "hooks")?,
                    Value::Object(_) => {}
                    other => bail!(
                        "hooks entries must be relative json paths or inline objects, got {other}"
                    ),
                }
            }
            Ok(())
        }
        Value::Object(_) => Ok(()),
        other => bail!("hooks must be a relative json path, inline object, or list; got {other}"),
    }
}

fn validate_commands(commands: Option<&CommandDefinitions>) -> Result<()> {
    let Some(commands) = commands else {
        return Ok(());
    };

    match commands {
        CommandDefinitions::Single(path) => validate_relative_path(path, "commands"),
        CommandDefinitions::List(paths) => {
            for path in paths {
                validate_relative_path(path, "commands")?;
            }
            Ok(())
        }
        CommandDefinitions::Mapping(entries) => {
            for (name, metadata) in entries {
                let has_source = metadata.source.is_some();
                let has_content = metadata.content.is_some();
                if has_source == has_content {
                    bail!("command '{name}' must have exactly one of 'source' or 'content'");
                }
                if let Some(source) = &metadata.source {
                    validate_relative_path(source, "commands")?;
                }
            }
            Ok(())
        }
    }
}

pub fn validate_manifest(manifest: &PluginManifest) -> Result<()> {
    if manifest.name.trim().is_empty() {
        bail!("plugin name cannot be empty");
    }
    if manifest.name.contains(' ') {
        bail!("plugin name cannot contain spaces: {}", manifest.name);
    }

    validate_commands(manifest.commands.as_ref())?;
    validate_agent_paths(manifest.agents.as_ref())?;
    validate_path_list(manifest.skills.as_ref(), "skills")?;
    validate_path_list(manifest.output_styles.as_ref(), "outputStyles")?;
    validate_hooks(manifest.hooks.as_ref())?;
    Ok(())
}

fn skill_name_from_path(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skill".to_owned())
}

fn manifest_skill_entries(root: &Path, manifest: &PluginManifest) -> Vec<SkillEntry> {
    let Some(paths) = manifest.skills.as_ref() else {
        return Vec::new();
    };

    paths
        .values()
        .into_iter()
        .map(|path| {
            let relative = path.trim_start_matches("./");
            let resolved = root.join(relative);
            let skill_path =
                if resolved.file_name().and_then(|value| value.to_str()) == Some(SKILL_FILE_NAME) {
                    resolved.clone()
                } else {
                    resolved.join(SKILL_FILE_NAME)
                };

            SkillEntry {
                name: skill_name_from_path(&resolved),
                path: skill_path,
                source: SkillSource::Manifest,
            }
        })
        .collect()
}

fn command_metadata_from_entry(entry: &CommandMetadata) -> (String, Vec<String>) {
    let description = entry
        .description
        .clone()
        .unwrap_or_else(|| "Plugin command".to_owned());
    let mut aliases = Vec::new();
    if let Some(argument_hint) = &entry.argument_hint {
        for alias in argument_hint
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            aliases.push(alias.to_owned());
        }
    }
    (description, aliases)
}

fn plugin_command_specs(root: &Path, manifest: &PluginManifest) -> Vec<CommandSpec> {
    let Some(commands) = manifest.commands.as_ref() else {
        return Vec::new();
    };

    match commands {
        CommandDefinitions::Single(path) => vec![command_spec(
            skill_name_from_path(Path::new(path)),
            format!("Plugin command from {}", path.trim_start_matches("./")),
            Vec::new(),
            CommandSource::Plugin,
            Some(
                root.join(path.trim_start_matches("./"))
                    .display()
                    .to_string(),
            ),
        )],
        CommandDefinitions::List(paths) => paths
            .iter()
            .map(|path| {
                command_spec(
                    skill_name_from_path(Path::new(path)),
                    format!("Plugin command from {}", path.trim_start_matches("./")),
                    Vec::new(),
                    CommandSource::Plugin,
                    Some(
                        root.join(path.trim_start_matches("./"))
                            .display()
                            .to_string(),
                    ),
                )
            })
            .collect(),
        CommandDefinitions::Mapping(entries) => entries
            .iter()
            .map(|(name, metadata)| {
                let (description, aliases) = command_metadata_from_entry(metadata);
                let origin = metadata.source.as_ref().map(|source| {
                    root.join(source.trim_start_matches("./"))
                        .display()
                        .to_string()
                });
                command_spec(
                    name.clone(),
                    description,
                    aliases,
                    CommandSource::Plugin,
                    origin,
                )
            })
            .collect(),
    }
}

fn skill_command_specs(entries: &[SkillEntry]) -> Vec<CommandSpec> {
    entries
        .iter()
        .map(|entry| {
            let source = match entry.source {
                SkillSource::Manifest => CommandSource::Plugin,
                SkillSource::LegacySkillsDir | SkillSource::LegacyCommandsDir => {
                    CommandSource::Skill
                }
            };
            command_spec(
                entry.name.clone(),
                format!("Skill command from {}", entry.path.display()),
                Vec::new(),
                source,
                Some(entry.path.display().to_string()),
            )
        })
        .collect()
}

fn collect_legacy_skill_entries(
    root: &Path,
    relative_dir: &str,
    source: SkillSource,
) -> Result<Vec<SkillEntry>> {
    let base = root.join(relative_dir);
    if !base.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for dirent in
        std::fs::read_dir(&base).with_context(|| format!("failed to read {}", base.display()))?
    {
        let dirent = dirent?;
        let path = dirent.path();
        let file_type = dirent.file_type()?;

        if file_type.is_dir() {
            let skill_file = path.join(SKILL_FILE_NAME);
            if skill_file.exists() {
                entries.push(SkillEntry {
                    name: dirent.file_name().to_string_lossy().into_owned(),
                    path: skill_file,
                    source: source.clone(),
                });
            }
            continue;
        }

        if source == SkillSource::LegacyCommandsDir
            && file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("md")
        {
            entries.push(SkillEntry {
                name: skill_name_from_path(&path),
                path,
                source: SkillSource::LegacyCommandsDir,
            });
        }
    }

    Ok(entries)
}

#[async_trait]
impl PluginRuntime for OutOfProcessPluginRuntime {
    async fn load_manifest(&self, root: &Path) -> Result<LoadedPlugin> {
        let manifest_path = root.join(PLUGIN_MANIFEST_PATH);
        let raw = tokio::fs::read_to_string(&manifest_path)
            .await
            .with_context(|| format!("failed to read {}", manifest_path.display()))?;
        let manifest = serde_json::from_str::<PluginManifest>(&raw)
            .with_context(|| format!("failed to decode {}", manifest_path.display()))?;
        validate_manifest(&manifest)?;

        Ok(LoadedPlugin {
            root: root.to_path_buf(),
            manifest,
        })
    }

    async fn discover_skills(&self, root: &Path) -> Result<Vec<SkillEntry>> {
        let manifest_path = root.join(PLUGIN_MANIFEST_PATH);
        let mut skills = if manifest_path.exists() {
            let loaded = self.load_manifest(root).await?;
            manifest_skill_entries(root, &loaded.manifest)
        } else {
            Vec::new()
        };

        skills.extend(collect_legacy_skill_entries(
            root,
            LEGACY_SKILLS_DIR,
            SkillSource::LegacySkillsDir,
        )?);
        skills.extend(collect_legacy_skill_entries(
            root,
            LEGACY_COMMANDS_DIR,
            SkillSource::LegacyCommandsDir,
        )?);
        skills.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
        skills.dedup_by(|left, right| left.path == right.path);
        Ok(skills)
    }

    async fn discover_commands(&self, root: &Path) -> Result<Vec<CommandSpec>> {
        let manifest_path = root.join(PLUGIN_MANIFEST_PATH);
        let mut commands = if manifest_path.exists() {
            let loaded = self.load_manifest(root).await?;
            plugin_command_specs(root, &loaded.manifest)
        } else {
            Vec::new()
        };
        let skills = self.discover_skills(root).await?;
        commands.extend(skill_command_specs(&skills));
        commands.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.origin.cmp(&right.origin))
        });
        commands.dedup_by(|left, right| left.name == right.name && left.origin == right.origin);
        Ok(commands)
    }

    async fn prepare_bridge(&self, request: BridgeLaunchRequest) -> Result<PluginBridgeDescriptor> {
        let executable = request
            .executable
            .clone()
            .ok_or_else(|| anyhow!("plugin bridge requires an executable"))?;

        Ok(PluginBridgeDescriptor {
            executable,
            args: request.args,
            env: request.env,
        })
    }

    async fn start_bridge(&self, request: BridgeLaunchRequest) -> Result<PluginBridgeStatus> {
        let descriptor = self.prepare_bridge(request.clone()).await?;
        let key = bridge_key(&request.plugin_root, request.component.as_deref());
        let mut child = Command::new(&descriptor.executable);
        child
            .args(&descriptor.args)
            .envs(&descriptor.env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let spawned = child.spawn().with_context(|| {
            format!(
                "failed to start plugin bridge {}",
                descriptor.executable.display()
            )
        })?;
        let pid = spawned.id();

        if let Some(mut existing) = bridge_registry().lock().unwrap().remove(&key) {
            let _ = existing.child.start_kill();
        }
        let mut registry = bridge_registry().lock().unwrap();
        registry.insert(
            key,
            RunningPluginBridge {
                component: request.component.clone(),
                descriptor: descriptor.clone(),
                child: spawned,
            },
        );

        Ok(PluginBridgeStatus {
            component: request.component,
            descriptor: Some(descriptor),
            running: true,
            pid,
            last_error: None,
        })
    }

    async fn stop_bridge(
        &self,
        root: &Path,
        component: Option<&str>,
    ) -> Result<PluginBridgeStatus> {
        let key = bridge_key(root, component);
        let Some(mut running) = bridge_registry().lock().unwrap().remove(&key) else {
            return Ok(PluginBridgeStatus {
                component: component.map(str::to_owned),
                running: false,
                last_error: Some("plugin bridge is not running".to_owned()),
                ..PluginBridgeStatus::default()
            });
        };
        let pid = running.child.id();
        running
            .child
            .start_kill()
            .with_context(|| "failed to stop plugin bridge".to_owned())?;
        Ok(PluginBridgeStatus {
            component: running.component,
            descriptor: Some(running.descriptor),
            running: false,
            pid,
            last_error: None,
        })
    }

    async fn bridge_status(
        &self,
        root: &Path,
        component: Option<&str>,
    ) -> Result<PluginBridgeStatus> {
        let key = bridge_key(root, component);
        let mut registry = bridge_registry().lock().unwrap();
        if let Some(running) = registry.get_mut(&key) {
            let running_now = running.child.try_wait()?.is_none();
            let pid = running.child.id();
            return Ok(PluginBridgeStatus {
                component: running.component.clone(),
                descriptor: Some(running.descriptor.clone()),
                running: running_now,
                pid,
                last_error: None,
            });
        }
        Ok(PluginBridgeStatus {
            component: component.map(str::to_owned),
            running: false,
            last_error: Some("plugin bridge is not running".to_owned()),
            ..PluginBridgeStatus::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeLaunchRequest, OutOfProcessPluginRuntime, PluginRuntime, LEGACY_SKILLS_DIR,
        PLUGIN_MANIFEST_PATH, SKILL_FILE_NAME,
    };
    use code_agent_core::CommandSource;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn make_temp_dir(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("code-agent-{label}-{stamp}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    #[tokio::test]
    async fn loads_manifest_with_relative_paths() {
        let root = make_temp_dir("plugin-load");
        let manifest_path = root.join(PLUGIN_MANIFEST_PATH);
        write_file(
            &manifest_path,
            r#"{
              "name": "release-tools",
              "version": "1.0.0",
              "commands": {
                "about": {
                  "source": "./commands/about.md",
                  "description": "About this plugin"
                }
              },
              "agents": "./agents/release.md",
              "skills": ["./skills/release"],
              "outputStyles": "./output-styles",
              "hooks": "./hooks/hooks.json"
            }"#,
        );

        let runtime = OutOfProcessPluginRuntime;
        let loaded = runtime.load_manifest(&root).await.unwrap();

        assert_eq!(loaded.manifest.name, "release-tools");
        assert!(loaded.manifest.commands.is_some());
        assert!(loaded.manifest.skills.is_some());
    }

    #[tokio::test]
    async fn rejects_non_relative_manifest_paths() {
        let root = make_temp_dir("plugin-invalid");
        let manifest_path = root.join(PLUGIN_MANIFEST_PATH);
        write_file(
            &manifest_path,
            r#"{
              "name": "invalid-plugin",
              "commands": "/tmp/about.md"
            }"#,
        );

        let runtime = OutOfProcessPluginRuntime;
        let error = runtime.load_manifest(&root).await.unwrap_err().to_string();

        assert!(error.contains("relative path"));
    }

    #[tokio::test]
    async fn discovers_skill_directories() {
        let root = make_temp_dir("skill-discovery");
        write_file(
            &root.join(PLUGIN_MANIFEST_PATH),
            r#"{
              "name": "skills-plugin",
              "skills": "./packaged/release"
            }"#,
        );
        write_file(
            &root.join("packaged/release").join(SKILL_FILE_NAME),
            "# Release\n",
        );
        write_file(
            &root
                .join(LEGACY_SKILLS_DIR)
                .join("review")
                .join(SKILL_FILE_NAME),
            "# Review\n",
        );
        write_file(&root.join(".claude/commands/triage.md"), "# Triage\n");

        let runtime = OutOfProcessPluginRuntime;
        let skills = runtime.discover_skills(&root).await.unwrap();

        let names = skills
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["release", "review", "triage"]);
    }

    #[tokio::test]
    async fn loads_fixture_plugin_and_discovers_skills() {
        let root = workspace_root().join("fixtures/plugin-fixtures/review-tools");
        let runtime = OutOfProcessPluginRuntime;
        let plugin = runtime.load_manifest(&root).await.unwrap();
        let skills = runtime.discover_skills(&root).await.unwrap();
        let mut names = skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();

        assert_eq!(plugin.manifest.name, "review-tools");
        assert!(plugin.manifest.mcp_servers.contains_key("example"));
        assert_eq!(names, vec!["audit", "review", "triage"]);
    }

    #[tokio::test]
    async fn discovers_plugin_and_skill_commands() {
        let root = workspace_root().join("fixtures/plugin-fixtures/review-tools");
        let runtime = OutOfProcessPluginRuntime;
        let commands = runtime.discover_commands(&root).await.unwrap();
        let mut names = commands
            .iter()
            .map(|command| (command.name.as_str(), command.source.clone()))
            .collect::<Vec<_>>();
        names.sort_by(|left, right| left.0.cmp(right.0));

        assert!(names.contains(&("about", CommandSource::Plugin)));
        assert!(names.contains(&("audit", CommandSource::Skill)));
        assert!(names.contains(&("review", CommandSource::Plugin)));
        assert!(names.contains(&("triage", CommandSource::Skill)));
    }

    #[tokio::test]
    async fn starts_and_stops_plugin_bridge_processes() {
        let root = make_temp_dir("plugin-bridge");
        let runtime = OutOfProcessPluginRuntime;

        let started = runtime
            .start_bridge(BridgeLaunchRequest {
                plugin_root: root.clone(),
                component: Some("runtime".to_owned()),
                executable: Some(PathBuf::from("sh")),
                args: vec!["-c".to_owned(), "sleep 30".to_owned()],
                ..BridgeLaunchRequest::default()
            })
            .await
            .unwrap();
        let running = runtime.bridge_status(&root, Some("runtime")).await.unwrap();
        let stopped = runtime.stop_bridge(&root, Some("runtime")).await.unwrap();
        let after = runtime.bridge_status(&root, Some("runtime")).await.unwrap();

        assert!(started.running);
        assert!(running.running);
        assert!(running.pid.is_some());
        assert!(!stopped.running);
        assert!(!after.running);
    }
}
