use super::*;

#[tokio::test]
async fn repl_mcp_auth_login_starts_device_flow() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = std::io::Read::read(&mut stream, &mut buffer).unwrap();
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
            let path = header_text
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let response = match path.as_str() {
                "/.well-known/oauth-authorization-server" => json!({
                    "device_authorization_endpoint": format!("http://{address}/device_authorization"),
                    "token_endpoint": format!("http://{address}/token")
                }),
                "/device_authorization" => json!({
                    "device_code": "device-123",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "https://verify.example.com",
                    "verification_uri_complete": "https://verify.example.com/complete",
                    "expires_in": 900,
                    "interval": 5
                }),
                other => panic!("unexpected auth path: {other}"),
            };
            let body = response.to_string();
            let response_text = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            std::io::Write::write_all(&mut stream, response_text.as_bytes()).unwrap();
        }
    });

    let root = temp_session_root("repl-mcp-auth");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);
    write_test_file(
        &root.join(".claude-plugin/plugin.json"),
        &format!(
            r#"{{
                  "name": "mcp-tools",
                  "mcpServers": {{
                    "example": {{
                      "url": "http://{address}/mcp",
                      "auth": {{
                        "type": "oauth_device",
                        "clientId": "client-123",
                        "audience": "example"
                      }}
                    }}
                  }}
                }}"#
        ),
    );

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "mcp".to_owned(),
            args: vec![
                "auth-login".to_owned(),
                root.display().to_string(),
                "example".to_owned(),
            ],
            raw_input: format!("/mcp auth-login {} example", root.display()),
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("\"device_code\": \"device-123\""));
    assert!(status.contains("\"verification_uri\""));
}

#[tokio::test]
async fn repl_remote_control_reports_local_state() {
    let root = temp_session_root("repl-remote");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let mut active_model = DEFAULT_OPENAI_REASONING_MODEL.to_owned();
    let mut raw_messages = Vec::new();
    let mut vim_state = code_agent_ui::vim::VimState::default();
    let mut repl_session = repl_session_state(session_id);

    let status = handle_repl_slash_command(
        &registry,
        CommandInvocation {
            name: "remote-control".to_owned(),
            raw_input: "/remote-control".to_owned(),
            ..CommandInvocation::default()
        },
        &store,
        &tool_registry,
        &root,
        None,
        ApiProvider::OpenAI,
        &mut active_model,
        &mut repl_session,
        &mut raw_messages,
        false,
        &mut vim_state,
        false,
        false,
    )
    .await
    .unwrap();

    assert!(status.contains("\"session_id\""));
    assert!(status.contains("\"task_count\""));
    assert!(status.contains("\"question_count\""));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_control_directive_command_reaches_bridge() {
    let root = temp_session_root("remote-directive");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let address = temp_tcp_address();
    let server_root = root.clone();
    let server_address = address.clone();
    let server = tokio::spawn(async move {
        let server_store = ActiveSessionStore::Local(LocalSessionStore::new(server_root.clone()));
        let server_tool_registry = compatibility_tool_registry();
        let handler = LocalBridgeHandler {
            store: &server_store,
            tool_registry: &server_tool_registry,
            cwd: server_root,
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };
        serve_direct_session(
            BridgeServerConfig {
                bind_address: server_address,
                session_id: Some(session_id),
                allow_remote_tools: true,
            },
            handler,
        )
        .await
        .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let output = render_remote_control_command(
        &registry,
        &CommandInvocation {
            name: "remote-control".to_owned(),
            args: vec![
                "directive".to_owned(),
                address.clone(),
                "agent=coordinator".to_owned(),
                "delegate the review".to_owned(),
            ],
            raw_input: format!(
                "/remote-control directive {address} agent=coordinator delegate the review"
            ),
        },
        &Cli {
            bridge_receive_count: Some(24),
            ..Cli::default()
        },
        &store,
        &tool_registry,
        &root,
        ApiProvider::FirstParty,
        "claude-sonnet-4-6",
        session_id,
        &[],
        false,
    )
    .await
    .unwrap();
    let record = server.await.unwrap();

    assert!(output.contains("assistant_synthesis") || output.contains("delegate the review"));
    assert!(record
        .envelopes
        .iter()
        .any(|envelope| matches!(envelope, RemoteEnvelope::AssistantDirective { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_control_answer_command_round_trips_question_response() {
    let root = temp_session_root("remote-answer");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let registry = resolved_command_registry(&root, None).await;
    let session_id = SessionId::new_v4();
    let question_id = Uuid::new_v4();
    let address = temp_tcp_address();
    let server_root = root.clone();
    let server_address = address.clone();
    let server = tokio::spawn(async move {
        let server_store = ActiveSessionStore::Local(LocalSessionStore::new(server_root.clone()));
        let server_tool_registry = compatibility_tool_registry();
        let handler = LocalBridgeHandler {
            store: &server_store,
            tool_registry: &server_tool_registry,
            cwd: server_root,
            provider: ApiProvider::FirstParty,
            active_model: "claude-sonnet-4-6".to_owned(),
            session_id,
            raw_messages: Vec::new(),
            live_runtime: false,
            allow_remote_tools: true,
            pending_permission: None,
            voice_streams: BTreeMap::new(),
        };
        serve_direct_session(
            BridgeServerConfig {
                bind_address: server_address,
                session_id: Some(session_id),
                allow_remote_tools: true,
            },
            handler,
        )
        .await
        .unwrap()
    });

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let output = render_remote_control_command(
        &registry,
        &CommandInvocation {
            name: "remote-control".to_owned(),
            args: vec![
                "answer".to_owned(),
                address.clone(),
                question_id.to_string(),
                "approved".to_owned(),
            ],
            raw_input: format!("/remote-control answer {address} {question_id} approved"),
        },
        &Cli {
            bridge_receive_count: Some(8),
            ..Cli::default()
        },
        &store,
        &tool_registry,
        &root,
        ApiProvider::FirstParty,
        "claude-sonnet-4-6",
        session_id,
        &[],
        false,
    )
    .await
    .unwrap();
    let record = server.await.unwrap();

    assert!(output.contains(&question_id.to_string()));
    assert!(record.envelopes.iter().any(|envelope| {
        matches!(
            envelope,
            RemoteEnvelope::QuestionResponse { response }
                if response.question_id == question_id
        )
    }));
}

#[tokio::test]
async fn local_bridge_handler_runs_prompt_turns() {
    let store =
        ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root("bridge-prompt")));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let envelopes = handler
        .on_envelope(&RemoteEnvelope::Message {
            message: build_text_message(
                session_id,
                MessageRole::User,
                "bridge hello".to_owned(),
                None,
            ),
        })
        .await
        .unwrap();

    assert!(envelopes.iter().any(|envelope| match envelope {
        RemoteEnvelope::Message { message } => message.blocks.iter().any(
            |block| matches!(block, ContentBlock::Text { text } if text.contains("bridge hello"))
        ),
        _ => false,
    }));
}

#[tokio::test]
async fn local_bridge_handler_supports_assistant_and_voice_inputs() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "bridge-assistant",
    )));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let directive = handler
        .on_envelope(&RemoteEnvelope::AssistantDirective {
            directive: AssistantDirective {
                agent_id: Some("coordinator".to_owned()),
                instruction: "delegate the review".to_owned(),
                ..AssistantDirective::default()
            },
        })
        .await
        .unwrap();
    let voice = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "text/plain".to_owned(),
                payload_base64: base64_encode(b"voice hello"),
                sequence: 1,
                stream_id: Some("voice".to_owned()),
                is_final: true,
            },
        })
        .await
        .unwrap();

    let directive_messages = directive
        .iter()
        .filter(|envelope| matches!(envelope, RemoteEnvelope::Message { .. }))
        .count();
    assert!(directive_messages >= 2);
    assert!(voice
        .iter()
        .any(|envelope| matches!(envelope, RemoteEnvelope::Message { .. })));
}

#[tokio::test]
async fn local_bridge_handler_emits_tool_call_and_result_envelopes() {
    let root = temp_session_root("bridge-tool");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: root,
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let envelopes = handler
        .on_envelope(&RemoteEnvelope::Message {
            message: build_text_message(
                session_id,
                MessageRole::User,
                "tool:memory {\"action\":\"write\",\"value\":{\"note\":\"ok\"}}".to_owned(),
                None,
            ),
        })
        .await
        .unwrap();

    assert!(envelopes.iter().any(
        |envelope| matches!(envelope, RemoteEnvelope::ToolCall { call } if call.name == "memory")
    ));
    assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::ToolResult { result } if result.tool_call_id == "echo_tool_call")));
}

#[tokio::test]
async fn local_bridge_handler_buffers_streamed_voice_frames() {
    let store = ActiveSessionStore::Local(LocalSessionStore::new(temp_session_root(
        "bridge-voice-stream",
    )));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let partial = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "text/plain".to_owned(),
                payload_base64: base64_encode(b"voice "),
                sequence: 1,
                stream_id: Some("stream-a".to_owned()),
                is_final: false,
            },
        })
        .await
        .unwrap();
    let final_chunk = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "text/plain".to_owned(),
                payload_base64: base64_encode(b"hello"),
                sequence: 2,
                stream_id: Some("stream-a".to_owned()),
                is_final: true,
            },
        })
        .await
        .unwrap();

    assert!(partial.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Ack { note } if note.starts_with("voice_frame_buffered:"))));
    assert!(final_chunk
        .iter()
        .any(|envelope| matches!(envelope, RemoteEnvelope::Message { .. })));
}

#[tokio::test]
async fn local_bridge_handler_persists_binary_voice_frames() {
    let root = temp_session_root("bridge-voice-binary");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: root.clone(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let first = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "audio/wav".to_owned(),
                payload_base64: base64_encode(&[0, 255]),
                sequence: 1,
                stream_id: Some("binary-stream".to_owned()),
                is_final: false,
            },
        })
        .await
        .unwrap();
    let second = handler
        .on_envelope(&RemoteEnvelope::VoiceFrame {
            frame: VoiceFrame {
                format: "audio/wav".to_owned(),
                payload_base64: base64_encode(&[12, 13]),
                sequence: 2,
                stream_id: Some("binary-stream".to_owned()),
                is_final: true,
            },
        })
        .await
        .unwrap();

    let saved_path = second
        .iter()
        .find_map(|envelope| match envelope {
            RemoteEnvelope::Ack { note } => {
                note.strip_prefix("voice_frame_saved:").map(PathBuf::from)
            }
            _ => None,
        })
        .expect("missing voice_frame_saved ack");

    assert!(first.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Ack { note } if note.starts_with("voice_frame_buffered:"))));
    assert!(saved_path.exists());
    assert_eq!(fs::read(saved_path).unwrap(), vec![0, 255, 12, 13]);
}

#[tokio::test]
async fn local_bridge_handler_requires_permission_for_remote_tool_calls() {
    let root = temp_session_root("bridge-remote-tool");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root.clone()));
    let tool_registry = compatibility_tool_registry();
    let session_id = SessionId::new_v4();
    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: root,
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id,
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: false,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let initial = handler
        .on_envelope(&RemoteEnvelope::ToolCall {
            call: code_agent_core::ToolCall {
                id: "remote-write".to_owned(),
                name: "file_write".to_owned(),
                input_json: json!({
                    "path": "remote.txt",
                    "content": "ok"
                })
                .to_string(),
                thought_signature: None,
            },
        })
        .await
        .unwrap();

    let request_id = initial
        .iter()
        .find_map(|envelope| match envelope {
            RemoteEnvelope::PermissionRequest { request } => Some(request.id.clone()),
            _ => None,
        })
        .expect("missing permission request");

    let approved = handler
        .on_envelope(&RemoteEnvelope::PermissionResponse {
            response: RemotePermissionResponse {
                id: request_id,
                approved: true,
                note: None,
            },
        })
        .await
        .unwrap();

    assert!(initial.iter().any(|envelope| matches!(envelope, RemoteEnvelope::PermissionRequest { request } if request.tool_name == "file_write")));
    assert!(approved.iter().any(
        |envelope| matches!(envelope, RemoteEnvelope::ToolResult { result } if !result.is_error)
    ));
}

#[tokio::test]
async fn local_bridge_handler_resumes_existing_sessions() {
    let root = temp_session_root("bridge-resume");
    let store = ActiveSessionStore::Local(LocalSessionStore::new(root));
    let tool_registry = compatibility_tool_registry();
    let resumed_session = SessionId::new_v4();
    let persisted = build_text_message(
        resumed_session,
        MessageRole::Assistant,
        "resumed output".to_owned(),
        None,
    );
    store
        .append_message(resumed_session, &persisted)
        .await
        .unwrap();

    let mut handler = LocalBridgeHandler {
        store: &store,
        tool_registry: &tool_registry,
        cwd: env::temp_dir(),
        provider: ApiProvider::FirstParty,
        active_model: "claude-sonnet-4-6".to_owned(),
        session_id: SessionId::new_v4(),
        raw_messages: Vec::new(),
        live_runtime: false,
        allow_remote_tools: true,
        pending_permission: None,
        voice_streams: BTreeMap::new(),
    };

    let envelopes = handler
        .on_envelope(&RemoteEnvelope::ResumeSession {
            request: ResumeSessionRequest {
                target: resumed_session.to_string(),
            },
        })
        .await
        .unwrap();

    assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::Message { message } if message_text(message).contains("resumed output"))));
    assert!(envelopes.iter().any(|envelope| matches!(envelope, RemoteEnvelope::SessionState { state } if state.session_id == Some(resumed_session))));
}
