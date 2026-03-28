# Path-Scoped Query URI Design

## Status

Proposed

## Relation to Unified URI Model

This document defines the provider-agnostic path-scoped query part of the unified `agents://` URI system.

See also:

- [Agents URI Design](./agents-uri-design.md)
- [Subagent URI Design Across Providers](./subagent-uri-design.md)

## Context

`xurl` already supports provider-scoped URIs such as `agents://codex/...` and provider-scoped queries such as `agents://codex?q=...`.

What it does not have today is a provider-agnostic way to query conversations by local working path. The goal is to make path scope a first-class concept without overloading provider URIs or introducing ambiguous global aliases.

## Goals

- Keep existing provider-scoped URI behavior unchanged.
- Add one canonical URI form for path-scoped queries.
- Support convenient path shorthand based on current working directory and home directory.
- Keep all provider-agnostic forms list-shaped rather than single-thread aliases.
- Preserve one canonical output form after normalization.

## Non-Goals

- No `agents://current` alias.
- No provider-agnostic global query form such as `agents://?...`.
- No arbitrary relative path grammar outside `.` / `..` and `~`.
- No path canonicalization that depends on filesystem existence or symlink resolution.

## URI Model

### Provider-Scoped URIs (unchanged)

- `agents://codex`
- `agents://codex/<session_id>`
- `agents://codex/<role>`
- `agents://codex/<session_id>/<child_id>`

These continue to mean provider-scoped query, thread read, role-scoped query, and child drill-down.

### Canonical Path-Scoped Query URI

- `agents:///abs/path`
- `agents:///abs/path?q=refactor&limit=20`

This is the only canonical provider-agnostic URI form.

It always means:

- query conversations under a local path scope
- return a collection result
- never resolve directly to one thread

### Path Shorthand Input Forms

- `agents://.`
- `agents://./subdir`
- `agents://..`
- `agents://../repo`
- `agents://~`
- `agents://~/repo`

These are input shorthands only. They must normalize to `agents:///abs/path?...` before the normal query pipeline runs.

## Why Triple Slash

`agents://dir/to/path` is not suitable because `dir` is read as the URI authority and visually collides with provider forms such as `agents://codex/...`.

`agents:///abs/path` follows the same absolute-path shape as `file:///abs/path`:

- empty authority
- absolute local path in the URI path component

This makes the path scope explicit and keeps it disjoint from provider names.

## Normalization Rules

### Canonical Absolute Path

- `agents:///abs/path?...` stays unchanged.

### Current Working Directory Relative Shorthand

Resolve against process `cwd`:

- `agents://.` -> `agents:///cwd`
- `agents://./subdir` -> `agents:///cwd/subdir`
- `agents://..` -> `agents:///parent-of-cwd`
- `agents://../repo` -> `agents:///parent-of-cwd/repo`

### Home Relative Shorthand

Resolve against process home directory:

- `agents://~` -> `agents:///home`
- `agents://~/repo` -> `agents:///home/repo`

### Path Processing

Normalization should:

- join against `cwd` or `HOME` lexically
- collapse `.` and `..` segments lexically
- preserve query parameters

Normalization should not:

- require the target path to exist
- call `realpath`
- resolve symlinks

The URI model should stay stable even when the filesystem changes.

## Query Semantics

Path-scoped URIs always produce a collection query.

Default behavior:

- no `q` means recent conversations under the path scope
- `q=<keyword>` filters the collection by keyword
- results are sorted by `updated_at desc`
- default `limit` is `10`

Recommended first-stage query parameters:

- `q`
- `limit`
- `providers`

Possible later parameters:

- `days`
- `since`
- `match=tree|exact`

## Scope Matching

Each provider should expose one derived `scope_path` for query filtering:

- Codex: session metadata `cwd`
- Claude: project path or original path
- Gemini: project root
- Pi: session header `cwd`
- OpenCode: session `directory`
- Amp: thread or history `cwd`

Recommended default matching mode is `tree`:

- match when `scope_path == requested_path`
- match when `scope_path` is under `requested_path`

This makes `agents:///repo` include sessions started from `/repo` and nested work directories under `/repo/...`.

`exact` can be added later if strict equality becomes necessary.

## Parsing Priority

The parser should resolve URI forms in this order:

1. `agents:///...`
2. `agents://.` / `agents://..` / `agents://./...` / `agents://../...`
3. `agents://~` / `agents://~/...`
4. existing provider-scoped URI parsing

This keeps shorthand path forms explicit while avoiding collisions with provider names.

## Output Contract

Canonical output should never preserve shorthand.

Examples:

- input `agents://.?q=refactor`
- normalized `agents:///Users/alice/repo?q=refactor`

- input `agents://~/work/xurl`
- normalized `agents:///Users/alice/work/xurl`

All rendered query metadata should expose the normalized canonical URI rather than the original shorthand input.

## Compatibility Notes

- Existing provider URIs remain unchanged.
- Existing provider-scoped query behavior remains unchanged.
- Path-scoped query adds a new provider-agnostic collection mode only.
- No single-thread alias is introduced by this design.

## Implementation Direction

Recommended layering:

1. Add a path-query resolver that recognizes canonical path URIs and path shorthands.
2. Normalize shorthand input into `agents:///abs/path?...`.
3. Build a provider-agnostic query request from the normalized path.
4. Reuse provider candidate collection and apply path filtering before final sorting and limiting.

This keeps provider-specific parsing and provider-specific read flows stable while introducing one new query axis: local path scope.
