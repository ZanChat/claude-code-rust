use super::{
    compatibility_command_registry, coordinator_tasks, create_agent_task, create_workflow_task_set,
    resume_tasks_for_question, AgentTaskRequest, CommandCategory, CommandSource, LocalTaskStore,
    PermissionMode, PermissionPolicy, ProviderSelection, QuestionRequest, RuntimeConfig,
    RuntimeMode, TaskRecord, TaskStatus, TaskStore, TurnRequest, WorkflowTaskRequest,
};
use std::path::PathBuf;

#[test]
fn resolves_aliases_and_parses_slash_commands() {
    let registry = compatibility_command_registry();
    let parsed = registry
        .parse_slash_command("/remote foo bar")
        .expect("slash command should resolve");

    assert_eq!(parsed.name, "remote-control");
    assert_eq!(parsed.args, vec!["foo".to_string(), "bar".to_string()]);
    assert_eq!(
        registry.resolve("plugin").map(|spec| &spec.category),
        Some(&CommandCategory::Tooling)
    );
    assert_eq!(
        registry.resolve("skills").map(|spec| spec.source.clone()),
        Some(CommandSource::BuiltIn)
    );
    assert!(registry.is_remote_safe("session"));
    assert!(registry.is_bridge_safe("compact"));
    assert!(!registry.is_bridge_safe("model"));
}

#[test]
fn keeps_runtime_requests_in_one_canonical_shape() {
    let request = TurnRequest {
        input: "/compact".to_owned(),
        command: compatibility_command_registry().parse_slash_command("/compact"),
        runtime: RuntimeConfig {
            cwd: PathBuf::from("/tmp/project"),
            mode: Some(RuntimeMode::Interactive),
            provider: Some(ProviderSelection {
                provider: "firstParty".to_owned(),
                model: Some("claude-sonnet-4-6".to_owned()),
            }),
            permission_policy: PermissionPolicy {
                mode: Some(PermissionMode::Ask),
                ..PermissionPolicy::default()
            },
            ..RuntimeConfig::default()
        },
    };

    assert_eq!(request.command.unwrap().name, "compact");
}

#[test]
fn creates_agent_and_workflow_tasks_via_shared_helpers() {
    let root = std::env::temp_dir().join(format!("code-agent-core-{}", uuid::Uuid::new_v4()));
    let store = LocalTaskStore::new(root);

    let agent = create_agent_task(
        &store,
        AgentTaskRequest {
            title: "review".to_owned(),
            prompt: Some("check the diff".to_owned()),
            ..AgentTaskRequest::default()
        },
    )
    .unwrap();
    let workflow = create_workflow_task_set(
        &store,
        WorkflowTaskRequest {
            title: "release".to_owned(),
            steps: vec!["build".to_owned(), "test".to_owned()],
            ..WorkflowTaskRequest::default()
        },
    )
    .unwrap();

    assert_eq!(agent.kind, "agent");
    assert_eq!(workflow.workflow.kind, "workflow");
    assert_eq!(workflow.children.len(), 2);
    assert!(workflow
        .children
        .iter()
        .all(|child| child.parent_task_id == Some(workflow.workflow.id)));
}

#[test]
fn resumes_waiting_tasks_for_answered_question() {
    let root = std::env::temp_dir().join(format!("code-agent-core-{}", uuid::Uuid::new_v4()));
    let store = LocalTaskStore::new(root);
    let question = store
        .record_question(QuestionRequest::new("approve?"))
        .unwrap();
    let mut task = TaskRecord::new("agent", "needs input");
    task.status = TaskStatus::WaitingForInput;
    task.question_id = Some(question.id);
    let created = store.create_task(task).unwrap();

    let resumed = resume_tasks_for_question(&store, question.id).unwrap();

    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].id, created.id);
    assert_eq!(resumed[0].status, TaskStatus::Running);
}

#[test]
fn splits_coordinator_directives_into_worker_tasks() {
    let tasks = coordinator_tasks("1. inspect auth\n2. check bridge state\n3. report blockers");
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks[0], "inspect auth");
    assert_eq!(tasks[1], "check bridge state");
}
