# code-agent-rust Implementation Spec

## Mission

`code-agent-rust` is a greenfield Rust rewrite of the restored Claude Code behavior.

The implementation target is a third-party agent tool that keeps near-full parity with the current CLI, agent loop, tool execution, session behavior, and advanced subsystems, while replacing the TypeScript/Bun runtime with a Rust-native architecture.

This document incorporates the final pre-implementation review and supersedes any earlier wording that left provider scope, transcript compatibility, plugin execution, or advanced subsystem handling ambiguous.

Primary goals:
- Preserve the current Claude-Code-main-run workflows and operator experience as closely as practical.
- Ship as a third-party tool, not as a fork carrying private platform assumptions.
- Support Anthropic, OpenAI family, and openai-compatible or third-party providers already represented in the current codebase.
- Replace leaked, internal, or private logic with clean Rust-native implementations.

## Progress Update

Status as of 2026-04-03:
- Milestones 1 through 6 are implemented in code and covered by workspace tests; Milestone 5 now includes pane-driven ratatui REPL coverage for transcript, permissions, tasks, file/diff preview, logs, banners, and slash-command flows.
- Milestone 7 non-voice scope is substantially implemented: local bridge, direct-connect, resumable remote sessions, permission prompts, question/response envelopes, coordinator-backed directives, IDE-style direct transport, and shared core orchestration helpers exist in Rust.
- Milestone 8 is partially implemented: the compatibility matrix and transcript contract docs are expanded, and fixture-backed parity coverage now exists for commands, transcripts, provider stream parsing, and plugin discovery.
- Voice remains intentionally deferred from the current finish target and does not block completion work for the non-voice port.

Implemented areas:
- workspace, crate boundaries, canonical message/task/question models, and command registry
- provider runtime for `firstParty`, `bedrock`, `vertex`, `foundry`, `openai`, `chatgpt-codex`, and `openai-compatible`
- production auth failure behavior instead of silent offline fallback; `EchoProvider` remains test-only
- JSONL transcript compatibility, resume by session id or transcript path, compaction persistence, and materialized compacted runtime state
- built-in tools for file, shell, search, network, MCP, task, memory, messaging, and workflow flows
- MCP transport support plus OAuth device-flow auth, token cache, refresh, and command/tool integration
- plugin manifest and skill compatibility, plus out-of-process plugin bridge lifecycle management
- bridge websocket/tcp/ide transports, resumable remote sessions, remote tool permission gating, question/response flow, and coordinator-backed assistant directives
- pane-driven ratatui REPL/TUI with transcript, statusline, notifications, permission prompts, tasks, diff/file previews, logs, and interactive slash-command handling
- shared core orchestration helpers for agent/workflow creation, coordinator worker/synthesis lifecycle, and question/response resumption
- fixture-backed compatibility coverage for command parsing, compacted transcript resume, provider stream parsing, and plugin/skill discovery

Remaining non-voice finish blockers:
- Coordinator and agent orchestration are shared and test-covered, but still sequential; deeper parity with the original multi-agent runtime, richer child-task lifecycle management, and broader workflow semantics remain open.
- Remote and IDE compatibility still rely on the Rust-owned bridge protocol rather than matching every observable detail of the original private cloud/session flows; additional protocol hardening and interoperability coverage are still required.
- Migration tooling remains thinner than the original target: transcript and config compatibility are documented and tested, but dedicated import/conversion helpers are still limited.

Current validation baseline:
- `cargo check --workspace`
- `cargo test --workspace`

## Product And Compatibility Target

Compatibility target: high compatibility with the current Claude Code workflows.

Preserve where practical:
- command names and slash command semantics
- interactive REPL flow and TUI structure
- session, transcript, and resume concepts
- tool lifecycle, tool-call continuation, and MCP behavior
- plugin and skill discovery conventions
- compaction, session-memory, and retry behavior
- remote, bridge, assistant, coordinator, and voice-class subsystems in v1

Behavioral compatibility is sufficient, literal source parity is not required, for:
- internal architecture and module boundaries
- telemetry and experiment systems
- internal naming and feature-gate structure
- provider client internals
- plugin runtime internals when external behavior stays compatible

Non-goals:
- porting leaked private code literally
- preserving Bun, Ink, or React implementation details
- preserving Anthropic-internal telemetry/event naming

## Repository Shape

The Rust repo uses this workspace layout:

```text
code-agent-rust/
├── Cargo.toml
├── rust-toolchain.toml
├── IMPLEMENTATION.md
├── crates/
│   ├── cli/
│   ├── core/
│   ├── providers/
│   ├── tools/
│   ├── mcp/
│   ├── session/
│   ├── ui/
│   ├── plugins/
│   └── bridge/
├── fixtures/
│   ├── transcripts/
│   ├── provider-streams/
│   ├── command-golden/
│   └── plugin-fixtures/
└── docs/
    ├── compatibility-matrix.md
    └── transcript-format.md
```

Workspace responsibilities:
- `crates/cli`: binary entrypoint, argument parsing, env/config bootstrap, CLI handlers, non-interactive command mode
- `crates/core`: canonical message model, agent loop, command registry, orchestration, permissions, compaction coordination, bootstrap, state, tasks, coordinator logic, proactive/buddy-style behaviors
- `crates/providers`: provider trait set, auth backends, model metadata, streaming adapters, token accounting, secure credential access, `~/.codex/auth.json` compatibility
- `crates/tools`: built-in tools, schema definitions, execution runtime, tool permissions
- `crates/mcp`: MCP client/server support, config loading, transport/session lifecycle, auth helpers
- `crates/session`: transcript store, session state, resume, compact boundary persistence, session memory, read/write-compatible JSONL support
- `crates/ui`: ratatui-based REPL, panes, input, statusline, diff rendering, keybindings, vim mode, screens, output styles, view models
- `crates/plugins`: plugin and skill discovery, manifest compatibility, commands/agents/skills/output-styles/hooks config, LSP/MCP plugin integration, out-of-process compatibility bridge
- `crates/bridge`: remote control, IDE bridge, direct-connect, websocket/IPC transports, remote session manager, server-side protocol adapters

No TypeScript top-level area is left unassigned. Any remaining utilities or support code from the source tree must map into one of the crates above.

## Baseline Rust Stack

Required baseline stack:
- `tokio` for async runtime and subprocess/network coordination
- `reqwest` for HTTP clients
- `serde` and `serde_json` for canonical data models and provider payloads
- `clap` for CLI parsing
- `ratatui` for terminal rendering
- `crossterm` for terminal IO and key handling
- `tracing` and `tracing-subscriber` for diagnostics
- websocket support via `tokio-tungstenite` or an equivalent crate
- `schemars` or equivalent for JSON-schema-backed tool definitions

Preferred supporting crates:
- `anyhow` for application error surfaces
- `thiserror` for structured internal errors
- `parking_lot` or standard sync primitives depending on contention measurements
- `uuid`, `time`, and `chrono`-class utilities as needed for transcript/session identities
- `notify` for filesystem watch flows used by plugins, skills, or settings reload

## Source Of Truth

The source of truth for behavior is the current TypeScript project at:
- `claude-code-src/src/main.tsx`
- `claude-code-src/src/commands.ts`
- `claude-code-src/src/services/`
- `claude-code-src/src/tools/`

The Rust project should follow the original UX and operator flow as closely as practical, while translating architecture into Rust-native boundaries.

## Canonical Internal Model

The Rust implementation must define one canonical internal message and event model in `crates/core`.

Requirements:
- All providers map incoming and outgoing payloads into the canonical message model.
- Tools, plugins, MCP resources, transcripts, compaction, and UI all consume the same canonical model.
- Provider-specific blocks must be normalized into internal enums rather than leaking provider wire types across the codebase.
- Token usage, cache usage, and context-window metrics must be attached as structured metadata on canonical messages.
- Streaming deltas must accumulate into canonical assistant messages before transcript persistence and UI rendering.

Required internal domains:
- messages
- assistant output blocks
- tool calls and tool results
- attachments and synthetic messages
- system and boundary messages
- transcript events
- session state
- provider usage and context metadata
- command results

## CLI Bootstrap And Argument Parsing

Ownership crate: `crates/cli`

Source-of-truth behavior:
- `src/dev-entry.ts`
- `src/main.tsx`
- `src/commands.ts`

Compatibility target:
- keep top-level binary usage aligned with the current `claude` command shape
- preserve major flags, prompt invocation, version/help behavior, and command routing
- preserve slash-command execution from interactive mode

Implementation notes:
- Use `clap` for top-level parsing, but route command execution through a registry owned by `crates/core`.
- Keep non-interactive prompt execution and interactive REPL startup as first-class modes.
- Separate bootstrap phases:
  - environment and config load
  - auth bootstrap
  - provider/model resolution
  - plugin and MCP load
  - session bootstrap
  - TUI launch or non-interactive execution
- Add compatibility shims for environment variables currently used by the TypeScript app.

Replace, do not port literally:
- restored-source bootstrap workarounds
- Bun-specific dev-entry logic
- private startup profiling and experiment bootstrap wiring

## REPL And TUI Rendering

Ownership crate: `crates/ui`

Source-of-truth behavior:
- `src/components/`
- `src/hooks/`
- `src/ink/`
- `src/replLauncher.ts`

Compatibility target:
- preserve the overall information architecture of the REPL
- preserve input, output, streaming, statusline, notifications, permissions, and compact-boundary presentation
- preserve key interaction patterns for slash commands, interrupts, tool feedback, and resume

Implementation notes:
- Build the TUI on `ratatui` and `crossterm`.
- Use a unidirectional state model:
  - `core` owns canonical session/app state
  - `ui` owns render-only projections and input dispatch
- Rebuild high-value views first:
  - transcript/messages pane
  - prompt input
  - status line
  - tool/progress indicators
  - compact and warning banners
  - permission prompts
  - diff and file preview panes
- Implement UI test seams with deterministic state snapshots.

Replace, do not port literally:
- Ink renderer internals
- React hooks and component lifecycle patterns
- Bun-specific terminal integration

## Command Registry

Ownership crate: `crates/core`

Source-of-truth behavior:
- `src/commands.ts`
- `src/commands/`

Compatibility target:
- preserve command names and enablement semantics where practical
- preserve slash-command invocation patterns and prompt-generation behavior
- preserve interactive and non-interactive command routing

Implementation notes:
- Define a Rust `CommandSpec` registry with:
  - name
  - aliases
  - enablement predicate
  - interactive vs non-interactive capability
  - handler contract
  - help metadata
- Separate pure command resolution from execution.
- Group commands into:
  - session and context
  - auth and provider
  - config and settings
  - tooling and plugins
  - advanced/bridge/assistant flows
- Produce a command compatibility matrix against current TS commands.

Replace, do not port literally:
- feature-flag dead-code-elimination patterns
- private/internal commands not appropriate for a third-party tool

## Provider, Auth, And Model Abstraction

Ownership crate: `crates/providers`

Source-of-truth behavior:
- `src/services/api/`
- `src/utils/model/`
- `src/utils/auth.ts`
- `src/utils/openaiAuth.ts`
- `src/utils/chatgptCodex.ts`

Compatibility target:
- support Anthropic, OpenAI family, and openai-compatible or third-party providers already modeled in the current codebase
- preserve model resolution semantics where possible
- preserve reasoning/completion model split where used
- preserve tool-call and streaming behavior by provider family

Required Rust interfaces:
- `ApiProvider`
- `AuthResolver`
- `Provider`
- `ProviderStream`
- `ModelCatalog`
- `UsageAccounting`
- `ContextWindowResolver`

Provider requirements:
- `firstParty`
- `bedrock`
- `vertex`
- `foundry`
- `openai`
- `chatgpt-codex`
- `openai-compatible`

Compatibility contract:
- preserve current provider-selection behavior across `CLAUDE_CODE_API_PROVIDER` and the provider-specific env toggles
- preserve `chatgpt-codex` compatibility via `~/.codex/auth.json`, local auth snapshot loading, and token-refresh-capable auth resolution
- preserve Anthropic/OpenAI vendor and model names where they are externally meaningful

Implementation notes:
- Keep provider-specific transport and auth code isolated from `core`.
- Normalize all streamed deltas into the canonical internal model.
- Centralize provider capability metadata:
  - tool support
  - thinking/reasoning modes
  - context window
  - max output
  - cache or prompt-edit support if applicable
- Maintain vendor/model naming exactly where externally meaningful.

Replace, do not port literally:
- private analytics attached to provider calls
- leaked internal endpoint assumptions
- first-party-only growth/experimentation hooks

## Agent Loop And Tool Execution

Ownership crates:
- `crates/core`
- `crates/tools`

Source-of-truth behavior:
- `src/query.ts`
- `src/Tool.ts`
- `src/tools/`
- `src/query/deps.ts`

Compatibility target:
- preserve the current agent query loop shape
- preserve streamed assistant/tool interleaving
- preserve multi-turn tool continuation semantics
- preserve command-triggered model interactions

Implementation notes:
- `core` owns the main agent loop.
- `tools` owns built-in tool definitions and execution adapters.
- Define a Rust `Tool` trait with:
  - metadata
  - JSON schema
  - permission requirements
  - execution entrypoint
  - structured result contract
- Preserve tool classes conceptually:
  - file operations
  - shell/exec
  - search/glob/grep
  - web/network
  - MCP
  - agent/team/task tools
  - workflow/config/skill tools
- Preserve interrupt, cancellation, and timeout semantics.
- Keep permission checks explicit and centralized.

Replace, do not port literally:
- TS class hierarchy details
- restored-source shims
- internal-only tools with no third-party value

## MCP Integration

Ownership crate: `crates/mcp`

Source-of-truth behavior:
- `src/services/mcp/`
- `src/tools/MCP*`

Compatibility target:
- preserve MCP server discovery, config loading, auth handling, tool/resource exposure, and session lifecycle
- preserve MCP as a first-class extension path

Implementation notes:
- Support stdio and network transports required by the current project behavior.
- Keep MCP config and server records versioned and serializable.
- Build MCP client abstractions independent of UI.
- Expose MCP tools and resources through the same canonical tool registry used by built-ins.

Replace, do not port literally:
- private registry integrations
- first-party bootstrap shortcuts

## Session Persistence And Transcript Import/Export

Ownership crate: `crates/session`

Source-of-truth behavior:
- `src/utils/sessionStorage.ts`
- `src/services/SessionMemory/`
- transcript and resume handling spread across `src/`

Compatibility target:
- preserve session resume behavior
- preserve compact boundaries and transcript continuity
- preserve direct read/write compatibility with the current JSONL transcript format

Implementation notes:
- Treat the current JSONL transcript layout as the v1 on-disk contract.
- Support:
  - resume by session id
  - resume by explicit `.jsonl` path
  - compact boundary persistence
  - subagent transcript subpaths
- Persist:
  - canonical messages
  - session metadata
  - compact boundaries
  - provider usage metadata
  - plugin or tool state required for resume
- Use `docs/transcript-format.md` to document the compatibility contract and any optional sidecar metadata.

Replace, do not port literally:
- TS-specific persistence shortcuts
- private metadata not needed by a third-party tool

## Compact, Session Memory, And Retry Logic

Ownership crates:
- `crates/core`
- `crates/session`
- `crates/providers`

Source-of-truth behavior:
- `src/services/compact/`
- `src/services/SessionMemory/`
- token and context helpers under `src/utils/`

Compatibility target:
- preserve manual compact, auto-compact, reactive compact, and session-memory compact behavior
- preserve warning thresholds, blocking thresholds, and resume-safe compaction boundaries

Implementation notes:
- Keep compaction policy provider-aware but provider-independent at the orchestration layer.
- Port the current concepts:
  - effective context window
  - auto-compact threshold
  - warning and blocking thresholds
  - session-memory compaction
  - circuit breaker and prompt-too-long handling
- Build compaction around canonical messages and transcript boundaries, not provider wire payloads.
- Keep token estimation and usage accounting separated:
  - estimated context from local messages
  - precise usage from provider responses

Replace, do not port literally:
- private experiment flags
- telemetry-only branches
- internal naming of compaction-related experiments

## Plugin And Skill Compatibility Bridge

Ownership crate: `crates/plugins`

Source-of-truth behavior:
- `src/plugins/`
- `src/skills/`
- plugin and skill loading under `src/utils/plugins/` and related helpers

Compatibility target:
- preserve current skill and plugin directory concepts as much as practical
- keep MCP plus compatibility bridge, not MCP-only
- preserve `.claude-plugin/plugin.json`
- preserve plugin components:
  - `commands`
  - `agents`
  - `skills`
  - `hooks`
  - `output-styles`
  - `mcpServers`
  - `lspServers`
  - `userConfig`
- preserve skill directory formats:
  - `.claude/skills/<name>/SKILL.md`
  - legacy `.claude/commands`

Implementation notes:
- Support bundled skills/plugins and local discovery paths.
- Load manifests, skill metadata, hooks config, MCP declarations, and LSP declarations natively in Rust.
- Use an out-of-process compatibility bridge for executable legacy-compatible behavior that is not yet reimplemented natively.
- Do not embed a JS or TS runtime in-process.
- Produce a clear compatibility table:
  - natively supported
  - adapted
  - intentionally unsupported

Replace, do not port literally:
- private plugin marketplace assumptions
- first-party-only plugin infrastructure

## Remote, Bridge, And Server Flows

Ownership crate: `crates/bridge`

Source-of-truth behavior:
- `src/bridge/`
- `src/remote/`
- `src/server/`

Compatibility target:
- include these advanced systems in v1
- preserve direct-connect and remote-control workflows where practical

Implementation notes:
- Separate transport protocol, session lifecycle, and UI attachment logic.
- Support websocket and local IPC transports needed by the original flows.
- Preserve CLI and TUI workflows, but replace Anthropic-private remote/session endpoints with Rust-owned transports or provider-neutral adapters.
- Keep bridge events mapped into canonical internal events.
- Build resumable reconnect flows explicitly.

Replace, do not port literally:
- private remote infrastructure assumptions
- leaked first-party transport naming and event labels

## Assistant, Coordinator, And Multi-Agent Systems

Ownership crates:
- `crates/core`
- `crates/bridge`

Source-of-truth behavior:
- `src/assistant/`
- `src/coordinator/`
- multi-agent tools and swarm helpers under `src/`

Compatibility target:
- preserve assistant-mode and coordinator-class workflows in v1
- preserve local multi-agent orchestration semantics where practical

Implementation notes:
- Use canonical messages and shared session state across main and sub-agents.
- Keep agent spawning, handoff, and cross-agent messaging explicit in the `core` interfaces.
- Isolate provider usage from coordinator logic.

Replace, do not port literally:
- Anthropic-internal labels and system naming
- private gating and experiment-specific paths

## Voice Subsystem

Ownership crates:
- `crates/ui`
- `crates/core`
- `crates/bridge` if remote voice transport is required

Source-of-truth behavior:
- `src/voice/`
- voice-related commands and UI hooks under `src/`

Compatibility target:
- include voice infrastructure in v1 plan
- preserve command, state, and interaction surfaces where practical

Implementation notes:
- Keep audio capture, transcription, and playback isolated from the agent core.
- Treat provider-facing voice capabilities as optional adapters, not as assumptions in the core loop.
- If exact parity is blocked by private dependencies, ship a Rust-native replacement with equivalent user behavior.

Replace, do not port literally:
- private native bindings
- proprietary transport dependencies that cannot ship in a third-party tool
- private cloud APIs that are not required for third-party parity

## Privacy And Naming Migration

Must not be ported literally:
- Anthropic-internal telemetry names
- GrowthBook, Datadog, or private analytics wiring
- internal experiment flags and leaked labels
- private service integrations not required for third-party operation
- first-party product naming that implies internal ownership

Naming rules:
- keep Anthropic, OpenAI, ChatGPT Codex, and provider/model names when they refer to external APIs, models, auth modes, or compatibility surfaces
- rename internal product concepts, event names, gates, internal service labels, and private subsystem names to neutral third-party names
- do not reproduce leaked internal identifiers unless they are required for compatibility with external user-facing files

## Migration Strategy

### Milestone 1: Workspace bootstrap and shared model
Status:
- implemented

Deliverables:
- workspace `Cargo.toml`
- crate skeletons
- canonical message and event model
- error, config, and logging foundations

Exit criteria:
- workspace builds
- shared model crate compiles cleanly
- CLI binary boots to a placeholder command dispatcher

### Milestone 2: Provider layer and auth
Status:
- implemented for the required provider families in the current Rust runtime

Deliverables:
- provider trait set
- `firstParty`, `bedrock`, `vertex`, `foundry`, `openai`, `chatgpt-codex`, and `openai-compatible` support scaffolding
- auth/config resolution
- model metadata and context window registry

Exit criteria:
- non-interactive prompts work across the required v1 provider modes that are implemented in this milestone
- streaming adapters normalize into canonical messages

### Milestone 3: Core agent loop and tool runtime
Status:
- implemented for the current local runtime, with additional parity work still open in coordinator depth and hardening

Deliverables:
- canonical query loop
- tool registry
- built-in file/shell/search/web tool set
- permission model

Exit criteria:
- tool-call continuation works end to end
- core coding-agent workflow functions in non-interactive and interactive execution

### Milestone 4: Session, transcript, and compaction support
Status:
- implemented

Deliverables:
- read/write-compatible JSONL transcript persistence
- resume support
- compact boundaries
- manual and auto compact
- session-memory support

Exit criteria:
- sessions resume after compaction
- transcript compatibility is defined and tested for session id and `.jsonl` path resume

### Milestone 5: CLI and TUI parity
Status:
- implemented for the main workflow parity target, including pane-driven ratatui rendering, slash-command routing, permission/task/file/diff/log panes, and UI snapshot coverage

Deliverables:
- ratatui REPL
- input, output, statusline, banners, diff panes
- slash command routing

Exit criteria:
- interactive UX covers the main Claude Code workflows
- selected command and prompt flows match current behavior

### Milestone 6: Plugin and skill compatibility bridge
Status:
- implemented for manifest loading, skill discovery, bridge lifecycle, and MCP/LSP declarations; additional hardening remains

Deliverables:
- plugin and skill loader
- compatibility adapters
- out-of-process compatibility bridge
- bundled and local plugin/skill support
- MCP integration finalized into the external extension surface

Exit criteria:
- existing plugin or skill scenarios load through native or adapted runtime paths

### Milestone 7: Advanced subsystems
Status:
- in progress, with non-voice remote/direct-connect/coordinator flows implemented on Rust-owned transports and shared core orchestration helpers; voice remains deferred from the current finish target

Deliverables:
- remote and bridge flows
- direct-connect/server flows
- assistant/coordinator flows
- voice subsystem
- Rust-owned replacements for private transport dependencies where needed

Exit criteria:
- advanced flows are functional in Rust and mapped into the canonical architecture

### Milestone 8: Compatibility hardening and migration tooling
Status:
- partially implemented: compatibility docs and fixture-backed parity coverage are in place; dedicated migration/import helpers remain limited

Deliverables:
- compatibility matrix
- transcript migration/import tools
- config migration helpers
- parity gap audit and cleanup

Exit criteria:
- documented parity coverage exists
- major migration paths from the TS tool are tested

## Testing And Acceptance

Required automated test categories:
- golden tests for command parsing
- read/write round-trip tests for current JSONL transcripts, including compact boundaries, session id resume, `.jsonl` path resume, and subagent transcript paths
- provider contract tests for all required v1 provider modes, including `chatgpt-codex` auth file handling and token refresh behavior
- tool-loop tests for streamed assistant output, tool-use/tool-result continuation, cancellation, and permission gating
- token and context accounting tests
- compaction and session-memory tests
- session resume tests after compact boundaries
- plugin compatibility tests covering `.claude-plugin/plugin.json`, skill discovery, hooks loading, MCP/LSP declarations, and out-of-process bridge launch/failure behavior
- MCP integration tests
- TUI integration or snapshot tests for transcript rendering, input, statusline, compact warnings, permission prompts, and diff/file panes
- end-to-end smoke tests for:
  - Anthropic-style local workflow
  - OpenAI-family workflow
  - openai-compatible workflow
  - MCP-backed workflow
  - remote/bridge session workflow
  - assistant/coordinator flow

Required fixtures:
- transcript fixtures with compact boundaries
- provider stream fixtures for the required provider families
- command resolution golden files
- plugin and skill fixture directories

Current fixture baseline now present in-repo:
- `fixtures/command-golden/slash-commands.json`
- `fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl`
- `fixtures/provider-streams/anthropic_tool_use.json`
- `fixtures/provider-streams/openai_tool_call.json`
- `fixtures/plugin-fixtures/review-tools/`

Acceptance criteria:
- selected high-value TypeScript workflows produce equivalent observable behavior in Rust
- the required v1 providers support the core coding-agent loop as implemented for their milestone
- session resume, compact, and tool continuation are stable
- command compatibility is documented and enforced by tests where possible
- existing session transcripts can be resumed directly by Rust without one-off migration
- plugin and skill directories load without reorganization
- private telemetry, leaked internal code paths, and Anthropic-private remote endpoints are absent from the Rust implementation

## Required Initial Deliverables

The initial scaffold in the Rust repo must include:
- the workspace structure defined in this document
- the crate skeletons listed above
- a top-level `Cargo.toml`
- `rust-toolchain.toml`
- `docs/compatibility-matrix.md`
- `docs/transcript-format.md`

`IMPLEMENTATION.md` remains the execution-spec source of truth until the repo has enough code to replace it with crate-local docs.
