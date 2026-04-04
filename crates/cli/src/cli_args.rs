use std::collections::BTreeMap;
use std::env;
use std::path::PathBuf;

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
    pub(crate) prompt: Vec<String>,
}

pub(crate) fn parse_assignment_args(args: &[String]) -> BTreeMap<String, String> {
    args.iter()
        .filter_map(|arg| arg.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

pub(crate) fn parse_cli() -> Cli {
    let mut cli = Cli::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => cli.provider = args.next(),
            "--model" => cli.model = args.next(),
            "--session-root" => cli.session_root = args.next().map(PathBuf::from),
            "--print-workspace" => cli.print_workspace = true,
            "--list-commands" => cli.list_commands = true,
            "--list-sessions" => cli.list_sessions = true,
            "--tui" => cli.tui = true,
            "--repl" => cli.repl = true,
            "-c" | "--continue" => cli.continue_latest = true,
            "--plugin-root" => cli.plugin_root = args.next().map(PathBuf::from),
            "--show-plugin" => cli.show_plugin = true,
            "--list-skills" => cli.list_skills = true,
            "--list-mcp" => cli.list_mcp = true,
            "--bridge-server" => cli.bridge_server = args.next(),
            "--bridge-connect" => cli.bridge_connect = args.next(),
            "--bridge-receive-count" => {
                cli.bridge_receive_count = args.next().and_then(|value| value.parse().ok())
            }
            "--assistant-directive" => cli.assistant_directive = args.next(),
            "--assistant-agent" => cli.assistant_agent = args.next(),
            "--voice-text" => cli.voice_text = args.next(),
            "--voice-file" => cli.voice_file = args.next().map(PathBuf::from),
            "--voice-format" => cli.voice_format = args.next(),
            "--resume" => cli.resume = args.next(),
            "--clear-session" => cli.clear_session = args.next(),
            "--tool" => cli.tool = args.next(),
            "--input" => cli.input = args.next(),
            "--help" | "-h" => {
                println!(
                    "Usage: code-agent-rust [--provider NAME] [--model NAME] [-c|--continue] [--resume TARGET] [--list-sessions] [--tool NAME --input JSON] [--tui|--repl] [--voice-text TEXT|--voice-file PATH] [--bridge-server ADDR|tcp://ADDR --bridge-connect URL|tcp://ADDR] [prompt]"
                );
                println!("Slash commands such as '/help', '/resume <session>', '/clear', '/compact', '/model', and '/config' are supported.");
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("{}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            value => cli.prompt.push(value.to_owned()),
        }
    }

    cli
}
