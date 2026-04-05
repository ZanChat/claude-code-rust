fn persist_voice_capture(
    cwd: &Path,
    stream_id: &str,
    format: &str,
    payload: &[u8],
) -> Result<PathBuf> {
    let path = cwd
        .join(".code-agent")
        .join("voice")
        .join(format!("{stream_id}.{}", voice_extension(format)));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, payload)?;
    Ok(path)
}

fn compaction_event(outcome: &CompactionOutcome) -> Option<RemoteEnvelope> {
    outcome
        .boundary_message
        .blocks
        .iter()
        .find_map(|block| match block {
            ContentBlock::Boundary { boundary } => Some(RemoteEnvelope::Event {
                event: AppEvent::CompactApplied {
                    kind: boundary.kind.clone(),
                },
            }),
            _ => None,
        })
}

fn remote_envelopes_from_new_messages(
    messages: &[Message],
    start_index: usize,
) -> Vec<RemoteEnvelope> {
    let mut envelopes = Vec::new();
    for message in messages.iter().skip(start_index) {
        match message.role {
            MessageRole::Assistant => {
                envelopes.push(RemoteEnvelope::Message {
                    message: message.clone(),
                });
                for block in &message.blocks {
                    if let ContentBlock::ToolCall { call } = block {
                        envelopes.push(RemoteEnvelope::ToolCall { call: call.clone() });
                    }
                }
            }
            MessageRole::Tool => {
                for block in &message.blocks {
                    if let ContentBlock::ToolResult { result } = block {
                        envelopes.push(RemoteEnvelope::ToolResult {
                            result: result.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    envelopes
}

struct LocalBridgeHandler<'a> {
    store: &'a ActiveSessionStore,
    tool_registry: &'a ToolRegistry,
    cwd: PathBuf,
    provider: ApiProvider,
    active_model: String,
    session_id: SessionId,
    raw_messages: Vec<Message>,
    live_runtime: bool,
    allow_remote_tools: bool,
    pending_permission: Option<PendingRemoteTool>,
    voice_streams: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug)]
struct PendingRemoteTool {
    request: RemotePermissionRequest,
    call: code_agent_core::ToolCall,
}

impl<'a> LocalBridgeHandler<'a> {
    fn task_store(&self) -> CoreLocalTaskStore {
        task_store_for(&self.cwd)
    }

    fn tool_runtime_envelopes(&self, tool_name: &str, metadata: &Value) -> Vec<RemoteEnvelope> {
        let mut envelopes = Vec::new();
        if let Ok(task) = serde_json::from_value::<TaskRecord>(metadata.clone()) {
            envelopes.push(RemoteEnvelope::TaskState { task });
        }
        if let Some(task) = metadata
            .get("workflow")
            .cloned()
            .and_then(|value| serde_json::from_value::<TaskRecord>(value).ok())
        {
            envelopes.push(RemoteEnvelope::TaskState { task });
        }
        if let Ok(question) = serde_json::from_value::<QuestionRequest>(metadata.clone()) {
            envelopes.push(RemoteEnvelope::Question { question });
        }
        if tool_name == "ask_user_question" {
            if let Some(question) = metadata
                .get("id")
                .and_then(|_| serde_json::from_value::<QuestionRequest>(metadata.clone()).ok())
            {
                envelopes.push(RemoteEnvelope::Question { question });
            }
        }
        envelopes
    }

    fn session_state(&self) -> RemoteSessionState {
        RemoteSessionState {
            endpoint: "local".to_owned(),
            connected: true,
            session_id: Some(self.session_id),
            provider: Some(self.provider.to_string()),
            model: Some(self.active_model.clone()),
            message_count: materialize_runtime_messages(&self.raw_messages).len(),
            pending_permission_id: self
                .pending_permission
                .as_ref()
                .map(|pending| pending.request.id.clone()),
            last_error: None,
        }
    }

    fn session_state_envelope(&self) -> RemoteEnvelope {
        RemoteEnvelope::SessionState {
            state: self.session_state(),
        }
    }

    fn with_session_state(&self, mut outbound: Vec<RemoteEnvelope>) -> Vec<RemoteEnvelope> {
        outbound.push(self.session_state_envelope());
        outbound
    }

    async fn resume_session(&mut self, target: &str) -> Result<Vec<RemoteEnvelope>> {
        let (session_id, _, messages) = self.store.load_resume_target(target).await?;
        self.session_id = session_id;
        self.raw_messages = messages;
        self.pending_permission = None;
        let runtime_messages = materialize_runtime_messages(&self.raw_messages);
        let start = runtime_messages.len().saturating_sub(8);
        let mut outbound = runtime_messages
            .into_iter()
            .skip(start)
            .map(|message| RemoteEnvelope::Message { message })
            .collect::<Vec<_>>();
        outbound.push(self.session_state_envelope());
        Ok(outbound)
    }

    async fn run_remote_tool_call(
        &mut self,
        call: code_agent_core::ToolCall,
    ) -> Result<Vec<RemoteEnvelope>> {
        let parent_id = self.raw_messages.last().map(|message| message.id);
        let tool_call_message = build_assistant_message(
            self.session_id,
            parent_id,
            String::new(),
            vec![call.clone()],
        );
        self.store
            .append_message(self.session_id, &tool_call_message)
            .await?;
        self.raw_messages.push(tool_call_message.clone());

        let (result, supplemental) = match serde_json::from_str::<Value>(&call.input_json) {
            Ok(input) => match self
                .tool_registry
                .invoke(
                    ToolCallRequest {
                        tool_name: call.name.clone(),
                        input,
                    },
                    &ToolContext {
                        session_id: Some(self.session_id),
                        cwd: self.cwd.clone(),
                        provider: Some(self.provider.to_string()),
                        model: Some(self.active_model.clone()),
                        ..ToolContext::default()
                    },
                )
                .await
            {
                Ok(output) => (
                    code_agent_core::ToolResult {
                        tool_call_id: call.id.clone(),
                        output_text: output.content,
                        is_error: output.is_error,
                    },
                    self.tool_runtime_envelopes(&call.name, &output.metadata),
                ),
                Err(error) => (
                    code_agent_core::ToolResult {
                        tool_call_id: call.id.clone(),
                        output_text: error.to_string(),
                        is_error: true,
                    },
                    Vec::new(),
                ),
            },
            Err(error) => (
                code_agent_core::ToolResult {
                    tool_call_id: call.id.clone(),
                    output_text: format!("invalid tool input JSON: {error}"),
                    is_error: true,
                },
                Vec::new(),
            ),
        };

        let tool_message = build_tool_result_message(
            self.session_id,
            result.tool_call_id.clone(),
            result.output_text.clone(),
            result.is_error,
            Some(tool_call_message.id),
        );
        self.store
            .append_message(self.session_id, &tool_message)
            .await?;
        self.raw_messages.push(tool_message);

        let mut outbound = vec![
            RemoteEnvelope::ToolCall { call },
            RemoteEnvelope::ToolResult { result },
        ];
        outbound.extend(supplemental);
        Ok(self.with_session_state(outbound))
    }

    async fn execute_remote_tool_call(
        &mut self,
        call: code_agent_core::ToolCall,
    ) -> Result<Vec<RemoteEnvelope>> {
        let Some(spec) = self.tool_registry.get(&call.name).map(|tool| tool.spec()) else {
            return Ok(self.with_session_state(vec![RemoteEnvelope::Error {
                message: format!("unknown tool: {}", call.name),
            }]));
        };

        if spec.needs_permission && !self.allow_remote_tools {
            let request = RemotePermissionRequest {
                id: Uuid::new_v4().to_string(),
                tool_name: call.name.clone(),
                input_json: call.input_json.clone(),
                read_only: spec.read_only,
                reason: Some("remote tool execution requires approval".to_owned()),
            };
            self.pending_permission = Some(PendingRemoteTool {
                request: request.clone(),
                call,
            });
            return Ok(self.with_session_state(vec![RemoteEnvelope::PermissionRequest { request }]));
        }

        self.pending_permission = None;
        self.run_remote_tool_call(call).await
    }

    async fn execute_prompt(&mut self, prompt_text: String) -> Result<Vec<RemoteEnvelope>> {
        let start_index = self.raw_messages.len();
        let (applied_compaction, _, _, _, _) = execute_local_turn(
            self.store,
            self.tool_registry,
            self.cwd.clone(),
            self.provider,
            self.active_model.clone(),
            self.session_id,
            &mut self.raw_messages,
            prompt_text,
            self.live_runtime,
            None,
        )
        .await?;

        let mut outbound = Vec::new();
        if let Some(outcome) = applied_compaction.as_ref().and_then(compaction_event) {
            outbound.push(outcome);
        }
        outbound.extend(remote_envelopes_from_new_messages(
            &self.raw_messages,
            start_index + 1,
        ));
        if outbound.is_empty() {
            outbound.push(RemoteEnvelope::Ack {
                note: "no_output".to_owned(),
            });
        }
        Ok(self.with_session_state(outbound))
    }

    async fn execute_coordinator(
        &mut self,
        directive: &AssistantDirective,
    ) -> Result<Vec<RemoteEnvelope>> {
        let tasks = coordinator_tasks(&directive.instruction);
        if tasks.is_empty() {
            return Ok(vec![RemoteEnvelope::Ack {
                note: "empty_coordinator_directive".to_owned(),
            }]);
        }

        let start_index = self.raw_messages.len();
        let mut outbound = Vec::new();
        let mut worker_summaries = Vec::new();
        let task_store = self.task_store();
        let coordinator_task =
            create_coordinator_task(&task_store, self.session_id, directive.instruction.clone())?;
        outbound.push(RemoteEnvelope::TaskState {
            task: coordinator_task.clone(),
        });
        let codec = JsonlTranscriptCodec;

        for (index, task) in tasks.iter().enumerate() {
            let worker_start = self.raw_messages.len();
            let agent_id = uuid::Uuid::new_v4();
            let transcript_path = agent_transcript_path_for(
                &self.cwd,
                self.session_id,
                agent_id,
                Some("coordinator"),
            );
            let worker_task = create_coordinator_worker_task(
                &task_store,
                self.session_id,
                coordinator_task.id,
                agent_id,
                format!("worker {}", index + 1),
                task.clone(),
                Some(transcript_path.clone()),
            )?;
            outbound.push(RemoteEnvelope::TaskState {
                task: worker_task.clone(),
            });
            let worker_prompt = format!(
                "[worker {}/{}]\nTask: {}\nReturn concise findings only.",
                index + 1,
                tasks.len(),
                task
            );
            let (applied_compaction, _, _, _, _) = execute_local_turn(
                self.store,
                self.tool_registry,
                self.cwd.clone(),
                self.provider,
                self.active_model.clone(),
                self.session_id,
                &mut self.raw_messages,
                worker_prompt,
                self.live_runtime,
                None,
            )
            .await?;
            if let Some(event) = applied_compaction.as_ref().and_then(compaction_event) {
                outbound.push(event);
            }
            let worker_findings = self
                .raw_messages
                .iter()
                .skip(worker_start)
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
                .map(message_text)
                .filter(|text| !text.trim().is_empty())
                .unwrap_or_else(|| "no findings".to_owned());
            for message in self.raw_messages.iter().skip(worker_start) {
                codec.append_message(&transcript_path, message).await?;
            }
            let worker_task = update_task_record(
                &task_store,
                worker_task,
                TaskStatus::Completed,
                Some(worker_findings.clone()),
            )?;
            outbound.push(RemoteEnvelope::TaskState { task: worker_task });
            worker_summaries.push(format!("worker {}: {}", index + 1, worker_findings));
        }

        let synthesis_task = create_coordinator_synthesis_task(
            &task_store,
            self.session_id,
            coordinator_task.id,
            directive.instruction.clone(),
        )?;
        outbound.push(RemoteEnvelope::TaskState {
            task: synthesis_task.clone(),
        });
        let synthesis_prompt = format!(
            "[coordinator synthesis]\nOriginal directive: {}\n{}\nRespond with a combined answer.",
            directive.instruction.trim(),
            worker_summaries.join("\n")
        );
        let (applied_compaction, _, _, _, _) = execute_local_turn(
            self.store,
            self.tool_registry,
            self.cwd.clone(),
            self.provider,
            self.active_model.clone(),
            self.session_id,
            &mut self.raw_messages,
            synthesis_prompt,
            self.live_runtime,
            None,
        )
        .await?;
        if let Some(event) = applied_compaction.as_ref().and_then(compaction_event) {
            outbound.push(event);
        }
        let synthesis_output = self
            .raw_messages
            .iter()
            .skip(start_index)
            .rev()
            .find(|message| message.role == MessageRole::Assistant)
            .map(message_text)
            .unwrap_or_default();
        let synthesis_task = update_task_record(
            &task_store,
            synthesis_task,
            TaskStatus::Completed,
            Some(synthesis_output.clone()),
        )?;
        outbound.push(RemoteEnvelope::TaskState {
            task: synthesis_task,
        });
        let coordinator_task = update_task_record(
            &task_store,
            coordinator_task,
            TaskStatus::Completed,
            Some(synthesis_output),
        )?;
        outbound.push(RemoteEnvelope::TaskState {
            task: coordinator_task,
        });
        outbound.extend(remote_envelopes_from_new_messages(
            &self.raw_messages,
            start_index,
        ));
        if outbound.is_empty() {
            outbound.push(RemoteEnvelope::Ack {
                note: "no_output".to_owned(),
            });
        }
        Ok(self.with_session_state(outbound))
    }
}

#[async_trait]
impl BridgeSessionHandler for LocalBridgeHandler<'_> {
    async fn on_connect(
        &mut self,
        _record: &code_agent_bridge::BridgeSessionRecord,
    ) -> Result<Vec<RemoteEnvelope>> {
        Ok(vec![
            RemoteEnvelope::Event {
                event: AppEvent::RemoteConnected,
            },
            self.session_state_envelope(),
        ])
    }

    async fn on_envelope(&mut self, envelope: &RemoteEnvelope) -> Result<Vec<RemoteEnvelope>> {
        match envelope {
            RemoteEnvelope::Message { message } => {
                let prompt_text = message_text(message);
                if prompt_text.trim().is_empty() {
                    return Ok(vec![RemoteEnvelope::Ack {
                        note: "empty_message".to_owned(),
                    }]);
                }
                self.execute_prompt(prompt_text).await
            }
            RemoteEnvelope::AssistantDirective { directive } => {
                let prompt = directive.instruction.trim();
                if prompt.is_empty() {
                    return Ok(vec![RemoteEnvelope::Ack {
                        note: "empty_assistant_directive".to_owned(),
                    }]);
                }
                if directive.agent_id.as_deref() == Some("coordinator") {
                    return self.execute_coordinator(directive).await;
                }
                let decorated = directive
                    .agent_id
                    .as_ref()
                    .map(|agent_id| format!("[assistant:{agent_id}] {prompt}"))
                    .unwrap_or_else(|| prompt.to_owned());
                self.execute_prompt(decorated).await
            }
            RemoteEnvelope::VoiceFrame { frame } => {
                let payload = base64_decode(&frame.payload_base64)?;
                let stream_id = frame
                    .stream_id
                    .clone()
                    .unwrap_or_else(|| "default".to_owned());
                let buffered = self.voice_streams.entry(stream_id.clone()).or_default();
                buffered.extend_from_slice(&payload);
                if !frame.is_final {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: format!("voice_frame_buffered:{stream_id}"),
                    }]));
                }

                let payload = self.voice_streams.remove(&stream_id).unwrap_or_default();
                let prompt = match String::from_utf8(payload.clone()) {
                    Ok(prompt) => prompt,
                    Err(_) => {
                        let path =
                            persist_voice_capture(&self.cwd, &stream_id, &frame.format, &payload)?;
                        return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                            note: format!("voice_frame_saved:{}", path.display()),
                        }]));
                    }
                };
                if prompt.trim().is_empty() {
                    let path = persist_voice_capture(
                        &self.cwd,
                        &stream_id,
                        &frame.format,
                        prompt.as_bytes(),
                    )?;
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: format!("voice_frame_saved:{}", path.display()),
                    }]));
                }
                self.execute_prompt(prompt).await
            }
            RemoteEnvelope::ResumeSession { request } => {
                if request.target.trim().is_empty() {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: "empty_resume_target".to_owned(),
                    }]));
                }
                self.resume_session(&request.target).await
            }
            RemoteEnvelope::TaskState { .. } => Ok(Vec::new()),
            RemoteEnvelope::Question { question } => {
                let stored = self.task_store().record_question(question.clone())?;
                Ok(self.with_session_state(vec![RemoteEnvelope::Question { question: stored }]))
            }
            RemoteEnvelope::QuestionResponse { response } => {
                let store = self.task_store();
                let stored = store.answer_question(response.clone())?;
                let resumed = resume_tasks_for_question(&store, stored.question_id)?;
                let mut outbound = vec![RemoteEnvelope::QuestionResponse { response: stored }];
                outbound.extend(
                    resumed
                        .into_iter()
                        .map(|task| RemoteEnvelope::TaskState { task }),
                );
                Ok(self.with_session_state(outbound))
            }
            RemoteEnvelope::ToolCall { call } => self.execute_remote_tool_call(call.clone()).await,
            RemoteEnvelope::PermissionResponse { response } => {
                let Some(pending) = self.pending_permission.clone() else {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                        note: "no_pending_permission".to_owned(),
                    }]));
                };
                if pending.request.id != response.id {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::Error {
                        message: format!("unknown permission request: {}", response.id),
                    }]));
                }
                self.pending_permission = None;
                if !response.approved {
                    return Ok(self.with_session_state(vec![RemoteEnvelope::ToolResult {
                        result: code_agent_core::ToolResult {
                            tool_call_id: pending.call.id,
                            output_text: response
                                .note
                                .clone()
                                .unwrap_or_else(|| "remote tool permission denied".to_owned()),
                            is_error: true,
                        },
                    }]));
                }
                self.run_remote_tool_call(pending.call).await
            }
            RemoteEnvelope::Interrupt => Ok(vec![RemoteEnvelope::Ack {
                note: "interrupt".to_owned(),
            }]),
            RemoteEnvelope::ToolResult { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "tool_result_received".to_owned(),
                }]))
            }
            RemoteEnvelope::Event { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "event_received".to_owned(),
                }]))
            }
            RemoteEnvelope::SessionState { .. } => Ok(Vec::new()),
            RemoteEnvelope::PermissionRequest { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "permission_request_received".to_owned(),
                }]))
            }
            RemoteEnvelope::Error { .. } => {
                Ok(self.with_session_state(vec![RemoteEnvelope::Ack {
                    note: "error_received".to_owned(),
                }]))
            }
            RemoteEnvelope::Ack { .. } => Ok(Vec::new()),
        }
    }
}

