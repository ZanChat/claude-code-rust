pub(crate) const ROOT_COMMAND_NAME: &str = "ccrust";
pub(crate) const ROOT_DESCRIPTION: &str =
    "Claude Code - starts an interactive session by default, use -p/--print for non-interactive output";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TopLevelCommandName {
    Mcp,
    Server,
    Open,
    Auth,
    Plugin,
    SetupToken,
    Agents,
    RemoteControl,
    Doctor,
    Update,
    Install,
    Rollback,
    Export,
    Task,
    Completion,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TopLevelCommandSpec {
    pub(crate) name: TopLevelCommandName,
    pub(crate) primary: &'static str,
    pub(crate) aliases: &'static [&'static str],
    pub(crate) description: &'static str,
    pub(crate) hidden: bool,
    pub(crate) usage: &'static str,
    pub(crate) details: &'static [&'static str],
}

const ROOT_OPTION_LINES: &[&str] = &[
    "  -h, --help                         Display help for command",
    "  -v, --version                      Output the version number",
    "  --provider <provider>              Provider override for this launch",
    "  --session-root <path>              Use an explicit session storage directory",
    "  -p, --print                        Print response and exit",
    "  --output-format <format>           Output format: text, json, stream-json",
    "  --input-format <format>            Input format: text, stream-json",
    "  --json-schema <schema>             JSON Schema for structured output",
    "  --include-hook-events              Include hook lifecycle events in stream-json output",
    "  --include-partial-messages         Include assistant message deltas in stream-json output",
    "  -c, --continue                     Continue the most recent conversation",
    "  -r, --resume [value]               Resume a conversation by session ID or search term",
    "  --fork-session                     Fork the resumed conversation into a new session",
    "  --resume-session-at <message-id>   Resume only through the selected assistant message",
    "  --rewind-files <user-message-id>   Restore files to the selected user turn and exit",
    "  --no-session-persistence           Disable transcript persistence in print mode",
    "  --permission-mode <mode>           Permission mode for the session",
    "  --model <model>                    Model for the current session",
    "  --settings <file-or-json>          Additional settings sources",
    "  --add-dir <directories...>         Additional directories to allow tool access to",
    "  --mcp-config <configs...>          Load MCP servers from JSON files or JSON strings",
    "  --plugin-root <path>               Override the plugin root for this launch",
    "  --ide                              Auto-connect to the IDE when exactly one bridge is available",
    "  --strict-mcp-config                Only use MCP servers provided by --mcp-config",
    "  --plugin-dir <path>                Load a plugin directory for this session only",
    "  --file <specs...>                  Download file resources before starting the session",
    "  --tool <name>                      Invoke a tool directly",
    "  --input <json>                     Tool input JSON for --tool",
];

const MCP_DETAILS: &[&str] = &[
    "",
    "Configure and manage MCP servers",
    "",
    "Subcommands:",
    "  add <name> <commandOrUrl>          Add an MCP server",
    "  add-json <name> <json>             Add an MCP server from a JSON string",
    "  add-from-claude-desktop            Import MCP servers from Claude Desktop",
    "  get <name>                         Show a configured MCP server",
    "  list                               List configured MCP servers",
    "  remove <name>                      Remove an MCP server",
    "  reset-project-choices              Reset project-scoped server approvals",
    "  serve                              Start the Claude Code MCP server",
];

const SERVER_DETAILS: &[&str] = &[
    "",
    "Start a Claude Code session server",
    "",
    "Options:",
    "  --host <string>                    Bind address",
    "  --port <number>                    HTTP port",
    "  --auth-token <token>               Bearer token for auth",
    "  --unix <path>                      Listen on a unix domain socket",
    "  --workspace <dir>                  Default working directory for detached sessions",
    "  --idle-timeout <ms>                Idle timeout for detached sessions in milliseconds",
    "  --max-sessions <n>                 Maximum concurrent sessions",
];

const OPEN_DETAILS: &[&str] = &[
    "",
    "Connect to a Claude Code server",
    "",
    "Options:",
    "  -p, --print [prompt]               Print mode (headless)",
    "  --output-format <format>           Output format: text, json, stream-json",
];

const AUTH_DETAILS: &[&str] = &[
    "",
    "Manage authentication",
    "",
    "Subcommands:",
    "  login                              Sign in",
    "  logout                             Clear the active login state",
    "  status                             Show authentication status",
];

const PLUGIN_DETAILS: &[&str] = &[
    "",
    "Manage Claude Code plugins",
    "",
    "Subcommands:",
    "  validate <path>                    Validate a plugin or marketplace manifest",
    "  list                               List installed plugins",
    "  install <plugin>                   Install a plugin",
    "  uninstall <plugin>                 Uninstall a plugin",
    "  enable <plugin>                    Enable a plugin",
    "  disable [plugin]                   Disable a plugin",
    "  update <plugin>                    Update a plugin",
    "  marketplace <subcommand>           Manage marketplaces",
];

const SETUP_TOKEN_DETAILS: &[&str] = &[
    "",
    "Set up a long-lived authentication token.",
    "",
    "The Rust CLI routes this through the managed login onboarding flow.",
];

const AGENTS_DETAILS: &[&str] = &[
    "",
    "List configured agents",
    "",
    "Options:",
    "  --setting-sources <sources>        Comma-separated setting sources to load",
];

const REMOTE_CONTROL_DETAILS: &[&str] = &[
    "",
    "Connect your local environment for remote-control sessions via claude.ai/code",
];

const DOCTOR_DETAILS: &[&str] = &[
    "",
    "Check the health of the rewrite and runtime parity surface.",
];

const UPDATE_DETAILS: &[&str] = &[
    "",
    "Install the current ccrust binary into ~/.cargo/bin/ccrust and refresh the rollback snapshot history.",
    "",
    "Aliases:",
    "  upgrade                            Alias for update",
];

const INSTALL_DETAILS: &[&str] = &[
    "",
    "Install a ccrust binary into ~/.cargo/bin/ccrust.",
    "",
    "Arguments:",
    "  [target]                           Current binary, a local executable path, or a recorded snapshot label",
    "",
    "Options:",
    "  --force                            Reinstall even if the destination already matches the source",
];

const ROLLBACK_DETAILS: &[&str] = &[
    "",
    "Restore a previously snapshotted local ccrust install.",
    "",
    "Arguments:",
    "  [target]                           Snapshot index (1-based), label, or snapshot file name",
    "",
    "Options:",
    "  -l, --list                         Show available rollback snapshots",
    "  --dry-run                          Preview the selected rollback target without restoring it",
    "  --safe                             Restore the newest recorded rollback snapshot",
];

const EXPORT_DETAILS: &[&str] = &[
    "",
    "Export a transcript to a plain-text file.",
    "",
    "Arguments:",
    "  <source>                           Session ID, session index, or .json/.jsonl transcript path",
    "  <outputFile>                       Destination text file path",
];

const TASK_DETAILS: &[&str] = &[
    "",
    "Manage local task records.",
    "",
    "Subcommands:",
    "  create <subject>                   Create a task",
    "  list                               List tasks",
    "  get <id>                           Show one task",
    "  update <id>                        Update an existing task",
    "  dir                                Show the backing task directory",
];

const COMPLETION_DETAILS: &[&str] = &[
    "",
    "Generate shell completion scripts.",
    "",
    "Arguments:",
    "  <shell>                            bash, zsh, or fish",
    "",
    "Options:",
    "  --output <file>                    Write the completion script to a file instead of stdout",
];

const TOP_LEVEL_COMMAND_SPECS: &[TopLevelCommandSpec] = &[
    TopLevelCommandSpec {
        name: TopLevelCommandName::Mcp,
        primary: "mcp",
        aliases: &[],
        description: "Configure and manage MCP servers",
        hidden: false,
        usage: "ccrust mcp <subcommand> [options]",
        details: MCP_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Server,
        primary: "server",
        aliases: &[],
        description: "Start a Claude Code session server",
        hidden: false,
        usage: "ccrust server [options]",
        details: SERVER_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Open,
        primary: "open",
        aliases: &[],
        description: "Connect to a Claude Code server",
        hidden: false,
        usage: "ccrust open <cc-url> [options]",
        details: OPEN_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Auth,
        primary: "auth",
        aliases: &[],
        description: "Manage authentication",
        hidden: false,
        usage: "ccrust auth <subcommand> [options]",
        details: AUTH_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Plugin,
        primary: "plugin",
        aliases: &["plugins"],
        description: "Manage Claude Code plugins",
        hidden: false,
        usage: "ccrust plugin <subcommand> [options]",
        details: PLUGIN_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::SetupToken,
        primary: "setup-token",
        aliases: &[],
        description: "Set up a long-lived authentication token",
        hidden: false,
        usage: "ccrust setup-token",
        details: SETUP_TOKEN_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Agents,
        primary: "agents",
        aliases: &[],
        description: "List configured agents",
        hidden: false,
        usage: "ccrust agents [options]",
        details: AGENTS_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::RemoteControl,
        primary: "remote-control",
        aliases: &["rc"],
        description: "Connect your local environment for remote-control sessions",
        hidden: true,
        usage: "ccrust remote-control <subcommand> [options]",
        details: REMOTE_CONTROL_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Doctor,
        primary: "doctor",
        aliases: &[],
        description: "Check rewrite and runtime health",
        hidden: false,
        usage: "ccrust doctor",
        details: DOCTOR_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Update,
        primary: "update",
        aliases: &["upgrade"],
        description: "Install the current build and refresh rollback history",
        hidden: false,
        usage: "ccrust update",
        details: UPDATE_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Install,
        primary: "install",
        aliases: &[],
        description: "Install ccrust into ~/.cargo/bin",
        hidden: false,
        usage: "ccrust install [target] [--force]",
        details: INSTALL_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Rollback,
        primary: "rollback",
        aliases: &[],
        description: "Restore a previous local install snapshot",
        hidden: false,
        usage: "ccrust rollback [target] [options]",
        details: ROLLBACK_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Export,
        primary: "export",
        aliases: &[],
        description: "Export a transcript to text",
        hidden: false,
        usage: "ccrust export <source> <outputFile>",
        details: EXPORT_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Task,
        primary: "task",
        aliases: &[],
        description: "Manage local task records",
        hidden: false,
        usage: "ccrust task <subcommand> [options]",
        details: TASK_DETAILS,
    },
    TopLevelCommandSpec {
        name: TopLevelCommandName::Completion,
        primary: "completion",
        aliases: &[],
        description: "Generate shell completions",
        hidden: false,
        usage: "ccrust completion <shell> [--output <file>]",
        details: COMPLETION_DETAILS,
    },
];

pub(crate) fn top_level_command_specs() -> &'static [TopLevelCommandSpec] {
    TOP_LEVEL_COMMAND_SPECS
}

pub(crate) fn top_level_command_spec(name: TopLevelCommandName) -> &'static TopLevelCommandSpec {
    TOP_LEVEL_COMMAND_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .expect("top-level command spec must exist")
}

pub(crate) fn match_top_level_command_name(token: &str) -> Option<TopLevelCommandName> {
    TOP_LEVEL_COMMAND_SPECS.iter().find_map(|spec| {
        (spec.primary == token || spec.aliases.contains(&token)).then_some(spec.name)
    })
}

pub(crate) fn is_known_top_level_command_token(token: &str) -> bool {
    match_top_level_command_name(token).is_some()
}

pub(crate) fn render_root_help_text() -> String {
    let mut lines = vec![
        format!("Usage: {ROOT_COMMAND_NAME} [options] [prompt]"),
        format!("       {ROOT_COMMAND_NAME} [options] <command> [command-options]"),
        String::new(),
        ROOT_DESCRIPTION.to_owned(),
        String::new(),
        "Commands:".to_owned(),
    ];

    let mut visible_commands = TOP_LEVEL_COMMAND_SPECS
        .iter()
        .filter(|spec| !spec.hidden)
        .collect::<Vec<_>>();
    visible_commands.sort_by_key(|spec| spec.primary);
    lines.extend(
        visible_commands
            .into_iter()
            .map(|spec| format!("  {:<18} {}", spec.primary, spec.description)),
    );

    lines.push(String::new());
    lines.push("Options:".to_owned());
    lines.extend(ROOT_OPTION_LINES.iter().map(|line| (*line).to_owned()));
    lines.join("\n")
}

pub(crate) fn render_top_level_command_help_text(name: TopLevelCommandName) -> String {
    let spec = top_level_command_spec(name);
    let mut lines = vec![format!("Usage: {}", spec.usage)];
    lines.extend(spec.details.iter().map(|line| (*line).to_owned()));
    lines.join("\n")
}

pub(crate) fn render_unknown_option_error(option: &str) -> String {
    format!("error: unknown option '{option}'\n\nRun '{ROOT_COMMAND_NAME} --help' for usage.")
}
