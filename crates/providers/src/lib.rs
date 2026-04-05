pub mod auth;
pub use auth::*;
pub mod http;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use code_agent_core::{ContentBlock, Message, MessageRole, TokenUsage, ToolCall};
use hmac::Hmac;
pub use http::*;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use time::macros::format_description;
use time::OffsetDateTime;
use tokio::time::sleep;

type HmacSha256 = Hmac<Sha256>;

const URI_COMPONENT_ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');
const OPENAI_AUTH_URL: &str = "https://auth.openai.com/oauth/token";
const CHATGPT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const OPENAI_RESPONSES_DEFAULT_MAX_RETRIES: usize = 10;
const OPENAI_RESPONSES_BASE_DELAY_MS: u64 = 500;
const OPENAI_RESPONSES_MAX_DELAY_MS: u64 = 32_000;
const OPENAI_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: u64 = 60;
const CODEX_AUTH_LOCK_RETRIES: usize = 5;
const CODEX_AUTH_LOCK_MIN_DELAY_MS: u64 = 50;
const CODEX_AUTH_LOCK_MAX_DELAY_MS: u64 = 250;

include!("providers/api.rs");
include!("providers/openai_events.rs");
include!("providers/auth_status.rs");
include!("providers/models.rs");

#[cfg(test)]
mod tests;
