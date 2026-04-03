# code-agent-rust

`code-agent-rust` is a powerful, third-party Rust-native reimplementation of the Claude Code agent workflows. It offers high compatibility with agent tasks, standard tools, plugins, and MCP functionality while maintaining full performance in a clean systems language.

## Installation

You will need [Rust and Cargo](https://rustup.rs/) installed to build the project.

Clone the repository and compile the CLI using Cargo:

```bash
git clone git@github.com:ZanChat/claude-code-rust.git
cd claude-code-rust

# Build the release binary
cargo build --release

# The executable will be available at
./target/release/code-agent-rust
```

You can also run it directly inside your source tree during development:

```bash
cargo run --bin code-agent-rust -- [arguments...]
```

## Running the Agent

You can boot into the interactive REPL modes:

```bash
# Starts the standard prompt UI
cargo run --bin code-agent-rust -- --repl
```

Or pass a command/prompt non-interactively:

```bash
cargo run --bin code-agent-rust -- 'Refactor the authentication logic in src/auth.rs'
```

## Supported API Providers & Authentication

`code-agent-rust` natively supports an extensible set of LLM providers. Authentication relies primarily on environment variables.

### 1. Anthropic (First-Party)

This is the default provider if not expressly overridden.

```bash
export ANTHROPIC_API_KEY="sk-ant-api..."
cargo run --bin code-agent-rust -- --repl
```

### 2. OpenAI

To use OpenAI models like `gpt-4o`, set the provider flag and use the standard OpenAI API key format:

```bash
export OPENAI_API_KEY="sk-..."
cargo run --bin code-agent-rust -- --provider openai --model gpt-4o --repl
```

### 3. OpenAI-Compatible Providers

Allows you to use generic providers conforming to the OpenAI schema formatting:

```bash
export OPENAI_API_KEY="your-custom-token"
export OPENAI_BASE_URL="https://api.yourprovider.com/v1"
cargo run --bin code-agent-rust -- --provider openai-compatible --repl
```

### 4. Amazon Bedrock & GCP Vertex

Cloud adapters are natively supported. Make sure you have your standard AWS credentials (`~/.aws/credentials`) or Google Cloud credentials initialized:

```bash
# For AWS Bedrock
cargo run --bin code-agent-rust -- --provider bedrock --repl

# For GCP Vertex
cargo run --bin code-agent-rust -- --provider vertex --repl
```

### 5. Custom / Foundry / Codex

We also preserve native integration paths for tools like Foundry or custom Codex environments, provided you have the associated internal legacy keys defined.

```bash
# Explicit overriding
export CLAUDE_CODE_API_PROVIDER="foundry"
cargo run --bin code-agent-rust -- --repl
```

## Built-In Slash Commands

Inside the REPL, you can launch built-in commands natively. Examples include:

- `/vim` - Toggles full Vim state machine input compatibility.
- `/status` - Prints runtime provider and environment status.
- `/files`, `/diff` - Renders context file contents and context diffs.
- `/clear` - Truncates the active conversation boundaries.

More commands and plugins are natively supported. Run `/help` in the UI to see your complete plugin and compatibility matrix mapping.
