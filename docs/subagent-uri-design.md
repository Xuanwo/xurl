# Subagent URI Design Across Providers

## Status

Proposed

## Relation to Unified URI Model

This document defines the provider-scoped child drill-down part of the unified `agents://` URI system.

See also:

- [Agents URI Design](./agents-uri-design.md)
- [Path-Scoped Query URI Design](./path-query-uri-design.md)

## Context

`xurl` currently resolves a single thread URI into one local thread file and renders a timeline view. This works for primary conversations, but it does not provide a first-class way to inspect subagent lifecycle state or drill down into a specific subagent context under a parent thread.

The existing URI behavior is inconsistent with subagent use cases because it only models one `session_id` and does not encode parent/child scope in the URI itself.

## Goals

- Keep backward compatibility for existing provider URIs.
- Use one URI shape across providers for subagent drill-down.
- Support both aggregate metadata discovery and single-agent drill-down.
- Use one explicit CLI mode switch (`-I/--head`) for metadata-only output.
- Make markdown metadata stable for automation consumers.

## Non-Goals

- Defining provider-specific transport details for remote RPC.
- Replacing the existing single-thread render pipeline.
- Introducing provider-specific query parameter syntax for subagent views.

## Unified URI Model

### Existing URIs (unchanged)

- `agents://<provider>`
- `agents://<provider>/<thread_id>`
- `agents://<provider>/<role>`

Within the unified `agents://` model:

- `agents://<provider>` stays provider-scoped query
- `agents://<provider>/<thread_id>` stays main thread read
- `agents://<provider>/<role>` stays role-scoped query or role-based create

### New Drill-Down URI (provider-consistent)

- Drill down into one subagent:
  - `agents://<provider>/<main_thread_id>/<agent_id>`

## CLI Mode Model

### Aggregate Metadata

- Aggregate subagents/entries under a parent thread is triggered by `--head`:
  - `xurl -I 'agents://<provider>/<main_thread_id>'`

### Single-Agent Drill-Down

- Drill-down view uses the URI path segments under the provider-scoped form:
  - `xurl 'agents://<provider>/<main_thread_id>/<agent_id>'`

### Mode Constraints

- `--head` can be used with both parent and drill-down URIs.
- `--head` always renders frontmatter-only markdown output.

## Provider Mapping

### Codex

- `agent_id` is treated as a subagent identifier scoped by `main_thread_id`.
- Current local evidence shows `agent_id` commonly equals child thread id, but implementation must still validate parent-child relation.
- Parent lifecycle is inferred from tool calls such as `spawn_agent`, `wait`, `send_input`, `resume_agent`, and `close_agent`.

### Claude

- `agent_id` maps to transcript field `agentId`.
- Candidate files are discovered from:
  - `<project>/<main_session_id>/subagents/agent-*.jsonl`
  - `<project>/agent-*.jsonl` filtered by `sessionId == main_session_id`
- Validation should require `isSidechain == true` and matching `sessionId`.

## Resolution Flow

### Aggregate: `-I agents://<provider>/<main>`

1. Resolve and load parent thread.
2. Discover child/subagent records for that provider.
3. Validate parent-child linkage.
4. Build per-agent status summary.
5. Render frontmatter only, including discovery lists (`subagents` / `entries`) when available.

### Drill-Down: `agents://<provider>/<main>/<agent>`

1. Resolve and load parent thread.
2. Locate target agent/thread using provider mapping rules.
3. Validate linkage between parent and agent.
4. Build lifecycle summary from parent and excerpt from agent transcript.
5. Render combined markdown view (default mode) or frontmatter only (`--head`).

## Status Normalization

Preferred normalized states:

- `pendingInit`
- `running`
- `completed`
- `errored`
- `shutdown`
- `notFound`

Each response should include `status_source`, for example:

- `protocol`
- `parent_rollout`
- `child_rollout`
- `inferred`

## Output Contract

### Markdown

Use a consistent section layout:

1. `Agent Status Summary`
2. `Lifecycle (Parent Thread)`
3. `Thread Excerpt (Child Thread)`

### Frontmatter

Single-thread timeline output includes YAML frontmatter fields for machine use:

- `uri`
- `thread_source`
- `provider`
- `session_id`
- `mode`
- `subagents` (Codex/Claude parent thread in head mode)
- `entries` (Pi parent thread in head mode)

## Compatibility Rules

- Existing single-thread URIs must behave exactly as today.
- New subagent support must not require query parameters.
- Parser must reject malformed path shapes with actionable errors.
- CLI must reject invalid mode combinations with actionable errors.

## Risks

- Codex local rollout may miss complete collaboration events.
- Claude status is inferred from local transcripts, not protocol-native.
- `agent_id == child_thread_id` in Codex is observational, not guaranteed by contract.

## Test Scope

- URI parsing unit tests:
  - existing URIs
  - `agents://<provider>/<main>/<agent>`
  - malformed path rejection
- CLI argument tests:
  - `-I agents://<provider>/<main>`
  - `-I agents://<provider>/<main>/<agent>`
  - invalid `--list` (unsupported flag)
- Provider tests:
  - Codex parent-child validation and lifecycle extraction
  - Claude file discovery in both known layouts
- CLI integration tests:
  - markdown for aggregate and drill-down URIs
  - stderr warnings and exit-code behavior unchanged
