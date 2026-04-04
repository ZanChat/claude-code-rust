use crate::{resume_command_for_session, resume_picker_item};
use code_agent_session::SessionStore;
use code_agent_session::{LocalSessionStore, ProjectSessionStore, SessionSummary};
use uuid::Uuid;

use code_agent_core::{Message, SessionId};

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::cli_args::Cli;

use code_agent_ui::ChoiceListState;

#[derive(Clone, Debug)]
pub(crate) struct ResumeTargetHint {
    pub(crate) session_id: SessionId,
    pub(crate) transcript_path: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ReplSessionState {
    pub(crate) session_id: SessionId,
    pub(crate) transcript_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResumePickerState {
    pub(crate) sessions: Vec<SessionSummary>,
    pub(crate) selected: usize,
}

pub(crate) enum ActiveSessionStore {
    Local(LocalSessionStore),
    Project(ProjectSessionStore),
}

impl ActiveSessionStore {
    pub(crate) fn new(cwd: PathBuf, session_root: Option<PathBuf>) -> Self {
        match session_root {
            Some(root) => Self::Local(LocalSessionStore::new(root)),
            None => Self::Project(ProjectSessionStore::new(cwd)),
        }
    }

    pub(crate) fn root_dir(&self) -> &Path {
        match self {
            Self::Local(store) => store.root_dir(),
            Self::Project(store) => store.storage_dir(),
        }
    }

    pub(crate) async fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        match self {
            Self::Local(store) => store.list_sessions().await,
            Self::Project(store) => store.list_sessions().await,
        }
    }

    pub(crate) async fn transcript_path(&self, session_id: SessionId) -> Result<PathBuf> {
        Ok(match self {
            Self::Local(store) => store.transcript_path_for_session(session_id),
            Self::Project(store) => store.transcript_path_for_session(session_id),
        })
    }

    pub(crate) async fn load_resume_target(
        &self,
        value: &str,
    ) -> Result<(SessionId, PathBuf, Vec<Message>)> {
        match self {
            Self::Local(store) => store.load_resume_target(value).await,
            Self::Project(store) => store.load_resume_target(value).await,
        }
    }

    pub(crate) async fn append_message(
        &self,
        session_id: SessionId,
        message: &Message,
    ) -> Result<()> {
        match self {
            Self::Local(store) => store.append_message(session_id, message).await,
            Self::Project(store) => store.append_message(session_id, message).await,
        }
    }

    pub(crate) async fn load_session(&self, session_id: SessionId) -> Result<Vec<Message>> {
        match self {
            Self::Local(store) => store.load_session(session_id).await,
            Self::Project(store) => store.load_session(session_id).await,
        }
    }
}

pub(crate) fn choose_active_session(
    _cli: &Cli,
    explicit_resume: Option<(SessionId, PathBuf, Vec<Message>)>,
) -> Result<(SessionId, Option<PathBuf>, Vec<Message>)> {
    if let Some((session_id, path, messages)) = explicit_resume {
        return Ok((session_id, Some(path), messages));
    }

    Ok((Uuid::new_v4(), None, Vec::new()))
}

pub(crate) fn resumable_sessions(
    sessions: Vec<SessionSummary>,
    current_session_id: SessionId,
) -> Vec<SessionSummary> {
    sessions
        .into_iter()
        .filter(|summary| summary.session_id != current_session_id)
        .collect()
}

pub(crate) fn build_resume_choice_list(picker: &ResumePickerState) -> ChoiceListState {
    ChoiceListState {
        title: "Resume conversation".to_owned(),
        subtitle: Some("Enter to select · Esc to cancel".to_owned()),
        items: picker.sessions.iter().map(resume_picker_item).collect(),
        selected: picker.selected,
        empty_message: Some("No conversations found to resume.".to_owned()),
    }
}

pub(crate) fn resume_hint_text(resume_hint: &ResumeTargetHint) -> Option<String> {
    if !resume_hint.transcript_path.exists() {
        return None;
    }
    Some(format!(
        "\nResume this session with:\n{}\n",
        resume_command_for_session(resume_hint.session_id)
    ))
}

pub(crate) fn print_resume_hint(resume_hint: &ResumeTargetHint) {
    if let Some(hint) = resume_hint_text(resume_hint) {
        print!("{hint}");
    }
}
