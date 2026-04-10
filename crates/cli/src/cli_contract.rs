use crate::cli_args::{Cli, RuntimeCliOptions};
use crate::cli_graph::{match_top_level_command_name, TopLevelCommandName};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OutputMode {
    Text,
    Json,
    StreamJson,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InputMode {
    Text,
    StreamJson,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrintModeConfig {
    pub(crate) enabled: bool,
    pub(crate) output_mode: OutputMode,
    pub(crate) input_mode: InputMode,
    pub(crate) json_schema: Option<String>,
    pub(crate) include_hook_events: bool,
    pub(crate) include_partial_messages: bool,
    pub(crate) replay_user_messages: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResumeTargetConfig {
    pub(crate) continue_latest: bool,
    pub(crate) resume_target: Option<String>,
    pub(crate) fork_session: bool,
    pub(crate) from_pr: Option<String>,
    pub(crate) resume_session_at: Option<String>,
    pub(crate) rewind_files: Option<String>,
    pub(crate) session_persistence: bool,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BackgroundSessionCommand {
    pub(crate) name: Option<String>,
    pub(crate) args: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RemoteSessionServerCommand {
    pub(crate) args: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpdatePipelineCommand {
    pub(crate) name: String,
    pub(crate) args: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GlobalOptions {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) session_root: Option<PathBuf>,
    pub(crate) plugin_root: Option<PathBuf>,
    pub(crate) print_mode: PrintModeConfig,
    pub(crate) resume: ResumeTargetConfig,
    pub(crate) add_dirs: Vec<String>,
    pub(crate) settings: Option<String>,
    pub(crate) permission_mode: Option<String>,
    pub(crate) ide: bool,
    pub(crate) strict_mcp_config: bool,
    pub(crate) files: Vec<String>,
    pub(crate) worktree: Option<String>,
    pub(crate) tmux: bool,
    pub(crate) runtime: RuntimeCliOptions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TopLevelCommand {
    RootPrompt {
        prompt: Option<String>,
    },
    Named {
        name: String,
        command_name: Option<TopLevelCommandName>,
        args: Vec<String>,
    },
    ToolRun {
        tool_name: String,
    },
    BridgeConnect {
        address: String,
    },
    BridgeServer {
        address: String,
    },
    ListCommands,
    ListSessions,
    ShowPlugin,
    ListSkills,
    ListMcp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FastPathCommand {
    OpenUrl { url: String },
    SubcommandHelp { name: TopLevelCommandName },
    Ssh { args: Vec<String> },
    Open { args: Vec<String> },
    RemoteControl { args: Vec<String> },
    Server(RemoteSessionServerCommand),
    Update(UpdatePipelineCommand),
    Completion { args: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CliContract {
    pub(crate) global: GlobalOptions,
    pub(crate) command: TopLevelCommand,
    pub(crate) fast_path: Option<FastPathCommand>,
}

impl Default for OutputMode {
    fn default() -> Self {
        Self::Text
    }
}

impl Default for InputMode {
    fn default() -> Self {
        Self::Text
    }
}

pub(crate) fn build_cli_contract(cli: &Cli) -> CliContract {
    let print_mode = PrintModeConfig {
        enabled: cli.compat_flag_enabled("print"),
        output_mode: match cli.compat_value("output-format") {
            Some("json") => OutputMode::Json,
            Some("stream-json") => OutputMode::StreamJson,
            _ => OutputMode::Text,
        },
        input_mode: match cli.compat_value("input-format") {
            Some("stream-json") => InputMode::StreamJson,
            _ => InputMode::Text,
        },
        json_schema: cli.compat_value("json-schema").map(ToOwned::to_owned),
        include_hook_events: cli.compat_flag_enabled("include-hook-events"),
        include_partial_messages: cli.compat_flag_enabled("include-partial-messages"),
        replay_user_messages: cli.compat_flag_enabled("replay-user-messages"),
    };

    let resume = ResumeTargetConfig {
        continue_latest: cli.continue_latest,
        resume_target: cli.resume.clone(),
        fork_session: cli.compat_flag_enabled("fork-session"),
        from_pr: cli.compat_value("from-pr").map(ToOwned::to_owned),
        resume_session_at: cli.compat_value("resume-session-at").map(ToOwned::to_owned),
        rewind_files: cli.compat_value("rewind-files").map(ToOwned::to_owned),
        session_persistence: !cli.compat_flag_enabled("no-session-persistence"),
    };

    let global = GlobalOptions {
        provider: cli.provider.clone(),
        model: cli.model.clone(),
        session_root: cli.session_root.clone(),
        plugin_root: cli.plugin_root.clone(),
        print_mode,
        resume,
        add_dirs: cli.compat_values_owned("add-dir"),
        settings: cli.compat_value("settings").map(ToOwned::to_owned),
        permission_mode: cli.permission_mode.clone(),
        ide: cli.compat_flag_enabled("ide"),
        strict_mcp_config: cli.compat_flag_enabled("strict-mcp-config"),
        files: cli.compat_values_owned("file"),
        worktree: cli.compat_value("worktree").map(ToOwned::to_owned),
        tmux: cli.compat_flag_enabled("tmux"),
        runtime: cli.runtime_options(),
    };

    let command = if cli.list_commands {
        TopLevelCommand::ListCommands
    } else if cli.list_sessions {
        TopLevelCommand::ListSessions
    } else if cli.show_plugin {
        TopLevelCommand::ShowPlugin
    } else if cli.list_skills {
        TopLevelCommand::ListSkills
    } else if cli.list_mcp {
        TopLevelCommand::ListMcp
    } else if let Some(address) = cli.bridge_connect.clone() {
        TopLevelCommand::BridgeConnect { address }
    } else if let Some(address) = cli.bridge_server.clone() {
        TopLevelCommand::BridgeServer { address }
    } else if let Some(tool_name) = cli.tool.clone() {
        TopLevelCommand::ToolRun { tool_name }
    } else if let Some((name, args)) = cli.command_tokens.split_first() {
        TopLevelCommand::Named {
            name: name.clone(),
            command_name: match_top_level_command_name(name),
            args: args.to_vec(),
        }
    } else {
        TopLevelCommand::RootPrompt {
            prompt: (!cli.prompt.is_empty()).then(|| cli.prompt.join(" ")),
        }
    };

    let fast_path = match &command {
        TopLevelCommand::Named {
            name,
            command_name,
            args,
        } if args.iter().any(|arg| arg == "--help" || arg == "-h") => {
            command_name.map(|resolved| FastPathCommand::SubcommandHelp { name: resolved })
        }
        TopLevelCommand::Named { name, args, .. } if name == "open" => {
            Some(FastPathCommand::Open { args: args.clone() })
        }
        TopLevelCommand::Named { name, args, .. } if name == "ssh" => {
            Some(FastPathCommand::Ssh { args: args.clone() })
        }
        TopLevelCommand::Named { name, args, .. }
            if name == "remote-control" && !args.is_empty() =>
        {
            Some(FastPathCommand::RemoteControl { args: args.clone() })
        }
        TopLevelCommand::Named { name, args, .. } if name == "server" => {
            Some(FastPathCommand::Server(RemoteSessionServerCommand {
                args: args.clone(),
            }))
        }
        TopLevelCommand::Named { name, args, .. }
            if matches!(
                name.as_str(),
                "update" | "upgrade" | "up" | "rollback" | "install"
            ) =>
        {
            Some(FastPathCommand::Update(UpdatePipelineCommand {
                name: name.clone(),
                args: args.clone(),
            }))
        }
        TopLevelCommand::Named { name, args, .. } if name == "completion" => {
            Some(FastPathCommand::Completion { args: args.clone() })
        }
        TopLevelCommand::RootPrompt {
            prompt: Some(prompt),
        } if prompt.starts_with("cc://") || prompt.starts_with("cc+unix://") => {
            Some(FastPathCommand::OpenUrl {
                url: prompt.clone(),
            })
        }
        _ => None,
    };

    CliContract {
        global,
        command,
        fast_path,
    }
}
