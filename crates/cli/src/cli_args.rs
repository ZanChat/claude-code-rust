mod compat;

use crate::cli_graph::{is_known_top_level_command_token, render_root_help_text};
use anyhow::{anyhow, bail, Result};
use ccrust_tools::ToolPermissionMode;
use compat::{find_ts_top_level_option, find_ts_top_level_short_option, ts_top_level_option_specs};
pub(crate) use compat::{CompatOptionSpec, CompatValueKind};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeCliOptions {
    pub(crate) max_turns: Option<usize>,
    pub(crate) tool_permission_mode: Option<ToolPermissionMode>,
}

#[derive(Debug, Default)]
pub(crate) struct Cli {
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) session_root: Option<PathBuf>,
    pub(crate) print_workspace: bool,
    pub(crate) list_commands: bool,
    pub(crate) list_sessions: bool,
    pub(crate) tui: bool,
    pub(crate) repl: bool,
    pub(crate) plugin_root: Option<PathBuf>,
    pub(crate) show_plugin: bool,
    pub(crate) list_skills: bool,
    pub(crate) list_mcp: bool,
    pub(crate) bridge_server: Option<String>,
    pub(crate) bridge_connect: Option<String>,
    pub(crate) bridge_receive_count: Option<usize>,
    pub(crate) assistant_directive: Option<String>,
    pub(crate) assistant_agent: Option<String>,
    pub(crate) voice_text: Option<String>,
    pub(crate) voice_file: Option<PathBuf>,
    pub(crate) voice_format: Option<String>,
    pub(crate) continue_latest: bool,
    pub(crate) resume: Option<String>,
    pub(crate) clear_session: Option<String>,
    pub(crate) tool: Option<String>,
    pub(crate) input: Option<String>,
    pub(crate) permission_mode: Option<String>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) compat_flags: BTreeSet<String>,
    pub(crate) compat_values: BTreeMap<String, Vec<String>>,
    pub(crate) command_tokens: Vec<String>,
    pub(crate) prompt: Vec<String>,
}

enum ParsedCli {
    Cli(Cli),
    Help,
    Version,
}

impl Cli {
    pub(crate) fn compat_flag_enabled(&self, name: &str) -> bool {
        self.compat_flags.contains(name)
    }

    pub(crate) fn compat_value(&self, name: &str) -> Option<&str> {
        self.compat_values
            .get(name)
            .and_then(|values| values.first().map(String::as_str))
    }

    pub(crate) fn compat_values_owned(&self, name: &str) -> Vec<String> {
        self.compat_values.get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn runtime_options(&self) -> RuntimeCliOptions {
        RuntimeCliOptions {
            max_turns: self.max_turns,
            tool_permission_mode: self.tool_permission_mode(),
        }
    }

    pub(crate) fn tool_permission_mode(&self) -> Option<ToolPermissionMode> {
        if self.compat_flag_enabled("dangerously-skip-permissions")
            || self.compat_flag_enabled("dangerously-skip-permissions-with-classifiers")
        {
            return Some(ToolPermissionMode::Allow);
        }

        self.permission_mode
            .as_deref()
            .and_then(map_ts_permission_mode)
    }
}

pub(crate) fn parse_assignment_args(args: &[String]) -> BTreeMap<String, String> {
    args.iter()
        .filter_map(|arg| arg.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

pub(crate) fn ts_top_level_cli_option_specs() -> &'static [CompatOptionSpec] {
    ts_top_level_option_specs()
}

pub(crate) fn parse_cli() -> Result<Cli> {
    match parse_cli_tokens(env::args().skip(1).collect())? {
        ParsedCli::Cli(cli) => Ok(cli),
        ParsedCli::Help => {
            print_help();
            std::process::exit(0);
        }
        ParsedCli::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
    }
}

pub(crate) fn parse_cli_from(args: Vec<String>) -> Result<Cli> {
    match parse_cli_tokens(args)? {
        ParsedCli::Cli(cli) => Ok(cli),
        ParsedCli::Help => bail!("--help is only available via the real CLI entrypoint"),
        ParsedCli::Version => bail!("--version is only available via the real CLI entrypoint"),
    }
}

fn parse_cli_tokens(args: Vec<String>) -> Result<ParsedCli> {
    let mut cli = Cli::default();
    let mut index = 0usize;
    let mut positional_only = false;

    while index < args.len() {
        let arg = &args[index];

        if positional_only {
            cli.prompt.push(arg.clone());
            index += 1;
            continue;
        }

        if arg == "--" {
            positional_only = true;
            index += 1;
            continue;
        }

        match arg.as_str() {
            "--help" | "-h" => return Ok(ParsedCli::Help),
            "--version" | "-v" | "-V" => return Ok(ParsedCli::Version),
            _ => {}
        }

        if let Some(short) = parse_short_option(arg) {
            let spec = find_ts_top_level_short_option(short)
                .ok_or_else(|| anyhow!("unknown option: {arg}"))?;
            let values = consume_option_values(&args, &mut index, arg, spec, None)?;
            apply_compat_option(&mut cli, spec, values)?;
            index += 1;
            continue;
        }

        if let Some((name, inline_value)) = parse_long_option(arg) {
            match name {
                "provider" => {
                    cli.provider = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "session-root" => {
                    cli.session_root = Some(PathBuf::from(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?))
                }
                "print-workspace" => cli.print_workspace = true,
                "list-commands" => cli.list_commands = true,
                "list-sessions" => cli.list_sessions = true,
                "tui" => cli.tui = true,
                "repl" => cli.repl = true,
                "plugin-root" => {
                    cli.plugin_root = Some(PathBuf::from(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?))
                }
                "show-plugin" => cli.show_plugin = true,
                "list-skills" => cli.list_skills = true,
                "list-mcp" => cli.list_mcp = true,
                "bridge-server" => {
                    cli.bridge_server = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "bridge-connect" => {
                    cli.bridge_connect = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "bridge-receive-count" => {
                    let value = consume_required_value(&args, &mut index, arg, inline_value)?;
                    cli.bridge_receive_count = Some(parse_usize_value(arg, &value)?);
                }
                "assistant-directive" => {
                    cli.assistant_directive = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "assistant-agent" => {
                    cli.assistant_agent = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "voice-text" => {
                    cli.voice_text = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "voice-file" => {
                    cli.voice_file = Some(PathBuf::from(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?))
                }
                "voice-format" => {
                    cli.voice_format = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "clear-session" => {
                    cli.clear_session = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "tool" => {
                    cli.tool = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                "input" => {
                    cli.input = Some(consume_required_value(
                        &args,
                        &mut index,
                        arg,
                        inline_value,
                    )?)
                }
                _ => {
                    if let Some(spec) = find_ts_top_level_option(name) {
                        let values =
                            consume_option_values(&args, &mut index, arg, spec, inline_value)?;
                        apply_compat_option(&mut cli, spec, values)?;
                        index += 1;
                        continue;
                    }
                    bail!("unknown option: {arg}");
                }
            }

            index += 1;
            continue;
        }

        if cli.prompt.is_empty() && is_known_top_level_command_token(arg) {
            cli.command_tokens = args[index..].to_vec();
            break;
        }

        if cli.prompt.is_empty() && (arg.starts_with("cc://") || arg.starts_with("cc+unix://")) {
            cli.prompt.push(arg.clone());
            index += 1;
            continue;
        }

        if arg.starts_with('-') {
            bail!("unknown option: {arg}");
        }

        cli.prompt.push(arg.clone());
        index += 1;
    }

    Ok(ParsedCli::Cli(cli))
}

fn parse_short_option(arg: &str) -> Option<char> {
    let short = arg.strip_prefix('-')?;
    if short.starts_with('-') || short.len() != 1 {
        return None;
    }
    short.chars().next()
}

fn parse_long_option(arg: &str) -> Option<(&str, Option<String>)> {
    let option = arg.strip_prefix("--")?;
    if option.is_empty() {
        return None;
    }

    option.split_once('=').map_or_else(
        || Some((option, None)),
        |(name, value)| Some((name, Some(value.to_owned()))),
    )
}

fn consume_required_value(
    args: &[String],
    index: &mut usize,
    display: &str,
    inline_value: Option<String>,
) -> Result<String> {
    if let Some(value) = inline_value {
        return Ok(value);
    }

    let next_index = *index + 1;
    let value = args
        .get(next_index)
        .ok_or_else(|| anyhow!("missing value for {display}"))?;
    if value.starts_with('-') {
        bail!("missing value for {display}");
    }
    *index = next_index;
    Ok(value.clone())
}

fn consume_optional_value(
    args: &[String],
    index: &mut usize,
    inline_value: Option<String>,
) -> Option<String> {
    if let Some(value) = inline_value {
        return Some(value);
    }

    let next_index = *index + 1;
    let value = args.get(next_index)?;
    if value.starts_with('-') {
        return None;
    }

    *index = next_index;
    Some(value.clone())
}

fn consume_variadic_values(
    args: &[String],
    index: &mut usize,
    inline_value: Option<String>,
) -> Vec<String> {
    let mut values = Vec::new();
    if let Some(value) = inline_value {
        values.push(value);
    }

    loop {
        let next_index = *index + 1;
        let Some(value) = args.get(next_index) else {
            break;
        };
        if value.starts_with('-') {
            break;
        }
        *index = next_index;
        values.push(value.clone());
    }

    values
}

fn consume_option_values(
    args: &[String],
    index: &mut usize,
    display: &str,
    spec: &CompatOptionSpec,
    inline_value: Option<String>,
) -> Result<Vec<String>> {
    match spec.value_kind {
        CompatValueKind::Flag => {
            if inline_value.is_some() {
                bail!("{display} does not accept a value");
            }
            Ok(Vec::new())
        }
        CompatValueKind::RequiredValue => Ok(vec![consume_required_value(
            args,
            index,
            display,
            inline_value,
        )?]),
        CompatValueKind::OptionalValue => Ok(consume_optional_value(args, index, inline_value)
            .into_iter()
            .collect()),
        CompatValueKind::OneOrMoreValues => {
            let values = consume_variadic_values(args, index, inline_value);
            if values.is_empty() {
                bail!("missing value for {display}");
            }
            Ok(values)
        }
    }
}

fn apply_compat_option(cli: &mut Cli, spec: &CompatOptionSpec, values: Vec<String>) -> Result<()> {
    match spec.canonical {
        "continue" => cli.continue_latest = true,
        "resume" => {
            if let Some(value) = values.first() {
                cli.resume = Some(value.clone());
            } else {
                cli.continue_latest = true;
            }
        }
        "model" => {
            if let Some(value) = values.first() {
                cli.model = Some(value.clone());
            }
        }
        "permission-mode" => {
            let value = values
                .first()
                .ok_or_else(|| anyhow!("missing value for --permission-mode"))?;
            validate_permission_mode(value)?;
            cli.permission_mode = Some(value.clone());
        }
        "max-turns" => {
            let value = values
                .first()
                .ok_or_else(|| anyhow!("missing value for --max-turns"))?;
            let parsed = parse_usize_value("--max-turns", value)?;
            if parsed == 0 {
                bail!("--max-turns must be greater than 0");
            }
            cli.max_turns = Some(parsed);
        }
        _ => {}
    }

    cli.compat_flags.insert(spec.canonical.to_owned());
    if !values.is_empty() {
        cli.compat_values
            .entry(spec.canonical.to_owned())
            .or_default()
            .extend(values);
    }
    Ok(())
}

fn parse_usize_value(display: &str, value: &str) -> Result<usize> {
    value
        .parse::<usize>()
        .map_err(|error| anyhow!("invalid value for {display}: {value} ({error})"))
}

fn validate_permission_mode(value: &str) -> Result<()> {
    if map_ts_permission_mode(value).is_some() {
        return Ok(());
    }

    bail!(
        "invalid value for --permission-mode: {value} (expected one of: acceptEdits, auto, bubble, bypassPermissions, default, dontAsk, plan)"
    )
}

fn map_ts_permission_mode(value: &str) -> Option<ToolPermissionMode> {
    match value {
        "bypassPermissions" => Some(ToolPermissionMode::Allow),
        "acceptEdits" | "auto" | "bubble" | "default" | "plan" => Some(ToolPermissionMode::Ask),
        "dontAsk" => Some(ToolPermissionMode::Deny),
        _ => None,
    }
}

fn print_help() {
    println!("{}", render_root_help_text());
}
