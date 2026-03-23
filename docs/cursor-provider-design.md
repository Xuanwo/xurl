# Cursor Provider Design Notes

## Status

Implemented for main-thread read, query, and write.

Current unsupported areas:

- role-based create
- child/subagent drill-down

## Purpose

This document records the verified Cursor Agent behavior that `xurl` relies on
today and the current integration contract for the `cursor` provider.

## Implemented Provider Contract

### Supported URI Forms

- `agents://cursor`
- `agents://cursor/<chat_id>`
- `agents://cursor?q=<keyword>`
- `agents:///path?providers=cursor`
- `agents://cursor -d "..."`
- `agents://cursor/<chat_id> -d "..."`

### Unsupported URI Forms

- `agents://cursor/<role> -d "..."`
- `agents://cursor/<chat_id>/<child_id>`

The current implementation returns explicit provider errors for these cases
instead of guessing Cursor-specific semantics.

### Root Resolution

`xurl` resolves the Cursor root in this order:

1. `CURSOR_DATA_DIR`
2. `CURSOR_CONFIG_DIR`
3. `~/.cursor`

The provider scans chats under:

- `<root>/chats/<project_id>/<chat_id>/store.db`

## Verified Storage Model

### 1. `store.db` Is the Source of Truth

Observed local chat storage uses:

- `~/.cursor/chats/<project_id>/<chat_id>/store.db`

The database schema is:

- `meta(key TEXT PRIMARY KEY, value TEXT)`
- `blobs(id TEXT PRIMARY KEY, data BLOB)`

The `meta` row keyed by `0` contains chat metadata, including the latest root
blob reference. Message reconstruction is derived from the blob graph, not from
an append-only transcript file.

### 2. Readable Transcript JSONL Exists but Is Not Canonical

Observed transcript output also exists under:

- `~/.cursor/projects/<workspace_key>/agent-transcripts/<chat_id>/<chat_id>.jsonl`

This export is useful for understanding Cursor layout, but it is not used as
the canonical render/query source in `xurl`.

### 3. Transcript Export Can Mix Visible Text and Reasoning

Observed transcript output can merge assistant-visible text and hidden reasoning
into the same serialized text stream.

Implications:

- transcript JSONL is not safe for user-facing rendering
- transcript JSONL is not safe for `q=` indexing

For this reason, `xurl` reconstructs Cursor conversations from `store.db` and
drops reasoning parts before rendering or search indexing.

## Read and Query Strategy

### Message Reconstruction

`xurl` hydrates the current blob graph from `store.db`, extracts structured
message payloads, and keeps only user-visible message content:

- `user` messages are rendered
- `assistant` messages are rendered
- `system` messages are skipped
- summary-only synthetic messages are skipped
- `reasoning` and `redacted-reasoning` parts are skipped

The hydrated conversation is materialized into an internal JSONL view so the
shared renderer and query pipeline can reuse the existing message logic.

### Query Semantics

Cursor query candidates are enumerated from `store.db` files under the Cursor
chat root.

For each candidate:

- thread metadata comes from hydrated store content
- `scope_path` is derived from the workspace file URI when present
- keyword search uses only the visible hydrated text

This keeps Cursor query behavior aligned with the `xurl` contract used by other
providers: find what the user saw, not hidden model reasoning.

## Write Strategy

### CLI Entry Points

Observed `cursor-agent` CLI behavior includes:

- `cursor-agent create-chat`
- `cursor-agent --resume <chat_id>`
- `cursor-agent --continue`
- `cursor-agent --print`
- `cursor-agent --output-format text|json|stream-json`

Official references:

- <https://docs.cursor.com/en/cli/using>
- <https://docs.cursor.com/zh/cli/reference/parameters>

### Current `xurl` Behavior

`xurl` uses `cursor-agent` for non-interactive write flows:

- create:
  - `cursor-agent create-chat`
  - `cursor-agent --resume <chat_id> --print --output-format stream-json`
- append:
  - `cursor-agent --resume <chat_id> --print --output-format stream-json`

The implementation parses `stream-json` output, captures assistant-visible text,
and returns the final visible response.

Role-based create stays unsupported because Cursor `mode` does not currently map
to a stable `xurl` role concept.

## Subagent Status

Cursor bundle layout suggests subagent transcript support may exist, but this is
not implemented in `xurl` yet.

Reasons:

- no verified parent-child contract is enforced in the current provider path
- no sanitized real fixture for Cursor child drill-down is committed
- transcript path shape alone is not enough to guarantee correct navigation

`xurl` therefore treats Cursor subagent drill-down as unsupported for now.

## Constraints and Follow-Ups

- Cursor storage is more structured than other local providers, so provider code
  must keep following verified store semantics instead of transcript shortcuts.
- If Cursor later exposes durable named agents, role URI support can be revisited.
- If a real child-thread fixture is added, subagent support can be implemented on
  top of the same provider contract without changing main-thread URI behavior.
