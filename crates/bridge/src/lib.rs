use anyhow::{anyhow, Result};
use async_trait::async_trait;
use code_agent_core::{
    AppEvent, Message, QuestionRequest, QuestionResponse, SessionId, TaskRecord, ToolCall,
    ToolResult,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RemoteMode {
    Local,
    DirectConnect,
    WebSocket,
    IdeBridge,
    Voice,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemoteEndpoint {
    pub mode: Option<RemoteMode>,
    pub scheme: String,
    pub address: String,
    pub session_id: Option<SessionId>,
    pub headers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceFrame {
    pub format: String,
    pub payload_base64: String,
    pub sequence: u64,
    #[serde(default)]
    pub stream_id: Option<String>,
    #[serde(default)]
    pub is_final: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssistantDirective {
    pub agent_id: Option<String>,
    pub instruction: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumeSessionRequest {
    pub target: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemotePermissionRequest {
    pub id: String,
    pub tool_name: String,
    pub input_json: String,
    pub read_only: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemotePermissionResponse {
    pub id: String,
    pub approved: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteEnvelope {
    Message { message: Message },
    Event { event: AppEvent },
    TaskState { task: TaskRecord },
    Question { question: QuestionRequest },
    QuestionResponse { response: QuestionResponse },
    ToolCall { call: ToolCall },
    ToolResult { result: ToolResult },
    AssistantDirective { directive: AssistantDirective },
    VoiceFrame { frame: VoiceFrame },
    ResumeSession { request: ResumeSessionRequest },
    SessionState { state: RemoteSessionState },
    PermissionRequest { request: RemotePermissionRequest },
    PermissionResponse { response: RemotePermissionResponse },
    Interrupt,
    Error { message: String },
    Ack { note: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSessionState {
    pub endpoint: String,
    pub connected: bool,
    pub session_id: Option<SessionId>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub message_count: usize,
    pub pending_permission_id: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BridgeServerConfig {
    pub bind_address: String,
    pub session_id: Option<SessionId>,
    pub allow_remote_tools: bool,
}

#[async_trait]
pub trait RemoteTransport: Send + Sync {
    async fn connect(&mut self, endpoint: &RemoteEndpoint) -> Result<()>;
    async fn send(&mut self, envelope: RemoteEnvelope) -> Result<()>;
    async fn receive(&mut self) -> Result<Option<RemoteEnvelope>>;
}

#[async_trait]
pub trait BridgeSessionHandler: Send {
    async fn on_connect(&mut self, _record: &BridgeSessionRecord) -> Result<Vec<RemoteEnvelope>> {
        Ok(Vec::new())
    }

    async fn on_envelope(&mut self, envelope: &RemoteEnvelope) -> Result<Vec<RemoteEnvelope>>;
}

pub struct RemoteSessionManager<T: RemoteTransport> {
    pub transport: T,
    pub endpoint: RemoteEndpoint,
    pub state: RemoteSessionState,
}

impl<T: RemoteTransport> RemoteSessionManager<T> {
    pub fn new(transport: T, endpoint: RemoteEndpoint) -> Self {
        Self {
            state: RemoteSessionState {
                endpoint: endpoint.address.clone(),
                session_id: endpoint.session_id,
                ..RemoteSessionState::default()
            },
            transport,
            endpoint,
        }
    }

    pub async fn connect(&mut self) -> Result<()> {
        self.transport.connect(&self.endpoint).await?;
        self.state.connected = true;
        Ok(())
    }

    pub async fn send_message(&mut self, message: Message) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::Message { message })
            .await
    }

    pub async fn publish_event(&mut self, event: AppEvent) -> Result<()> {
        self.transport.send(RemoteEnvelope::Event { event }).await
    }

    pub async fn publish_task(&mut self, task: TaskRecord) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::TaskState { task })
            .await
    }

    pub async fn request_question(&mut self, question: QuestionRequest) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::Question { question })
            .await
    }

    pub async fn answer_question(&mut self, response: QuestionResponse) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::QuestionResponse { response })
            .await
    }

    pub async fn send_tool_result(&mut self, result: ToolResult) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::ToolResult { result })
            .await
    }

    pub async fn publish_state(&mut self, state: RemoteSessionState) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::SessionState { state })
            .await
    }

    pub async fn request_permission(&mut self, request: RemotePermissionRequest) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::PermissionRequest { request })
            .await
    }

    pub async fn resume_session(&mut self, request: ResumeSessionRequest) -> Result<()> {
        self.transport
            .send(RemoteEnvelope::ResumeSession { request })
            .await
    }

    pub async fn interrupt(&mut self) -> Result<()> {
        self.transport.send(RemoteEnvelope::Interrupt).await
    }

    pub async fn receive(&mut self) -> Result<Option<RemoteEnvelope>> {
        self.transport.receive().await
    }
}

#[derive(Default)]
pub struct WebSocketRemoteTransport {
    stream: Option<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
}

#[async_trait]
impl RemoteTransport for WebSocketRemoteTransport {
    async fn connect(&mut self, endpoint: &RemoteEndpoint) -> Result<()> {
        let mut request = endpoint.address.as_str().into_client_request()?;
        for (key, value) in &endpoint.headers {
            request.headers_mut().insert(
                HeaderName::from_bytes(key.as_bytes())?,
                HeaderValue::from_str(value)?,
            );
        }
        let (stream, _) = connect_async(request).await?;
        self.stream = Some(stream);
        Ok(())
    }

    async fn send(&mut self, envelope: RemoteEnvelope) -> Result<()> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| anyhow!("remote transport is not connected"))?;
        stream
            .send(WsMessage::Text(serde_json::to_string(&envelope)?.into()))
            .await?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<RemoteEnvelope>> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| anyhow!("remote transport is not connected"))?;
        while let Some(message) = stream.next().await {
            let message = message?;
            if !message.is_text() {
                continue;
            }
            return Ok(Some(serde_json::from_str(message.to_text()?)?));
        }
        Ok(None)
    }
}

#[derive(Default)]
pub struct DirectTcpRemoteTransport {
    reader: Option<BufReader<tokio::net::tcp::OwnedReadHalf>>,
    writer: Option<tokio::net::tcp::OwnedWriteHalf>,
}

#[async_trait]
impl RemoteTransport for DirectTcpRemoteTransport {
    async fn connect(&mut self, endpoint: &RemoteEndpoint) -> Result<()> {
        let address = normalize_direct_address(&endpoint.address);
        let stream = TcpStream::connect(address).await?;
        let (reader, writer) = stream.into_split();
        self.reader = Some(BufReader::new(reader));
        self.writer = Some(writer);
        Ok(())
    }

    async fn send(&mut self, envelope: RemoteEnvelope) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| anyhow!("direct transport is not connected"))?;
        writer
            .write_all(format!("{}\n", serde_json::to_string(&envelope)?).as_bytes())
            .await?;
        writer.flush().await?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<RemoteEnvelope>> {
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| anyhow!("direct transport is not connected"))?;
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(line.trim_end())?))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BridgeSessionRecord {
    pub remote_address: String,
    pub envelopes: Vec<RemoteEnvelope>,
}

struct AckOnlyHandler;

#[async_trait]
impl BridgeSessionHandler for AckOnlyHandler {
    async fn on_envelope(&mut self, envelope: &RemoteEnvelope) -> Result<Vec<RemoteEnvelope>> {
        Ok(vec![RemoteEnvelope::Ack {
            note: match envelope {
                RemoteEnvelope::AssistantDirective { .. } => "assistant_directive".to_owned(),
                RemoteEnvelope::VoiceFrame { .. } => "voice_frame".to_owned(),
                RemoteEnvelope::TaskState { .. } => "task_state".to_owned(),
                RemoteEnvelope::Question { .. } => "question".to_owned(),
                RemoteEnvelope::QuestionResponse { .. } => "question_response".to_owned(),
                RemoteEnvelope::ResumeSession { .. } => "resume_session".to_owned(),
                RemoteEnvelope::SessionState { .. } => "session_state".to_owned(),
                RemoteEnvelope::PermissionRequest { .. } => "permission_request".to_owned(),
                RemoteEnvelope::PermissionResponse { .. } => "permission_response".to_owned(),
                RemoteEnvelope::Message { .. } => "message".to_owned(),
                RemoteEnvelope::Event { .. } => "event".to_owned(),
                RemoteEnvelope::ToolCall { .. } => "tool_call".to_owned(),
                RemoteEnvelope::ToolResult { .. } => "tool_result".to_owned(),
                RemoteEnvelope::Interrupt => "interrupt".to_owned(),
                RemoteEnvelope::Error { .. } => "error".to_owned(),
                RemoteEnvelope::Ack { .. } => "ack".to_owned(),
            },
        }])
    }
}

pub async fn serve_bridge_session<H: BridgeSessionHandler>(
    config: BridgeServerConfig,
    mut handler: H,
) -> Result<BridgeSessionRecord> {
    let listener = TcpListener::bind(&config.bind_address).await?;
    let (stream, remote_address) = listener.accept().await?;
    let mut socket = accept_async(stream).await?;
    let mut record = BridgeSessionRecord {
        remote_address: remote_address.to_string(),
        envelopes: Vec::new(),
    };

    for envelope in handler.on_connect(&record).await? {
        socket
            .send(WsMessage::Text(serde_json::to_string(&envelope)?.into()))
            .await?;
    }

    while let Some(message) = socket.next().await {
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                if record.envelopes.is_empty() {
                    return Err(error.into());
                }
                break;
            }
        };
        if !message.is_text() {
            continue;
        }

        let envelope = serde_json::from_str::<RemoteEnvelope>(message.to_text()?)?;
        let stop = matches!(envelope, RemoteEnvelope::Interrupt);
        record.envelopes.push(envelope.clone());
        for response in handler.on_envelope(&envelope).await? {
            socket
                .send(WsMessage::Text(serde_json::to_string(&response)?.into()))
                .await?;
        }
        if stop {
            break;
        }
    }

    Ok(record)
}

pub async fn serve_bridge_once(config: BridgeServerConfig) -> Result<BridgeSessionRecord> {
    serve_bridge_session(config, AckOnlyHandler).await
}

pub async fn serve_direct_session<H: BridgeSessionHandler>(
    config: BridgeServerConfig,
    mut handler: H,
) -> Result<BridgeSessionRecord> {
    let listener = TcpListener::bind(normalize_direct_address(&config.bind_address)).await?;
    let (stream, remote_address) = listener.accept().await?;
    let (reader_half, mut writer_half) = stream.into_split();
    let mut reader = BufReader::new(reader_half);
    let mut record = BridgeSessionRecord {
        remote_address: remote_address.to_string(),
        envelopes: Vec::new(),
    };

    for envelope in handler.on_connect(&record).await? {
        writer_half
            .write_all(format!("{}\n", serde_json::to_string(&envelope)?).as_bytes())
            .await?;
    }
    writer_half.flush().await?;

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            break;
        }
        let envelope = serde_json::from_str::<RemoteEnvelope>(line.trim_end())?;
        let stop = matches!(envelope, RemoteEnvelope::Interrupt);
        record.envelopes.push(envelope.clone());
        for response in handler.on_envelope(&envelope).await? {
            writer_half
                .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
                .await?;
        }
        writer_half.flush().await?;
        if stop {
            break;
        }
    }

    Ok(record)
}

pub async fn connect_and_exchange(
    endpoint: RemoteEndpoint,
    outbound: Vec<RemoteEnvelope>,
    receive_limit: usize,
) -> Result<Vec<RemoteEnvelope>> {
    let mut transport: Box<dyn RemoteTransport> = match endpoint.mode {
        Some(RemoteMode::DirectConnect | RemoteMode::IdeBridge) => {
            Box::new(DirectTcpRemoteTransport::default())
        }
        Some(RemoteMode::WebSocket) => Box::new(WebSocketRemoteTransport::default()),
        _ if endpoint.address.starts_with("ws://") || endpoint.address.starts_with("wss://") => {
            Box::new(WebSocketRemoteTransport::default())
        }
        _ if endpoint.address.starts_with("tcp://")
            || endpoint.address.starts_with("direct://") =>
        {
            Box::new(DirectTcpRemoteTransport::default())
        }
        _ => Box::new(WebSocketRemoteTransport::default()),
    };
    transport.connect(&endpoint).await?;
    for envelope in outbound {
        transport.send(envelope).await?;
    }
    let mut inbound = Vec::new();
    while inbound.len() < receive_limit {
        let Some(envelope) = transport.receive().await? else {
            break;
        };
        inbound.push(envelope);
    }
    Ok(inbound)
}

fn normalize_direct_address(address: &str) -> &str {
    address
        .strip_prefix("tcp://")
        .or_else(|| address.strip_prefix("direct://"))
        .or_else(|| address.strip_prefix("ide://"))
        .unwrap_or(address)
}

pub fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    let mut index = 0usize;
    while index < input.len() {
        let remaining = input.len() - index;
        let b0 = input[index];
        let b1 = if remaining > 1 { input[index + 1] } else { 0 };
        let b2 = if remaining > 2 { input[index + 2] } else { 0 };
        output.push(TABLE[(b0 >> 2) as usize] as char);
        output.push(TABLE[((b0 & 0x03) << 4 | (b1 >> 4)) as usize] as char);
        output.push(if remaining > 1 {
            TABLE[((b1 & 0x0f) << 2 | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        output.push(if remaining > 2 {
            TABLE[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
        index += 3;
    }
    output
}

pub fn base64_decode(input: &str) -> Result<Vec<u8>> {
    fn value_of(ch: u8) -> Option<u8> {
        match ch {
            b'A'..=b'Z' => Some(ch - b'A'),
            b'a'..=b'z' => Some(ch - b'a' + 26),
            b'0'..=b'9' => Some(ch - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let sanitized = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    if sanitized.len() % 4 != 0 {
        return Err(anyhow!("invalid base64 length"));
    }

    let mut output = Vec::with_capacity((sanitized.len() / 4) * 3);
    for chunk in sanitized.chunks_exact(4) {
        let mut values = [0u8; 4];
        let mut padding = 0usize;
        for (index, byte) in chunk.iter().copied().enumerate() {
            if byte == b'=' {
                padding += 1;
                values[index] = 0;
            } else {
                values[index] = value_of(byte).ok_or_else(|| anyhow!("invalid base64 byte"))?;
            }
        }
        output.push((values[0] << 2) | (values[1] >> 4));
        if padding < 2 {
            output.push((values[1] << 4) | (values[2] >> 2));
        }
        if padding == 0 {
            output.push((values[2] << 6) | values[3]);
        }
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{
        base64_decode, base64_encode, connect_and_exchange, serve_bridge_once,
        serve_bridge_session, serve_direct_session, AssistantDirective, BridgeServerConfig,
        BridgeSessionHandler, RemoteEndpoint, RemoteEnvelope, RemoteMode, VoiceFrame,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use code_agent_core::{ContentBlock, Message, MessageRole};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relays_websocket_messages_and_special_envelopes() {
        let address = "127.0.0.1:43125";
        let server = tokio::spawn(async move {
            serve_bridge_once(BridgeServerConfig {
                bind_address: address.to_owned(),
                ..BridgeServerConfig::default()
            })
            .await
            .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let message = Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "bridge hello".to_owned(),
            }],
        );
        let inbound = connect_and_exchange(
            RemoteEndpoint {
                mode: Some(RemoteMode::WebSocket),
                scheme: "ws".to_owned(),
                address: format!("ws://{address}"),
                ..RemoteEndpoint::default()
            },
            vec![
                RemoteEnvelope::Message { message },
                RemoteEnvelope::AssistantDirective {
                    directive: AssistantDirective {
                        agent_id: Some("coordinator".to_owned()),
                        instruction: "delegate this".to_owned(),
                        ..AssistantDirective::default()
                    },
                },
                RemoteEnvelope::VoiceFrame {
                    frame: VoiceFrame {
                        format: "pcm16".to_owned(),
                        payload_base64: base64_encode(b"voice"),
                        sequence: 1,
                        stream_id: Some("stream-1".to_owned()),
                        is_final: true,
                    },
                },
                RemoteEnvelope::Interrupt,
            ],
            4,
        )
        .await
        .unwrap();
        let record = server.await.unwrap();

        assert_eq!(inbound.len(), 4);
        assert!(matches!(inbound[0], RemoteEnvelope::Ack { .. }));
        assert_eq!(record.envelopes.len(), 4);
        assert!(matches!(
            record.envelopes[1],
            RemoteEnvelope::AssistantDirective { .. }
        ));
        assert!(matches!(
            record.envelopes[2],
            RemoteEnvelope::VoiceFrame { .. }
        ));
    }

    struct EchoHandler;

    #[async_trait]
    impl BridgeSessionHandler for EchoHandler {
        async fn on_connect(
            &mut self,
            _record: &super::BridgeSessionRecord,
        ) -> Result<Vec<RemoteEnvelope>> {
            Ok(vec![RemoteEnvelope::Ack {
                note: "connected".to_owned(),
            }])
        }

        async fn on_envelope(&mut self, envelope: &RemoteEnvelope) -> Result<Vec<RemoteEnvelope>> {
            Ok(match envelope {
                RemoteEnvelope::Message { message } => vec![RemoteEnvelope::Message {
                    message: Message::new(
                        MessageRole::Assistant,
                        vec![ContentBlock::Text {
                            text: format!("echo {}", message.blocks.len()),
                        }],
                    ),
                }],
                _ => vec![RemoteEnvelope::Ack {
                    note: "other".to_owned(),
                }],
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supports_handler_driven_bridge_sessions() {
        let address = "127.0.0.1:43126";
        let server = tokio::spawn(async move {
            serve_bridge_session(
                BridgeServerConfig {
                    bind_address: address.to_owned(),
                    ..BridgeServerConfig::default()
                },
                EchoHandler,
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let inbound = connect_and_exchange(
            RemoteEndpoint {
                mode: Some(RemoteMode::WebSocket),
                scheme: "ws".to_owned(),
                address: format!("ws://{address}"),
                ..RemoteEndpoint::default()
            },
            vec![RemoteEnvelope::Message {
                message: Message::new(
                    MessageRole::User,
                    vec![ContentBlock::Text {
                        text: "hello".to_owned(),
                    }],
                ),
            }],
            2,
        )
        .await
        .unwrap();
        let record = server.await.unwrap();

        assert!(matches!(inbound[0], RemoteEnvelope::Ack { .. }));
        assert!(matches!(inbound[1], RemoteEnvelope::Message { .. }));
        assert_eq!(record.envelopes.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supports_direct_connect_sessions() {
        let address = "127.0.0.1:43129";
        let server = tokio::spawn(async move {
            serve_direct_session(
                BridgeServerConfig {
                    bind_address: format!("tcp://{address}"),
                    ..BridgeServerConfig::default()
                },
                EchoHandler,
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let inbound = connect_and_exchange(
            RemoteEndpoint {
                mode: Some(RemoteMode::DirectConnect),
                scheme: "tcp".to_owned(),
                address: format!("tcp://{address}"),
                ..RemoteEndpoint::default()
            },
            vec![RemoteEnvelope::Message {
                message: Message::new(
                    MessageRole::User,
                    vec![ContentBlock::Text {
                        text: "hello direct".to_owned(),
                    }],
                ),
            }],
            2,
        )
        .await
        .unwrap();
        let record = server.await.unwrap();

        assert!(matches!(inbound[0], RemoteEnvelope::Ack { .. }));
        assert!(matches!(inbound[1], RemoteEnvelope::Message { .. }));
        assert_eq!(record.envelopes.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supports_ide_bridge_sessions() {
        let address = "127.0.0.1:43130";
        let server = tokio::spawn(async move {
            serve_direct_session(
                BridgeServerConfig {
                    bind_address: format!("ide://{address}"),
                    ..BridgeServerConfig::default()
                },
                EchoHandler,
            )
            .await
            .unwrap()
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let inbound = connect_and_exchange(
            RemoteEndpoint {
                mode: Some(RemoteMode::IdeBridge),
                scheme: "ide".to_owned(),
                address: format!("ide://{address}"),
                ..RemoteEndpoint::default()
            },
            vec![RemoteEnvelope::Message {
                message: Message::new(
                    MessageRole::User,
                    vec![ContentBlock::Text {
                        text: "hello ide".to_owned(),
                    }],
                ),
            }],
            2,
        )
        .await
        .unwrap();
        let record = server.await.unwrap();

        assert!(matches!(inbound[0], RemoteEnvelope::Ack { .. }));
        assert!(matches!(inbound[1], RemoteEnvelope::Message { .. }));
        assert_eq!(record.envelopes.len(), 1);
    }

    #[test]
    fn decodes_base64_payloads() {
        assert_eq!(base64_decode(&base64_encode(b"voice")).unwrap(), b"voice");
    }
}
