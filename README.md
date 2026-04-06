# ccrust

The `code-agent-rust` repository builds the `ccrust` binary: a Rust-native reimplementation of Claude Code agent workflows with support for tasks, tools, plugins, MCP, and multiple providers.

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
ccrust --repl

# Non-interactive prompt
ccrust 'Refactor the auth logic in src/auth.rs'
```

## Supported API Providers & Authentication

### 1. Anthropic (First-Party) — Default

```bash
export ANTHROPIC_API_KEY="sk-ant-api..."
ccrust --repl
```

### 2. OpenAI

For OpenAI-family providers, the agent uses a dual-model architecture matching the original TS implementation:
- reasoning model: `REASONING_MODEL` (default `gpt-5.4`) for thinking-enabled turns
- completion model: `COMPLETION_MODEL` (default `gpt-5.3-codex`) for standard turns and utility calls

The agent automatically selects the appropriate model per request based on whether reasoning is active for a given turn.

```bash
export OPENAI_API_KEY="sk-..."
ccrust --provider openai --repl
```

### 3. OpenAI-Compatible Providers

```bash
export OPENAI_API_KEY="your-custom-token"
export OPENAI_BASE_URL="https://api.yourprovider.com/v1"
ccrust --provider openai-compatible --repl
```

### 4. ChatGPT Codex

Uses `~/.codex/auth.json` for authentication with automatic token refresh:

```bash
ccrust --provider chatgpt-codex --repl
```

### 5. Amazon Bedrock

```bash
export AWS_ACCESS_KEY_ID="..."
export AWS_SECRET_ACCESS_KEY="..."
export AWS_REGION="us-east-1"
ccrust --provider bedrock --repl
```

### 6. Google Cloud Vertex AI

```bash
export VERTEX_ACCESS_TOKEN="..."
ccrust --provider vertex --repl
```

### 7. Azure AI Foundry

```bash
export ANTHROPIC_FOUNDRY_API_KEY="..."
ccrust --provider foundry --repl
```

## Environment Variables Reference

### Provider Selection

| Variable | Description | Default |
|---|---|---|
| `CLAUDE_CODE_API_PROVIDER` | Override the active provider (`firstParty`, `openai`, `chatgpt-codex`, `openai-compatible`, `bedrock`, `vertex`, `foundry`) | `firstParty` |

### Authentication

| Variable | Description |
|---|---|
| `ANTHROPIC_API_KEY` | API key for Anthropic first-party provider |
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

### Base URLs

| Variable | Description | Default |
|---|---|---|
| `ANTHROPIC_BASE_URL` | Override Anthropic API endpoint | `https://api.anthropic.com` |
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

### OpenAI Thinking

| Variable | Description | Default |
|---|---|---|
| `REASONING_MODEL_THINK` | Reasoning effort for the reasoning model (`low`, `medium`, `high`, `xhigh`) | `xhigh` |
| `COMPLETION_MODEL_THINK` | Reasoning effort for the completion model (`low`, `medium`, `high`, `xhigh`) | `xhigh` |

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

## License

MIT
