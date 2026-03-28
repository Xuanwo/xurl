# Agents URI Design

## Status

Proposed

## Purpose

This document defines the top-level URI model for `xurl`.

Two detailed design documents extend this model:

- [Path-Scoped Query URI Design](./path-query-uri-design.md)
- [Subagent URI Design Across Providers](./subagent-uri-design.md)

The goal is to keep one coherent URI system instead of growing provider-specific and query-specific exceptions independently.

## Design Principles

- Keep provider-scoped thread access stable.
- Use `agents://` as the shared URI family for conversation access.
- Separate provider identity from local path scope.
- Keep path-scoped forms collection-shaped.
- Keep shorthand input forms canonicalizable.

## URI Families

### 1. Provider-Scoped URIs

These target one provider directly.

Examples:

- `agents://codex`
- `agents://codex/<session_id>`
- `agents://codex/<role>`
- `agents://codex/<session_id>/<child_id>`

These URIs cover:

- provider-scoped query
- main thread read
- role-scoped query
- child/subagent drill-down

### 2. Path-Scoped Query URIs

These target a local path scope rather than a provider name.

Canonical form:

- `agents:///abs/path`
- `agents:///abs/path?q=refactor&limit=20`

Input shorthand:

- `agents://.`
- `agents://./subdir`
- `agents://..`
- `agents://../repo`
- `agents://~`
- `agents://~/repo`

These URIs always mean collection query. They do not directly identify one thread.

## Canonicalization Rules

### Canonical Provider Form

Provider-scoped canonical URIs stay provider-scoped:

- `agents://codex/<session_id>`
- `agents://claude/<session_id>/<agent_id>`

### Canonical Path Form

All path shorthand input must normalize to:

- `agents:///abs/path?...`

Examples:

- `agents://.` -> `agents:///current/cwd`
- `agents://../repo` -> `agents:///parent/repo`
- `agents://~/work/xurl` -> `agents:///home/work/xurl`

Normalization is lexical:

- join relative input against `cwd` or `HOME`
- collapse `.` and `..`
- preserve query parameters

Normalization should not depend on path existence or symlink resolution.

## Parsing Priority

The URI parser should resolve input in this order:

1. canonical path query: `agents:///...`
2. path shorthand from `cwd`: `agents://.` / `agents://..` / descendants
3. path shorthand from home: `agents://~` / `agents://~/...`
4. existing provider-scoped parsing

This order keeps `.` / `..` / `~` unambiguous without weakening provider names.

## Query Shape vs Thread Shape

The design intentionally separates:

- collection-shaped URIs
- single-thread URIs

Collection-shaped:

- `agents://codex`
- `agents://codex?q=...`
- `agents://codex/<role>`
- `agents:///abs/path?...`

Single-thread:

- `agents://codex/<session_id>`
- `agents://claude/<session_id>`
- `agents://codex/<main_thread_id>/<child_id>`

This prevents path-scoped inputs from implicitly acting like unstable aliases to one thread.

## Compatibility Boundaries

- Existing provider-scoped thread URIs remain valid.
- Existing provider-scoped query behavior remains valid.
- Path-scoped query is additive.
- No `agents://current` alias is part of this design.
- No provider-agnostic global query form such as `agents://?...` is part of this design.

## Implementation Layers

Recommended layers:

1. URI classification
2. shorthand normalization
3. provider-scoped resolution or path-scoped query construction
4. existing render/query pipelines

This keeps the new design incremental:

- provider readers remain provider-specific
- path-scoped query becomes one extra selector axis
- subagent drill-down remains provider-scoped
