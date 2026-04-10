# ccrust

The `claude-code-rust` repository builds the `ccrust` binary: a Rust-native reimplementation of Claude Code agent workflows with support for tasks, tools, plugins, MCP, and multiple providers, including Anthropic, native Gemini, ChatGPT Codex, and OpenAI-compatible backends such as OpenRouter. It is targeted fully compatible with Claude's official Claude Code with fully rewriting with Rust.

You can use your own Codex Plan with Claude Code's agent workflow.

We also introduced some prompt compacting but strictly keeping Claude Code's original prompt caching and format, and fixed some bugs of token cost in original Claude Code. It should work exactly the same way with original Claude Code.

In ZanChat AI, we use this tool with Codex Plan for production-level development. As we are updating the tools everyday, so we recommend you to upgrade it frequently. We'll try our best to keep it stable.

## Installation

Requires [Rust and Cargo](https://rustup.rs/).

```bash
git clone git@github.com:ZanChat/claude-code-rust.git
cd claude-code-rust

cargo install --path crates/cli --locked
```

Cargo installs the binary as `ccrust` into `~/.cargo/bin` by default.

To refresh a local build while testing changes:

```bash
cargo install --path crates/cli --locked --force
```

## Running ccrust

```bash
# Check the installed binary
ccrust --version

# Interactive REPL
ccrust

# Non-interactive prompt
ccrust 'Refactor the auth logic in src/auth.rs'

# Headless JSON output
ccrust --print --output-format json 'Summarize the current diff'

# Pick a provider explicitly for one launch
ccrust --provider chatgpt-codex
```

`ccrust` loads saved login config from `~/.claude/.env.ccrust` and then
`./.claude/.env.ccrust`. Later sources override earlier ones:
project-local config overrides user config, real environment variables override
saved config, and CLI flags override all of them.

The top-level CLI now exposes real backends for these command families:
- `ccrust mcp`, `ccrust plugin`, `ccrust agents`, `ccrust doctor`, `ccrust setup-token`
- `ccrust server` and headless `ccrust open <endpoint> -p [prompt]`
- `ccrust update`, `ccrust install`, `ccrust rollback`
- `ccrust task`, `ccrust export`, `ccrust completion`

Current compatibility gap: TS direct-connect `cc://` and `cc+unix://` URLs still are not implemented in the Rust rewrite, and `ccrust ssh` still reports that remote SSH session orchestration is not available yet.

If no provider is configured, bare `ccrust` opens onboarding automatically.
Inside the REPL, `/login` opens the same onboarding flow and `/logout` clears
the tracked `*.env.ccrust` config for that session.

For OpenAI-family auth compatibility, `ccrust` uses the same shared Codex
credentials file as the TS version and official `codex login`: `auth.json` is
resolved from `CODEX_AUTH_JSON_PATH`, then
`CLAUDE_CODE_CODEX_AUTH_JSON_PATH`, then `CODEX_HOME` /
`CLAUDE_CODE_CODEX_HOME`, and finally `~/.codex/auth.json`.

Any ccrust-only fallback auth snapshot is stored separately at
`~/.claude/ccrust/code-agent-auth.json` by default, so using `ccrust` does not
move or replace the shared `auth.json` that other tools continue to use.

## Supported API Providers & Authentication

### 1. Anthropic (First-Party) — Default

```bash
export ANTHROPIC_API_KEY="sk-ant-api..."
ccrust
```

### 2. OpenAI-Compatible

For OpenAI-family providers, the agent uses a dual-model architecture matching the original TS implementation:
- reasoning model: `REASONING_MODEL` (default `gpt-5.4`) for thinking-enabled turns
- completion model: `COMPLETION_MODEL` (default `gpt-5.3-codex`) for standard turns and utility calls

The agent automatically selects the appropriate model per request based on whether reasoning is active for a given turn.

```bash
export OPENAI_API_KEY="sk-..."
ccrust --provider openai-compatible
```

Official OpenAI uses the same `openai-compatible` provider mode and defaults to `https://api.openai.com/v1` when `OPENAI_BASE_URL` is unset. The legacy `openai` provider name is still accepted as an alias for compatibility.

For other OpenAI-compatible APIs:

```bash
export OPENAI_API_KEY="your-custom-token"
export OPENAI_BASE_URL="https://api.yourprovider.com/v1"
ccrust --provider openai-compatible
```

The interactive `/login` flow also includes presets for OpenAI, OpenRouter,
Gemini, and a custom OpenAI-compatible base URL.

### 3. Gemini — Native Gemini API

Native Gemini is the preferred way to use Gemini with `ccrust`.

```bash
export GEMINI_API_KEY="..."
ccrust --provider gemini
```

The native Gemini provider defaults to:
- reasoning model: `GEMINI_REASONING_MODEL` or `gemini-2.5-pro`
- fast model: `GEMINI_COMPLETION_MODEL` or `gemini-2.5-flash`
- base URL: `GEMINI_BASE_URL` or `https://generativelanguage.googleapis.com/v1beta`

`GOOGLE_API_KEY` and `OPENAI_API_KEY` are also accepted. For model/base URL compatibility, native Gemini also falls back to `REASONING_MODEL`, `COMPLETION_MODEL`, `REASONING_MODEL_THINK`, `COMPLETION_MODEL_THINK`, and `OPENAI_BASE_URL`. If `OPENAI_BASE_URL` points at Gemini's OpenAI-compatible `/openai` endpoint, `ccrust` normalizes it back to the native `generateContent` base URL automatically. The interactive `/login` flow still saves provider-specific Gemini keys by default.

### 4. ChatGPT Codex

Uses the shared Codex `auth.json` for authentication with automatic token
refresh. Path resolution matches the TS version and official Codex tooling:
`CODEX_AUTH_JSON_PATH` -> `CLAUDE_CODE_CODEX_AUTH_JSON_PATH` -> `CODEX_HOME` /
`CLAUDE_CODE_CODEX_HOME` -> `~/.codex/auth.json`.

```bash
ccrust --provider chatgpt-codex
```

`/logout` does not delete the shared Codex `auth.json`; it only clears
ccrust-managed local config and fallback snapshot state.

### 5. Amazon Bedrock

```bash
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
export AWS_REGION="us-east-1"
ccrust --provider bedrock
```

### 6. Google Cloud Vertex AI

```bash
export VERTEX_ACCESS_TOKEN="..."
ccrust --provider vertex
```

### 7. Azure AI Foundry

```bash
export ANTHROPIC_FOUNDRY_API_KEY="..."
ccrust --provider foundry
```

## Environment Variables Reference

### Provider Selection

| Variable | Description | Default |
|---|---|---|
| `CLAUDE_CODE_API_PROVIDER` | Override the active provider (`firstParty`, `gemini`, `openai-compatible`, `chatgpt-codex`, `bedrock`, `vertex`, `foundry`). `openai` is accepted as a legacy alias for `openai-compatible`. | `firstParty` |

### Authentication

| Variable | Description |
|---|---|
| `ANTHROPIC_API_KEY` | API key for Anthropic first-party provider |
| `GEMINI_API_KEY` | API key for the native Gemini provider (`GOOGLE_API_KEY` and `OPENAI_API_KEY` also work) |
| `GOOGLE_API_KEY` | Alternative API key for the native Gemini provider |
| `OPENAI_API_KEY` | API key / bearer token for OpenAI-family providers |
| `AWS_ACCESS_KEY_ID` | AWS access key for Bedrock |
| `AWS_SECRET_ACCESS_KEY` | AWS secret key for Bedrock |
| `AWS_SESSION_TOKEN` | Optional AWS session token for Bedrock |
| `AWS_BEARER_TOKEN_BEDROCK` | Direct bearer token for Bedrock |
| `VERTEX_ACCESS_TOKEN` | OAuth access token for Vertex AI |
| `GOOGLE_OAUTH_ACCESS_TOKEN` | Alternative Google OAuth token for Vertex AI |
| `ANTHROPIC_FOUNDRY_API_KEY` | API key for Azure AI Foundry |
| `AZURE_API_KEY` | Alternative API key for Azure AI Foundry |
| `AZURE_AUTH_TOKEN` | Bearer token for Azure AI Foundry |
| `FOUNDRY_AUTH_TOKEN` | Alternative bearer token for Foundry |
| `CODEX_AUTH_JSON_PATH` | Explicit path to the shared Codex/OpenAI auth file |
| `CLAUDE_CODE_CODEX_AUTH_JSON_PATH` | Alternate explicit path to the shared Codex/OpenAI auth file |
| `CODEX_HOME` | Base directory for Codex state such as `auth.json` |
| `CLAUDE_CODE_CODEX_HOME` | Alternate base directory for Codex state such as `auth.json` |

### Base URLs

| Variable | Description | Default |
|---|---|---|
| `ANTHROPIC_BASE_URL` | Override Anthropic API endpoint | `https://api.anthropic.com` |
| `GEMINI_BASE_URL` | Override Gemini API endpoint (`OPENAI_BASE_URL` also works and `/openai` is normalized) | `https://generativelanguage.googleapis.com/v1beta` |
| `OPENAI_BASE_URL` | Override OpenAI API endpoint | `https://api.openai.com/v1` |
| `ANTHROPIC_BEDROCK_BASE_URL` / `BEDROCK_BASE_URL` | Override Bedrock endpoint | Auto-detected from region |
| `ANTHROPIC_VERTEX_BASE_URL` / `VERTEX_BASE_URL` | Override Vertex AI endpoint | Auto-detected from project/region |
| `ANTHROPIC_FOUNDRY_BASE_URL` / `FOUNDRY_BASE_URL` | Override Foundry endpoint | Derived from resource name |
| `ANTHROPIC_FOUNDRY_RESOURCE` | Azure AI Foundry resource name | — |

### Model Selection

| Variable | Description | Default |
|---|---|---|
| `REASONING_MODEL` | Model for thinking-enabled turns | `gpt-5.4` |
| `COMPLETION_MODEL` | Model for standard and utility turns | `gpt-5.3-codex` |
| `GEMINI_REASONING_MODEL` | Default model for native Gemini main turns (`REASONING_MODEL` also works) | `gemini-2.5-pro` |
| `GEMINI_COMPLETION_MODEL` | Default model for native Gemini fast turns (`COMPLETION_MODEL` also works) | `gemini-2.5-flash` |

### OpenAI Thinking

| Variable | Description | Default |
|---|---|---|
| `REASONING_MODEL_THINK` | Reasoning effort for the reasoning model (`low`, `medium`, `high`, `xhigh`); native Gemini also uses this when `CLAUDE_CODE_API_PROVIDER=gemini` | `xhigh` |
| `COMPLETION_MODEL_THINK` | Reasoning effort for the completion model (`low`, `medium`, `high`, `xhigh`); native Gemini also uses this when `CLAUDE_CODE_API_PROVIDER=gemini` | `xhigh` |

### Claude Thinking

| Variable | Description | Default |
|---|---|---|
| `CLAUDE_CODE_DISABLE_THINKING` | Disable thinking entirely for Claude models | `false` |
| `CLAUDE_CODE_DISABLE_ADAPTIVE_THINKING` | Force budget-based thinking instead of adaptive | `false` |
| `MAX_THINKING_TOKENS` | Token budget for non-adaptive thinking | `10000` |

### Region & Project

| Variable | Description | Default |
|---|---|---|
| `AWS_REGION` / `BEDROCK_AWS_REGION` / `AWS_DEFAULT_REGION` | AWS region for Bedrock | `us-east-1` |
| `CLAUDE_CODE_VERTEX_REGION` / `CLOUD_ML_REGION` | GCP region for Vertex AI | `us-east5` |
| `CLAUDE_CODE_VERTEX_PROJECT_ID` / `GOOGLE_CLOUD_PROJECT` | GCP project for Vertex AI | — |

### Auth Skip Flags

| Variable | Description |
|---|---|
| `CLAUDE_CODE_SKIP_BEDROCK_AUTH` | Skip AWS SigV4 auth for Bedrock |
| `CLAUDE_CODE_SKIP_VERTEX_AUTH` | Skip OAuth for Vertex AI |
| `CLAUDE_CODE_SKIP_FOUNDRY_AUTH` | Skip auth for Foundry |

### Retry

| Variable | Description | Default |
|---|---|---|
| `LLM_RETRY_COUNT` | Max retries on 50x errors | `3` |

## Built-In Slash Commands

Inside the REPL:

| Command | Description |
|---|---|
| `/vim` | Toggle Vim mode |
| `/status` | Print runtime provider and environment status |
| `/ide` | Inspect IDE bridge compatibility and connection state |
| `/model <name>` | Switch active model |
| `/files` | View context files |
| `/diff` | View context diffs |
| `/clear` | Reset conversation |
| `/compact` | Compact context to save tokens |
| `/help` | Show all available commands including plugins |

## Top-Level Command Notes

### Print Mode

`ccrust --print` runs the root command in non-interactive mode. The Rust CLI supports the TS-style print contract for:
- `--output-format text|json|stream-json`
- `--input-format text|stream-json`
- `--json-schema <schema>`
- `--include-hook-events`
- `--include-partial-messages`
- `--replay-user-messages`
- `--resume-session-at <assistant-message-id>`
- `--no-session-persistence`

### Local Install Management

`ccrust update` and `ccrust install [target]` install the current binary, a local executable path, or a previously recorded snapshot into `~/.cargo/bin/ccrust`. Each install records rollback snapshots under `~/.claude/ccrust/install-history/` and stores state in `~/.claude/ccrust/install-state.json`.

`ccrust rollback [target]` restores one of those recorded snapshots. Use `ccrust rollback --list` to inspect available entries and `ccrust rollback --dry-run` to preview the selected target.

### Tasks, Export, and Completion

`ccrust task` supports:
- `task create <subject> [-d|--description <text>] [-l|--list <id>]`
- `task list [--pending] [--json] [-l|--list <id>]`
- `task get <id> [-l|--list <id>]`
- `task update <id> [--subject <text>] [-d|--description <text>] [-s|--status <status>] [--owner <agentId>] [--clear-owner] [-l|--list <id>]`
- `task dir [-l|--list <id>]`

`ccrust export <source> <outputFile>` renders a transcript to text from a session ID, a recent session index, or an existing `.json` / `.jsonl` transcript path.

`ccrust completion <shell> [--output <file>]` generates shell completion scripts for `bash`, `zsh`, and `fish`.

## License

MIT
