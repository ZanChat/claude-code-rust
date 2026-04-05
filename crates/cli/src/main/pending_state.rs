#[derive(Clone, Debug)]
struct PendingReplStep {
    step: usize,
    start_index: usize,
    status_label: String,
    status_detail: Option<String>,
    task_status: TaskStatus,
    expanded: bool,
    touched: bool,
}

impl PendingReplStep {
    fn id(&self) -> String {
        format!("pending-step-{}", self.step)
    }
}

#[derive(Clone, Debug)]
struct PendingReplView {
    messages: Vec<Message>,
    spinner_verb: String,
    progress_label: String,
    steps: Vec<PendingReplStep>,
    queued_inputs: Vec<String>,
    show_transcript_details: bool,
}

impl PendingReplView {
    fn new(messages: Vec<Message>, progress_label: impl Into<String>) -> Self {
        let progress_label = progress_label.into();
        Self {
            messages,
            spinner_verb: pending_spinner_verb(&progress_label),
            progress_label,
            steps: Vec::new(),
            queued_inputs: Vec::new(),
            show_transcript_details: false,
        }
    }
}

fn update_pending_repl_view(
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
    messages: &[Message],
    progress_label: impl Into<String>,
) {
    let Some(pending_view) = pending_view else {
        return;
    };
    if let Ok(mut state) = pending_view.lock() {
        state.messages = materialize_runtime_messages(messages);
        state.progress_label = progress_label.into();
    }
}

fn update_pending_repl_step_view(
    pending_view: Option<&Arc<Mutex<PendingReplView>>>,
    step: usize,
    step_start_index: usize,
    messages: &[Message],
    progress_label: impl Into<String>,
    status_detail: Option<String>,
    task_status: TaskStatus,
) {
    let Some(pending_view) = pending_view else {
        return;
    };
    if let Ok(mut state) = pending_view.lock() {
        let runtime_messages = materialize_runtime_messages(messages);
        let runtime_start_index = step_start_index.min(runtime_messages.len());
        let progress_label = progress_label.into();
        if !state.steps.iter().any(|entry| entry.step == step) {
            if let Some(previous) = state.steps.last_mut() {
                if !previous.touched {
                    previous.expanded = false;
                }
            }
            state.steps.push(PendingReplStep {
                step,
                start_index: runtime_start_index,
                status_label: progress_label.clone(),
                status_detail: None,
                task_status: task_status.clone(),
                expanded: true,
                touched: false,
            });
        }
        if let Some(entry) = state.steps.iter_mut().find(|entry| entry.step == step) {
            entry.start_index = runtime_start_index.min(runtime_messages.len());
            entry.status_label = progress_label;
            entry.status_detail = status_detail;
            entry.task_status = task_status;
        }
        state.messages = runtime_messages;
        state.progress_label = state
            .steps
            .iter()
            .find(|entry| entry.step == step)
            .map(|entry| {
                compose_pending_progress_label(&entry.status_label, entry.status_detail.as_deref())
            })
            .unwrap_or_else(|| "working".to_owned());
    }
}

fn queue_pending_repl_input(pending_view: &Arc<Mutex<PendingReplView>>, prompt_text: String) {
    if let Ok(mut state) = pending_view.lock() {
        state.queued_inputs.push(prompt_text);
    }
}

fn take_pending_repl_inputs(pending_view: &Arc<Mutex<PendingReplView>>) -> Vec<String> {
    pending_view
        .lock()
        .map(|mut state| mem::take(&mut state.queued_inputs))
        .unwrap_or_default()
}

fn toggle_pending_repl_group(pending_view: &Arc<Mutex<PendingReplView>>, group_id: &str) {
    if let Ok(mut state) = pending_view.lock() {
        if let Some(entry) = state.steps.iter_mut().find(|entry| entry.id() == group_id) {
            entry.expanded = !entry.expanded;
            entry.touched = true;
        }
    }
}

fn toggle_pending_repl_transcript_details(pending_view: &Arc<Mutex<PendingReplView>>) {
    if let Ok(mut state) = pending_view.lock() {
        state.show_transcript_details = !state.show_transcript_details;
    }
}

fn pending_repl_snapshot(pending_view: &Arc<Mutex<PendingReplView>>) -> PendingReplView {
    pending_view
        .lock()
        .map(|state| state.clone())
        .unwrap_or_else(|_| PendingReplView::new(Vec::new(), "working"))
}

fn pending_interrupt_messages(
    session_id: SessionId,
    raw_messages: &[Message],
    pending_view: &PendingReplView,
) -> Vec<Message> {
    let mut interrupt_messages = pending_view
        .messages
        .iter()
        .filter(|message| {
            raw_messages
                .iter()
                .all(|existing| existing.id != message.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let parent_id = interrupt_messages
        .last()
        .map(|message| message.id)
        .or_else(|| raw_messages.last().map(|message| message.id));
    interrupt_messages.push(build_user_interruption_message(session_id, parent_id));
    interrupt_messages
}

fn provider_assistant_message(
    session_id: SessionId,
    parent_id: Option<Uuid>,
    text: String,
    tool_calls: Vec<code_agent_core::ToolCall>,
    provider: ApiProvider,
    model: &str,
    usage: Option<code_agent_core::TokenUsage>,
) -> Message {
    let mut assistant_message = build_assistant_message(session_id, parent_id, text, tool_calls);
    assistant_message.metadata.provider = Some(provider.to_string());
    assistant_message.metadata.model = Some(model.to_owned());
    assistant_message.metadata.usage = usage;
    assistant_message
}

fn pending_spinner_frame(tick: usize) -> &'static str {
    const FRAMES: [&str; 4] = ["-", "\\", "|", "/"];
    FRAMES[tick % FRAMES.len()]
}
