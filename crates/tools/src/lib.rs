use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use code_agent_core::{
    create_agent_task, create_workflow_task_set, AgentTaskRequest, LocalTaskStore, QuestionRequest,
    SessionId, TaskRecord, TaskStatus, TaskStore, WorkflowTaskRequest,
};
use code_agent_mcp::{
    call_tool_from_config, clear_cached_auth_token, clear_pending_device_flow,
    list_resources_from_config, load_cached_auth_token, load_pending_device_flow,
    parse_mcp_server_configs, poll_oauth_device_flow, read_resource_from_config,
    refresh_oauth_device_token, start_oauth_device_flow, store_cached_auth_token,
    CachedMcpAuthToken, McpAuthConfig, McpServerConfig,
};
use code_agent_plugins::{OutOfProcessPluginRuntime, PluginRuntime};
use reqwest::Method;
use schemars::schema::RootSchema;
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;

include!("tools/registry.rs");
include!("tools/helpers.rs");
include!("tools/fs_shell.rs");
include!("tools/web.rs");
include!("tools/search.rs");
include!("tools/mcp.rs");
include!("tools/tasks.rs");

#[cfg(test)]
mod tests;
