fn parse_task_status(input: &str) -> Result<TaskStatus> {
    match input.trim() {
        "pending" => Ok(TaskStatus::Pending),
        "running" => Ok(TaskStatus::Running),
        "waiting_for_input" => Ok(TaskStatus::WaitingForInput),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" => Ok(TaskStatus::Cancelled),
        other => bail!("unsupported task status: {other}"),
    }
}

fn list_task_records(context: &ToolContext) -> Result<Vec<TaskRecord>> {
    task_store(&context.cwd).list_tasks()
}

fn plan_mode_state_path(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("plan-mode.json")
}

fn plan_file_path(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("plan.md")
}

fn worktree_state_path(cwd: &Path) -> PathBuf {
    runtime_dir(cwd).join("worktree-session.json")
}

fn read_text_if_present(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn task_output_content(task: &TaskRecord) -> String {
    task.output
        .clone()
        .or_else(|| task.artifact_path.as_deref().and_then(read_text_if_present))
        .or_else(|| task.transcript_path.as_deref().and_then(read_text_if_present))
        .unwrap_or_default()
}

async fn wait_for_task(
    store: &LocalTaskStore,
    task_id: uuid::Uuid,
    timeout_ms: u64,
) -> Result<Option<TaskRecord>> {
    let start = std::time::Instant::now();
    loop {
        let task = store.get_task(task_id)?;
        let Some(task) = task else {
            return Ok(None);
        };
        if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            return Ok(Some(task));
        }
        if start.elapsed().as_millis() >= u128::from(timeout_ms) {
            return Ok(Some(task));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn slugify_worktree_name(value: &str) -> Result<String> {
    let slug = value.trim();
    if slug.is_empty() {
        bail!("worktree name cannot be empty");
    }
    if slug.len() > 64 {
        bail!("worktree name must be 64 characters or fewer");
    }
    if slug
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/'))
    {
        Ok(slug.to_owned())
    } else {
        bail!("worktree name may contain only letters, digits, dots, underscores, dashes, and '/'");
    }
}

async fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .kill_on_drop(true)
        .args(args)
        .current_dir(cwd)
        .output()
        .await
        .with_context(|| format!("failed to execute git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorktreeSessionState {
    original_cwd: PathBuf,
    repo_root: PathBuf,
    worktree_path: PathBuf,
    branch: String,
    original_head_commit: String,
}

#[derive(Clone, Debug)]
struct AgentTool;

#[async_trait]
impl Tool for AgentTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "agent",
            "Spawn or resume an agent task.",
            ToolKind::Agent,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let action = input_string_or(&input, "action", "spawn");
        let store = task_store(&context.cwd);
        match action.as_str() {
            "spawn" | "create" => {
                let created = create_agent_task(
                    &store,
                    AgentTaskRequest {
                        session_id: context.session_id,
                        title: input_string_or(&input, "title", "agent task"),
                        prompt: optional_string(&input, "prompt")
                            .or_else(|| optional_string(&input, "instruction")),
                        run_inline: input_bool_or(&input, "run_inline", false),
                        ..AgentTaskRequest::default()
                    },
                )?;
                Ok(ToolOutput {
                    content: format!("created agent task {}", created.id),
                    is_error: false,
                    metadata: serde_json::to_value(&created)?,
                })
            }
            "resume" | "get" => {
                let task_id = uuid::Uuid::parse_str(&input_string_any(&input, &["task_id", "taskId"])?)?;
                let task = store
                    .get_task(task_id)?
                    .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
                Ok(ToolOutput {
                    content: serde_json::to_string_pretty(&task)?,
                    is_error: false,
                    metadata: serde_json::to_value(&task)?,
                })
            }
            other => bail!("unsupported agent action: {other}"),
        }
    }
}

#[derive(Clone, Debug)]
struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_create",
            "Create a background task.",
            ToolKind::Task,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let mut task = TaskRecord::new(
            input_string_or(&input, "kind", "task"),
            input_string_or(&input, "title", "task"),
        );
        task.session_id = context.session_id;
        task.input = optional_string(&input, "input").or_else(|| optional_string(&input, "prompt"));
        task.metadata = string_map_field(&input, "metadata")?;
        let created = task_store(&context.cwd).create_task(task)?;
        Ok(ToolOutput {
            content: format!("created task {}", created.id),
            is_error: false,
            metadata: serde_json::to_value(&created)?,
        })
    }
}

#[derive(Clone, Debug)]
struct TaskGetTool;

#[async_trait]
impl Tool for TaskGetTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_get",
            "Inspect a single background task.",
            ToolKind::Task,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id = uuid::Uuid::parse_str(&input_string_any(&input, &["task_id", "taskId"])?)?;
        let task = task_store(&context.cwd)
            .get_task(task_id)?
            .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&task)?,
            is_error: false,
            metadata: serde_json::to_value(&task)?,
        })
    }
}

#[derive(Clone, Debug)]
struct TaskListTool;

#[async_trait]
impl Tool for TaskListTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_list",
            "List background tasks.",
            ToolKind::Task,
            true,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let status = optional_string(&input, "status");
        let tasks = list_task_records(context)?
            .into_iter()
            .filter(|task| {
                status
                    .as_ref()
                    .map(|expected| format!("{:?}", task.status).eq_ignore_ascii_case(expected))
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        Ok(ToolOutput {
            content: serde_json::to_string_pretty(&tasks)?,
            is_error: false,
            metadata: json!({ "count": tasks.len() }),
        })
    }
}

#[derive(Clone, Debug)]
struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_update",
            "Update task metadata or ownership.",
            ToolKind::Task,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id =
            uuid::Uuid::parse_str(&input_string_any(&input, &["task_id", "taskId", "shell_id"])?)?;
        let store = task_store(&context.cwd);
        let mut task = store
            .get_task(task_id)?
            .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        if let Some(title) = optional_string(&input, "title") {
            task.title = title;
        }
        if let Some(output) = optional_string(&input, "output") {
            task.output = Some(output);
        }
        if let Some(status) = optional_string(&input, "status") {
            task.status = parse_task_status(&status)?;
        }
        if let Some(metadata) = input.get("metadata") {
            if metadata.is_object() {
                task.metadata = string_map_field(&input, "metadata")?;
            }
        }
        let saved = store.save_task(task)?;
        Ok(ToolOutput {
            content: format!("updated task {}", saved.id),
            is_error: false,
            metadata: serde_json::to_value(&saved)?,
        })
    }
}

#[derive(Clone, Debug)]
struct TaskStopTool;

#[async_trait]
impl Tool for TaskStopTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "task_stop",
            "Stop a running task.",
            ToolKind::Task,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let task_id = uuid::Uuid::parse_str(&input_string_any(&input, &["task_id", "taskId"])?)?;
        let store = task_store(&context.cwd);
        let mut task = store
            .get_task(task_id)?
            .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        task.status = TaskStatus::Cancelled;
        task.output =
            Some(optional_string(&input, "reason").unwrap_or_else(|| "stopped by user".to_owned()));
        let saved = store.save_task(task)?;
        Ok(ToolOutput {
            content: format!("stopped task {}", saved.id),
            is_error: false,
            metadata: serde_json::to_value(&saved)?,
        })
    }
}

fn send_message_text(input: &Value) -> Result<String> {
    match input.get("message") {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(other) => Ok(serde_json::to_string(other)?),
        None => input_string(input, "message"),
    }
}

#[derive(Clone, Debug)]
struct SendMessageTool;

#[async_trait]
impl Tool for SendMessageTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "send_message",
            "Send a message to a user, teammate, or remote bridge.",
            ToolKind::Ui,
            false,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let text = send_message_text(&input)?;
        let value = json!({
            "session_id": context.session_id,
            "target": optional_string_any(&input, &["target", "to"])
                .unwrap_or_else(|| "local".to_owned()),
            "summary": optional_string(&input, "summary"),
            "message": text,
        });
        let path = runtime_dir(&context.cwd).join("messages.jsonl");
        append_jsonl(&path, &value)?;
        Ok(ToolOutput {
            content: value["message"].as_str().unwrap_or_default().to_owned(),
            is_error: false,
            metadata: json!({ "path": path, "entry": value }),
        })
    }
}

#[derive(Clone, Debug)]
struct AskUserQuestionTool;

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "ask_user_question",
            "Pause execution and request additional user input.",
            ToolKind::Ui,
            false,
            false,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let mut request = QuestionRequest::new(input_string(&input, "prompt")?);
        request.task_id = optional_string(&input, "task_id")
            .map(|value| uuid::Uuid::parse_str(&value))
            .transpose()?;
        request.choices = string_list_field(&input, "choices")?;
        request.context = string_map_field(&input, "context")?;
        let store = task_store(&context.cwd);
        let stored = store.record_question(request)?;
        if let Some(task_id) = stored.task_id {
            if let Some(mut task) = store.get_task(task_id)? {
                task.status = TaskStatus::WaitingForInput;
                task.question_id = Some(stored.id);
                let _ = store.save_task(task)?;
            }
        }
        Ok(ToolOutput {
            content: format!("question recorded {}", stored.id),
            is_error: false,
            metadata: serde_json::to_value(&stored)?,
        })
    }
}

#[derive(Clone, Debug)]
struct WorkflowTool;

#[async_trait]
impl Tool for WorkflowTool {
    fn spec(&self) -> ToolSpec {
        compatibility_tool(
            "workflow",
            "Start a workflow-oriented multi-step automation.",
            ToolKind::Agent,
            false,
            true,
        )
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let store = task_store(&context.cwd);
        let workflow = create_workflow_task_set(
            &store,
            WorkflowTaskRequest {
                session_id: context.session_id,
                title: input_string_or(&input, "title", "workflow"),
                prompt: optional_string(&input, "prompt")
                    .or_else(|| optional_string(&input, "description")),
                steps: string_list_field(&input, "steps")?,
                ..WorkflowTaskRequest::default()
            },
        )?;

        Ok(ToolOutput {
            content: format!("created workflow {}", workflow.workflow.id),
            is_error: false,
            metadata: json!({
                "workflow": workflow.workflow,
                "child_task_ids": workflow.children.iter().map(|child| child.id).collect::<Vec<_>>()
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct AgentCompatTool;

#[async_trait]
impl Tool for AgentCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Agent".to_owned(),
            description: "Spawn or resume an agent task.".to_owned(),
            kind: ToolKind::Agent,
            input_schema: schemars::schema_for!(AgentCompatInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let AgentCompatInput {
            action,
            description,
            title,
            prompt,
            instruction,
            task_id,
            run_inline,
            run_in_background,
            name,
            subagent_type,
            model,
            team_name,
            mode,
            isolation,
            cwd,
        } = parse_tool_input(input)?;
        let prompt = prompt.or(instruction).or_else(|| description.clone());
        let title = title
            .or(description)
            .or(name)
            .unwrap_or_else(|| "agent task".to_owned());
        let action = action.unwrap_or_else(|| {
            if task_id.is_some() {
                "resume".to_owned()
            } else {
                "spawn".to_owned()
            }
        });
        invoke_tool_alias(
            "agent",
            json!({
                "action": action,
                "taskId": task_id,
                "title": title,
                "prompt": prompt,
                "run_inline": if run_in_background { false } else { run_inline },
                "subagent_type": subagent_type,
                "model": model,
                "team_name": team_name,
                "mode": mode,
                "isolation": isolation,
                "cwd": cwd,
            }),
            context,
        )
        .await
    }
}

#[derive(Clone, Debug)]
struct TaskStopCompatTool;

#[async_trait]
impl Tool for TaskStopCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskStop".to_owned(),
            description: "Stop a running task.".to_owned(),
            kind: ToolKind::Task,
            input_schema: schemars::schema_for!(TaskStopToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        invoke_tool_alias("task_stop", input, context).await
    }
}

#[derive(Clone, Debug)]
struct SendMessageCompatTool;

#[async_trait]
impl Tool for SendMessageCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "SendMessage".to_owned(),
            description: "Send a message to a user, teammate, or remote bridge.".to_owned(),
            kind: ToolKind::Ui,
            input_schema: schemars::schema_for!(SendMessageCompatInput),
            read_only: false,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let SendMessageCompatInput { to, summary, message } = parse_tool_input(input)?;
        invoke_tool_alias(
            "send_message",
            json!({
                "target": to,
                "summary": summary,
                "message": message,
            }),
            context,
        )
        .await
    }
}

#[derive(Clone, Debug)]
struct AskUserQuestionCompatTool;

#[async_trait]
impl Tool for AskUserQuestionCompatTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "AskUserQuestion".to_owned(),
            description: "Pause execution and request additional user input.".to_owned(),
            kind: ToolKind::Ui,
            input_schema: schemars::schema_for!(AskUserQuestionCompatInput),
            read_only: false,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let AskUserQuestionCompatInput {
            questions,
            prompt,
            choices,
            context: question_context,
            task_id,
            answers,
            annotations,
            metadata,
        } = parse_tool_input(input)?;
        let prompt = prompt
            .or_else(|| {
                questions.as_ref().map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.question.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            })
            .ok_or_else(|| anyhow!("AskUserQuestion requires 'prompt' or 'questions'"))?;
        let choices = if choices.is_empty() {
            questions
                .as_ref()
                .and_then(|entries| entries.first())
                .map(|entry| {
                    entry
                        .options
                        .iter()
                        .map(|option| option.label.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        } else {
            choices
        };
        let mut request = QuestionRequest::new(prompt);
        request.task_id = task_id
            .as_deref()
            .map(uuid::Uuid::parse_str)
            .transpose()?;
        request.choices = choices;
        request.context = question_context;
        let store = task_store(&context.cwd);
        let stored = store.record_question(request)?;
        if let Some(task_id) = stored.task_id {
            if let Some(mut task) = store.get_task(task_id)? {
                task.status = TaskStatus::WaitingForInput;
                task.question_id = Some(stored.id);
                let _ = store.save_task(task)?;
            }
        }
        Ok(ToolOutput {
            content: format!("question recorded {}", stored.id),
            is_error: false,
            metadata: json!({
                "question": stored,
                "questions": questions,
                "answers": answers.unwrap_or_default(),
                "annotations": annotations,
                "metadata": metadata,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct TaskOutputTool;

#[async_trait]
impl Tool for TaskOutputTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskOutput".to_owned(),
            description: "Read output from a running or completed background task.".to_owned(),
            kind: ToolKind::Task,
            input_schema: schemars::schema_for!(TaskOutputToolInput),
            read_only: true,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let TaskOutputToolInput {
            task_id,
            block,
            timeout,
        } = parse_tool_input(input)?;
        let task_id = uuid::Uuid::parse_str(&task_id)?;
        let store = task_store(&context.cwd);
        let task = if block {
            wait_for_task(&store, task_id, timeout).await?
        } else {
            store.get_task(task_id)?
        }
        .ok_or_else(|| anyhow!("unknown task id: {task_id}"))?;
        let retrieval_status = if matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            if block { "timeout" } else { "not_ready" }
        } else {
            "success"
        };
        let output = task_output_content(&task);
        Ok(ToolOutput {
            content: if output.is_empty() {
                serde_json::to_string_pretty(&task)?
            } else {
                output.clone()
            },
            is_error: matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled),
            metadata: json!({
                "retrieval_status": retrieval_status,
                "task": task,
                "output": output,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct EnterPlanModeTool;

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "EnterPlanMode".to_owned(),
            description: "Record that the session has entered plan mode.".to_owned(),
            kind: ToolKind::Session,
            input_schema: schemars::schema_for!(EmptyToolInput),
            read_only: false,
            needs_permission: false,
        }
    }

    async fn invoke(&self, _input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let path = plan_mode_state_path(&context.cwd);
        ensure_parent_dir(&path)?;
        fs::write(
            &path,
            serde_json::to_vec_pretty(&json!({
                "active": true,
                "cwd": context.cwd,
            }))?,
        )
        .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(ToolOutput {
            content: "Entered plan mode. Focus on exploration and planning until ExitPlanMode is called.".to_owned(),
            is_error: false,
            metadata: json!({ "path": path }),
        })
    }
}

#[derive(Clone, Debug)]
struct ExitPlanModeTool;

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ExitPlanMode".to_owned(),
            description: "Persist the current plan and mark plan mode as exited.".to_owned(),
            kind: ToolKind::Session,
            input_schema: schemars::schema_for!(ExitPlanModeToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let ExitPlanModeToolInput {
            plan,
            plan_file_path: requested_plan_file_path,
        } = parse_tool_input(input)?;
        let plan_path = requested_plan_file_path
            .as_deref()
            .map(|path| resolve_path(&context.cwd, path))
            .unwrap_or_else(|| plan_file_path(&context.cwd));
        let state_path = plan_mode_state_path(&context.cwd);
        if let Some(plan) = plan.filter(|value| !value.trim().is_empty()) {
            ensure_parent_dir(&plan_path)?;
            fs::write(&plan_path, plan.as_bytes())
                .with_context(|| format!("failed to write {}", plan_path.display()))?;
        }
        ensure_parent_dir(&state_path)?;
        fs::write(
            &state_path,
            serde_json::to_vec_pretty(&json!({
                "active": false,
                "cwd": context.cwd,
                "plan_path": plan_path,
            }))?,
        )
        .with_context(|| format!("failed to write {}", state_path.display()))?;
        Ok(ToolOutput {
            content: format!(
                "Exited plan mode. Current plan is recorded at {}.",
                plan_path.display()
            ),
            is_error: false,
            metadata: json!({
                "path": state_path,
                "plan_path": plan_path,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct SendUserMessageTool;

#[async_trait]
impl Tool for SendUserMessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "SendUserMessage".to_owned(),
            description: "Write a user-visible checkpoint message.".to_owned(),
            kind: ToolKind::Ui,
            input_schema: schemars::schema_for!(SendUserMessageToolInput),
            read_only: false,
            needs_permission: false,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let SendUserMessageToolInput {
            message,
            title,
            attachments,
            status,
        } = parse_tool_input(input)?;
        let value = json!({
            "session_id": context.session_id,
            "target": "user",
            "title": title,
            "message": message,
            "attachments": attachments,
            "status": status,
        });
        let path = runtime_dir(&context.cwd).join("messages.jsonl");
        append_jsonl(&path, &value)?;
        Ok(ToolOutput {
            content: value["message"].as_str().unwrap_or_default().to_owned(),
            is_error: false,
            metadata: json!({ "path": path, "entry": value }),
        })
    }
}

#[derive(Clone, Debug)]
struct EnterWorktreeTool;

#[async_trait]
impl Tool for EnterWorktreeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "EnterWorktree".to_owned(),
            description: "Create a git worktree and record it for the current session.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(EnterWorktreeToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let EnterWorktreeToolInput { name } = parse_tool_input(input)?;
        let state_path = worktree_state_path(&context.cwd);
        if state_path.exists() {
            bail!("Already in a worktree session");
        }
        let slug = name
            .as_deref()
            .map(slugify_worktree_name)
            .transpose()?
            .unwrap_or_else(|| format!("ccrust-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let repo_root = PathBuf::from(git_stdout(&context.cwd, &["rev-parse", "--show-toplevel"]).await?);
        let repo_name = repo_root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("worktree");
        let branch = format!("ccrust/{}", slug.replace('/', "-"));
        let worktree_path = repo_root
            .parent()
            .unwrap_or(&context.cwd)
            .join(format!("{repo_name}-{}", slug.replace('/', "-")));
        let worktree_path_string = worktree_path.to_string_lossy().into_owned();
        let original_head_commit = git_stdout(&repo_root, &["rev-parse", "HEAD"]).await?;
        git_stdout(
            &repo_root,
            &["worktree", "add", "-b", &branch, &worktree_path_string, "HEAD"],
        )
        .await?;
        let state = WorktreeSessionState {
            original_cwd: context.cwd.clone(),
            repo_root,
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            original_head_commit,
        };
        ensure_parent_dir(&state_path)?;
        fs::write(&state_path, serde_json::to_vec_pretty(&state)?)
            .with_context(|| format!("failed to write {}", state_path.display()))?;
        Ok(ToolOutput {
            content: format!(
                "Created worktree at {} on branch {}. The Rust compatibility runtime records the worktree, but the session cwd does not change automatically.",
                worktree_path.display(),
                branch
            ),
            is_error: false,
            metadata: json!({
                "worktreePath": worktree_path,
                "worktreeBranch": branch,
                "statePath": state_path,
            }),
        })
    }
}

#[derive(Clone, Debug)]
struct ExitWorktreeTool;

#[async_trait]
impl Tool for ExitWorktreeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ExitWorktree".to_owned(),
            description: "Keep or remove the recorded worktree for the current session.".to_owned(),
            kind: ToolKind::FileSystem,
            input_schema: schemars::schema_for!(ExitWorktreeToolInput),
            read_only: false,
            needs_permission: true,
        }
    }

    async fn invoke(&self, input: Value, context: &ToolContext) -> Result<ToolOutput> {
        let ExitWorktreeToolInput {
            action,
            discard_changes,
        } = parse_tool_input(input)?;
        let state_path = worktree_state_path(&context.cwd);
        if !state_path.exists() {
            return Ok(ToolOutput {
                content: "No-op: there is no active EnterWorktree session to exit. No filesystem changes were made.".to_owned(),
                is_error: false,
                metadata: json!({
                    "action": action,
                    "active": false,
                }),
            });
        }
        let raw = fs::read_to_string(&state_path)
            .with_context(|| format!("failed to read {}", state_path.display()))?;
        let state: WorktreeSessionState = serde_json::from_str(&raw)
            .with_context(|| format!("failed to decode {}", state_path.display()))?;
        match action.as_str() {
            "keep" => {
                fs::remove_file(&state_path)
                    .with_context(|| format!("failed to remove {}", state_path.display()))?;
                Ok(ToolOutput {
                    content: format!(
                        "Kept worktree {} on branch {}. Session worktree state was cleared.",
                        state.worktree_path.display(),
                        state.branch
                    ),
                    is_error: false,
                    metadata: json!({
                        "action": "keep",
                        "worktreePath": state.worktree_path,
                        "worktreeBranch": state.branch,
                    }),
                })
            }
            "remove" => {
                let status = git_stdout(&state.worktree_path, &["status", "--porcelain"]).await?;
                let commits_ahead = git_stdout(
                    &state.worktree_path,
                    &["rev-list", "--count", &format!("{}..HEAD", state.original_head_commit)],
                )
                .await?
                .parse::<u64>()
                .unwrap_or_default();
                if (!status.trim().is_empty() || commits_ahead > 0) && !discard_changes {
                    bail!(
                        "worktree has uncommitted changes or {} commits ahead; rerun with discard_changes=true to remove it",
                        commits_ahead
                    );
                }
                let worktree_path_string = state.worktree_path.to_string_lossy().into_owned();
                git_stdout(
                    &state.repo_root,
                    &["worktree", "remove", "--force", &worktree_path_string],
                )
                .await?;
                let _ = git_stdout(&state.repo_root, &["branch", "-D", &state.branch]).await;
                fs::remove_file(&state_path)
                    .with_context(|| format!("failed to remove {}", state_path.display()))?;
                Ok(ToolOutput {
                    content: format!(
                        "Removed worktree {} and deleted branch {}.",
                        state.worktree_path.display(),
                        state.branch
                    ),
                    is_error: false,
                    metadata: json!({
                        "action": "remove",
                        "worktreePath": state.worktree_path,
                        "worktreeBranch": state.branch,
                    }),
                })
            }
            other => bail!("unsupported ExitWorktree action: {other}"),
        }
    }
}

pub fn compatibility_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(FileReadTool);
    registry.register(ReadTool);
    registry.register(FileWriteTool);
    registry.register(WriteTool);
    registry.register(FileEditTool);
    registry.register(EditTool);
    registry.register(NotebookEditTool);
    registry.register(GlobTool);
    registry.register(GlobCompatTool);
    registry.register(GrepTool);
    registry.register(GrepCompatTool);
    registry.register(BashTool);
    registry.register(BashCompatTool);
    registry.register(PowerShellTool);
    registry.register(TerminalCaptureTool);
    registry.register(WebFetchTool);
    registry.register(WebFetchCompatTool);
    registry.register(WebSearchTool);
    registry.register(WebSearchCompatTool);
    registry.register(WebBrowserTool);
    registry.register(McpTool);
    registry.register(ListMcpResourcesTool);
    registry.register(ListMcpResourcesCompatTool);
    registry.register(ReadMcpResourceTool);
    registry.register(ReadMcpResourceCompatTool);
    registry.register(McpAuthTool);
    registry.register(AgentTool);
    registry.register(AgentCompatTool);
    registry.register(TaskCreateTool);
    registry.register(TaskGetTool);
    registry.register(TaskListTool);
    registry.register(TaskUpdateTool);
    registry.register(TaskStopTool);
    registry.register(TaskStopCompatTool);
    registry.register(TaskOutputTool);
    registry.register(TodoWriteTool);
    registry.register(TodoWriteCompatTool);
    registry.register(MemoryTool);
    registry.register(SendMessageTool);
    registry.register(SendMessageCompatTool);
    registry.register(SendUserMessageTool);
    registry.register(AskUserQuestionTool);
    registry.register(AskUserQuestionCompatTool);
    registry.register(EnterPlanModeTool);
    registry.register(ExitPlanModeTool);
    registry.register(EnterWorktreeTool);
    registry.register(ExitWorktreeTool);
    registry.register(SkillTool);
    registry.register(ToolSearchTool);
    registry.register(WorkflowTool);

    registry
}
