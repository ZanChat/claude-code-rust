fn env_value<const N: usize>(names: [&str; N]) -> Option<String> {
    names
        .into_iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

fn read_chatgpt_codex_models_cache() -> Option<ChatGPTCodexModelsCache> {
    let path = codex_home_dir().join("models_cache.json");
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn openai_chat_tool_call(call: &ToolCall) -> Value {
    let mut tool_call = json!({
        "id": call.id,
        "type": "function",
        "function": {
            "name": call.name,
            "arguments": call.input_json,
        }
    });

    if let Some(thought_signature) = call
        .thought_signature
        .as_deref()
        .filter(|thought_signature| !thought_signature.trim().is_empty())
    {
        tool_call["extra_content"] = json!({
            "google": {
                "thought_signature": thought_signature,
            }
        });
    }

    tool_call
}

fn openai_chat_thought_signature(tool_call: &Value) -> Option<String> {
    tool_call
        .get("extra_content")
        .and_then(|extra_content| extra_content.get("google"))
        .and_then(|google| google.get("thought_signature"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|thought_signature| !thought_signature.trim().is_empty())
}

fn events_from_anthropic_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    let mut events = Vec::new();

    for block in value
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("anthropic response missing content array"))?
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if !text.is_empty() {
                    events.push(ProviderEvent::MessageDelta { text });
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("anthropic tool_use block missing id"))?
                    .to_owned();
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("anthropic tool_use block missing name"))?
                    .to_owned();
                let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                events.push(ProviderEvent::ToolCall {
                    call: ToolCall {
                        id: id.clone(),
                        name,
                        input_json: serde_json::to_string(&input)?,
                        thought_signature: None,
                    },
                });
                events.push(ProviderEvent::ToolCallBoundary { id });
            }
            Some(other) => {
                events.push(ProviderEvent::Error {
                    message: format!("unsupported Anthropic content block type: {other}"),
                });
            }
            None => {}
        }
    }

    if let Some(usage) = anthropic_usage(value) {
        events.push(ProviderEvent::Usage { usage });
    }
    events.push(ProviderEvent::Stop {
        reason: value
            .get("stop_reason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_owned(),
    });

    Ok(events)
}

fn anthropic_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_creation_input_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_read_input_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
    })
}

fn events_from_openai_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    if value.get("choices").is_some() {
        return events_from_openai_chat_response(value);
    }
    events_from_openai_responses_response(value)
}

fn parse_openai_sse_event(raw_event: &str) -> Result<Option<Value>> {
    let data = raw_event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = data.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return Ok(None);
    }

    let value: Value = serde_json::from_str(trimmed)
        .map_err(|error| anyhow!("failed to parse OpenAI Responses SSE event: {error}"))?;
    Ok(Some(value))
}

fn provider_events_from_openai_sse_event(
    value: &Value,
    saw_text_delta: &mut bool,
    completed: &mut bool,
) -> Result<Vec<ProviderEvent>> {
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_text.delta") => {
            if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                if !delta.is_empty() {
                    *saw_text_delta = true;
                    return Ok(vec![ProviderEvent::MessageDelta {
                        text: delta.to_owned(),
                    }]);
                }
            }
            Ok(Vec::new())
        }
        Some("response.completed") => {
            *completed = true;
            let response = value
                .get("response")
                .ok_or_else(|| anyhow!("response.completed event missing response payload"))?;
            events_from_openai_responses_response_parts(response, !*saw_text_delta)
        }
        Some("error") | Some("response.failed") => Err(anyhow!(openai_sse_error_message(value))),
        _ => Ok(Vec::new()),
    }
}

#[cfg(test)]
fn events_from_openai_sse_body(body: &str) -> Result<Vec<ProviderEvent>> {
    let normalized = body.replace("\r\n", "\n");
    let mut events = Vec::new();
    let mut completed = false;
    let mut saw_text_delta = false;

    for raw_event in normalized.split("\n\n") {
        if let Some(value) = parse_openai_sse_event(raw_event)? {
            events.extend(provider_events_from_openai_sse_event(
                &value,
                &mut saw_text_delta,
                &mut completed,
            )?);
        }
    }

    if !completed {
        return Err(anyhow!(
            "responses stream completed without a response.completed event"
        ));
    }
    Ok(events)
}

fn openai_sse_error_message(value: &Value) -> String {
    for pointer in [
        "/error/message",
        "/response/error/message",
        "/detail",
        "/message",
    ] {
        if let Some(message) = value.pointer(pointer).and_then(Value::as_str) {
            if !message.trim().is_empty() {
                return message.to_owned();
            }
        }
    }

    compact_error_body(&value.to_string())
}

fn events_from_openai_chat_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or_else(|| anyhow!("openai response missing choices"))?;
    let message = choice
        .get("message")
        .ok_or_else(|| anyhow!("openai response missing assistant message"))?;

    let mut events = Vec::new();
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        if !text.is_empty() {
            events.push(ProviderEvent::MessageDelta {
                text: text.to_owned(),
            });
        }
    }
    for tool_call in message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = tool_call
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("openai tool call missing id"))?
            .to_owned();
        let function = tool_call
            .get("function")
            .ok_or_else(|| anyhow!("openai tool call missing function"))?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("openai tool call missing function name"))?
            .to_owned();
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("{}")
            .to_owned();
        events.push(ProviderEvent::ToolCall {
            call: ToolCall {
                id: id.clone(),
                name,
                input_json: arguments,
                thought_signature: openai_chat_thought_signature(tool_call),
            },
        });
        events.push(ProviderEvent::ToolCallBoundary { id });
    }

    if let Some(usage) = openai_usage(value) {
        events.push(ProviderEvent::Usage { usage });
    }
    events.push(ProviderEvent::Stop {
        reason: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .unwrap_or("stop")
            .to_owned(),
    });

    Ok(events)
}

fn events_from_openai_responses_response(value: &Value) -> Result<Vec<ProviderEvent>> {
    events_from_openai_responses_response_parts(value, true)
}

fn events_from_openai_responses_response_parts(
    value: &Value,
    include_text: bool,
) -> Result<Vec<ProviderEvent>> {
    if let Some(message) = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
    {
        if !message.trim().is_empty() {
            return Err(anyhow!(message.to_owned()));
        }
    }

    let mut events = Vec::new();

    for item in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match item.get("type").and_then(Value::as_str) {
            Some("message") if include_text => {
                for part in item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if part.get("type").and_then(Value::as_str) == Some("output_text") {
                        if let Some(text) = part.get("text").and_then(Value::as_str) {
                            if !text.is_empty() {
                                events.push(ProviderEvent::MessageDelta {
                                    text: text.to_owned(),
                                });
                            }
                        }
                    }
                }
            }
            Some("function_call") => {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("responses function_call missing id"))?
                    .to_owned();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("responses function_call missing name"))?
                    .to_owned();
                let arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}")
                    .to_owned();
                events.push(ProviderEvent::ToolCall {
                    call: ToolCall {
                        id: id.clone(),
                        name,
                        input_json: arguments,
                        thought_signature: None,
                    },
                });
                events.push(ProviderEvent::ToolCallBoundary { id });
            }
            _ => {}
        }
    }

    if let Some(usage) = openai_usage(value) {
        events.push(ProviderEvent::Usage { usage });
    }
    let stop_reason = if events
        .iter()
        .any(|event| matches!(event, ProviderEvent::ToolCall { .. }))
    {
        "tool_use".to_owned()
    } else if value
        .get("incomplete_details")
        .and_then(|details| details.get("reason"))
        .and_then(Value::as_str)
        == Some("max_output_tokens")
    {
        "max_tokens".to_owned()
    } else {
        "end_turn".to_owned()
    };
    events.push(ProviderEvent::Stop {
        reason: stop_reason,
    });

    Ok(events)
}

fn openai_usage(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage")?;
    Some(TokenUsage {
        input_tokens: usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        output_tokens: usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
    })
}
