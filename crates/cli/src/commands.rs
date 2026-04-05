use super::*;

use anyhow::Context;
use std::collections::VecDeque;
use std::fs;
use std::io::{stdout, Write as _};
use std::process::{Command as StdCommand, Stdio};
use std::time::SystemTime;
use uuid::Uuid;

include!("commands/clipboard.rs");
include!("commands/renderers.rs");
include!("commands/repl_submit.rs");
include!("commands/repl_loop.rs");
include!("commands/noninteractive.rs");
