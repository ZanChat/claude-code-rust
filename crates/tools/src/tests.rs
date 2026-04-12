use super::{
    compatibility_tool_registry, compatibility_tool_specs, glob_matches, ToolCallRequest,
    ToolContext, ToolKind, ToolPermissionMode,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMessage;

fn make_temp_dir(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("code-agent-tools-{label}-{stamp}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

struct ScopedEnvVars {
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Vec<(String, Option<String>)>,
}

impl Drop for ScopedEnvVars {
    fn drop(&mut self) {
        for (key, previous) in self.previous.iter().rev() {
            match previous {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }
}

fn scoped_env_vars(vars: &[(&str, Option<&str>)]) -> ScopedEnvVars {
    let lock = env_lock().lock().unwrap();
    let previous = vars
        .iter()
        .map(|(key, _)| ((*key).to_owned(), env::var(key).ok()))
        .collect::<Vec<_>>();

    for (key, value) in vars {
        match value {
            Some(value) => env::set_var(key, value),
            None => env::remove_var(key),
        }
    }

    ScopedEnvVars {
        _lock: lock,
        previous,
    }
}

fn with_env_var<T>(key: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(&self.key, value),
                None => env::remove_var(&self.key),
            }
        }
    }

    let _guard = env_lock().lock().unwrap();
    let restore = EnvVarGuard {
        key: key.to_owned(),
        previous: env::var(key).ok(),
    };

    match value {
        Some(value) => env::set_var(key, value),
        None => env::remove_var(key),
    }

    let result = f();
    drop(restore);
    result
}

#[test]
fn exposes_expected_compatibility_tools() {
    let specs = compatibility_tool_specs();
    let names = specs
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<Vec<_>>();

    for expected in [
        "file_read",
        "bash",
        "mcp",
        "agent",
        "Read",
        "Edit",
        "Write",
        "NotebookEdit",
        "Bash",
        "Glob",
        "Grep",
        "WebFetch",
        "Agent",
        "TaskOutput",
        "TodoWrite",
        "TaskStop",
        "SendMessage",
        "SendUserMessage",
        "AskUserQuestion",
        "EnterPlanMode",
        "ExitPlanMode",
        "EnterWorktree",
        "ExitWorktree",
        "Skill",
        "ToolSearch",
        "ListMcpResourcesTool",
        "ReadMcpResourceTool",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    assert!(specs.iter().any(|spec| spec.kind == ToolKind::Task));
}

#[test]
fn bash_tool_schema_declares_command_input() {
    let registry = compatibility_tool_registry();
    let spec = registry.get("bash").unwrap().spec();
    let object = spec.input_schema.schema.object.as_ref().unwrap();

    assert!(object.properties.contains_key("command"));
    assert!(object.required.contains("command"));
}

#[test]
fn file_edit_tool_schema_declares_expected_fields() {
    let registry = compatibility_tool_registry();
    let spec = registry.get("file_edit").unwrap().spec();
    let object = spec.input_schema.schema.object.as_ref().unwrap();

    assert!(object.properties.contains_key("path"));
    assert!(object.properties.contains_key("old_string"));
    assert!(object.properties.contains_key("new_string"));
    assert!(object.properties.contains_key("replace_all"));
    assert!(object.required.contains("path"));
    assert!(object.required.contains("old_string"));
    assert!(object.required.contains("new_string"));
}

#[test]
fn matches_basic_globs() {
    assert!(glob_matches("src/**/*.rs", "src/cli/main.rs"));
    assert!(glob_matches("*.md", "README.md"));
    assert!(!glob_matches("src/*.rs", "src/cli/main.rs"));
}

#[tokio::test]
async fn reads_and_writes_files_via_registry() {
    let cwd = make_temp_dir("registry");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd: cwd.clone(),
        ..ToolContext::default()
    };

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_write".to_owned(),
                input: json!({
                    "path": "notes/example.txt",
                    "content": "hello from rust"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let read = registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_read".to_owned(),
                input: json!({ "path": "notes/example.txt" }),
            },
            &context,
        )
        .await
        .unwrap();

    assert_eq!(read.content, "hello from rust");
}

#[tokio::test]
async fn deny_permission_mode_blocks_permissioned_tools() {
    let cwd = make_temp_dir("deny-permission-mode");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd: cwd.clone(),
        permission_mode: Some(ToolPermissionMode::Deny),
        ..ToolContext::default()
    };

    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_write".to_owned(),
                input: json!({
                    "path": "blocked.txt",
                    "content": "should not be written"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    assert!(output.is_error);
    assert!(output
        .content
        .contains("blocked by the active permission mode"));
    assert_eq!(output.metadata["permission"], json!("denied"));
    assert!(!cwd.join("blocked.txt").exists());
}

#[tokio::test]
async fn ts_named_file_tools_round_trip_via_registry() {
    let cwd = make_temp_dir("ts-file-tools");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd,
        ..ToolContext::default()
    };

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "Write".to_owned(),
                input: json!({
                    "path": "notes/example.txt",
                    "content": "alpha beta"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "Edit".to_owned(),
                input: json!({
                    "path": "notes/example.txt",
                    "old_string": "beta",
                    "new_string": "gamma"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let read = registry
        .invoke(
            ToolCallRequest {
                tool_name: "Read".to_owned(),
                input: json!({ "path": "notes/example.txt" }),
            },
            &context,
        )
        .await
        .unwrap();

    assert_eq!(read.content, "alpha gamma");
}

#[tokio::test]
async fn bash_tool_accepts_string_and_alias_inputs() {
    let cwd = make_temp_dir("bash");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd,
        ..ToolContext::default()
    };

    let raw = registry
        .invoke(
            ToolCallRequest {
                tool_name: "bash".to_owned(),
                input: json!("printf raw-shell-input"),
            },
            &context,
        )
        .await
        .unwrap();
    let alias = registry
        .invoke(
            ToolCallRequest {
                tool_name: "bash".to_owned(),
                input: json!({ "input": "printf alias-shell-input" }),
            },
            &context,
        )
        .await
        .unwrap();

    assert_eq!(raw.content, "raw-shell-input");
    assert_eq!(alias.content, "alias-shell-input");
    assert!(!raw.is_error);
    assert!(!alias.is_error);
}

#[tokio::test]
async fn bash_tool_truncates_large_output() {
    let cwd = make_temp_dir("bash-truncate");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd,
        ..ToolContext::default()
    };

    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "bash".to_owned(),
                input: json!({
                    "command": "printf 'x%.0s' {1..150000}"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    assert!(output.content.contains("[output truncated]"));
    assert!(output.content.len() < 140_000);
    assert_eq!(output.metadata["truncated_output"], true);
}

#[tokio::test]
async fn task_output_reads_completed_task_output() {
    let cwd = make_temp_dir("task-output");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd,
        ..ToolContext::default()
    };

    let created = registry
        .invoke(
            ToolCallRequest {
                tool_name: "task_create".to_owned(),
                input: json!({
                    "kind": "agent",
                    "title": "compat task"
                }),
            },
            &context,
        )
        .await
        .unwrap();
    let task_id = created.metadata["id"].as_str().unwrap().to_owned();

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "task_update".to_owned(),
                input: json!({
                    "taskId": task_id,
                    "status": "completed",
                    "output": "task output body"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "TaskOutput".to_owned(),
                input: json!({
                    "taskId": task_id,
                    "block": false
                }),
            },
            &context,
        )
        .await
        .unwrap();

    assert_eq!(output.content, "task output body");
    assert_eq!(output.metadata["retrieval_status"], "success");
}

#[tokio::test]
async fn tool_search_supports_keyword_and_select_queries() {
    let registry = compatibility_tool_registry();

    let keyword = registry
        .invoke(
            ToolCallRequest {
                tool_name: "ToolSearch".to_owned(),
                input: json!({
                    "query": "notebook"
                }),
            },
            &ToolContext::default(),
        )
        .await
        .unwrap();
    let selected = registry
        .invoke(
            ToolCallRequest {
                tool_name: "ToolSearch".to_owned(),
                input: json!({
                    "query": "select:Read,Write"
                }),
            },
            &ToolContext::default(),
        )
        .await
        .unwrap();

    assert!(keyword.content.contains("NotebookEdit"));
    assert_eq!(selected.content, "Read\nWrite");
}

#[tokio::test]
async fn skill_tool_reads_legacy_skill_prompt() {
    let cwd = make_temp_dir("skill");
    let skill_dir = cwd.join(".claude").join("skills").join("demo");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "name: demo\n\nUse the demo skill.",
    )
    .unwrap();

    let registry = compatibility_tool_registry();
    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "Skill".to_owned(),
                input: json!({
                    "skill": "demo"
                }),
            },
            &ToolContext {
                cwd,
                ..ToolContext::default()
            },
        )
        .await
        .unwrap();

    assert!(output.content.contains("Use the demo skill."));
    assert_eq!(output.metadata["skill"], "demo");
}

#[tokio::test]
async fn skill_tool_reads_ancestor_project_skill_prompt() {
    let root = make_temp_dir("skill-ancestor-root");
    fs::create_dir_all(root.join(".git")).unwrap();
    let skill_dir = root.join(".claude").join("skills").join("review");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(skill_dir.join("SKILL.md"), "Use the project review skill.").unwrap();
    let cwd = root.join("src/nested");
    fs::create_dir_all(&cwd).unwrap();

    let registry = compatibility_tool_registry();
    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "Skill".to_owned(),
                input: json!({
                    "skill": "review"
                }),
            },
            &ToolContext {
                cwd,
                ..ToolContext::default()
            },
        )
        .await
        .unwrap();

    assert!(output.content.contains("Use the project review skill."));
    assert_eq!(output.metadata["skill"], "review");
}

#[test]
fn skill_tool_reads_user_home_skill_and_expands_prompt() {
    let home = make_temp_dir("skill-home");
    let home_path = home.display().to_string();

    with_env_var("CLAUDE_CONFIG_DIR", Some(&home_path), || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        runtime.block_on(async {
            let cwd = make_temp_dir("skill-home-cwd");
            let skill_dir = home.join("skills").join("triage");
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join("SKILL.md"),
                "---\narguments: target\n---\nReview $target from ${CLAUDE_SKILL_DIR} during ${CLAUDE_SESSION_ID}.",
            )
            .unwrap();

            let session_id = uuid::Uuid::new_v4();
            let registry = compatibility_tool_registry();
            let output = registry
                .invoke(
                    ToolCallRequest {
                        tool_name: "Skill".to_owned(),
                        input: json!({
                            "skill": "triage",
                            "args": "src/lib.rs"
                        }),
                    },
                    &ToolContext {
                        session_id: Some(session_id),
                        cwd,
                        ..ToolContext::default()
                    },
                )
                .await
                .unwrap();

            assert!(output.content.starts_with("Base directory for this skill:"));
            assert!(output.content.contains("Review src/lib.rs"));
            assert!(output.content.contains(&skill_dir.display().to_string()));
            assert!(output.content.contains(&session_id.to_string()));
            assert_eq!(output.metadata["skill"], "triage");
        });
    });
}

#[tokio::test]
async fn ask_user_question_accepts_ts_question_shape() {
    let cwd = make_temp_dir("ask-user-question");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd,
        ..ToolContext::default()
    };

    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "AskUserQuestion".to_owned(),
                input: json!({
                    "questions": [{
                        "question": "Which approach should we use?",
                        "header": "Approach",
                        "options": [
                            {
                                "label": "A",
                                "description": "Use approach A"
                            },
                            {
                                "label": "B",
                                "description": "Use approach B"
                            }
                        ]
                    }]
                }),
            },
            &context,
        )
        .await
        .unwrap();

    assert!(output.content.starts_with("question recorded "));
    assert_eq!(
        output.metadata["questions"][0]["question"],
        "Which approach should we use?"
    );
}

#[tokio::test]
async fn edits_and_reads_memory_via_registry() {
    let cwd = make_temp_dir("memory");
    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd: cwd.clone(),
        ..ToolContext::default()
    };

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_write".to_owned(),
                input: json!({
                    "path": "notes/example.txt",
                    "content": "alpha beta gamma"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_edit".to_owned(),
                input: json!({
                    "path": "notes/example.txt",
                    "old_string": "beta",
                    "new_string": "delta"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let edited = registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_read".to_owned(),
                input: json!({ "path": "notes/example.txt" }),
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(edited.content, "alpha delta gamma");

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_write".to_owned(),
                input: json!({
                    "filePath": "notes/example.txt",
                    "content": "beta beta"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_edit".to_owned(),
                input: json!({
                    "filePath": "notes/example.txt",
                    "oldString": "beta",
                    "new_str": "omega",
                    "replaceAll": true
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let aliased = registry
        .invoke(
            ToolCallRequest {
                tool_name: "file_read".to_owned(),
                input: json!({ "filePath": "notes/example.txt" }),
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(aliased.content, "omega omega");

    registry
        .invoke(
            ToolCallRequest {
                tool_name: "memory".to_owned(),
                input: json!({
                    "action": "write",
                    "value": { "summary": "remember this" }
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let memory = registry
        .invoke(
            ToolCallRequest {
                tool_name: "memory".to_owned(),
                input: json!({ "action": "read" }),
            },
            &context,
        )
        .await
        .unwrap();
    assert!(memory.content.contains("remember this"));
}

#[tokio::test]
async fn fetches_local_http_content() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/plain\r\n",
                "Content-Length: 11\r\n",
                "\r\n",
                "hello fetch"
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    let registry = compatibility_tool_registry();
    let output = registry
        .invoke(
            ToolCallRequest {
                tool_name: "web_fetch".to_owned(),
                input: json!({ "url": format!("http://{address}") }),
            },
            &ToolContext::default(),
        )
        .await
        .unwrap();

    assert_eq!(output.content, "hello fetch");
    assert_eq!(output.metadata["status"], 200);
}

#[tokio::test]
async fn invokes_live_mcp_tools_from_plugin_manifest() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|index| index + 4)
                .unwrap();
            let header_text = String::from_utf8_lossy(&request[..header_end]);
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("Content-Length")
                        .then_some(value.trim())
                })
                .unwrap()
                .parse::<usize>()
                .unwrap();
            while request.len() < header_end + content_length {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let body = serde_json::from_slice::<serde_json::Value>(
                &request[header_end..header_end + content_length],
            )
            .unwrap();
            let response = match body["method"].as_str().unwrap() {
                "tools/call" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "content": [{ "type": "text", "text": "mcp tool result" }],
                        "isError": false
                    }
                }),
                "resources/read" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "contents": [{ "uri": "memory://note", "text": "note body" }]
                    }
                }),
                other => panic!("unexpected method: {other}"),
            };
            let response_body = response.to_string();
            let response_text = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response_text.as_bytes()).unwrap();
        }
    });

    let cwd = make_temp_dir("mcp");
    fs::create_dir_all(cwd.join(".claude-plugin")).unwrap();
    fs::write(
        cwd.join(".claude-plugin/plugin.json"),
        serde_json::to_vec_pretty(&json!({
            "name": "demo-plugin",
            "mcpServers": {
                "demo": {
                    "url": format!("http://{address}")
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd: cwd.clone(),
        ..ToolContext::default()
    };

    let tool_result = registry
        .invoke(
            ToolCallRequest {
                tool_name: "mcp".to_owned(),
                input: json!({
                    "server": "demo",
                    "tool": "echo",
                    "arguments": { "value": "hi" }
                }),
            },
            &context,
        )
        .await
        .unwrap();

    let resource_result = registry
        .invoke(
            ToolCallRequest {
                tool_name: "read_mcp_resource".to_owned(),
                input: json!({
                    "server": "demo",
                    "uri": "memory://note"
                }),
            },
            &context,
        )
        .await
        .unwrap();

    assert_eq!(tool_result.content, "mcp tool result");
    assert_eq!(resource_result.content, "note body");
}

#[tokio::test]
async fn mcp_tool_uses_auto_connected_ide_server_in_vscode_terminal() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed_auth = Arc::new(Mutex::new(None::<String>));
    let observed_auth_server = observed_auth.clone();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket =
            accept_hdr_async(stream, move |request: &Request, mut response: Response| {
                response.headers_mut().insert(
                    "Sec-WebSocket-Protocol",
                    tokio_tungstenite::tungstenite::http::HeaderValue::from_static("mcp"),
                );
                *observed_auth_server.lock().unwrap() = request
                    .headers()
                    .get("x-claude-code-ide-authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                Ok(response)
            })
            .await
            .unwrap();

        while let Some(message) = socket.next().await {
            let message = message.unwrap();
            if !message.is_text() {
                continue;
            }
            let value = serde_json::from_str::<Value>(message.to_text().unwrap()).unwrap();
            match value["method"].as_str().unwrap() {
                "initialize" => {
                    socket
                        .send(WsMessage::Text(
                            json!({
                                "jsonrpc": "2.0",
                                "id": 1,
                                "result": {
                                    "protocolVersion": "2024-11-05",
                                    "capabilities": {}
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                }
                "notifications/initialized" => {}
                "tools/call" => {
                    socket
                        .send(WsMessage::Text(
                            json!({
                                "jsonrpc": "2.0",
                                "id": 2,
                                "result": {
                                    "content": [{ "type": "text", "text": "ide mcp result" }],
                                    "isError": false
                                }
                            })
                            .to_string()
                            .into(),
                        ))
                        .await
                        .unwrap();
                    break;
                }
                other => panic!("unexpected websocket method: {other}"),
            }
        }
    });

    let home = make_temp_dir("ide-auto-connect-home");
    let workspace = home.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(home.join(".claude/ide")).unwrap();
    fs::write(
        home.join(format!(".claude/ide/{}.lock", address.port())),
        serde_json::to_string(&json!({
            "workspaceFolders": [workspace.display().to_string()],
            "ideName": "Visual Studio Code",
            "transport": "ws",
            "authToken": "ide-secret-token"
        }))
        .unwrap(),
    )
    .unwrap();

    let home_path = home.display().to_string();
    let _env = scoped_env_vars(&[
        ("HOME", Some(home_path.as_str())),
        ("TERM_PROGRAM", Some("vscode")),
    ]);

    let registry = compatibility_tool_registry();
    let context = ToolContext {
        cwd: workspace,
        ..ToolContext::default()
    };

    let result = registry
        .invoke(
            ToolCallRequest {
                tool_name: "mcp".to_owned(),
                input: json!({
                    "server": "ide",
                    "tool": "getDiagnostics",
                    "arguments": {}
                }),
            },
            &context,
        )
        .await
        .unwrap();

    server.await.unwrap();

    assert_eq!(result.content, "ide mcp result");
    assert_eq!(
        observed_auth.lock().unwrap().as_deref(),
        Some("ide-secret-token")
    );
}
