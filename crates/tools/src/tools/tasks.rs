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
                let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
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
        let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
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
        let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
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
        let task_id = uuid::Uuid::parse_str(&input_string(&input, "task_id")?)?;
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
        let text = input_string(&input, "message")?;
        let value = json!({
            "session_id": context.session_id,
            "target": optional_string(&input, "target").unwrap_or_else(|| "local".to_owned()),
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

pub fn compatibility_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(FileReadTool);
    registry.register(FileWriteTool);
    registry.register(FileEditTool);
    registry.register(GlobTool);
    registry.register(GrepTool);
    registry.register(BashTool);
    registry.register(PowerShellTool);
    registry.register(TerminalCaptureTool);
    registry.register(WebFetchTool);
    registry.register(WebSearchTool);
    registry.register(WebBrowserTool);
    registry.register(McpTool);
    registry.register(ListMcpResourcesTool);
    registry.register(ReadMcpResourceTool);
    registry.register(McpAuthTool);
    registry.register(AgentTool);
    registry.register(TaskCreateTool);
    registry.register(TaskGetTool);
    registry.register(TaskListTool);
    registry.register(TaskUpdateTool);
    registry.register(TaskStopTool);
    registry.register(TodoWriteTool);
    registry.register(MemoryTool);
    registry.register(SendMessageTool);
    registry.register(AskUserQuestionTool);
    registry.register(WorkflowTool);

    registry
}

