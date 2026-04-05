use super::{
    call_tool_from_config, list_resources_from_config, list_tools_from_config,
    load_cached_auth_token, load_manifest_from_config, load_pending_device_flow,
    parse_mcp_server_configs, poll_oauth_device_flow, read_content_length_message,
    read_resource_from_config, refresh_oauth_device_token, start_oauth_device_flow,
    write_content_length_message, McpAuthConfig, McpRegistry, McpServerConfig, McpServerManifest,
    McpServerState, McpTransportConfig,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[test]
fn stores_server_configs_and_manifests() {
    let mut registry = McpRegistry::default();
    registry.register(
        "filesystem".to_owned(),
        McpServerConfig {
            name: "filesystem".to_owned(),
            transport: Some(McpTransportConfig::Stdio {
                command: "npx".to_owned(),
                args: vec!["@modelcontextprotocol/server-filesystem".to_owned()],
            }),
            ..McpServerConfig::default()
        },
    );
    registry.register_manifest(
        "github".to_owned(),
        McpServerManifest {
            config: McpServerConfig {
                name: "github".to_owned(),
                ..McpServerConfig::default()
            },
            state: McpServerState {
                connected: true,
                tool_count: 3,
                resource_count: 1,
                ..McpServerState::default()
            },
            ..McpServerManifest::default()
        },
    );

    assert_eq!(registry.list().len(), 2);
    assert_eq!(registry.get_manifest("github").unwrap().state.tool_count, 3);
    assert!(registry.get("filesystem").is_some());
}

#[test]
fn parses_plugin_style_server_configs() {
    let raw = BTreeMap::from([(
        "filesystem".to_owned(),
        json!({
            "command": "npx",
            "args": ["@modelcontextprotocol/server-filesystem", "."],
            "env": { "NODE_ENV": "production" },
            "auth": {
                "type": "oauth_device",
                "clientId": "client-123",
                "audience": "filesystem"
            }
        }),
    )]);

    let parsed = parse_mcp_server_configs(&raw);
    let config = parsed.get("filesystem").unwrap();

    assert_eq!(config.name, "filesystem");
    assert!(matches!(
        config.transport,
        Some(McpTransportConfig::Stdio { .. })
    ));
    assert!(matches!(
        config.auth,
        Some(McpAuthConfig::OAuthDevice { ref client_id, ref audience })
            if client_id == "client-123" && audience.as_deref() == Some("filesystem")
    ));
    assert_eq!(
        config.env.get("NODE_ENV").map(String::as_str),
        Some("production")
    );
}

#[tokio::test]
async fn frames_stdio_messages() {
    let (mut client, mut server) = tokio::io::duplex(1024);
    let body = json!({ "jsonrpc": "2.0", "id": 1, "result": { "ok": true } });

    let writer = tokio::spawn(async move {
        write_content_length_message(&mut client, &body)
            .await
            .unwrap();
    });
    let received = read_content_length_message(&mut server).await.unwrap();
    writer.await.unwrap();

    assert_eq!(received["result"]["ok"], true);
}

#[tokio::test]
async fn speaks_http_json_rpc_for_tools_and_resources() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    std::thread::spawn(move || {
        for _ in 0..6 {
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
            let header_text = String::from_utf8(request[..header_end].to_vec()).unwrap();
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

            let body =
                serde_json::from_slice::<Value>(&request[header_end..header_end + content_length])
                    .unwrap();
            let response = match body["method"].as_str().unwrap() {
                "tools/list" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "tools": [{
                            "name": "echo",
                            "description": "Echo a value",
                            "input_schema": { "type": "object" }
                        }]
                    }
                }),
                "tools/call" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "content": [{ "type": "text", "text": "echo result" }],
                        "isError": false
                    }
                }),
                "resources/list" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "resources": [{
                            "uri": "file:///tmp/demo.txt",
                            "name": "demo"
                        }]
                    }
                }),
                "resources/read" => json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "result": {
                        "contents": [{ "uri": "file:///tmp/demo.txt", "text": "resource body" }]
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

    let config = McpServerConfig {
        name: "http-server".to_owned(),
        transport: Some(McpTransportConfig::Http {
            url: format!("http://{address}"),
        }),
        ..McpServerConfig::default()
    };

    let tools = list_tools_from_config(&config).await.unwrap();
    let tool_result = call_tool_from_config(&config, "echo", json!({ "value": "hi" }))
        .await
        .unwrap();
    let resources = list_resources_from_config(&config).await.unwrap();
    let resource = read_resource_from_config(&config, "file:///tmp/demo.txt")
        .await
        .unwrap();
    let manifest = load_manifest_from_config(&config).await.unwrap();

    assert_eq!(tools[0].name, "echo");
    assert_eq!(tool_result.content_text, "echo result");
    assert_eq!(resources[0].uri, "file:///tmp/demo.txt");
    assert_eq!(resource.content_text, "resource body");
    assert_eq!(manifest.state.tool_count, 1);
}

#[tokio::test]
async fn performs_oauth_device_flow_and_refresh() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();

    std::thread::spawn(move || {
        for _ in 0..6 {
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
            let header_text = String::from_utf8(request[..header_end].to_vec()).unwrap();
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("Content-Length")
                        .then_some(value.trim())
                })
                .unwrap_or("0")
                .parse::<usize>()
                .unwrap();
            while request.len() < header_end + content_length {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }

            let request_line = header_text.lines().next().unwrap_or_default().to_owned();
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let body_text =
                String::from_utf8(request[header_end..header_end + content_length].to_vec())
                    .unwrap();

            let response = match path.as_str() {
                "/.well-known/oauth-authorization-server" => json!({
                    "device_authorization_endpoint": format!("http://{address}/device_authorization"),
                    "token_endpoint": format!("http://{address}/token")
                }),
                "/device_authorization" => {
                    assert!(body_text.contains("client_id=client-123"));
                    assert!(body_text.contains("audience=example"));
                    json!({
                        "device_code": "device-123",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": "https://verify.example.com",
                        "verification_uri_complete": "https://verify.example.com/complete",
                        "expires_in": 900,
                        "interval": 5
                    })
                }
                "/token"
                    if body_text.contains(
                        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
                    ) =>
                {
                    json!({
                        "access_token": "access-token-1",
                        "refresh_token": "refresh-token-1",
                        "token_type": "Bearer",
                        "expires_in": 1800
                    })
                }
                "/token" if body_text.contains("grant_type=refresh_token") => json!({
                    "access_token": "access-token-2",
                    "refresh_token": "refresh-token-2",
                    "token_type": "Bearer",
                    "expires_in": 1800
                }),
                other => panic!("unexpected oauth path: {other}"),
            };
            let response_body = response.to_string();
            let response_text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
            stream.write_all(response_text.as_bytes()).unwrap();
        }
    });

    let config = McpServerConfig {
        name: "oauth-server".to_owned(),
        transport: Some(McpTransportConfig::Http {
            url: format!("http://{address}/mcp"),
        }),
        auth: Some(McpAuthConfig::OAuthDevice {
            client_id: "client-123".to_owned(),
            audience: Some("example".to_owned()),
        }),
        ..McpServerConfig::default()
    };

    let flow = start_oauth_device_flow(&config).await.unwrap();
    assert_eq!(flow.device_code, "device-123");
    assert_eq!(flow.user_code.as_deref(), Some("ABCD-EFGH"));
    assert!(load_pending_device_flow(&config).unwrap().is_some());

    let token = poll_oauth_device_flow(&config, None).await.unwrap();
    assert_eq!(token.access_token, "access-token-1");
    assert_eq!(token.refresh_token.as_deref(), Some("refresh-token-1"));
    assert!(load_cached_auth_token(&config).unwrap().is_some());
    assert!(load_pending_device_flow(&config).unwrap().is_none());

    let refreshed = refresh_oauth_device_token(&config).await.unwrap();
    assert_eq!(refreshed.access_token, "access-token-2");
    assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-token-2"));
}

#[tokio::test]
async fn uses_cached_oauth_token_for_websocket_transport() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let observed_auth = Arc::new(Mutex::new(None::<String>));
    let observed_auth_server = observed_auth.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(stream, move |request: &Request, response: Response| {
            *observed_auth_server.lock().unwrap() = request
                .headers()
                .get("authorization")
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
                "tools/list" => {
                    socket
                        .send(WsMessage::Text(
                            json!({
                                "jsonrpc": "2.0",
                                "id": 2,
                                "result": {
                                    "tools": [{
                                        "name": "echo",
                                        "description": "Echo"
                                    }]
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

    let config = McpServerConfig {
        name: "oauth-websocket".to_owned(),
        transport: Some(McpTransportConfig::WebSocket {
            url: format!("ws://{address}"),
        }),
        auth: Some(McpAuthConfig::OAuthDevice {
            client_id: "client-123".to_owned(),
            audience: Some("example".to_owned()),
        }),
        ..McpServerConfig::default()
    };
    super::store_cached_auth_token(
        &config,
        &super::CachedMcpAuthToken {
            access_token: "access-token-1".to_owned(),
            refresh_token: Some("refresh-token-1".to_owned()),
            token_type: Some("Bearer".to_owned()),
            expires_at_unix_ms: None,
        },
    )
    .unwrap();

    let tools = list_tools_from_config(&config).await.unwrap();
    server.await.unwrap();

    assert_eq!(tools[0].name, "echo");
    assert_eq!(
        observed_auth.lock().unwrap().as_deref(),
        Some("Bearer access-token-1")
    );
}
