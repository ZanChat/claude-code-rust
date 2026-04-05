fn open_transcript_search(search_state: &mut ReplTranscriptSearchState, transcript_scroll: u16) {
    if !search_state.open {
        search_state.saved_input_buffer = search_state.input_buffer.clone();
        search_state.saved_active_item = search_state.active_item;
        search_state.anchor_scroll = transcript_scroll;
    }
    search_state.open = true;
    search_state.input_buffer.cursor = search_state.input_buffer.chars.len();
}

fn cancel_transcript_search(
    search_state: &mut ReplTranscriptSearchState,
    transcript_scroll: &mut u16,
) {
    search_state.input_buffer = search_state.saved_input_buffer.clone();
    search_state.active_item = search_state.saved_active_item;
    search_state.open = false;
    *transcript_scroll = search_state.anchor_scroll;
}

fn sync_transcript_search_preview(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    search_state: &mut ReplTranscriptSearchState,
    transcript_scroll: &mut u16,
) {
    let query = search_state.input_buffer.as_str();
    if query.trim().is_empty() {
        search_state.active_item = None;
        *transcript_scroll = search_state.anchor_scroll;
        return;
    }

    let matches = transcript_search_match_items(ui_state, &query);
    if matches.is_empty() {
        search_state.active_item = None;
        *transcript_scroll = search_state.anchor_scroll;
        return;
    }

    let active_item = search_state
        .active_item
        .filter(|item| matches.contains(item))
        .unwrap_or(matches[0]);
    search_state.active_item = Some(active_item);
    if let Some(scroll) =
        transcript_search_scroll_for_view(ui_state, terminal_width, terminal_height, active_item)
    {
        *transcript_scroll = scroll;
    }
}

fn step_transcript_search_match(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    search_state: &mut ReplTranscriptSearchState,
    transcript_scroll: &mut u16,
    reverse: bool,
) -> bool {
    let query = search_state.input_buffer.as_str();
    let matches = transcript_search_match_items(ui_state, &query);
    if matches.is_empty() {
        search_state.active_item = None;
        return false;
    }

    let next_index = match search_state
        .active_item
        .and_then(|item| matches.iter().position(|candidate| *candidate == item))
    {
        Some(index) if reverse => index.checked_sub(1).unwrap_or(matches.len() - 1),
        Some(index) => {
            if index + 1 < matches.len() {
                index + 1
            } else {
                0
            }
        }
        None if reverse => matches.len() - 1,
        None => 0,
    };

    let active_item = matches[next_index];
    search_state.active_item = Some(active_item);
    if let Some(scroll) =
        transcript_search_scroll_for_view(ui_state, terminal_width, terminal_height, active_item)
    {
        *transcript_scroll = scroll;
    }
    true
}

fn push_message_action_items(
    items: &mut Vec<ReplMessageActionItem>,
    messages: &[Message],
    item_index: &mut usize,
) {
    for message in messages {
        let transcript_line = transcript_line_from_message(message);
        if !transcript_line.text.trim().is_empty() {
            items.push(ReplMessageActionItem {
                item_index: *item_index,
                message: message.clone(),
                history_group_id: None,
            });
        }
        *item_index += 1;
    }
}

fn message_action_items_from_runtime(
    runtime_messages: &[Message],
    pending_view: Option<&PendingReplView>,
    interaction_state: &ReplInteractionState,
) -> Vec<ReplMessageActionItem> {
    let mut items = Vec::new();

    if let Some(pending_view) = pending_view.filter(|view| !view.steps.is_empty()) {
        let first_step_start = pending_view
            .steps
            .first()
            .map(|step| step.start_index.min(runtime_messages.len()))
            .unwrap_or(runtime_messages.len());
        let history = build_history_transcript_presentation(
            &runtime_messages[..first_step_start],
            &interaction_state.expanded_history_groups,
        );
        items.extend(history.action_items);
        let mut item_index = history.item_count;

        if !pending_view.show_transcript_details {
            return items;
        }

        for (index, step) in pending_view.steps.iter().enumerate() {
            item_index += 1;

            if !step.expanded {
                continue;
            }

            let end_index = pending_view
                .steps
                .get(index + 1)
                .map(|next| next.start_index)
                .unwrap_or(runtime_messages.len())
                .min(runtime_messages.len());
            let start_index = step.start_index.min(end_index);
            push_message_action_items(
                &mut items,
                &runtime_messages[start_index..end_index],
                &mut item_index,
            );
        }

        return items;
    }

    items.extend(
        build_history_transcript_presentation(
            runtime_messages,
            &interaction_state.expanded_history_groups,
        )
        .action_items,
    );
    items
}

fn enter_message_actions(
    interaction_state: &mut ReplInteractionState,
    items: &[ReplMessageActionItem],
) -> bool {
    let Some(item) = items.last() else {
        return false;
    };

    interaction_state.transcript_search.reset();
    interaction_state.prompt_history_search = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    interaction_state.transcript_selection = None;
    interaction_state.message_actions = Some(ReplMessageActionState {
        selected_item: item.item_index,
    });
    true
}

fn normalize_message_actions(
    interaction_state: &mut ReplInteractionState,
    items: &[ReplMessageActionItem],
) -> Option<usize> {
    let actions = interaction_state.message_actions.as_mut()?;
    if items.is_empty() {
        interaction_state.message_actions = None;
        return None;
    }

    if !items
        .iter()
        .any(|item| item.item_index == actions.selected_item)
    {
        actions.selected_item = items.last()?.item_index;
    }

    Some(actions.selected_item)
}

fn selected_message_action_item<'a>(
    interaction_state: &mut ReplInteractionState,
    items: &'a [ReplMessageActionItem],
) -> Option<&'a ReplMessageActionItem> {
    let selected_item = normalize_message_actions(interaction_state, items)?;
    items.iter().find(|item| item.item_index == selected_item)
}

fn move_message_action_selection(
    interaction_state: &mut ReplInteractionState,
    items: &[ReplMessageActionItem],
    navigation: ReplMessageActionNavigation,
) -> bool {
    let Some(selected_item) = normalize_message_actions(interaction_state, items) else {
        return false;
    };

    let Some(current_index) = items
        .iter()
        .position(|item| item.item_index == selected_item)
    else {
        return false;
    };

    let target_index = match navigation {
        ReplMessageActionNavigation::Top => 0,
        ReplMessageActionNavigation::Bottom => items.len().saturating_sub(1),
        ReplMessageActionNavigation::Prev => {
            current_index.checked_sub(1).unwrap_or(items.len() - 1)
        }
        ReplMessageActionNavigation::Next => {
            if current_index + 1 < items.len() {
                current_index + 1
            } else {
                0
            }
        }
        ReplMessageActionNavigation::PrevUser => (1..=items.len())
            .find_map(|offset| {
                let index = (current_index + items.len() - offset) % items.len();
                (items[index].message.role == MessageRole::User).then_some(index)
            })
            .unwrap_or(current_index),
        ReplMessageActionNavigation::NextUser => (1..=items.len())
            .find_map(|offset| {
                let index = (current_index + offset) % items.len();
                (items[index].message.role == MessageRole::User).then_some(index)
            })
            .unwrap_or(current_index),
    };

    let next_item = &items[target_index];
    if next_item.item_index == selected_item {
        return false;
    }

    if let Some(actions) = interaction_state.message_actions.as_mut() {
        actions.selected_item = next_item.item_index;
        return true;
    }

    false
}

fn sync_message_action_preview(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    interaction_state: &ReplInteractionState,
    transcript_scroll: &mut u16,
) {
    let Some(selected_item) = interaction_state
        .message_actions
        .as_ref()
        .map(|actions| actions.selected_item)
    else {
        return;
    };

    if let Some(scroll) =
        transcript_search_scroll_for_view(ui_state, terminal_width, terminal_height, selected_item)
    {
        *transcript_scroll = scroll;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TranscriptSelectionMove {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

fn should_clear_transcript_selection_on_key(key: &KeyEvent) -> bool {
    let is_nav = matches!(
        key.code,
        KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    );
    if is_nav
        && (key.modifiers.contains(KeyModifiers::SHIFT)
            || key.modifiers.contains(KeyModifiers::ALT)
            || key.modifiers.contains(KeyModifiers::SUPER))
    {
        return false;
    }
    true
}

fn transcript_selection_move_for_key(
    key: &KeyEvent,
    selection_exists: bool,
) -> Option<TranscriptSelectionMove> {
    if !key.modifiers.contains(KeyModifiers::SHIFT)
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }

    match key.code {
        KeyCode::Left => Some(TranscriptSelectionMove::Left),
        KeyCode::Right => Some(TranscriptSelectionMove::Right),
        KeyCode::Home => Some(TranscriptSelectionMove::LineStart),
        KeyCode::End => Some(TranscriptSelectionMove::LineEnd),
        KeyCode::Up if selection_exists => Some(TranscriptSelectionMove::Up),
        KeyCode::Down if selection_exists => Some(TranscriptSelectionMove::Down),
        _ => None,
    }
}

fn transcript_selectable_line_position(
    selectable_lines: &[TranscriptSelectableLine],
    line_index: usize,
) -> Option<usize> {
    selectable_lines
        .iter()
        .position(|line| line.line_index == line_index)
}

fn transcript_selection_default_focus(
    selectable_lines: &[TranscriptSelectableLine],
) -> Option<TranscriptSelectionPoint> {
    let line = selectable_lines.last()?;
    Some(TranscriptSelectionPoint {
        line_index: line.line_index,
        column: line.text.chars().count(),
    })
}

fn move_transcript_selection_focus(
    selectable_lines: &[TranscriptSelectableLine],
    focus: &TranscriptSelectionPoint,
    selection_move: TranscriptSelectionMove,
) -> Option<TranscriptSelectionPoint> {
    let current_position = transcript_selectable_line_position(selectable_lines, focus.line_index)?;
    let current_line = &selectable_lines[current_position];
    let current_len = current_line.text.chars().count();

    let next_focus = match selection_move {
        TranscriptSelectionMove::Left => {
            if focus.column > 0 {
                TranscriptSelectionPoint {
                    line_index: focus.line_index,
                    column: focus.column - 1,
                }
            } else {
                let previous_line = selectable_lines.get(current_position.checked_sub(1)?)?;
                TranscriptSelectionPoint {
                    line_index: previous_line.line_index,
                    column: previous_line.text.chars().count(),
                }
            }
        }
        TranscriptSelectionMove::Right => {
            if focus.column < current_len {
                TranscriptSelectionPoint {
                    line_index: focus.line_index,
                    column: focus.column + 1,
                }
            } else {
                let next_line = selectable_lines.get(current_position + 1)?;
                TranscriptSelectionPoint {
                    line_index: next_line.line_index,
                    column: 0,
                }
            }
        }
        TranscriptSelectionMove::LineStart => TranscriptSelectionPoint {
            line_index: focus.line_index,
            column: 0,
        },
        TranscriptSelectionMove::LineEnd => TranscriptSelectionPoint {
            line_index: focus.line_index,
            column: current_len,
        },
        TranscriptSelectionMove::Up => {
            let previous_line = selectable_lines.get(current_position.checked_sub(1)?)?;
            TranscriptSelectionPoint {
                line_index: previous_line.line_index,
                column: focus.column.min(previous_line.text.chars().count()),
            }
        }
        TranscriptSelectionMove::Down => {
            let next_line = selectable_lines.get(current_position + 1)?;
            TranscriptSelectionPoint {
                line_index: next_line.line_index,
                column: focus.column.min(next_line.text.chars().count()),
            }
        }
    };

    (next_focus != *focus).then_some(next_focus)
}

fn move_transcript_selection(
    interaction_state: &mut ReplInteractionState,
    selectable_lines: &[TranscriptSelectableLine],
    selection_move: TranscriptSelectionMove,
) -> Option<TranscriptSelectionPoint> {
    let anchor = interaction_state
        .transcript_selection
        .as_ref()
        .map(|selection| selection.anchor.clone())
        .or_else(|| transcript_selection_default_focus(selectable_lines))?;
    let focus = interaction_state
        .transcript_selection
        .as_ref()
        .map(|selection| selection.focus.clone())
        .unwrap_or_else(|| anchor.clone());
    let next_focus = move_transcript_selection_focus(selectable_lines, &focus, selection_move)?;

    if next_focus == anchor {
        interaction_state.transcript_selection = None;
        return Some(anchor);
    }

    interaction_state.transcript_selection = Some(TranscriptSelectionState {
        anchor,
        focus: next_focus.clone(),
    });
    Some(next_focus)
}

fn sync_transcript_selection_preview(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    terminal_height: u16,
    interaction_state: &ReplInteractionState,
    transcript_scroll: &mut u16,
) {
    let Some(focus_line) = interaction_state
        .transcript_selection
        .as_ref()
        .map(|selection| selection.focus.line_index)
    else {
        return;
    };

    if let Some(scroll) =
        transcript_visual_scroll_for_view(ui_state, terminal_width, terminal_height, focus_line)
    {
        *transcript_scroll = scroll;
    }
}

fn transcript_selection_copy_text(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    interaction_state: &ReplInteractionState,
) -> Option<String> {
    interaction_state
        .transcript_selection
        .as_ref()
        .and_then(|selection| {
            transcript_selection_text_for_view(ui_state, terminal_width, selection)
        })
}

fn normalize_prompt_selection(
    selection: &PromptSelectionState,
    input_len: usize,
) -> Option<(usize, usize)> {
    let start = selection.anchor.min(selection.focus).min(input_len);
    let end = selection.anchor.max(selection.focus).min(input_len);
    (start < end).then_some((start, end))
}

fn clear_prompt_selection(interaction_state: &mut ReplInteractionState) {
    interaction_state.prompt_selection = None;
}

fn clear_prompt_mouse_anchor(interaction_state: &mut ReplInteractionState) {
    interaction_state.prompt_mouse_anchor = None;
}

fn is_paste_shortcut(key: &KeyEvent) -> bool {
    key_matches_char_with_modifiers(key, 'v', KeyModifiers::SUPER)
        || key_matches_char_with_modifiers(key, 'v', KeyModifiers::CONTROL)
        || (matches!(key.code, KeyCode::Insert)
            && key_routing_modifiers(key.modifiers) == KeyModifiers::SHIFT)
}

fn insert_buffer_text(input_buffer: &mut code_agent_ui::InputBuffer, text: &str) -> bool {
    let mut inserted = false;
    for ch in text.chars() {
        input_buffer.push(ch);
        inserted = true;
    }
    inserted
}

fn prompt_selection_move_for_key(key: &KeyEvent) -> Option<PromptSelectionMove> {
    if key_routing_modifiers(key.modifiers) != KeyModifiers::SHIFT {
        return None;
    }

    match key.code {
        KeyCode::Left => Some(PromptSelectionMove::Left),
        KeyCode::Right => Some(PromptSelectionMove::Right),
        KeyCode::Home => Some(PromptSelectionMove::LineStart),
        KeyCode::End => Some(PromptSelectionMove::LineEnd),
        _ => None,
    }
}

fn prompt_selection_text(
    input_buffer: &code_agent_ui::InputBuffer,
    interaction_state: &ReplInteractionState,
) -> Option<String> {
    let (start, end) = interaction_state
        .prompt_selection
        .as_ref()
        .and_then(|selection| normalize_prompt_selection(selection, input_buffer.chars.len()))?;
    Some(input_buffer.chars[start..end].iter().collect())
}

fn repl_selection_copy_text(
    ui_state: &code_agent_ui::UiState,
    terminal_width: u16,
    input_buffer: &code_agent_ui::InputBuffer,
    interaction_state: &ReplInteractionState,
) -> Option<String> {
    if interaction_state.transcript_selection.is_some() {
        transcript_selection_copy_text(ui_state, terminal_width, interaction_state)
    } else {
        prompt_selection_text(input_buffer, interaction_state)
    }
}

fn move_prompt_selection_focus(
    input_buffer: &code_agent_ui::InputBuffer,
    focus: usize,
    selection_move: PromptSelectionMove,
) -> Option<usize> {
    match selection_move {
        PromptSelectionMove::Left => focus.checked_sub(1),
        PromptSelectionMove::Right => (focus < input_buffer.chars.len()).then_some(focus + 1),
        PromptSelectionMove::LineStart => (focus > 0).then_some(0),
        PromptSelectionMove::LineEnd => {
            (focus < input_buffer.chars.len()).then_some(input_buffer.chars.len())
        }
    }
}

fn move_prompt_selection(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    selection_move: PromptSelectionMove,
) -> bool {
    let anchor = interaction_state
        .prompt_selection
        .as_ref()
        .map(|selection| selection.anchor)
        .unwrap_or_else(|| input_buffer.cursor.min(input_buffer.chars.len()));
    let focus = interaction_state
        .prompt_selection
        .as_ref()
        .map(|selection| selection.focus)
        .unwrap_or(anchor);
    let Some(next_focus) = move_prompt_selection_focus(input_buffer, focus, selection_move) else {
        return false;
    };

    input_buffer.cursor = next_focus;
    if next_focus == anchor {
        interaction_state.prompt_selection = None;
    } else {
        interaction_state.prompt_selection = Some(PromptSelectionState {
            anchor,
            focus: next_focus,
        });
    }
    true
}

fn set_prompt_cursor(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    cursor: usize,
) -> bool {
    let next_cursor = cursor.min(input_buffer.chars.len());
    let changed =
        input_buffer.cursor != next_cursor || interaction_state.prompt_selection.is_some();
    input_buffer.cursor = next_cursor;
    interaction_state.prompt_selection = None;
    changed
}

fn insert_prompt_text(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    text: &str,
) -> bool {
    let deleted = delete_prompt_selection(interaction_state, input_buffer);
    let inserted = insert_buffer_text(input_buffer, text);
    deleted || inserted
}

fn set_prompt_selection(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
    anchor: usize,
    focus: usize,
) -> bool {
    let anchor = anchor.min(input_buffer.chars.len());
    let focus = focus.min(input_buffer.chars.len());
    let previous_cursor = input_buffer.cursor;
    let previous_selection = interaction_state.prompt_selection.clone();
    let had_message_actions = interaction_state.message_actions.take().is_some();
    let had_transcript_selection = interaction_state.transcript_selection.take().is_some();

    input_buffer.cursor = focus;
    interaction_state.prompt_selection =
        (anchor != focus).then_some(PromptSelectionState { anchor, focus });

    previous_cursor != input_buffer.cursor
        || previous_selection != interaction_state.prompt_selection
        || had_message_actions
        || had_transcript_selection
}

fn handle_prompt_mouse_action(
    mouse_kind: &MouseEventKind,
    cursor: usize,
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) -> bool {
    let cursor = cursor.min(input_buffer.chars.len());
    match mouse_kind {
        MouseEventKind::Down(MouseButton::Left) => {
            interaction_state.prompt_mouse_anchor = Some(cursor);
            let had_message_actions = interaction_state.message_actions.take().is_some();
            let had_transcript_selection = interaction_state.transcript_selection.take().is_some();
            had_message_actions
                || had_transcript_selection
                || set_prompt_cursor(interaction_state, input_buffer, cursor)
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let anchor = interaction_state
                .prompt_mouse_anchor
                .unwrap_or_else(|| input_buffer.cursor.min(input_buffer.chars.len()));
            interaction_state.prompt_mouse_anchor = Some(anchor);
            set_prompt_selection(interaction_state, input_buffer, anchor, cursor)
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(anchor) = interaction_state.prompt_mouse_anchor.take() else {
                return false;
            };
            set_prompt_selection(interaction_state, input_buffer, anchor, cursor)
        }
        _ => false,
    }
}

fn delete_prompt_selection(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut code_agent_ui::InputBuffer,
) -> bool {
    let Some((start, end)) = interaction_state
        .prompt_selection
        .as_ref()
        .and_then(|selection| normalize_prompt_selection(selection, input_buffer.chars.len()))
    else {
        return false;
    };

    input_buffer.chars.drain(start..end);
    input_buffer.cursor = start;
    interaction_state.prompt_selection = None;
    true
}

fn primary_input_keys(tool_name: &str) -> &'static [&'static str] {
    match tool_name {
        "Read" | "Edit" | "Write" | "NotebookEdit" | "file_read" | "file_edit" | "file_write"
        | "read_file" | "create_file" | "apply_patch" | "view_image" | "list_dir"
        | "edit_notebook_file" | "create_directory" => {
            &["file_path", "path", "filePath", "dirPath", "notebook_path"]
        }
        "Bash" | "bash" | "powershell" | "terminal_capture" | "run_in_terminal" => &["command"],
        "Grep" | "grep" | "grep_search" => &["pattern", "query"],
        "Glob" | "glob" | "file_search" => &["pattern", "query"],
        "WebFetch" | "fetch_webpage" => &["url", "query"],
        "WebSearch" | "semantic_search" => &["query"],
        "Task" | "Agent" | "runSubagent" => &["prompt", "description", "query"],
        _ => &[
            "command",
            "file_path",
            "path",
            "filePath",
            "dirPath",
            "query",
            "pattern",
            "url",
            "prompt",
            "title",
        ],
    }
}

fn primary_input_label(key: &str) -> &'static str {
    match key {
        "file_path" | "path" | "filePath" | "dirPath" | "notebook_path" => "path",
        "command" => "command",
        "query" => "query",
        "pattern" => "pattern",
        "url" => "url",
        "prompt" => "prompt",
        "description" => "description",
        "title" => "title",
        _ => "input",
    }
}

fn primary_input_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(primary_input_string)
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn tool_primary_input(call: &code_agent_core::ToolCall) -> Option<ToolPrimaryInput> {
    let payload = serde_json::from_str::<Value>(&call.input_json).ok()?;
    for key in primary_input_keys(&call.name) {
        if let Some(value) = payload.get(*key).and_then(primary_input_string) {
            return Some(ToolPrimaryInput {
                label: primary_input_label(key),
                value,
            });
        }
    }
    None
}

fn message_tool_call(message: &Message) -> Option<&code_agent_core::ToolCall> {
    message.blocks.iter().find_map(|block| match block {
        ContentBlock::ToolCall { call } => Some(call),
        _ => None,
    })
}

fn message_tool_result(message: &Message) -> Option<&code_agent_core::ToolResult> {
    message.blocks.iter().find_map(|block| match block {
        ContentBlock::ToolResult { result } => Some(result),
        _ => None,
    })
}

fn message_primary_input(message: &Message) -> Option<ToolPrimaryInput> {
    message_tool_call(message).and_then(tool_primary_input)
}

fn message_action_copy_text(message: &Message) -> Option<String> {
    let text = message_text(message);
    if !text.trim().is_empty() {
        return Some(text);
    }

    if let Some(primary_input) = message_primary_input(message) {
        return Some(primary_input.value);
    }

    let transcript_line = transcript_line_from_message(message);
    (!transcript_line.text.trim().is_empty()).then_some(transcript_line.text)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistoryTranscriptToolKind {
    Read,
    Search,
    List,
    Bash,
}

#[derive(Clone, Debug, Default)]
struct HistoryTranscriptPresentation {
    items: Vec<TranscriptItem>,
    action_items: Vec<ReplMessageActionItem>,
    group_ids: Vec<String>,
    item_count: usize,
}

#[derive(Clone, Debug)]
struct HistoryTranscriptGroupAccumulator {
    first_message: Message,
    messages: Vec<Message>,
    tracked_call_ids: BTreeSet<String>,
    read_count: usize,
    search_count: usize,
    list_count: usize,
    bash_count: usize,
    hints: Vec<String>,
}

impl HistoryTranscriptGroupAccumulator {
    fn new(
        message: &Message,
        call: &code_agent_core::ToolCall,
        kind: HistoryTranscriptToolKind,
    ) -> Self {
        let mut accumulator = Self {
            first_message: message.clone(),
            messages: Vec::new(),
            tracked_call_ids: BTreeSet::new(),
            read_count: 0,
            search_count: 0,
            list_count: 0,
            bash_count: 0,
            hints: Vec::new(),
        };
        accumulator.push_call(message, call, kind);
        accumulator
    }

    fn push_call(
        &mut self,
        message: &Message,
        call: &code_agent_core::ToolCall,
        kind: HistoryTranscriptToolKind,
    ) {
        self.messages.push(message.clone());
        self.tracked_call_ids.insert(call.id.clone());
        match kind {
            HistoryTranscriptToolKind::Read => self.read_count += 1,
            HistoryTranscriptToolKind::Search => self.search_count += 1,
            HistoryTranscriptToolKind::List => self.list_count += 1,
            HistoryTranscriptToolKind::Bash => self.bash_count += 1,
        }
        if let Some(hint) = history_transcript_hint(call) {
            self.hints.push(hint);
        }
    }

    fn push_result(&mut self, message: &Message) {
        self.messages.push(message.clone());
    }

    fn matches_result(&self, result: &code_agent_core::ToolResult) -> bool {
        self.tracked_call_ids.contains(&result.tool_call_id)
    }

    fn representative_message(&self) -> Message {
        self.first_message.clone()
    }

    fn into_group(self, expanded_history_groups: &BTreeSet<String>) -> TranscriptGroup {
        let group_id = history_transcript_group_id(&self.first_message);
        let mut unique_hints = Vec::new();
        for hint in self.hints.iter().rev() {
            if unique_hints
                .iter()
                .any(|existing: &String| existing == hint)
            {
                continue;
            }
            unique_hints.push(hint.clone());
            if unique_hints.len() == 2 {
                break;
            }
        }
        unique_hints.reverse();
        let subtitle = unique_hints.last().cloned();

        TranscriptGroup {
            id: group_id.clone(),
            title: history_transcript_group_title(&self),
            subtitle,
            expanded: expanded_history_groups.contains(&group_id),
            single_item: true,
            lines: history_transcript_detail_lines(&self.messages),
        }
    }
}

fn history_transcript_group_id(message: &Message) -> String {
    format!("{HISTORY_TRANSCRIPT_GROUP_PREFIX}{}", message.id)
}

fn is_history_transcript_group_id(group_id: &str) -> bool {
    group_id.starts_with(HISTORY_TRANSCRIPT_GROUP_PREFIX)
}

fn history_transcript_group_title(group: &HistoryTranscriptGroupAccumulator) -> String {
    let mut parts = Vec::new();

    if group.read_count > 0 {
        parts.push((
            "Read",
            "read",
            format!(
                "{} {}",
                group.read_count,
                if group.read_count == 1 {
                    "file"
                } else {
                    "files"
                }
            ),
        ));
    }
    if group.search_count > 0 {
        parts.push((
            "Searched",
            "searched",
            format!(
                "{} {}",
                group.search_count,
                if group.search_count == 1 {
                    "query"
                } else {
                    "queries"
                }
            ),
        ));
    }
    if group.list_count > 0 {
        parts.push((
            "Listed",
            "listed",
            format!(
                "{} {}",
                group.list_count,
                if group.list_count == 1 {
                    "directory"
                } else {
                    "directories"
                }
            ),
        ));
    }
    if group.bash_count > 0 {
        parts.push((
            "Ran",
            "ran",
            format!(
                "{} {}",
                group.bash_count,
                if group.bash_count == 1 {
                    "command"
                } else {
                    "commands"
                }
            ),
        ));
    }

    if parts.is_empty() {
        "Tool activity".to_owned()
    } else {
        parts
            .into_iter()
            .enumerate()
            .map(|(index, (leading, trailing, detail))| {
                format!("{} {detail}", if index == 0 { leading } else { trailing })
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn history_transcript_tool_kind(tool_name: &str) -> Option<HistoryTranscriptToolKind> {
    match tool_name {
        "Read" | "read" | "read_file" | "view_image" | "fetch_webpage" => {
            Some(HistoryTranscriptToolKind::Read)
        }
        "Grep" | "grep" | "grep_search" | "semantic_search" | "WebSearch" | "web_search"
        | "file_search" => Some(HistoryTranscriptToolKind::Search),
        "Glob" | "glob" | "list_dir" => Some(HistoryTranscriptToolKind::List),
        "Bash" | "bash" | "powershell" | "terminal_capture" | "run_in_terminal" => {
            Some(HistoryTranscriptToolKind::Bash)
        }
        _ => None,
    }
}

fn history_transcript_hint(call: &code_agent_core::ToolCall) -> Option<String> {
    let primary = tool_primary_input(call)?;
    let condensed = primary
        .value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if condensed.is_empty() {
        return None;
    }

    Some(preview_lines_from_text(condensed, 1, 56).join(" "))
}

fn history_preview_lines(text: &str, max_lines: usize, max_width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().count() <= max_width {
            lines.push(trimmed.to_owned());
        } else {
            let mut clipped = trimmed
                .chars()
                .take(max_width.saturating_sub(3))
                .collect::<String>();
            clipped.push_str("...");
            lines.push(clipped);
        }
        if lines.len() == max_lines {
            break;
        }
    }
    lines
}

fn history_tool_call_detail_text(call: &code_agent_core::ToolCall) -> String {
    let display_name = tool_display_name(&call.name);
    let Some(primary_input) = tool_primary_input(call) else {
        return display_name;
    };

    let compact = preview_lines_from_text(primary_input.value, 1, 72).join(" ");
    match primary_input.label {
        "query" | "pattern" => format!(r#"{display_name} ("{compact}")"#),
        _ => format!("{display_name} ({compact})"),
    }
}

fn history_tool_result_detail_lines(result: &code_agent_core::ToolResult) -> Vec<TranscriptLine> {
    let previews =
        history_preview_lines(&result.output_text, if result.is_error { 2 } else { 1 }, 76);
    let role = if result.is_error {
        "history_tool_error"
    } else {
        "history_tool_result"
    };

    previews
        .into_iter()
        .map(|text| TranscriptLine {
            role: role.to_owned(),
            text,
            author_label: None,
        })
        .collect()
}

fn history_transcript_detail_lines(messages: &[Message]) -> Vec<TranscriptLine> {
    let mut lines = Vec::new();

    for message in messages {
        if let Some(call) = message_tool_call(message) {
            lines.push(TranscriptLine {
                role: "history_tool_call".to_owned(),
                text: history_tool_call_detail_text(call),
                author_label: None,
            });
            continue;
        }

        if let Some(result) = message_tool_result(message) {
            lines.extend(history_tool_result_detail_lines(result));
        }
    }

    if lines.is_empty() {
        lines.push(TranscriptLine {
            role: "history_tool_result".to_owned(),
            text: "No additional details.".to_owned(),
            author_label: None,
        });
    }

    lines
}

fn push_history_transcript_line_item(
    presentation: &mut HistoryTranscriptPresentation,
    message: &Message,
) {
    let transcript_line = transcript_line_from_message(message);
    presentation
        .items
        .push(TranscriptItem::Line(transcript_line.clone()));
    if !transcript_line.text.trim().is_empty() {
        presentation.action_items.push(ReplMessageActionItem {
            item_index: presentation.item_count,
            message: message.clone(),
            history_group_id: None,
        });
    }
    presentation.item_count += 1;
}

fn push_history_transcript_group_item(
    presentation: &mut HistoryTranscriptPresentation,
    accumulator: HistoryTranscriptGroupAccumulator,
    expanded_history_groups: &BTreeSet<String>,
) {
    let representative_message = accumulator.representative_message();
    let group = accumulator.into_group(expanded_history_groups);
    presentation.group_ids.push(group.id.clone());
    let group_id = group.id.clone();
    presentation.items.push(TranscriptItem::Group(group));
    presentation.action_items.push(ReplMessageActionItem {
        item_index: presentation.item_count,
        message: representative_message,
        history_group_id: Some(group_id),
    });
    presentation.item_count += 1;
}

fn build_history_transcript_presentation(
    messages: &[Message],
    expanded_history_groups: &BTreeSet<String>,
) -> HistoryTranscriptPresentation {
    let mut presentation = HistoryTranscriptPresentation::default();
    let mut current_group: Option<HistoryTranscriptGroupAccumulator> = None;

    for message in messages {
        if let Some(call) = message_tool_call(message) {
            if let Some(kind) = history_transcript_tool_kind(&call.name) {
                match current_group.as_mut() {
                    Some(group) => group.push_call(message, call, kind),
                    None => {
                        current_group =
                            Some(HistoryTranscriptGroupAccumulator::new(message, call, kind));
                    }
                }
                continue;
            }
        }

        if let Some(result) = message_tool_result(message) {
            if current_group
                .as_ref()
                .is_some_and(|group| group.matches_result(result))
            {
                if let Some(group) = current_group.as_mut() {
                    group.push_result(message);
                }
                continue;
            }
        }

        if let Some(group) = current_group.take() {
            push_history_transcript_group_item(&mut presentation, group, expanded_history_groups);
        }
        push_history_transcript_line_item(&mut presentation, message);
    }

    if let Some(group) = current_group.take() {
        push_history_transcript_group_item(&mut presentation, group, expanded_history_groups);
    }

    presentation
}

fn history_transcript_group_ids(messages: &[Message]) -> Vec<String> {
    build_history_transcript_presentation(messages, &BTreeSet::new()).group_ids
}

fn message_actions_ui_state(
    interaction_state: &ReplInteractionState,
    items: &[ReplMessageActionItem],
) -> Option<TranscriptMessageActionsState> {
    let selected_item = interaction_state
        .message_actions
        .as_ref()
        .map(|actions| actions.selected_item)?;
    let item = items.iter().find(|item| item.item_index == selected_item)?;

    Some(TranscriptMessageActionsState {
        active_item: selected_item,
        enter_label: item
            .history_group_id
            .as_ref()
            .map(|group_id| {
                if interaction_state.expanded_history_groups.contains(group_id) {
                    "collapse".to_owned()
                } else {
                    "expand".to_owned()
                }
            })
            .or_else(|| (item.message.role == MessageRole::User).then(|| "edit".to_owned())),
        primary_input_label: message_primary_input(&item.message)
            .map(|input| input.label.to_owned()),
    })
}

