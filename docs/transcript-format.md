# Transcript Format

## V1 Contract

`code-agent-rust` reads and writes the current Claude Code JSONL transcript format directly.

The runtime preserves these user-visible behaviors:
- transcript file extension remains `.jsonl`
- resume supports both session id lookup and explicit `.jsonl` path input
- compact boundaries stay representable on disk
- subagent transcripts remain addressable under per-session subpaths
- transcript persistence remains append-oriented

## On-disk message model

Each JSONL line is a serialized canonical `Message` from `crates/core`.
Important fields:
- `id`
- `parent_id`
- `session_id`
- `role`
- `blocks`
- `metadata`
- `created_at_unix_ms`

The runtime stores canonical content blocks rather than provider wire payloads directly:
- `text`
- `tool_call`
- `tool_result`
- `attachment`
- `boundary`

## Compact boundary semantics

Compaction writes two transcript entries:
1. a synthetic assistant summary message tagged with `compact_summary`
2. a synthetic assistant boundary message tagged with `compact_boundary`

The summary message records:
- the compaction kind in `metadata.attributes.compaction_kind`
- the trigger in `metadata.attributes.compaction_trigger`
- summarized transcript lines in normal text content

The boundary block records:
- `kind`
- `summary_message_id`
- `preserved_tail_id`

`materialize_runtime_messages` reconstructs the replayable runtime view by:
- locating the latest boundary
- loading its referenced summary message
- appending the preserved tail starting at `preserved_tail_id`
- excluding older boundary markers and superseded compact summaries

## Resume behavior

`LocalSessionStore` and `ProjectSessionStore` support two resume inputs:
- session id
- explicit `.jsonl` path

A valid explicit path must still resolve to a transcript whose filename stem is a UUID session id.

## Subagent transcript paths

Subagent transcripts live under the session-specific subdirectory shape:

```text
<project-storage>/<session-id>/subagents/<optional-subdir>/agent-<agent-id>.jsonl
```

This keeps coordinator/worker and other subagent transcripts addressable without changing the top-level session transcript contract.

## Fixture coverage

The repository includes a compacted transcript fixture at:

- `fixtures/transcripts/77777777-7777-4777-8777-777777777777.jsonl`

That fixture is used to verify:
- JSONL decoding
- runtime materialization through a compact boundary
- explicit `.jsonl` resume behavior
