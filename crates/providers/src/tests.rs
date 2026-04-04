use super::{
    codex_auth_file_path, collect_provider_response, collect_provider_text,
    compatibility_model_catalog, decode_jwt_claims, events_from_anthropic_response,
    events_from_openai_response, events_from_openai_sse_body, get_anthropic_auth_material,
    get_openai_auth_status, get_openai_credential_hint, get_token_freshness, is_openai_provider,
    provider_base_url, provider_descriptor, refresh_codex_access_token, resolve_api_provider,
    resolve_provider_model, sign_bedrock_request, ApiProvider, AuthMaterial, AuthRequest,
    AuthResolver, EchoProvider, EnvironmentAuthResolver, HttpProvider, ModelCatalog,
    OpenAIAuthSource, OpenAITokenFreshness, ProviderRequest, ProviderToolDefinition,
    DEFAULT_OPENAI_COMPLETION_MODEL, DEFAULT_OPENAI_REASONING_MODEL,
};
use code_agent_core::{ContentBlock, Message, MessageRole, ToolCall};
use serde_json::json;
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Mutex;
use time::{Date, Month, PrimitiveDateTime, Time};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn fixture_json(relative: &str) -> serde_json::Value {
    let path = workspace_root().join(relative);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
struct CapturedHttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

async fn spawn_json_server(
    response_body: serde_json::Value,
) -> (String, std::thread::JoinHandle<CapturedHttpRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let body_string = serde_json::to_string(&response_body).unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];

        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                let header_end = position + 4;
                let header_text = String::from_utf8_lossy(&buffer[..header_end]);
                let content_length = header_text
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                while buffer.len() < header_end + content_length {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                }
                if buffer.len() < header_end + content_length {
                    break;
                }
                let request_text = String::from_utf8_lossy(&buffer);
                let mut lines = request_text.lines();
                let request_line = lines.next().unwrap();
                let mut request_line_parts = request_line.split_whitespace();
                let method = request_line_parts.next().unwrap().to_owned();
                let path = request_line_parts.next().unwrap().to_owned();
                let mut headers = BTreeMap::new();
                for line in lines.by_ref() {
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
                    }
                }
                let body =
                    String::from_utf8_lossy(&buffer[header_end..header_end + content_length])
                        .to_string();
                let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body_string.len(),
                        body_string
                    );
                stream.write_all(response.as_bytes()).unwrap();
                return CapturedHttpRequest {
                    method,
                    path,
                    headers,
                    body,
                };
            }
        }

        panic!("server did not receive a complete HTTP request")
    });

    (format!("http://{address}"), handle)
}

fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = env::var(key).ok();
    match value {
        Some(value) => env::set_var(key, value),
        None => env::remove_var(key),
    }
    f();
    match previous {
        Some(value) => env::set_var(key, value),
        None => env::remove_var(key),
    }
}

fn with_env_lock<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().unwrap();
    f()
}

#[test]
fn resolves_provider_from_env_flags() {
    with_env_lock(|| {
        with_env_var("CLAUDE_CODE_API_PROVIDER", None, || {
            with_env_var("CLAUDE_CODE_USE_BEDROCK", Some("1"), || {
                let provider = resolve_api_provider(None).expect("provider resolution should work");
                assert_eq!(provider, ApiProvider::Bedrock);
            });
        });
    });
}

#[test]
fn uses_codex_home_for_auth_path() {
    with_env_lock(|| {
        with_env_var("CODEX_HOME", Some("/tmp/codex-home"), || {
            assert_eq!(
                codex_auth_file_path(),
                PathBuf::from("/tmp/codex-home/auth.json")
            );
        });
    });
}

#[test]
fn detects_openai_api_key_auth() {
    with_env_lock(|| {
        with_env_var("OPENAI_API_KEY", Some("test-key"), || {
            let status = get_openai_auth_status(ApiProvider::OpenAI);
            assert_eq!(status.source, OpenAIAuthSource::OpenAiApiKey);
            assert!(status.has_credentials);
        });
    });
}

#[test]
fn reads_chatgpt_codex_auth_snapshot() {
    with_env_lock(|| {
        let root = env::temp_dir().join(format!("codex-auth-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("temp dir should exist");
        let auth_path = root.join("auth.json");
        fs::write(
            &auth_path,
            r#"{
  "auth_mode":"chatgpt",
  "tokens":{
    "access_token":"header.eyJleHAiOjQ3MDAwMDAwMDAsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    "refresh_token":"refresh-token",
    "account_id":"acct_123"
  }
}"#,
        )
        .expect("auth file should be written");

        with_env_var(
            "CODEX_HOME",
            Some(root.to_str().expect("utf8 path")),
            || {
                let status = get_openai_auth_status(ApiProvider::ChatGPTCodex);
                assert!(status.has_credentials);
                assert_eq!(status.source, OpenAIAuthSource::CodexAuthToken);
                assert_eq!(status.account_id.as_deref(), Some("acct_123"));
            },
        );
    });
}

#[test]
fn decodes_minimal_jwt_claims() {
    let claims = decode_jwt_claims(Some(
        "header.eyJleHAiOjQ3MDAwMDAwMDAsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    ))
    .expect("claims should decode");
    assert_eq!(claims.email.as_deref(), Some("user@example.com"));
}

#[test]
fn treats_undecodable_tokens_as_fresh_for_openai_compatibility() {
    assert_eq!(
        get_token_freshness(Some("not-a-jwt")),
        OpenAITokenFreshness::Fresh
    );
}

#[tokio::test]
async fn refreshes_codex_auth_atomically() {
    let _guard = ENV_LOCK.lock().unwrap();
    let root = env::temp_dir().join(format!(
        "codex-refresh-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let auth_path = root.join("auth.json");
    fs::write(
            &auth_path,
            r#"{
  "auth_mode":"chatgpt",
  "tokens":{
    "access_token":"header.eyJleHAiOjEsImNsaWVudF9pZCI6ImNsaWVudDEyMyIsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    "refresh_token":"old-refresh-token",
    "id_token":"header.eyJhdWQiOlsiY2xpZW50MTIzIl0sImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
    "account_id":"acct_123"
  }
}"#,
        )
        .unwrap();

    let (base_url, server) = spawn_json_server(json!({
            "access_token": "header.eyJleHAiOjQ3MDAwMDAwMDAsImNsaWVudF9pZCI6ImNsaWVudDEyMyIsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature",
            "refresh_token": "new-refresh-token",
            "id_token": "header.eyJhdWQiOlsiY2xpZW50MTIzIl0sImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature"
        }))
        .await;

    let previous_auth_url = env::var("OPENAI_AUTH_URL").ok();
    env::set_var("OPENAI_AUTH_URL", &base_url);
    let refreshed_result = refresh_codex_access_token(Some(auth_path.as_path())).await;
    match previous_auth_url {
        Some(value) => env::set_var("OPENAI_AUTH_URL", value),
        None => env::remove_var("OPENAI_AUTH_URL"),
    }
    let refreshed = refreshed_result.unwrap();

    let persisted = fs::read_to_string(&auth_path).unwrap();
    let persisted_json: serde_json::Value = serde_json::from_str(&persisted).unwrap();
    let captured = server.join().unwrap();

    assert_eq!(
            refreshed.as_deref(),
            Some(
                "header.eyJleHAiOjQ3MDAwMDAwMDAsImNsaWVudF9pZCI6ImNsaWVudDEyMyIsImVtYWlsIjoidXNlckBleGFtcGxlLmNvbSJ9.signature"
            )
        );
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/");
    assert!(captured.body.contains("grant_type=refresh_token"));
    assert!(captured.body.contains("refresh_token=old-refresh-token"));
    assert!(captured.body.contains("client_id=client123"));
    assert_eq!(
        persisted_json["tokens"]["refresh_token"].as_str(),
        Some("new-refresh-token")
    );
    assert_eq!(
        persisted_json["tokens"]["access_token"].as_str(),
        refreshed.as_deref()
    );
    assert_eq!(persisted_json["auth_mode"].as_str(), Some("chatgpt"));
    assert!(persisted.ends_with('\n'));
    assert!(!super::codex_auth_lock_path(&auth_path).exists());
}

#[test]
fn reports_config_migration_inputs() {
    with_env_lock(|| {
        with_env_var("CLAUDE_CODE_API_PROVIDER", Some("openai"), || {
            with_env_var("OPENAI_API_KEY", Some("test-key"), || {
                let report = super::config_migration_report(ApiProvider::OpenAI);
                assert_eq!(report.provider, ApiProvider::OpenAI);
                assert_eq!(
                    report.env.get("CLAUDE_CODE_API_PROVIDER"),
                    Some(&"openai".to_owned())
                );
                assert_eq!(
                    report.env.get("OPENAI_API_KEY"),
                    Some(&"test-key".to_owned())
                );
            });
        });
    });
}

#[test]
fn openai_provider_hint_mentions_expected_setup() {
    assert!(is_openai_provider(ApiProvider::OpenAICompatible));
    assert!(get_openai_credential_hint(ApiProvider::ChatGPTCodex).contains("codex login"));
}

#[test]
fn exposes_provider_descriptors_and_model_catalogs() {
    let descriptor = provider_descriptor(ApiProvider::Foundry);
    let catalog = compatibility_model_catalog(ApiProvider::OpenAI);

    assert_eq!(descriptor.display_name, "Microsoft Foundry");
    assert!(descriptor.supports_tool_use);
    assert!(catalog.get_model(DEFAULT_OPENAI_REASONING_MODEL).is_some());
    assert!(catalog.get_model(DEFAULT_OPENAI_COMPLETION_MODEL).is_some());
}

#[test]
fn openai_compatibility_catalog_honours_env_models() {
    with_env_lock(|| {
        with_env_var("REASONING_MODEL", Some("gemini-3.1-pro-preview"), || {
            with_env_var("COMPLETION_MODEL", Some("gemini-3.1-flash"), || {
                let catalog = compatibility_model_catalog(ApiProvider::OpenAICompatible);
                let listed = catalog
                    .list_models()
                    .into_iter()
                    .map(|model| model.id)
                    .collect::<Vec<_>>();
                assert_eq!(
                    listed.first().map(String::as_str),
                    Some("gemini-3.1-pro-preview")
                );
                assert!(listed.iter().any(|model| model == "gemini-3.1-flash"));
            });
        });
    });
}

#[test]
fn detects_gemini_openai_compatible_chat_completions_mode() {
    assert!(super::openai_compatible_uses_chat_completions(
        "https://generativelanguage.googleapis.com/v1beta/openai"
    ));
    assert!(!super::openai_compatible_uses_chat_completions(
        "https://compat.example/v1"
    ));
}

#[test]
fn reads_anthropic_env_auth_material() {
    with_env_lock(|| {
        with_env_var("ANTHROPIC_API_KEY", Some("anthropic-key"), || {
            let material = get_anthropic_auth_material(ApiProvider::FirstParty).unwrap();
            assert_eq!(material.api_key.as_deref(), Some("anthropic-key"));
            assert_eq!(material.source.as_deref(), Some("ANTHROPIC_API_KEY"));
        });
    });
}

#[tokio::test]
async fn resolves_auth_from_environment() {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var("OPENAI_API_KEY").ok();
    env::set_var("OPENAI_API_KEY", "openai-key");

    let resolver = EnvironmentAuthResolver;
    let auth = resolver
        .resolve_auth(AuthRequest {
            provider: ApiProvider::OpenAI,
            profile: None,
        })
        .await
        .unwrap();

    match previous {
        Some(value) => env::set_var("OPENAI_API_KEY", value),
        None => env::remove_var("OPENAI_API_KEY"),
    }

    assert_eq!(auth.api_key.as_deref(), Some("openai-key"));
}

#[tokio::test]
async fn streams_echo_provider_text() {
    let provider = EchoProvider::new(ApiProvider::OpenAI);
    let request = ProviderRequest {
        model: DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "Summarize the diff".to_owned(),
            }],
        )],
        ..ProviderRequest::default()
    };

    let (text, usage) = collect_provider_text(&provider, request).await.unwrap();

    assert!(text.contains("openai echo"));
    assert_eq!(usage.unwrap().input_tokens, 3);
}

#[tokio::test]
async fn collects_tool_calls_from_provider_stream() {
    let provider = EchoProvider::new(ApiProvider::OpenAI);
    let request = ProviderRequest {
        model: DEFAULT_OPENAI_REASONING_MODEL.to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "tool:file_read {\"path\":\"src/main.rs\"}".to_owned(),
            }],
        )],
        tools: vec![ProviderToolDefinition {
            name: "file_read".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: json!({"type":"object"}),
        }],
        ..ProviderRequest::default()
    };

    let collected = collect_provider_response(&provider, request).await.unwrap();

    assert!(collected.text.contains("requesting tool file_read"));
    assert_eq!(collected.tool_calls.len(), 1);
    assert_eq!(collected.tool_calls[0].name, "file_read");
}

#[test]
fn parses_anthropic_tool_use_response() {
    let events = events_from_anthropic_response(&fixture_json(
        "fixtures/provider-streams/anthropic_tool_use.json",
    ))
    .unwrap();

    assert!(
        matches!(&events[0], super::ProviderEvent::MessageDelta { text } if text.contains("inspect"))
    );
    assert!(
        matches!(&events[1], super::ProviderEvent::ToolCall { call } if call.name == "file_read")
    );
    assert!(
        matches!(&events[3], super::ProviderEvent::Usage { usage } if usage.output_tokens == 7)
    );
}

#[test]
fn parses_openai_tool_call_response() {
    let events = events_from_openai_response(&fixture_json(
        "fixtures/provider-streams/openai_tool_call.json",
    ))
    .unwrap();

    assert!(
        matches!(&events[0], super::ProviderEvent::MessageDelta { text } if text == "Need to search.")
    );
    assert!(matches!(&events[1], super::ProviderEvent::ToolCall { call } if call.id == "call_123"));
    assert!(
        matches!(&events[3], super::ProviderEvent::Usage { usage } if usage.input_tokens == 19)
    );
}

#[test]
fn parses_openai_tool_call_thought_signature() {
    let events = super::events_from_openai_chat_response(&json!({
        "choices": [{
            "message": {
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "extra_content": {
                        "google": {
                            "thought_signature": "signature-a"
                        }
                    },
                    "function": {
                        "name": "file_read",
                        "arguments": "{\"path\":\"src/main.rs\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))
    .unwrap();

    assert!(matches!(
        &events[0],
        super::ProviderEvent::ToolCall { call }
            if call.id == "call_123"
                && call.thought_signature.as_deref() == Some("signature-a")
    ));
}

#[test]
fn serializes_openai_chat_tool_call_thought_signature() {
    let assistant = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::ToolCall {
            call: ToolCall {
                id: "call_123".to_owned(),
                name: "file_read".to_owned(),
                input_json: "{\"path\":\"src/main.rs\"}".to_owned(),
                thought_signature: Some("signature-a".to_owned()),
            },
        }],
    );

    let encoded = super::openai_chat_messages(&[assistant]);

    assert_eq!(
        encoded[0]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "signature-a"
    );
}

#[test]
fn parses_openai_responses_tool_call_response() {
    let events = events_from_openai_response(&json!({
        "id": "resp_123",
        "output": [
            {
                "type": "message",
                "content": [
                    { "type": "output_text", "text": "Need to search." }
                ]
            },
            {
                "type": "function_call",
                "id": "fc_123",
                "call_id": "call_123",
                "name": "file_read",
                "arguments": "{\"path\":\"src/main.rs\"}"
            }
        ],
        "usage": {
            "input_tokens": 19,
            "output_tokens": 4
        }
    }))
    .unwrap();

    assert!(
        matches!(&events[0], super::ProviderEvent::MessageDelta { text } if text == "Need to search.")
    );
    assert!(matches!(&events[1], super::ProviderEvent::ToolCall { call } if call.id == "call_123"));
    assert!(
        matches!(&events[3], super::ProviderEvent::Usage { usage } if usage.input_tokens == 19)
    );
}

#[test]
fn parses_openai_responses_sse_stream() {
    let body = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Need to \"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"search.\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Need to search.\"}]},{\"type\":\"function_call\",\"id\":\"fc_123\",\"call_id\":\"call_123\",\"name\":\"file_read\",\"arguments\":\"{\\\"path\\\":\\\"src/main.rs\\\"}\"}],\"usage\":{\"input_tokens\":19,\"output_tokens\":4}}}\n\n"
        );
    let events = events_from_openai_sse_body(body).unwrap();

    assert!(matches!(
        &events[0],
        super::ProviderEvent::MessageDelta { text } if text == "Need to "
    ));
    assert!(matches!(
        &events[1],
        super::ProviderEvent::MessageDelta { text } if text == "search."
    ));
    assert!(matches!(&events[2], super::ProviderEvent::ToolCall { call } if call.id == "call_123"));
    assert!(
        matches!(&events[4], super::ProviderEvent::Usage { usage } if usage.input_tokens == 19)
    );
    assert!(matches!(&events[5], super::ProviderEvent::Stop { reason } if reason == "tool_use"));
}

#[test]
fn extracts_error_message_from_detail_array() {
    let body = r#"{
            "detail": [
                {
                    "loc": ["body", "input", 0, "call_id"],
                    "msg": "Field required"
                }
            ]
        }"#;

    assert_eq!(
        super::compact_error_body(body),
        "body.input.0.call_id: Field required"
    );
}

#[test]
fn uses_provider_specific_openai_base_urls() {
    with_env_lock(|| {
        with_env_var(
            "OPENAI_BASE_URL",
            Some("https://compat.example/v1/"),
            || {
                assert_eq!(
                    provider_base_url(ApiProvider::OpenAI),
                    "https://api.openai.com/v1"
                );
                assert_eq!(
                    provider_base_url(ApiProvider::ChatGPTCodex),
                    "https://chatgpt.com/backend-api"
                );
                assert_eq!(
                    provider_base_url(ApiProvider::OpenAICompatible),
                    "https://compat.example/v1"
                );
            },
        );
    });
}

#[test]
fn maps_provider_specific_model_ids() {
    assert_eq!(
        resolve_provider_model(ApiProvider::Bedrock, "claude-opus-4-1"),
        "us.anthropic.claude-opus-4-1-20250805-v1:0"
    );
    assert_eq!(
        resolve_provider_model(ApiProvider::Vertex, "claude-opus-4-1"),
        "claude-opus-4-1@20250805"
    );
    assert_eq!(
        resolve_provider_model(ApiProvider::Foundry, "claude-opus-4-1"),
        "claude-opus-4-1"
    );
}

#[test]
fn signs_bedrock_requests_with_sigv4() {
    let timestamp = PrimitiveDateTime::new(
        Date::from_calendar_date(2026, Month::January, 2).unwrap(),
        Time::from_hms(3, 4, 5).unwrap(),
    )
    .assume_utc();
    let signed = sign_bedrock_request(
        "POST",
        "bedrock-runtime.us-east-1.amazonaws.com",
        "/model/us.anthropic.claude-sonnet-4-6/invoke",
        br#"{"anthropic_version":"bedrock-2023-05-31"}"#,
        "us-east-1",
        "AKIDEXAMPLE",
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        Some("session-token"),
        timestamp,
    )
    .unwrap();

    assert!(signed
        .authorization
        .starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/"));
    assert_eq!(signed.amz_date, "20260102T030405Z");
    assert_eq!(signed.session_token.as_deref(), Some("session-token"));
    assert_eq!(signed.payload_sha256.len(), 64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_foundry_messages_requests() {
    let (base_url, server) = spawn_json_server(json!({
        "content": [{ "type": "text", "text": "foundry ok" }],
        "usage": { "input_tokens": 8, "output_tokens": 2 },
        "stop_reason": "end_turn"
    }))
    .await;
    let provider = HttpProvider::with_base_url(
        ApiProvider::Foundry,
        AuthMaterial {
            api_key: Some("foundry-key".to_owned()),
            ..AuthMaterial::default()
        },
        format!("{base_url}/anthropic"),
    );
    let request = ProviderRequest {
        model: "claude-sonnet-4-6".to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hello foundry".to_owned(),
            }],
        )],
        ..ProviderRequest::default()
    };

    let collected = collect_provider_response(&provider, request).await.unwrap();
    let captured = server.join().unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

    assert_eq!(collected.text, "foundry ok");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/anthropic/v1/messages");
    assert_eq!(
        captured.headers.get("x-api-key").map(String::as_str),
        Some("foundry-key")
    );
    assert_eq!(
        captured
            .headers
            .get("anthropic-version")
            .map(String::as_str),
        Some("2023-06-01")
    );
    assert_eq!(body["model"], "claude-sonnet-4-6");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_openai_responses_requests() {
    let (base_url, server) = spawn_json_server(json!({
        "output": [
            {
                "type": "message",
                "content": [{ "type": "output_text", "text": "openai ok" }]
            }
        ],
        "usage": { "input_tokens": 8, "output_tokens": 2 }
    }))
    .await;
    let provider = HttpProvider::with_base_url(
        ApiProvider::OpenAI,
        AuthMaterial {
            api_key: Some("openai-key".to_owned()),
            bearer_token: Some("openai-key".to_owned()),
            ..AuthMaterial::default()
        },
        format!("{base_url}/v1"),
    );
    let request = ProviderRequest {
        model: "gpt-5.4".to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hello openai".to_owned(),
            }],
        )],
        tools: vec![ProviderToolDefinition {
            name: "file_read".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: json!({"type":"object"}),
        }],
        ..ProviderRequest::default()
    };

    let collected = collect_provider_response(&provider, request).await.unwrap();
    let captured = server.join().unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

    assert_eq!(collected.text, "openai ok");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/v1/responses");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer openai-key")
    );
    assert_eq!(body["model"], "gpt-5.4");
    assert_eq!(body["stream"], true);
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(body["input"][0]["content"][0]["type"], "input_text");
    assert_eq!(body["tools"][0]["type"], "function");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_chatgpt_codex_responses_requests() {
    let (base_url, server) = spawn_json_server(json!({
        "output": [
            {
                "type": "message",
                "content": [{ "type": "output_text", "text": "codex ok" }]
            }
        ],
        "usage": { "input_tokens": 7, "output_tokens": 2 }
    }))
    .await;
    let provider = HttpProvider::with_base_url(
        ApiProvider::ChatGPTCodex,
        AuthMaterial {
            bearer_token: Some("codex-token".to_owned()),
            ..AuthMaterial::default()
        },
        base_url,
    );
    let request = ProviderRequest {
        model: "gpt-5.3-codex".to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hello codex".to_owned(),
            }],
        )],
        ..ProviderRequest::default()
    };

    let collected = collect_provider_response(&provider, request).await.unwrap();
    let captured = server.join().unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

    assert_eq!(collected.text, "codex ok");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/codex/responses");
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer codex-token")
    );
    assert_eq!(body["model"], "gpt-5.3-codex");
    assert_eq!(body["stream"], true);
    assert_eq!(body["input"][0]["role"], "user");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_vertex_raw_predict_requests() {
    let (base_url, server) = spawn_json_server(json!({
        "content": [{ "type": "text", "text": "vertex ok" }],
        "usage": { "input_tokens": 10, "output_tokens": 2 },
        "stop_reason": "end_turn"
    }))
    .await;
    let provider = HttpProvider::with_base_url(
        ApiProvider::Vertex,
        AuthMaterial {
            bearer_token: Some("vertex-token".to_owned()),
            ..AuthMaterial::default()
        },
        format!("{base_url}/v1/projects/demo/locations/us-east5"),
    );
    let request = ProviderRequest {
        model: "claude-opus-4-1".to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hello vertex".to_owned(),
            }],
        )],
        ..ProviderRequest::default()
    };

    let collected = collect_provider_response(&provider, request).await.unwrap();
    let captured = server.join().unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

    assert_eq!(collected.text, "vertex ok");
    assert_eq!(captured.method, "POST");
    assert!(captured
        .path
        .ends_with("/publishers/anthropic/models/claude-opus-4-1@20250805:rawPredict"));
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer vertex-token")
    );
    assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
    assert!(body.get("model").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sends_bedrock_invoke_requests() {
    let (base_url, server) = spawn_json_server(json!({
        "content": [{ "type": "text", "text": "bedrock ok" }],
        "usage": { "input_tokens": 9, "output_tokens": 2 },
        "stop_reason": "end_turn"
    }))
    .await;
    let provider = HttpProvider::with_base_url(
        ApiProvider::Bedrock,
        AuthMaterial {
            bearer_token: Some("bedrock-token".to_owned()),
            ..AuthMaterial::default()
        },
        base_url,
    );
    let request = ProviderRequest {
        model: "claude-haiku-4-5".to_owned(),
        messages: vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hello bedrock".to_owned(),
            }],
        )],
        ..ProviderRequest::default()
    };

    let collected = collect_provider_response(&provider, request).await.unwrap();
    let captured = server.join().unwrap();
    let body: serde_json::Value = serde_json::from_str(&captured.body).unwrap();

    assert_eq!(collected.text, "bedrock ok");
    assert_eq!(captured.method, "POST");
    assert!(captured
        .path
        .ends_with("/model/us.anthropic.claude-haiku-4-5-20251001-v1%3A0/invoke"));
    assert_eq!(
        captured.headers.get("authorization").map(String::as_str),
        Some("Bearer bedrock-token")
    );
    assert_eq!(body["anthropic_version"], "bedrock-2023-05-31");
    assert!(body.get("model").is_none());
}
