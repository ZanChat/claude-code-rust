use super::{
    compatibility_tool_registry, compatibility_tool_specs, glob_matches, ToolCallRequest,
    ToolContext, ToolKind,
};
use serde_json::json;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{SystemTime, UNIX_EPOCH};

fn make_temp_dir(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("code-agent-tools-{label}-{stamp}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn exposes_expected_compatibility_tools() {
    let specs = compatibility_tool_specs();
    let names = specs
        .iter()
        .map(|spec| spec.name.as_str())
        .collect::<Vec<_>>();

    assert!(names.contains(&"file_read"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"mcp"));
    assert!(names.contains(&"agent"));
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
