use super::{
    agent_transcript_path_for, compact_messages, estimate_message_tokens,
    extract_first_prompt_from_head, extract_json_string_field, extract_last_json_string_field,
    get_project_dir, list_sessions_in_dir, materialize_runtime_messages, sanitize_path,
    summarize_transcript_path, transcript_path_for, CompactionConfig, JsonlTranscriptCodec,
    LocalSessionStore, TranscriptCodec,
};
use code_agent_core::{BoundaryKind, ContentBlock, Message, MessageRole};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

fn with_env_var(key: &str, value: Option<&str>, f: impl FnOnce()) {
    let previous = env::var(key).ok();
    match value {
        Some(value) => env::set_var(key, value),
        None => env::remove_var(key),
    }
    f();
    match previous {
        Some(value) => env::set_var(key, value),
        None => env::remove_var(key),
    }
}

#[test]
fn sanitizes_and_hashes_long_paths() {
    let input = format!("/tmp/{}", "very-long-segment/".repeat(30));
    let sanitized = sanitize_path(&input);

    assert!(sanitized.len() > 200);
    assert!(sanitized.starts_with("-tmp-very-long-segment-"));
    assert!(sanitized
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
}

#[test]
fn resolves_project_and_agent_transcript_paths() {
    with_env_var("CLAUDE_CONFIG_DIR", Some("/tmp/claude-home"), || {
        let project = Path::new("/Users/example/worktree/project");
        let session_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let agent_id = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();

        assert_eq!(
            get_project_dir(project),
            PathBuf::from("/tmp/claude-home/projects/-Users-example-worktree-project")
        );
        assert_eq!(
                transcript_path_for(project, session_id),
                PathBuf::from(
                    "/tmp/claude-home/projects/-Users-example-worktree-project/11111111-1111-4111-8111-111111111111.jsonl"
                )
            );
        assert_eq!(
                agent_transcript_path_for(project, session_id, agent_id, Some("workflows/run-1")),
                PathBuf::from(
                    "/tmp/claude-home/projects/-Users-example-worktree-project/11111111-1111-4111-8111-111111111111/subagents/workflows/run-1/agent-22222222-2222-4222-8222-222222222222.jsonl"
                )
            );
    });
}

#[test]
fn extracts_json_string_fields_without_full_parse() {
    let text = r#"{"title":"first","title":"second","escaped":"say \"hello\""}"#;

    assert_eq!(
        extract_json_string_field(text, "escaped"),
        Some("say \"hello\"".to_owned())
    );
    assert_eq!(
        extract_last_json_string_field(text, "title"),
        Some("second".to_owned())
    );
}

#[test]
fn extracts_first_prompt_and_skips_metadata() {
    let head = concat!(
        "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>compact</command-name>\"}}\n",
        "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"ignored\"}}\n",
        "{\"type\":\"user\",\"message\":{\"content\":\"<bash-input>ls -la</bash-input>\"}}\n"
    );

    assert_eq!(extract_first_prompt_from_head(head), "! ls -la");
}

#[test]
fn falls_back_to_command_name_when_no_prompt_survives() {
    let head = concat!(
        "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>resume</command-name>\"}}\n",
        "{\"type\":\"user\",\"message\":{\"content\":\"<tool-output>ignored</tool-output>\"}}\n"
    );

    assert_eq!(extract_first_prompt_from_head(head), "resume");
}

fn make_temp_dir(label: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = env::temp_dir().join(format!("code-agent-session-{label}-{stamp}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn summarizes_and_lists_transcripts() {
    let dir = make_temp_dir("summary");
    let session_id = Uuid::parse_str("33333333-3333-4333-8333-333333333333").unwrap();
    let path = dir.join(format!("{session_id}.jsonl"));
    let mut message = Message::new(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "Summarize this session".to_owned(),
        }],
    );
    message.session_id = Some(session_id);

    JsonlTranscriptCodec
        .append_message(&path, &message)
        .await
        .unwrap();

    let summary = summarize_transcript_path(&path).unwrap().unwrap();
    assert_eq!(summary.session_id, session_id);
    assert_eq!(summary.message_count, 1);

    let sessions = list_sessions_in_dir(&dir).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].first_prompt, "Summarize this session");
}

#[test]
fn estimates_tokens_and_materializes_latest_compaction() {
    let session_id = Uuid::parse_str("44444444-4444-4444-8444-444444444444").unwrap();
    let mut messages = Vec::new();
    for index in 0..8 {
        let mut message = Message::new(
                if index % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                },
                vec![ContentBlock::Text {
                    text: format!(
                        "Message {index} contains enough repeated detail to make compaction worthwhile."
                    ),
                }],
            );
        message.session_id = Some(session_id);
        messages.push(message);
    }
    let outcome = compact_messages(
        &messages,
        &CompactionConfig {
            target_tokens_after: estimate_message_tokens(&messages) / 2,
            min_preserved_messages: 1,
            ..CompactionConfig::default()
        },
    )
    .unwrap();

    let mut transcript = messages.clone();
    transcript.push(outcome.summary_message.clone());
    transcript.push(outcome.boundary_message.clone());
    let runtime_messages = materialize_runtime_messages(&transcript);

    assert!(estimate_message_tokens(&runtime_messages) > 0);
    assert_eq!(
        runtime_messages.first().unwrap().id,
        outcome.summary_message.id
    );
    assert_eq!(
        runtime_messages.last().unwrap().id,
        messages.last().unwrap().id
    );
}

#[test]
fn skips_compaction_when_summary_would_not_help() {
    let session_id = Uuid::parse_str("66666666-6666-4666-8666-666666666666").unwrap();
    let mut first = Message::new(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "short prompt".to_owned(),
        }],
    );
    first.session_id = Some(session_id);
    let mut second = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::Text {
            text: "short reply".to_owned(),
        }],
    );
    second.session_id = Some(session_id);

    let outcome = compact_messages(
        &[first, second],
        &CompactionConfig {
            target_tokens_after: 1,
            min_preserved_messages: 1,
            ..CompactionConfig::default()
        },
    );

    assert!(outcome.is_none());
}

#[tokio::test]
async fn imports_fixture_transcript_into_session_root() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let fixture = workspace.join("fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl");
    let root = make_temp_dir("fixture-import");
    let imported = super::import_transcript_to_session_root(&JsonlTranscriptCodec, &fixture, &root)
        .await
        .unwrap();

    assert_eq!(
        imported.session_id.to_string(),
        "77777777-7777-4777-8777-777777777777"
    );
    assert!(imported.destination_path.exists());
    assert_eq!(imported.message_count, 6);
}

#[tokio::test]
async fn loads_fixture_transcript_and_resumes_by_jsonl_path() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let fixture = workspace.join("fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl");
    let messages = JsonlTranscriptCodec.read_messages(&fixture).await.unwrap();
    let runtime = materialize_runtime_messages(&messages);
    let root = make_temp_dir("fixture-resume");
    let store = LocalSessionStore::new(root);
    let (session_id, path, resumed) = store
        .load_resume_target(fixture.to_str().unwrap())
        .await
        .unwrap();

    assert_eq!(path, fixture);
    assert_eq!(
        session_id.to_string(),
        "77777777-7777-4777-8777-777777777777"
    );
    assert_eq!(runtime.len(), 3);
    assert!(runtime[0]
        .metadata
        .tags
        .contains(&"compact_summary".to_owned()));
    assert_eq!(resumed.len(), messages.len());
}

#[test]
fn compaction_reduces_runtime_size() {
    let session_id = Uuid::parse_str("55555555-5555-4555-8555-555555555555").unwrap();
    let mut messages = Vec::new();
    for index in 0..10 {
        let mut user = Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: format!(
                    "User message {index} with enough text to count toward the estimate."
                ),
            }],
        );
        user.session_id = Some(session_id);
        messages.push(user);

        let mut assistant = Message::new(
            MessageRole::Assistant,
            vec![ContentBlock::Text {
                text: format!("Assistant reply {index} with a matching amount of detail."),
            }],
        );
        assistant.session_id = Some(session_id);
        messages.push(assistant);
    }

    let before = estimate_message_tokens(&messages);
    let outcome = compact_messages(
        &messages,
        &CompactionConfig {
            kind: BoundaryKind::SessionMemory,
            trigger: "auto".to_owned(),
            target_tokens_after: before / 3,
            min_preserved_messages: 4,
            summary_line_limit: 6,
            max_tokens_before: Some(before),
        },
    )
    .unwrap();

    assert_eq!(outcome.boundary_message.blocks.len(), 1);
    assert!(outcome
        .summary_message
        .metadata
        .tags
        .contains(&"compact_summary".to_owned()));
    assert!(outcome.estimated_tokens_after < before);
    assert_eq!(
        outcome.runtime_messages.first().unwrap().id,
        outcome.summary_message.id
    );
}
