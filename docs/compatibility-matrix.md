# Compatibility Matrix

## Locked V1 Contracts

| Area | Contract |
| --- | --- |
| Provider modes | `firstParty`, `bedrock`, `vertex`, `foundry`, `openai`, `chatgpt-codex`, `openai-compatible` |
| Transcript format | Read and write the current JSONL transcript/session format directly |
| Resume modes | Resume by session id and by explicit `.jsonl` path |
| Plugin manifest | Preserve `.claude-plugin/plugin.json` |
| Skill layout | Preserve `.claude/skills/<name>/SKILL.md` and legacy `.claude/commands` |
| Plugin runtime | Native loading for static metadata plus out-of-process bridge for executable legacy-compatible behavior |
| Advanced subsystems | Remote, bridge, assistant/coordinator, and voice remain GA scope |

## Compatibility Status

| Surface | Status | Notes |
| --- | --- | --- |
| Top-level CLI flags and prompt mode | Native | Implemented in `crates/cli/src/main.rs` |
| Slash command registry | Native | Canonical registry lives in `crates/core/src/lib.rs` |
| Interactive REPL/TUI | Adapted | Ratatui UI preserves the main panes and operator flows, but not Ink internals |
| Session transcripts and resume | Native | JSONL append/read, compact boundaries, subagent transcript paths, and `.jsonl` resume are covered |
| Provider selection and auth env semantics | Native | `CLAUDE_CODE_*` and Codex auth snapshot behavior covered in `crates/providers` |
| Built-in file/shell/search/web/task tools | Native | Tool registry and permission metadata live in `crates/tools` |
| MCP transport and OAuth device flow | Native | Implemented in `crates/mcp` and exposed through tools/commands |
| Plugin metadata and skill discovery | Native | `.claude-plugin/plugin.json`, `.claude/skills`, and `.claude/commands` are loaded directly |
| Executable legacy plugin behavior | Adapted | Runs through the out-of-process compatibility bridge, not an embedded JS runtime |
| Remote/direct-connect/IDE bridge | Adapted | Rust-owned transport protocol replaces Anthropic-private cloud/session flows |
| Coordinator and agent workflow state | Adapted | Shared task/orchestration helpers live in `crates/core`, with bridge-driven execution in `crates/cli` |
| Voice transport | Deferred | Parsing and transport hooks exist, but voice parity remains intentionally incomplete |

## Fixture-backed parity coverage

| Fixture area | Path | Used by |
| --- | --- | --- |
| Slash command goldens | `fixtures/command-golden/slash-commands.json` | CLI tests |
| Transcript compatibility | `fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl` | Session tests |
| Provider parser coverage | `fixtures/provider-streams/*.json` | Provider tests |
| Plugin/skill compatibility | `fixtures/plugin-fixtures/review-tools/` | Plugin tests |

## Crate Ownership

| TypeScript Surface | Rust Crate |
| --- | --- |
| `src/main.tsx`, CLI startup, CLI handlers | `crates/cli` |
| `src/query.ts`, `src/bootstrap`, `src/state`, `src/tasks`, `src/coordinator`, `src/buddy`, `src/proactive` | `crates/core` |
| `src/services/api`, `src/utils/model`, `src/utils/auth.ts`, `src/utils/openaiAuth.ts` | `crates/providers` |
| `src/tools`, tool execution runtime, shell/file/network adapters | `crates/tools` |
| `src/services/mcp`, MCP-backed tool exposure | `crates/mcp` |
| `src/utils/sessionStorage.ts`, `src/services/compact`, `src/services/SessionMemory` | `crates/session` |
| `src/components`, `src/screens`, `src/outputStyles`, `src/keybindings`, `src/vim` | `crates/ui` |
| `src/plugins`, `src/skills`, `src/utils/plugins`, `src/utils/skills` | `crates/plugins` |
| `src/bridge`, `src/remote`, `src/server`, remote transports | `crates/bridge` |

## Validation Checklist

- Command names and slash-command behavior match the TypeScript source-of-truth set.
- Existing transcripts open without one-off migration.
- Provider selection behavior matches current `CLAUDE_CODE_*` env semantics.
- Plugin manifests and skill directories load without reorganization.
- Private analytics and Anthropic-private transport dependencies are absent from runtime-critical code.
- Fixture-backed coverage exists for command parsing, transcript resume/materialization, provider parser behavior, and plugin discovery.
