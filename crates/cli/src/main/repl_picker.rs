#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplTranscriptSearchState {
    input_buffer: ccrust_ui::InputBuffer,
    open: bool,
    active_item: Option<usize>,
    saved_input_buffer: ccrust_ui::InputBuffer,
    saved_active_item: Option<usize>,
    anchor_scroll: u16,
}

impl ReplTranscriptSearchState {
    fn ui_state(&self) -> TranscriptSearchState {
        TranscriptSearchState {
            input_buffer: self.input_buffer.clone(),
            open: self.open,
            active_item: self.active_item,
        }
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplPromptHistorySearchState {
    input_buffer: ccrust_ui::InputBuffer,
    original_input_buffer: ccrust_ui::InputBuffer,
    active_history_index: Option<usize>,
    active_match_position: Option<usize>,
    match_count: usize,
    failed_match: bool,
    last_query: String,
}

impl ReplPromptHistorySearchState {
    fn ui_state(&self) -> PromptHistorySearchState {
        PromptHistorySearchState {
            input_buffer: self.input_buffer.clone(),
            active_match: self.active_match_position.map(|position| position + 1),
            match_count: self.match_count,
            failed_match: self.failed_match,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplMessageActionState {
    selected_item: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplFilePickerState {
    indexed_root: Option<PathBuf>,
    indexed_paths: Vec<String>,
    truncated: bool,
    selected: usize,
    last_query: Option<String>,
    hidden_query: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplIdePickerState {
    candidates: Vec<DetectedIdeCandidate>,
    selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ReplCommandPickerAction {
    Status {
        status: String,
        banner: Option<String>,
    },
    PrefillInput {
        input: String,
        status: String,
        banner: Option<String>,
    },
    QueueInput {
        input: String,
        status: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplCommandPickerEntry {
    item: ChoiceListItem,
    action: ReplCommandPickerAction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplCommandPickerState {
    title: String,
    subtitle: Option<String>,
    items: Vec<ReplCommandPickerEntry>,
    selected: usize,
    empty_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplFilePickerToken {
    start: usize,
    end: usize,
    query: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReplFilePickerMatchSet {
    token: ReplFilePickerToken,
    paths: Vec<String>,
}

#[derive(Clone, Debug)]
struct ReplMessageActionItem {
    item_index: usize,
    message: Message,
    history_group_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplMessageActionNavigation {
    Prev,
    Next,
    PrevUser,
    NextUser,
    Top,
    Bottom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ToolPrimaryInput {
    label: &'static str,
    value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ReplInteractionState {
    transcript_mode: bool,
    expanded_history_groups: BTreeSet<String>,
    transcript_search: ReplTranscriptSearchState,
    prompt_history_search: Option<ReplPromptHistorySearchState>,
    message_actions: Option<ReplMessageActionState>,
    prompt_selection: Option<PromptSelectionState>,
    prompt_mouse_anchor: Option<usize>,
    transcript_selection: Option<TranscriptSelectionState>,
    file_picker: ReplFilePickerState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptSelectionMove {
    Left,
    Right,
    LineStart,
    LineEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplShortcutAction {
    CopySelection,
    ContextCtrlC,
    ToggleTranscriptMode,
    ToggleTranscriptDetails,
    PromptHistorySearch,
    EnterMessageActions,
    SelectPane(PaneKind),
    RotatePaneForward,
    RotatePaneBackward,
}

fn is_selection_copy_shortcut(key: &KeyEvent) -> bool {
    key_matches_char_with_modifiers(key, 'c', KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        || key_matches_char_with_modifiers(key, 'c', KeyModifiers::SUPER)
}

fn repl_shortcut_action_for_key(
    key: &KeyEvent,
    interaction_state: &ReplInteractionState,
) -> Option<ReplShortcutAction> {
    if (interaction_state.transcript_selection.is_some()
        || interaction_state.prompt_selection.is_some())
        && is_selection_copy_shortcut(key)
    {
        return Some(ReplShortcutAction::CopySelection);
    }

    if is_plain_ctrl_char(key, 'c') {
        return Some(ReplShortcutAction::ContextCtrlC);
    }

    if is_plain_ctrl_char(key, 'o') {
        return Some(ReplShortcutAction::ToggleTranscriptMode);
    }

    if !interaction_state.transcript_mode
        && interaction_state.message_actions.is_none()
        && interaction_state.transcript_selection.is_none()
        && is_plain_ctrl_char(key, 'r')
    {
        return Some(ReplShortcutAction::PromptHistorySearch);
    }

    if is_plain_ctrl_char(key, 'e') {
        return Some(ReplShortcutAction::ToggleTranscriptDetails);
    }

    if matches!(key.code, KeyCode::Up)
        && key.modifiers == KeyModifiers::SHIFT
        && interaction_state.message_actions.is_none()
        && interaction_state.prompt_history_search.is_none()
        && interaction_state.transcript_selection.is_none()
        && !interaction_state.transcript_search.open
    {
        return Some(ReplShortcutAction::EnterMessageActions);
    }

    if interaction_state.transcript_mode {
        return None;
    }

    if let Some(pane) = pane_from_shortcut(key) {
        return Some(ReplShortcutAction::SelectPane(pane));
    }

    match key.code {
        KeyCode::Tab => Some(ReplShortcutAction::RotatePaneForward),
        KeyCode::BackTab => Some(ReplShortcutAction::RotatePaneBackward),
        _ => None,
    }
}

fn enter_transcript_mode(interaction_state: &mut ReplInteractionState, active_pane: &mut PaneKind) {
    interaction_state.transcript_mode = true;
    interaction_state.prompt_history_search = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    *active_pane = PaneKind::Transcript;
}

fn exit_transcript_mode(interaction_state: &mut ReplInteractionState) {
    interaction_state.transcript_mode = false;
    interaction_state.transcript_search.reset();
    interaction_state.prompt_history_search = None;
    interaction_state.message_actions = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    interaction_state.transcript_selection = None;
}

fn toggle_history_transcript_group(interaction_state: &mut ReplInteractionState, group_id: &str) {
    if interaction_state.expanded_history_groups.contains(group_id) {
        interaction_state.expanded_history_groups.remove(group_id);
    } else {
        interaction_state
            .expanded_history_groups
            .insert(group_id.to_owned());
    }
}

fn toggle_all_history_transcript_groups(
    interaction_state: &mut ReplInteractionState,
    group_ids: &[String],
) -> bool {
    if group_ids.is_empty() {
        return false;
    }

    let all_expanded = group_ids
        .iter()
        .all(|group_id| interaction_state.expanded_history_groups.contains(group_id));

    if all_expanded {
        for group_id in group_ids {
            interaction_state.expanded_history_groups.remove(group_id);
        }
    } else {
        interaction_state
            .expanded_history_groups
            .extend(group_ids.iter().cloned());
    }

    true
}

fn should_show_prompt_file_picker(interaction_state: &ReplInteractionState) -> bool {
    !interaction_state.transcript_mode
        && interaction_state.prompt_history_search.is_none()
        && interaction_state.message_actions.is_none()
        && interaction_state.transcript_selection.is_none()
        && !interaction_state.transcript_search.open
}

fn should_skip_file_picker_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".jj"
            | ".next"
            | ".nuxt"
            | ".svn"
            | ".turbo"
            | ".yarn"
            | "build"
            | "coverage"
            | "dist"
            | "node_modules"
            | "target"
    )
}

fn normalize_file_picker_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn active_prompt_file_picker_token(
    input_buffer: &ccrust_ui::InputBuffer,
) -> Option<ReplFilePickerToken> {
    let chars = &input_buffer.chars;
    let cursor = input_buffer.cursor.min(chars.len());

    if chars.is_empty() || cursor > chars.len() {
        return None;
    }

    let mut start = cursor;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }

    let mut end = cursor;
    while end < chars.len() && !chars[end].is_whitespace() {
        end += 1;
    }

    if start >= chars.len() || chars[start] != '@' {
        return None;
    }

    let query = chars[start + 1..cursor].iter().collect::<String>();
    if query.contains(':') {
        return None;
    }

    Some(ReplFilePickerToken {
        start,
        end,
        query: query
            .trim_matches('"')
            .trim_start_matches("./")
            .replace('\\', "/"),
    })
}

fn repl_ide_picker_state_with_home(
    cwd: &Path,
    connected_ide: Option<&DetectedIdeCandidate>,
    home_override: Option<&Path>,
) -> ReplIdePickerState {
    let candidates = detect_workspace_ides(cwd, home_override);
    let selected = connected_ide
        .and_then(|connected| {
            candidates
                .iter()
                .position(|candidate| candidate.suggested_bridge == connected.suggested_bridge)
        })
        .unwrap_or(0);

    ReplIdePickerState {
        candidates,
        selected,
    }
}

fn repl_ide_picker_state(
    cwd: &Path,
    connected_ide: Option<&DetectedIdeCandidate>,
) -> ReplIdePickerState {
    repl_ide_picker_state_with_home(cwd, connected_ide, None)
}

fn build_ide_choice_list(
    picker: &ReplIdePickerState,
    connected_ide: Option<&DetectedIdeCandidate>,
) -> ChoiceListState {
    let subtitle = connected_ide
        .map(|candidate| {
            format!(
                "Enter to connect · Esc to cancel · current: {}",
                candidate.name
            )
        })
        .unwrap_or_else(|| "Enter to connect · Esc to cancel".to_owned());

    ChoiceListState {
        title: "IDE bridge".to_owned(),
        subtitle: Some(subtitle),
        items: picker
            .candidates
            .iter()
            .map(|candidate| {
                let connected = connected_ide.is_some_and(|current| {
                    current.suggested_bridge == candidate.suggested_bridge
                });
                let detail = if connected {
                    format!("{} · connected", candidate.suggested_bridge)
                } else {
                    candidate.suggested_bridge.clone()
                };

                ChoiceListItem {
                    label: candidate.name.clone(),
                    detail: Some(detail),
                    secondary: candidate.workspace_folders.first().map(|folder| {
                        preview_lines_from_text(folder.clone(), 1, 72).join(" ")
                    }),
                }
            })
            .collect(),
        selected: picker.selected.min(picker.candidates.len().saturating_sub(1)),
        empty_message: Some(
            "No IDE bridge detected for this workspace. Start a supported IDE with the Claude extension first."
                .to_owned(),
        ),
    }
}

fn build_command_choice_list(picker: &ReplCommandPickerState) -> ChoiceListState {
    ChoiceListState {
        title: picker.title.clone(),
        subtitle: picker.subtitle.clone(),
        items: picker.items.iter().map(|entry| entry.item.clone()).collect(),
        selected: picker.selected.min(picker.items.len().saturating_sub(1)),
        empty_message: picker.empty_message.clone(),
    }
}

fn repl_theme_picker_state() -> ReplCommandPickerState {
    let rust_theme_note =
        "Rust UI currently follows your terminal colors instead of persisting TS theme presets."
            .to_owned();
    let items = vec![
        (
            "Auto (match terminal)",
            "Follow your terminal palette automatically.",
        ),
        ("Dark mode", "Use the default dark Claude Code palette."),
        ("Light mode", "Use the default light Claude Code palette."),
        (
            "Dark mode (colorblind-friendly)",
            "Dark palette adjusted for colorblind accessibility.",
        ),
        (
            "Light mode (colorblind-friendly)",
            "Light palette adjusted for colorblind accessibility.",
        ),
        (
            "Dark mode (ANSI colors only)",
            "Dark palette restricted to ANSI-safe colors.",
        ),
        (
            "Light mode (ANSI colors only)",
            "Light palette restricted to ANSI-safe colors.",
        ),
    ]
    .into_iter()
    .map(|(label, detail)| ReplCommandPickerEntry {
        item: ChoiceListItem {
            label: label.to_owned(),
            detail: Some(detail.to_owned()),
            secondary: Some(rust_theme_note.clone()),
        },
        action: ReplCommandPickerAction::Status {
            status: format!("{label} is not persisted yet; the Rust UI keeps your terminal theme."),
            banner: Some(rust_theme_note.clone()),
        },
    })
    .collect();

    ReplCommandPickerState {
        title: "Theme".to_owned(),
        subtitle: Some("Enter to inspect parity notes · Esc to close".to_owned()),
        items,
        selected: 0,
        empty_message: Some("No theme presets available.".to_owned()),
    }
}

fn skill_entry_source_label(entry: &ccrust_plugins::SkillEntry, cwd: &Path) -> String {
    match entry.source {
        ccrust_plugins::SkillSource::Manifest => "plugin".to_owned(),
        ccrust_plugins::SkillSource::LegacyCommandsDir => {
            if entry.path.starts_with(claude_config_home_dir()) {
                "user command".to_owned()
            } else if entry.path.starts_with(cwd) {
                "project command".to_owned()
            } else {
                "command".to_owned()
            }
        }
        ccrust_plugins::SkillSource::LegacySkillsDir => {
            if entry.path.starts_with(claude_config_home_dir()) {
                "user skill".to_owned()
            } else if entry.path.starts_with(cwd) {
                "project skill".to_owned()
            } else {
                "skill".to_owned()
            }
        }
    }
}

async fn repl_skills_picker_state(
    cwd: &Path,
    plugin_root: Option<&PathBuf>,
) -> Result<ReplCommandPickerState> {
    let skills = resolved_skill_entries(cwd, plugin_root).await?;
    let items = skills
        .into_iter()
        .map(|entry| {
            let source = skill_entry_source_label(&entry, cwd);
            let path = preview_lines_from_text(entry.path.display().to_string(), 1, 72).join(" ");
            ReplCommandPickerEntry {
                item: ChoiceListItem {
                    label: format!("/{}", entry.name),
                    detail: Some(source.clone()),
                    secondary: Some(path),
                },
                action: ReplCommandPickerAction::PrefillInput {
                    input: format!("/{} ", entry.name),
                    status: format!("Inserted /{}", entry.name),
                    banner: None,
                },
            }
        })
        .collect();

    Ok(ReplCommandPickerState {
        title: "Skills".to_owned(),
        subtitle: Some("Enter to insert · Esc to close".to_owned()),
        items,
        selected: 0,
        empty_message: Some(
            "No skills found. Create skills in .claude/skills/ or ~/.claude/skills/."
                .to_owned(),
        ),
    })
}

fn is_agent_task_kind(kind: &str) -> bool {
    matches!(
        kind,
        "agent"
            | "workflow"
            | "workflow_step"
            | "coordinator"
            | "assistant_worker"
            | "assistant_synthesis"
    )
}

fn short_task_id(task_id: uuid::Uuid) -> String {
    task_id
        .to_string()
        .split('-')
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn task_status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::WaitingForInput => "waiting",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
    }
}

fn repl_agents_picker_state(cwd: &Path) -> Result<ReplCommandPickerState> {
    let mut tasks = task_store_for(cwd)
        .list_tasks()?
        .into_iter()
        .filter(|task| is_agent_task_kind(task.kind.as_str()))
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        right
            .updated_at_unix_ms
            .cmp(&left.updated_at_unix_ms)
            .then_with(|| left.title.cmp(&right.title))
    });

    let mut items = vec![ReplCommandPickerEntry {
        item: ChoiceListItem {
            label: "Create new agent".to_owned(),
            detail: Some("Start a delegated task".to_owned()),
            secondary: Some("Prefill /agents create".to_owned()),
        },
        action: ReplCommandPickerAction::PrefillInput {
            input: "/agents create ".to_owned(),
            status: "Enter a title for the new agent task".to_owned(),
            banner: None,
        },
    }];

    items.extend(tasks.into_iter().map(|task| ReplCommandPickerEntry {
        item: ChoiceListItem {
            label: if task.title.trim().is_empty() {
                format!("agent {}", short_task_id(task.id))
            } else {
                task.title.clone()
            },
            detail: Some(format!(
                "{} · {} · {}",
                task_status_label(task.status.clone()),
                task.kind,
                short_task_id(task.id)
            )),
            secondary: task
                .output
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(|value| preview_lines_from_text(value.to_owned(), 1, 72).join(" "))
                .or_else(|| {
                    task.transcript_path.as_ref().map(|path| {
                        preview_lines_from_text(path.display().to_string(), 1, 72).join(" ")
                    })
                }),
        },
        action: ReplCommandPickerAction::QueueInput {
            input: format!("/agents get {}", task.id),
            status: format!("Loading agent {}", short_task_id(task.id)),
        },
    }));

    Ok(ReplCommandPickerState {
        title: "Agents".to_owned(),
        subtitle: Some("Enter to inspect · Esc to close".to_owned()),
        items,
        selected: 0,
        empty_message: Some("No agents found for this workspace yet.".to_owned()),
    })
}

fn hook_picker_entries(root: &Path, hooks: &Value) -> Vec<ReplCommandPickerEntry> {
    match hooks {
        Value::String(path) => vec![ReplCommandPickerEntry {
            item: ChoiceListItem {
                label: path.trim_start_matches("./").to_owned(),
                detail: Some("Hook config".to_owned()),
                secondary: Some(root.join(path.trim_start_matches("./")).display().to_string()),
            },
            action: ReplCommandPickerAction::Status {
                status: format!("Hook config: {}", path.trim_start_matches("./")),
                banner: None,
            },
        }],
        Value::Array(items) => items
            .iter()
            .enumerate()
            .flat_map(|(index, entry)| match entry {
                Value::String(path) => vec![ReplCommandPickerEntry {
                    item: ChoiceListItem {
                        label: path.trim_start_matches("./").to_owned(),
                        detail: Some(format!("Hook config #{}", index + 1)),
                        secondary: Some(
                            root.join(path.trim_start_matches("./")).display().to_string(),
                        ),
                    },
                    action: ReplCommandPickerAction::Status {
                        status: format!("Hook config: {}", path.trim_start_matches("./")),
                        banner: None,
                    },
                }],
                Value::Object(map) => vec![ReplCommandPickerEntry {
                    item: ChoiceListItem {
                        label: format!("Inline hook config #{}", index + 1),
                        detail: Some(format!("{} top-level key(s)", map.len())),
                        secondary: Some(map.keys().cloned().collect::<Vec<_>>().join(", ")),
                    },
                    action: ReplCommandPickerAction::Status {
                        status: format!("Inline hook config #{}", index + 1),
                        banner: None,
                    },
                }],
                other => vec![ReplCommandPickerEntry {
                    item: ChoiceListItem {
                        label: format!("Hook entry #{}", index + 1),
                        detail: Some("Unsupported hook entry shape".to_owned()),
                        secondary: Some(preview_lines_from_text(other.to_string(), 1, 72).join(" ")),
                    },
                    action: ReplCommandPickerAction::Status {
                        status: format!("Hook entry #{}", index + 1),
                        banner: None,
                    },
                }],
            })
            .collect(),
        Value::Object(map) => vec![ReplCommandPickerEntry {
            item: ChoiceListItem {
                label: "Inline hook config".to_owned(),
                detail: Some(format!("{} top-level key(s)", map.len())),
                secondary: Some(map.keys().cloned().collect::<Vec<_>>().join(", ")),
            },
            action: ReplCommandPickerAction::Status {
                status: "Inline hook configuration loaded".to_owned(),
                banner: None,
            },
        }],
        _ => Vec::new(),
    }
}

fn repl_hooks_picker_state(cwd: &Path, plugin_root: Option<&PathBuf>) -> ReplCommandPickerState {
    let root = resolve_plugin_root_with_override(plugin_root, None, cwd);
    let items = load_plugin_manifest_sync(&root)
        .and_then(|manifest| manifest.hooks)
        .map(|hooks| hook_picker_entries(&root, &hooks))
        .unwrap_or_default();

    ReplCommandPickerState {
        title: "Hooks".to_owned(),
        subtitle: Some("Read-only hook configuration overview · Esc to close".to_owned()),
        items,
        selected: 0,
        empty_message: Some("No hook configuration found for this workspace.".to_owned()),
    }
}

fn rebuild_prompt_file_picker_index(cwd: &Path, file_picker: &mut ReplFilePickerState) {
    let root = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    if file_picker.indexed_root.as_ref() == Some(&root) {
        return;
    }

    let mut indexed_paths = Vec::new();
    let mut stack = vec![root.clone()];
    let mut truncated = false;

    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };

        for entry_result in entries {
            if indexed_paths.len() >= FILE_PICKER_MAX_INDEXED_FILES {
                truncated = true;
                break;
            }

            let Ok(entry) = entry_result else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();

            if file_type.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if should_skip_file_picker_dir(&name) {
                    continue;
                }
                stack.push(path);
                continue;
            }

            if !file_type.is_file() {
                continue;
            }

            let relative = path.strip_prefix(&root).unwrap_or(&path);
            let display = normalize_file_picker_path(relative);
            if !display.is_empty() {
                indexed_paths.push(display);
            }
        }

        if truncated {
            break;
        }
    }

    indexed_paths.sort();
    file_picker.indexed_root = Some(root);
    file_picker.indexed_paths = indexed_paths;
    file_picker.truncated = truncated;
}

fn sync_prompt_file_picker_state(
    cwd: &Path,
    input_buffer: &ccrust_ui::InputBuffer,
    interaction_state: &mut ReplInteractionState,
) {
    if !should_show_prompt_file_picker(interaction_state) {
        interaction_state.file_picker.last_query = None;
        interaction_state.file_picker.hidden_query = None;
        interaction_state.file_picker.selected = 0;
        return;
    }

    let Some(token) = active_prompt_file_picker_token(input_buffer) else {
        interaction_state.file_picker.last_query = None;
        interaction_state.file_picker.hidden_query = None;
        interaction_state.file_picker.selected = 0;
        return;
    };

    rebuild_prompt_file_picker_index(cwd, &mut interaction_state.file_picker);

    if interaction_state.file_picker.last_query.as_deref() != Some(token.query.as_str()) {
        interaction_state.file_picker.selected = 0;
        if interaction_state.file_picker.hidden_query.as_deref() != Some(token.query.as_str()) {
            interaction_state.file_picker.hidden_query = None;
        }
        interaction_state.file_picker.last_query = Some(token.query);
    }
}

fn file_picker_match_rank(path: &str, query: &str) -> Option<(u8, usize, usize)> {
    if query.is_empty() {
        return Some((0, path.chars().count(), path.matches('/').count()));
    }

    let normalized_path = path.to_lowercase();
    let normalized_query = query.to_lowercase();
    let file_name = normalized_path
        .rsplit('/')
        .next()
        .unwrap_or(normalized_path.as_str());

    if file_name == normalized_query {
        return Some((
            0,
            file_name.chars().count(),
            normalized_path.matches('/').count(),
        ));
    }
    if file_name.starts_with(&normalized_query) {
        return Some((
            1,
            file_name.chars().count(),
            normalized_path.matches('/').count(),
        ));
    }
    if normalized_path.starts_with(&normalized_query) {
        return Some((
            2,
            normalized_path.chars().count(),
            normalized_path.matches('/').count(),
        ));
    }
    if normalized_path
        .split('/')
        .any(|segment| segment.starts_with(&normalized_query))
    {
        return Some((
            3,
            normalized_path.chars().count(),
            normalized_path.matches('/').count(),
        ));
    }
    if normalized_path.contains(&normalized_query) {
        return Some((
            4,
            normalized_path.chars().count(),
            normalized_path.matches('/').count(),
        ));
    }

    None
}

fn prompt_file_picker_matches(
    cwd: &Path,
    input_buffer: &ccrust_ui::InputBuffer,
    interaction_state: &mut ReplInteractionState,
) -> Option<ReplFilePickerMatchSet> {
    sync_prompt_file_picker_state(cwd, input_buffer, interaction_state);
    if !should_show_prompt_file_picker(interaction_state) {
        return None;
    }

    let token = active_prompt_file_picker_token(input_buffer)?;
    if interaction_state.file_picker.hidden_query.as_deref() == Some(token.query.as_str()) {
        return None;
    }

    let mut ranked = interaction_state
        .file_picker
        .indexed_paths
        .iter()
        .filter_map(|path| {
            file_picker_match_rank(path, &token.query).map(|rank| (rank, path.clone()))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_rank, left_path), (right_rank, right_path)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| left_path.cmp(right_path))
    });

    Some(ReplFilePickerMatchSet {
        token,
        paths: ranked
            .into_iter()
            .take(FILE_PICKER_MAX_RESULTS)
            .map(|(_, path)| path)
            .collect(),
    })
}

fn prompt_file_picker_choice_list(
    cwd: &Path,
    input_buffer: &ccrust_ui::InputBuffer,
    interaction_state: &mut ReplInteractionState,
) -> Option<ChoiceListState> {
    let matches = prompt_file_picker_matches(cwd, input_buffer, interaction_state)?;
    let selected = interaction_state
        .file_picker
        .selected
        .min(matches.paths.len().saturating_sub(1));
    let mut subtitle = "Type to filter · Enter/Tab to insert · Esc to close".to_owned();
    if interaction_state.file_picker.truncated {
        subtitle.push_str(" · indexed first 5000 files");
    }

    Some(ChoiceListState {
        title: "File picker".to_owned(),
        subtitle: Some(subtitle),
        items: matches
            .paths
            .iter()
            .map(|path| ChoiceListItem {
                label: path.clone(),
                detail: Some(format!("Insert @{path}")),
                secondary: None,
            })
            .collect(),
        selected,
        empty_message: Some(if matches.token.query.is_empty() {
            "No files indexed for this workspace.".to_owned()
        } else {
            format!("No files match @{}.", matches.token.query)
        }),
    })
}

fn active_repl_choice_list(
    cwd: &Path,
    input_buffer: &ccrust_ui::InputBuffer,
    explicit_choice_list: Option<ChoiceListState>,
    interaction_state: &mut ReplInteractionState,
) -> Option<ChoiceListState> {
    explicit_choice_list
        .or_else(|| prompt_file_picker_choice_list(cwd, input_buffer, interaction_state))
}

fn handle_prompt_file_picker_key(
    cwd: &Path,
    key: &KeyEvent,
    input_buffer: &mut ccrust_ui::InputBuffer,
    interaction_state: &mut ReplInteractionState,
) -> bool {
    let Some(matches) = prompt_file_picker_matches(cwd, input_buffer, interaction_state) else {
        return false;
    };

    let max_index = matches.paths.len().saturating_sub(1);
    interaction_state.file_picker.selected = interaction_state.file_picker.selected.min(max_index);

    match key.code {
        KeyCode::Esc => {
            interaction_state.file_picker.hidden_query = Some(matches.token.query);
            true
        }
        KeyCode::Up => {
            interaction_state.file_picker.selected =
                interaction_state.file_picker.selected.saturating_sub(1);
            true
        }
        KeyCode::Down => {
            if interaction_state.file_picker.selected < max_index {
                interaction_state.file_picker.selected += 1;
            }
            true
        }
        KeyCode::PageUp => {
            interaction_state.file_picker.selected =
                interaction_state.file_picker.selected.saturating_sub(5);
            true
        }
        KeyCode::PageDown => {
            interaction_state.file_picker.selected =
                (interaction_state.file_picker.selected + 5).min(max_index);
            true
        }
        KeyCode::Home => {
            interaction_state.file_picker.selected = 0;
            true
        }
        KeyCode::End => {
            interaction_state.file_picker.selected = max_index;
            true
        }
        KeyCode::Enter | KeyCode::Tab => {
            let Some(selected_path) = matches
                .paths
                .get(interaction_state.file_picker.selected)
                .or_else(|| matches.paths.first())
            else {
                return false;
            };

            let replacement = format!("@{selected_path} ");
            input_buffer
                .chars
                .splice(matches.token.start..matches.token.end, replacement.chars());
            input_buffer.cursor = matches.token.start + replacement.chars().count();
            interaction_state.prompt_selection = None;
            interaction_state.file_picker.selected = 0;
            interaction_state.file_picker.last_query = None;
            interaction_state.file_picker.hidden_query = None;
            true
        }
        _ => false,
    }
}

fn open_prompt_history_search(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &ccrust_ui::InputBuffer,
) {
    let search_state = interaction_state
        .prompt_history_search
        .get_or_insert_with(|| ReplPromptHistorySearchState {
            original_input_buffer: input_buffer.clone(),
            ..ReplPromptHistorySearchState::default()
        });
    search_state.input_buffer.cursor = search_state.input_buffer.chars.len();
    interaction_state.message_actions = None;
    interaction_state.prompt_selection = None;
    interaction_state.prompt_mouse_anchor = None;
    interaction_state.transcript_selection = None;
}

fn cancel_prompt_history_search(
    interaction_state: &mut ReplInteractionState,
    input_buffer: &mut ccrust_ui::InputBuffer,
) -> bool {
    let Some(search_state) = interaction_state.prompt_history_search.take() else {
        return false;
    };
    *input_buffer = search_state.original_input_buffer;
    true
}

fn accept_prompt_history_search(interaction_state: &mut ReplInteractionState) -> bool {
    interaction_state.prompt_history_search.take().is_some()
}

fn prompt_history_match_cursor(entry: &str, query: &str) -> Option<usize> {
    let byte_index = entry.rfind(query)?;
    Some(entry[..byte_index].chars().count())
}

fn preview_prompt_history_match(
    history: &[String],
    history_index: usize,
    query: &str,
    input_buffer: &mut ccrust_ui::InputBuffer,
) {
    let Some(entry) = history.get(history_index) else {
        return;
    };

    input_buffer.replace(entry.clone());
    if let Some(cursor) = prompt_history_match_cursor(entry, query) {
        input_buffer.cursor = cursor.min(input_buffer.chars.len());
    }
}

fn sync_prompt_history_search_preview(
    history: &[String],
    search_state: &mut ReplPromptHistorySearchState,
    input_buffer: &mut ccrust_ui::InputBuffer,
) {
    let query = search_state.input_buffer.as_str();
    if query.is_empty() {
        *input_buffer = search_state.original_input_buffer.clone();
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.match_count = 0;
        search_state.failed_match = false;
        search_state.last_query.clear();
        return;
    }

    if search_state.last_query != query {
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.failed_match = false;
    }

    let matches = prompt_history_search_matches(history, &query);
    search_state.match_count = matches.len();
    search_state.last_query = query.clone();

    let Some(&history_index) = matches.first() else {
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.failed_match = true;
        return;
    };

    search_state.active_history_index = Some(history_index);
    search_state.active_match_position = Some(0);
    search_state.failed_match = false;
    preview_prompt_history_match(history, history_index, &query, input_buffer);
}

fn step_prompt_history_search_match(
    history: &[String],
    search_state: &mut ReplPromptHistorySearchState,
    input_buffer: &mut ccrust_ui::InputBuffer,
) -> bool {
    let query = search_state.input_buffer.as_str();
    if query.is_empty() {
        *input_buffer = search_state.original_input_buffer.clone();
        search_state.active_history_index = None;
        search_state.active_match_position = None;
        search_state.match_count = 0;
        search_state.failed_match = false;
        search_state.last_query.clear();
        return false;
    }

    let matches = prompt_history_search_matches(history, &query);
    search_state.match_count = matches.len();
    search_state.last_query = query.clone();

    let Some(current_position) = search_state.active_history_index.and_then(|current_index| {
        matches
            .iter()
            .position(|candidate| *candidate == current_index)
    }) else {
        return if let Some(&history_index) = matches.first() {
            search_state.active_history_index = Some(history_index);
            search_state.active_match_position = Some(0);
            search_state.failed_match = false;
            preview_prompt_history_match(history, history_index, &query, input_buffer);
            true
        } else {
            search_state.active_history_index = None;
            search_state.active_match_position = None;
            search_state.failed_match = true;
            false
        };
    };

    let Some(&history_index) = matches.get(current_position + 1) else {
        search_state.active_match_position = Some(current_position);
        search_state.failed_match = true;
        return false;
    };

    search_state.active_history_index = Some(history_index);
    search_state.active_match_position = Some(current_position + 1);
    search_state.failed_match = false;
    preview_prompt_history_match(history, history_index, &query, input_buffer);
    true
}

