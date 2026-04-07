#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModuleParityStatus {
    Ported,
    Split,
    Removed,
}

impl ModuleParityStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ported => "ported",
            Self::Split => "split",
            Self::Removed => "removed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleParityEntry {
    pub(crate) ts_module: &'static str,
    pub(crate) status: ModuleParityStatus,
    pub(crate) rust_modules: &'static [&'static str],
    pub(crate) note: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ModuleParityAudit {
    pub(crate) workspace_file: PathBuf,
    pub(crate) rewrite_root: PathBuf,
    pub(crate) ts_root: PathBuf,
    pub(crate) ts_modules: Vec<String>,
    pub(crate) rust_modules: Vec<String>,
    pub(crate) duplicate_manifest_entries: Vec<String>,
    pub(crate) unclassified_ts_modules: Vec<String>,
    pub(crate) stale_manifest_entries: Vec<String>,
    pub(crate) unmapped_rust_crates: Vec<String>,
}

impl ModuleParityAudit {
    fn status_count(&self, status: ModuleParityStatus) -> usize {
        module_parity_manifest()
            .iter()
            .filter(|entry| entry.status == status)
            .count()
    }
}

const MODULE_PARITY_MANIFEST: &[ModuleParityEntry] = &[
    ModuleParityEntry {
        ts_module: "QueryEngine.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "core", "providers", "session", "tools"],
        note: "Turn orchestration was decomposed into runtime, provider, session, and tool crates instead of a single engine file.",
    },
    ModuleParityEntry {
        ts_module: "Task.ts",
        status: ModuleParityStatus::Ported,
        rust_modules: &["core", "tools", "ui"],
        note: "Task records, scheduling, and UI reporting remain first-class runtime concepts.",
    },
    ModuleParityEntry {
        ts_module: "Tool.ts",
        status: ModuleParityStatus::Ported,
        rust_modules: &["tools", "core"],
        note: "Tool definitions and registry wiring live in dedicated Rust crates.",
    },
    ModuleParityEntry {
        ts_module: "assistant",
        status: ModuleParityStatus::Split,
        rust_modules: &["bridge", "cli", "providers"],
        note: "Assistant launch, auth handoff, and remote directives moved into startup and bridge flows.",
    },
    ModuleParityEntry {
        ts_module: "bootstrap",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "providers"],
        note: "Process bootstrap and auth bootstrap live in startup and provider code instead of a separate bootstrap tree.",
    },
    ModuleParityEntry {
        ts_module: "bridge",
        status: ModuleParityStatus::Ported,
        rust_modules: &["bridge", "cli"],
        note: "Remote and IDE bridge transport remains a dedicated runtime surface.",
    },
    ModuleParityEntry {
        ts_module: "buddy",
        status: ModuleParityStatus::Removed,
        rust_modules: &["cli"],
        note: "The companion sprite UI is gone; the rewrite keeps explicit mobile, desktop, and Chrome handoff commands instead.",
    },
    ModuleParityEntry {
        ts_module: "cli",
        status: ModuleParityStatus::Ported,
        rust_modules: &["cli"],
        note: "The command-line entrypoint remains a dedicated crate.",
    },
    ModuleParityEntry {
        ts_module: "commands",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "cli"],
        note: "Slash-command specs live in core while rendering and execution live in cli.",
    },
    ModuleParityEntry {
        ts_module: "commands.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "cli"],
        note: "Top-level command registry wiring was split between registry data and runtime handlers.",
    },
    ModuleParityEntry {
        ts_module: "components",
        status: ModuleParityStatus::Split,
        rust_modules: &["ui", "cli"],
        note: "Ink components became ratatui views and startup flows.",
    },
    ModuleParityEntry {
        ts_module: "constants",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "providers", "cli"],
        note: "Constants were redistributed to the crate that owns the behavior.",
    },
    ModuleParityEntry {
        ts_module: "context",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "session"],
        note: "Context packing moved into runtime prompt assembly and session materialization.",
    },
    ModuleParityEntry {
        ts_module: "context.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "session"],
        note: "Top-level context helpers were folded into runtime and session helpers.",
    },
    ModuleParityEntry {
        ts_module: "coordinator",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "cli"],
        note: "Coordinator task orchestration now lives in core task builders and cli runtime.",
    },
    ModuleParityEntry {
        ts_module: "cost-tracker.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli"],
        note: "Usage and cost reporting live in /usage, /status, and session metadata code.",
    },
    ModuleParityEntry {
        ts_module: "costHook.ts",
        status: ModuleParityStatus::Removed,
        rust_modules: &["cli", "ui"],
        note: "The React hook disappeared; usage reporting is driven by terminal runtime state instead.",
    },
    ModuleParityEntry {
        ts_module: "dev-entry.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli"],
        note: "Development entrypoint behavior is folded into the main Rust binary.",
    },
    ModuleParityEntry {
        ts_module: "dialogLaunchers.tsx",
        status: ModuleParityStatus::Removed,
        rust_modules: &["cli", "ui"],
        note: "Modal launcher components were replaced with terminal-native onboarding and picker flows.",
    },
    ModuleParityEntry {
        ts_module: "entrypoints",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "bridge"],
        note: "Separate JS entrypoints collapsed into the cli binary and bridge helpers.",
    },
    ModuleParityEntry {
        ts_module: "history.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["session", "cli"],
        note: "Transcript history persists through the session crate and cli resume flows.",
    },
    ModuleParityEntry {
        ts_module: "hooks",
        status: ModuleParityStatus::Split,
        rust_modules: &["plugins", "cli"],
        note: "Hook discovery and summary rendering moved into plugin and runtime helpers.",
    },
    ModuleParityEntry {
        ts_module: "ink",
        status: ModuleParityStatus::Split,
        rust_modules: &["ui"],
        note: "The Ink subtree became the ratatui UI crate.",
    },
    ModuleParityEntry {
        ts_module: "ink.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["ui"],
        note: "Top-level Ink wiring became ui crate entrypoints.",
    },
    ModuleParityEntry {
        ts_module: "interactiveHelpers.tsx",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "ui"],
        note: "Prompt, picker, and interaction helpers live in terminal runtime code.",
    },
    ModuleParityEntry {
        ts_module: "jobs",
        status: ModuleParityStatus::Split,
        rust_modules: &["tools", "core"],
        note: "Background work is modeled as tasks and tool-driven background jobs.",
    },
    ModuleParityEntry {
        ts_module: "keybindings",
        status: ModuleParityStatus::Split,
        rust_modules: &["ui", "cli"],
        note: "Keyboard handling lives in ui vim logic and cli event routing.",
    },
    ModuleParityEntry {
        ts_module: "main.tsx",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "ui"],
        note: "The TUI main loop is implemented by the cli binary and ui crate.",
    },
    ModuleParityEntry {
        ts_module: "memdir",
        status: ModuleParityStatus::Split,
        rust_modules: &["session"],
        note: "Memory summaries and session-memory boundaries live in the session crate instead of a dedicated memdir tree.",
    },
    ModuleParityEntry {
        ts_module: "migrations",
        status: ModuleParityStatus::Split,
        rust_modules: &["providers", "cli"],
        note: "Config and model migration reporting moved into provider and command helpers.",
    },
    ModuleParityEntry {
        ts_module: "moreright",
        status: ModuleParityStatus::Removed,
        rust_modules: &["ui"],
        note: "The right-side composer affordance is gone; pane switching is handled directly by terminal shortcuts.",
    },
    ModuleParityEntry {
        ts_module: "native-ts",
        status: ModuleParityStatus::Removed,
        rust_modules: &["bridge", "ui"],
        note: "Native JS bindings were replaced by Rust crates and native dependencies.",
    },
    ModuleParityEntry {
        ts_module: "outputStyles",
        status: ModuleParityStatus::Split,
        rust_modules: &["ui", "cli"],
        note: "Output presentation moved into theme commands and ratatui rendering.",
    },
    ModuleParityEntry {
        ts_module: "plugins",
        status: ModuleParityStatus::Ported,
        rust_modules: &["plugins"],
        note: "Plugin manifest loading and dynamic command discovery remain dedicated code.",
    },
    ModuleParityEntry {
        ts_module: "proactive",
        status: ModuleParityStatus::Removed,
        rust_modules: &["ui"],
        note: "The rewrite keeps an explicit user-driven turn loop instead of proactive prompt hooks.",
    },
    ModuleParityEntry {
        ts_module: "projectOnboardingState.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli"],
        note: "Onboarding preferences and draft state live in startup code.",
    },
    ModuleParityEntry {
        ts_module: "query",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "providers", "session", "tools"],
        note: "Streaming turn execution is decomposed across runtime, provider, session, and tool crates.",
    },
    ModuleParityEntry {
        ts_module: "query.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "providers"],
        note: "Top-level query dispatch moved into runtime and provider orchestration.",
    },
    ModuleParityEntry {
        ts_module: "remote",
        status: ModuleParityStatus::Ported,
        rust_modules: &["bridge", "cli"],
        note: "Remote control and bridge session support remain first-class.",
    },
    ModuleParityEntry {
        ts_module: "replLauncher.tsx",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli"],
        note: "REPL startup and session selection moved into cli launch code.",
    },
    ModuleParityEntry {
        ts_module: "schemas",
        status: ModuleParityStatus::Split,
        rust_modules: &["tools", "providers", "mcp"],
        note: "Schemas are emitted from Rust types and tool specs instead of TS schema helpers.",
    },
    ModuleParityEntry {
        ts_module: "screens",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "ui"],
        note: "Screen rendering became startup and TUI views.",
    },
    ModuleParityEntry {
        ts_module: "server",
        status: ModuleParityStatus::Split,
        rust_modules: &["bridge", "mcp"],
        note: "Server-side transport is split between bridge and MCP crates.",
    },
    ModuleParityEntry {
        ts_module: "services",
        status: ModuleParityStatus::Split,
        rust_modules: &["providers", "session", "plugins", "cli"],
        note: "Service-style helpers were distributed to the owning crate.",
    },
    ModuleParityEntry {
        ts_module: "setup.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli"],
        note: "Initial setup flow became startup and provider configuration logic.",
    },
    ModuleParityEntry {
        ts_module: "skills",
        status: ModuleParityStatus::Ported,
        rust_modules: &["plugins", "tools", "cli"],
        note: "Skill discovery and prompt loading remain explicit runtime features.",
    },
    ModuleParityEntry {
        ts_module: "ssh",
        status: ModuleParityStatus::Removed,
        rust_modules: &["cli", "bridge"],
        note: "The rewrite has no SSH session manager; use your shell or bridge transport for remote execution.",
    },
    ModuleParityEntry {
        ts_module: "state",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "ui", "session"],
        note: "Runtime state is split between session storage, TUI state, and command settings.",
    },
    ModuleParityEntry {
        ts_module: "tasks",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "tools", "session"],
        note: "Task orchestration is shared between core task records, tool APIs, and session persistence.",
    },
    ModuleParityEntry {
        ts_module: "tasks.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "tools"],
        note: "Top-level task helpers moved into core records and tool registry code.",
    },
    ModuleParityEntry {
        ts_module: "tools",
        status: ModuleParityStatus::Ported,
        rust_modules: &["tools"],
        note: "Compatibility tool surface remains a dedicated crate.",
    },
    ModuleParityEntry {
        ts_module: "tools.ts",
        status: ModuleParityStatus::Split,
        rust_modules: &["tools", "core"],
        note: "Top-level tool registry wiring is split between the tool crate and core metadata.",
    },
    ModuleParityEntry {
        ts_module: "types",
        status: ModuleParityStatus::Split,
        rust_modules: &["core", "providers", "session", "bridge"],
        note: "Shared types were distributed into the domain crate that owns them.",
    },
    ModuleParityEntry {
        ts_module: "upstreamproxy",
        status: ModuleParityStatus::Removed,
        rust_modules: &["providers"],
        note: "The dedicated upstream relay is gone; provider base URLs and external proxies drive transport instead.",
    },
    ModuleParityEntry {
        ts_module: "utils",
        status: ModuleParityStatus::Split,
        rust_modules: &["cli", "core", "providers", "session"],
        note: "Utility code was split into domain-local helpers instead of one shared utils tree.",
    },
    ModuleParityEntry {
        ts_module: "vim",
        status: ModuleParityStatus::Ported,
        rust_modules: &["ui", "cli"],
        note: "Vim-mode input handling remains explicit runtime behavior.",
    },
    ModuleParityEntry {
        ts_module: "voice",
        status: ModuleParityStatus::Split,
        rust_modules: &["bridge", "cli"],
        note: "Voice payload transport exists via CLI flags and bridge frames; there is no in-app capture widget.",
    },
];

pub(crate) fn module_parity_manifest() -> &'static [ModuleParityEntry] {
    MODULE_PARITY_MANIFEST
}

pub(crate) fn parity_placeholder_terms() -> &'static [&'static str] {
    &[
        "compatibility-surface",
        "compatibility state",
        "not persisted yet",
        "intentionally deferred",
        "not bundled",
        "not implemented",
        "not modeled",
        "still minimal",
        "not yet",
    ]
}

fn compile_time_workspace_file() -> Option<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join("Claude-Code-main-run.code-workspace"))
}

fn rewrite_workspace_candidates(cwd: &Path) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();

    for candidate in cwd
        .ancestors()
        .map(|ancestor| ancestor.join("Claude-Code-main-run.code-workspace"))
        .chain(compile_time_workspace_file())
    {
        if seen.insert(candidate.clone()) {
            candidates.push(candidate);
        }
    }

    candidates
}

fn find_rewrite_workspace_file(cwd: &Path) -> Option<PathBuf> {
    rewrite_workspace_candidates(cwd)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

pub(crate) fn find_rewrite_workspace_root(cwd: &Path) -> Option<PathBuf> {
    find_rewrite_workspace_file(cwd).and_then(|path| path.parent().map(Path::to_path_buf))
}

#[derive(serde::Deserialize)]
struct WorkspaceFolderEntry {
    path: String,
}

#[derive(serde::Deserialize)]
struct WorkspaceFileSpec {
    folders: Vec<WorkspaceFolderEntry>,
}

pub(crate) fn resolve_ts_repo_from_workspace_file(workspace_file: &Path) -> Result<PathBuf> {
    let content = fs::read_to_string(workspace_file).with_context(|| {
        format!("failed to read workspace file {}", workspace_file.display())
    })?;
    let spec: WorkspaceFileSpec = serde_json::from_str(&content).with_context(|| {
        format!("failed to parse workspace file {}", workspace_file.display())
    })?;
    let base_dir = workspace_file
        .parent()
        .context("workspace file has no parent directory")?;

    let ts_root = spec
        .folders
        .into_iter()
        .map(|entry| base_dir.join(entry.path))
        .find(|candidate| candidate.join("src").is_dir() && candidate.join("package.json").is_file())
        .context("workspace file does not point to the TypeScript rewrite source tree")?;

    Ok(fs::canonicalize(&ts_root).unwrap_or(ts_root))
}

pub(crate) fn list_ts_runtime_modules(ts_root: &Path) -> Result<Vec<String>> {
    let src_root = ts_root.join("src");
    let mut modules = fs::read_dir(&src_root)
        .with_context(|| format!("failed to list {}", src_root.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || name.ends_with(".test.ts")
                || name.ends_with(".test.tsx")
                || name.ends_with(".d.ts")
            {
                return None;
            }
            Some(name.trim_end_matches('/').to_owned())
        })
        .collect::<Vec<_>>();
    modules.sort();
    Ok(modules)
}

pub(crate) fn list_rust_runtime_modules(rewrite_root: &Path) -> Result<Vec<String>> {
    let crates_root = rewrite_root.join("crates");
    let mut crates = fs::read_dir(&crates_root)
        .with_context(|| format!("failed to list {}", crates_root.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    crates.sort();
    Ok(crates)
}

pub(crate) fn audit_module_parity(cwd: &Path) -> Result<ModuleParityAudit> {
    let workspace_file = find_rewrite_workspace_file(cwd).with_context(|| {
        format!(
            "could not find Claude-Code-main-run.code-workspace from {}",
            cwd.display()
        )
    })?;
    audit_module_parity_with_workspace_file(&workspace_file)
}

pub(crate) fn audit_module_parity_with_workspace_file(
    workspace_file: &Path,
) -> Result<ModuleParityAudit> {
    let rewrite_root = workspace_file
        .parent()
        .context("workspace file has no parent directory")?
        .to_path_buf();
    let ts_root = resolve_ts_repo_from_workspace_file(workspace_file)?;
    let ts_modules = list_ts_runtime_modules(&ts_root)?;
    let rust_modules = list_rust_runtime_modules(&rewrite_root)?;

    let mut manifest_lookup = BTreeMap::new();
    let mut duplicate_manifest_entries = Vec::new();
    for entry in module_parity_manifest() {
        if manifest_lookup.insert(entry.ts_module, entry).is_some() {
            duplicate_manifest_entries.push(entry.ts_module.to_owned());
        }
    }

    let ts_module_set = ts_modules.iter().cloned().collect::<BTreeSet<_>>();
    let manifest_module_set = module_parity_manifest()
        .iter()
        .map(|entry| entry.ts_module.to_owned())
        .collect::<BTreeSet<_>>();
    let unclassified_ts_modules = ts_modules
        .iter()
        .filter(|module| !manifest_lookup.contains_key(module.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let stale_manifest_entries = manifest_module_set
        .difference(&ts_module_set)
        .cloned()
        .collect::<Vec<_>>();
    let mapped_rust_crates = module_parity_manifest()
        .iter()
        .flat_map(|entry| entry.rust_modules.iter().copied())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    let unmapped_rust_crates = rust_modules
        .iter()
        .filter(|crate_name| !mapped_rust_crates.contains(crate_name.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    Ok(ModuleParityAudit {
        workspace_file: workspace_file.to_path_buf(),
        rewrite_root,
        ts_root,
        ts_modules,
        rust_modules,
        duplicate_manifest_entries,
        unclassified_ts_modules,
        stale_manifest_entries,
        unmapped_rust_crates,
    })
}

pub(crate) fn render_doctor_command(cwd: &Path) -> Result<String> {
    let Ok(audit) = audit_module_parity(cwd) else {
        return Ok([
            "Doctor".to_owned(),
            format!(
                "Rewrite parity audit is unavailable from {} because Claude-Code-main-run.code-workspace was not found.",
                cwd.display()
            ),
            "Use /status, /config, /mcp, /plugin, /skills, and /tasks for runtime inspection.".to_owned(),
        ]
        .join("\n"));
    };

    let manifest_lookup = module_parity_manifest()
        .iter()
        .map(|entry| (entry.ts_module, entry))
        .collect::<BTreeMap<_, _>>();

    let mut lines = vec![
        "Rewrite parity audit".to_owned(),
        format!("Workspace file: {}", audit.workspace_file.display()),
        format!("TypeScript root: {}", audit.ts_root.display()),
        format!("Rust root: {}", audit.rewrite_root.display()),
        format!("TypeScript runtime modules: {}", audit.ts_modules.len()),
        format!("Rust runtime crates: {}", audit.rust_modules.len()),
        format!("Ported: {}", audit.status_count(ModuleParityStatus::Ported)),
        format!("Split: {}", audit.status_count(ModuleParityStatus::Split)),
        format!("Removed: {}", audit.status_count(ModuleParityStatus::Removed)),
        format!("Duplicate manifest entries: {}", audit.duplicate_manifest_entries.len()),
        format!("Unclassified TS modules: {}", audit.unclassified_ts_modules.len()),
        format!("Stale manifest entries: {}", audit.stale_manifest_entries.len()),
        format!("Unmapped Rust crates: {}", audit.unmapped_rust_crates.len()),
    ];

    if !audit.duplicate_manifest_entries.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Duplicate manifest entries: {}",
            audit.duplicate_manifest_entries.join(", ")
        ));
    }
    if !audit.unclassified_ts_modules.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Unclassified TS modules: {}",
            audit.unclassified_ts_modules.join(", ")
        ));
    }
    if !audit.stale_manifest_entries.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Stale manifest entries: {}",
            audit.stale_manifest_entries.join(", ")
        ));
    }
    if !audit.unmapped_rust_crates.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "Unmapped Rust crates: {}",
            audit.unmapped_rust_crates.join(", ")
        ));
    }

    let removed_entries = audit
        .ts_modules
        .iter()
        .filter_map(|module| manifest_lookup.get(module.as_str()).copied())
        .filter(|entry| entry.status == ModuleParityStatus::Removed)
        .collect::<Vec<_>>();
    if !removed_entries.is_empty() {
        lines.push(String::new());
        lines.push("Removed intentionally".to_owned());
        for entry in removed_entries {
            let owners = if entry.rust_modules.is_empty() {
                "none".to_owned()
            } else {
                entry.rust_modules.join(", ")
            };
            lines.push(format!(
                "- {} -> {} | {} | {}",
                entry.ts_module,
                entry.status.label(),
                owners,
                entry.note
            ));
        }
    }

    lines.push(String::new());
    lines.push("Module map".to_owned());
    for module in &audit.ts_modules {
        if let Some(entry) = manifest_lookup.get(module.as_str()) {
            let owners = if entry.rust_modules.is_empty() {
                "none".to_owned()
            } else {
                entry.rust_modules.join(", ")
            };
            lines.push(format!(
                "- {} -> {} | {} | {}",
                entry.ts_module,
                entry.status.label(),
                owners,
                entry.note
            ));
        }
    }

    Ok(lines.join("\n"))
}