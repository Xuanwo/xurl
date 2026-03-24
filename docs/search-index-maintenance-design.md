# Search Index Maintenance Design

## Status

Implemented

## Purpose

This document defines how `xurl` maintains its optional local search index without introducing a user-facing command, a daemon, or a long-running background service.

The search index is a cache:

- it accelerates query candidate pruning
- it may cache provider discovery state such as manifests or watermarks
- it is never the source of truth
- it may be deleted and rebuilt at any time
- failures in the index layer must not affect normal `xurl` correctness

## Current Architecture

The implemented cache stack has four layers:

1. provider source of truth
   - provider transcript files or provider-owned sqlite databases
2. provider discovery cache
   - `provider_manifest`
   - `provider_watermarks`
3. thread materialization cache
   - `thread_materialization`
   - stores `search_text`, `scope_path`, and the source fingerprint that produced them
4. search index
   - `thread_fts`
   - used only for query candidate pruning

The key separation is:

- discovery cache avoids rescanning provider metadata
- materialization cache avoids rereading unchanged transcripts
- `thread_fts` avoids rescanning full text during query

All four layers live in one sidecar sqlite file so version mismatch can be handled by deleting one cache family and rebuilding.

## Goals

- Keep the index fully optional
- Avoid user-visible index management commands
- Avoid a daemon, watcher, or persistent worker process
- Keep query/read/write correctness independent from the index
- Move non-essential index maintenance off the foreground command path

## Non-Goals

- No public `index` subcommand
- No user-visible background service lifecycle
- No requirement that the index is globally up to date before every query
- No index-only query execution path that bypasses transcript verification

## Internal Worker Entry

`xurl` may launch itself in an internal maintenance mode:

```bash
xurl -X index <uri>
```

This mode is internal:

- `-X` is hidden from normal help output
- it is not documented in `README.md`
- it is not documented in `skills/xurl/SKILL.md`
- normal user workflows must not depend on calling it directly

The internal worker exists only so the foreground process can offload cache maintenance into a short-lived child process.

## Why Hidden Argv Instead of Env or Public Command

Hidden argv is preferred because:

- it keeps behavior explicit inside the process model
- it avoids environment-variable protocol drift
- it avoids exposing a user-facing command for an implementation detail
- it remains easy to debug in logs or process listings

This is still an internal contract, not a public interface.

## Foreground vs Background Responsibilities

### Foreground Command

Normal `xurl` invocations should:

- read from the existing search index when available
- fall back to direct transcript scanning when the index misses, is stale, or is unavailable
- keep the current thread visible after successful read/write hot paths
- optionally enqueue or trigger background maintenance after the main result is already ready

Foreground commands should not spend meaningful time doing broad index backfill.

### Background Worker

The internal worker should:

- resolve the requested URI scope
- collect candidates for that scope
- refresh stale or missing index entries
- optionally prune entries that no longer exist within that scope
- finish the requested scope in one run when possible

The worker should be best-effort only. Failures must not affect the parent command.

The implemented worker path currently focuses on refresh rather than explicit scoped prune. Missing or invalid threads naturally disappear when provider discovery data is rebuilt.

## Trigger Model

After a normal command succeeds, `xurl` may best-effort spawn:

```bash
xurl -X index <narrowest-relevant-uri>
```

Examples:

- provider query:
  - parent command: `xurl 'agents://codex?q=refactor'`
  - spawned worker: `xurl -X index agents://codex?q=refactor`
- path query:
  - parent command: `xurl 'agents:///repo?providers=codex,claude'`
  - spawned worker: `xurl -X index 'agents:///repo?providers=codex,claude'`
- thread read:
  - parent command: `xurl agents://codex/<session_id>`
  - spawned worker: `xurl -X index agents://codex/<session_id>`

The parent process must not wait for the child worker.

## Scope Rules

The worker scope is derived from the URI:

- `agents://<provider>`
  - refresh candidates for that provider
- `agents:///abs/path?...`
  - refresh candidates only under that path scope and provider filter
- `agents://<provider>/<session_id>`
  - refresh only that thread

This keeps maintenance narrow and aligned with actual usage.

## Correctness Contract

The search index must not be required for correctness.

That means:

- query results must still be found when the index is missing
- stale index entries must not suppress correct fallback matches
- index corruption must not cause normal read/query/write failure

If the index layer fails, `xurl` must degrade to direct scanning.

## Failure Handling

The index is cache state, so failure handling should be aggressive and simple.

Recommended policy:

- open failure: ignore the index for this command
- sqlite version mismatch: delete the local sqlite files and recreate the cache
- corruption: delete and recreate the index
- worker failure: ignore and continue
- worker spawn failure: ignore and continue

None of these should change the main command exit code.

## Concurrency Control

Multiple foreground commands may try to launch `-X index`.

The worker should use a lightweight lease so only one worker performs maintenance at a time.

Recommended options:

- sqlite lease row with expiry
- lock file in the index state directory

If the lease cannot be acquired, the worker should exit immediately.

Because the index is only a cache, missed maintenance is acceptable.

## Sidecar Cache Contents

The sidecar sqlite is not limited to FTS rows.

It may store:

- transcript search documents used by `thread_fts`
- transcript materialization rows used to rebuild `thread_fts` without rereading source files
- provider discovery manifests keyed by provider
- provider-level watermarks used to decide whether discovery data can be reused
- lightweight worker lease metadata

This keeps all cache state in one place and allows `xurl` to delete and rebuild the whole sqlite file when the cache schema changes.

In the current implementation the cache version is controlled by one global sqlite `user_version`, not per-table migration state.

## Sqlite Versioning

The cache sqlite should use a single global version via `PRAGMA user_version`.

If the on-disk version does not match the runtime version, `xurl` should:

1. delete the sqlite database together with its `-wal` and `-shm` files
2. recreate the cache from scratch

Because the index is optional and rebuild cost is acceptable, whole-file replacement is preferred over in-place migration.

## Provider Discovery Cache

Providers may benefit from caching candidate discovery separately from transcript search text.

Two cache forms are supported:

- watermark:
  - a cheap provider-level summary that tells whether the provider's discovery source has changed
- manifest:
  - the cached list of discovered threads plus lightweight metadata such as URI, source path, and optional scope path

When a provider watermark matches, the worker may reuse the manifest from the sidecar sqlite instead of rescanning the provider's external state.

The manifest is still cache data:

- transcript freshness must still be validated from current source metadata before trusting indexed search text
- missing or changed transcripts must not be hidden by manifest reuse

### Initial Provider Target

`codex` is the first provider to use this pattern.

Its external `state.sqlite` files provide:

- a natural provider discovery source
- a cheap watermark based on sqlite file metadata
- enough thread identity data to rebuild a provider manifest inside the sidecar cache

For `codex`, path-scoped maintenance also benefits from caching `scope_path` inside both the provider manifest and thread materialization rows, so path queries do not need to reread all transcripts just to recover scope metadata.

## Materialization Cache

The sidecar sqlite may also store a materialization cache per thread:

- `search_text`
- cached `scope_path`
- the source fingerprint that produced them

This cache exists so maintenance can distinguish between:

- source work:
  - reread and reparse the original transcript
- index work:
  - repopulate `thread_fts` from already materialized text

When the source fingerprint still matches, the worker should prefer rebuilding `thread_fts` from cached materialization data instead of rereading the transcript.

The current implementation also distinguishes between:

- materialized insert
  - FTS row missing, materialization row already present
- materialized replace
  - both rows present but FTS row stale or missing a fresh fingerprint
- built insert
  - transcript must be read and the FTS row does not exist yet
- built replace
  - transcript must be read and an old FTS row must be replaced

This split exists to avoid unnecessary `DELETE` work for rows that do not yet exist in `thread_fts`, which materially reduces full-build and rebuild cost.

## Index Update Strategy

### Query Path

Normal query should:

1. use the search index as the first lookup path when the query shape and provider support a safe fast path
2. verify indexed hits against the current transcript source before returning them
3. fall back to candidate collection and direct scanning when the fast path cannot satisfy the request
4. return results

It should not do broad in-process index refresh beyond trivial current-thread work.

This is implemented today:

- provider and path queries use safe index-first fast paths for supported query shapes
- indexed hits are always revalidated against current transcript source before being returned
- unsupported or insufficient fast-path results fall back to the original candidate scan path

### Read Path

A successful thread read may refresh that exact thread synchronously because:

- the raw transcript is already loaded
- the scope is narrow
- this preserves strong local visibility for recently opened threads

This is implemented today and writes both `thread_fts` and `thread_materialization` for the exact thread when possible.

### Write Path

A successful write may refresh the resulting thread synchronously when resolvable.

If the provider has not materialized the final transcript yet, the worker can catch it later.

This is implemented as a best-effort refresh and does not change write correctness if the cache path fails.

## Worker Runtime

The worker should prefer completing the requested scope in one run.

The index is already outside the foreground command path, so partial maintenance should not be the default behavior.

If future providers require chunked maintenance because one scope becomes too large, that should be added as a provider-specific continuation mechanism rather than a blanket global limit.

## Current Performance Interpretation

The current architecture has established these performance boundaries:

- query latency is mainly improved by `thread_fts` fast paths
- provider maintenance latency is reduced first by provider manifest reuse
- transcript reread cost is reduced by thread materialization reuse
- remaining rebuild cost is dominated by actual `thread_fts` insertion work

This means future maintenance optimization should focus on FTS write strategy before adding more caching layers.

## Relationship to User Docs

User-facing docs should only describe the user-visible behavior:

- repeated queries may become faster over time
- no daemon is required
- direct scanning remains the fallback path

User-facing docs should not mention `-X index`.

Internal regression benchmarking for this cache layer is defined separately in `docs/search-index-benchmark-design.md`.
