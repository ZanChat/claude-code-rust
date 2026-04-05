fn should_exit_repl(prompt_text: &str) -> bool {
    matches!(prompt_text.trim(), "quit" | "exit" | "/quit" | "/exit")
}

fn status_line_needs_marquee(status_line: &str) -> bool {
    status_line.chars().count() > 96
}

async fn resolve_continue_target(cli: &mut Cli, store: &ActiveSessionStore) -> Result<()> {
    if cli.resume.is_some() || !cli.continue_latest {
        return Ok(());
    }

    let summary = store
        .list_sessions()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("No conversation found to continue"))?;
    cli.resume = Some(summary.session_id.to_string());
    Ok(())
}

fn build_text_message(
    session_id: SessionId,
    role: MessageRole,
    text: String,
    parent_id: Option<Uuid>,
) -> Message {
    let mut message = Message::new(role, vec![ContentBlock::Text { text }]);
    message.session_id = Some(session_id);
    message.parent_id = parent_id;
    message
}

fn build_user_interruption_message(session_id: SessionId, parent_id: Option<Uuid>) -> Message {
    build_text_message(
        session_id,
        MessageRole::User,
        REQUEST_INTERRUPTED_MESSAGE.to_owned(),
        parent_id,
    )
}

fn build_ui_event_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    text: String,
    ui_role: &str,
    ui_author: Option<String>,
) -> Message {
    let mut message = build_text_message(session_id, MessageRole::Attachment, text, parent_id);
    message.metadata.tags.push(UI_EVENT_TAG.to_owned());
    message
        .metadata
        .attributes
        .insert(UI_ROLE_ATTRIBUTE.to_owned(), ui_role.to_owned());
    if let Some(author) = ui_author.filter(|value| !value.trim().is_empty()) {
        message
            .metadata
            .attributes
            .insert(UI_AUTHOR_ATTRIBUTE.to_owned(), author);
    }
    message
}

fn build_repl_command_input_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    raw_input: impl Into<String>,
) -> Message {
    build_ui_event_message(session_id, parent_id, raw_input.into(), "command", None)
}

fn build_repl_command_output_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    command_name: &str,
    output: impl Into<String>,
) -> Message {
    build_ui_event_message(
        session_id,
        parent_id,
        output.into(),
        "command_output",
        Some(format!("/{command_name}")),
    )
}

fn should_record_repl_command(name: &str) -> bool {
    !matches!(name, "clear" | "resume")
}

fn should_echo_command_result_in_footer(
    command_name: &str,
    command_recorded: bool,
    is_error: bool,
) -> bool {
    if command_recorded {
        return false;
    }
    if is_error {
        return true;
    }
    command_name != "resume"
}

async fn append_session_message(
    store: &ActiveSessionStore,
    raw_messages: &mut Vec<Message>,
    message: Message,
) -> Result<()> {
    let session_id = message
        .session_id
        .ok_or_else(|| anyhow!("session message missing session id"))?;
    store.append_message(session_id, &message).await?;
    raw_messages.push(message);
    Ok(())
}

async fn append_session_messages(
    store: &ActiveSessionStore,
    raw_messages: &mut Vec<Message>,
    messages: Vec<Message>,
) -> Result<()> {
    for message in messages {
        append_session_message(store, raw_messages, message).await?;
    }
    Ok(())
}
