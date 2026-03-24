# Search Index Benchmark Design

## Status

Implemented

## Purpose

This document defines a repository benchmark for the optional local search index so `xurl` can track performance regressions without depending on ad-hoc scripts.

The benchmark lives under `xurl-core/benches/` and runs via `cargo bench`.

## Entry

Run the benchmark with:

```bash
cargo bench -p xurl-core --bench search_index
```

Optional tuning arguments are passed after `--`:

```bash
cargo bench -p xurl-core --bench search_index -- --threads 2500 --samples 5
```

## Scope

The current benchmark covers the `codex` provider only.

Reasons:

- it is the simplest file-backed provider to synthesize deterministically
- it exercises the same collection-query and index-maintenance path used by production code
- it avoids provider-specific database setup that would make regression data noisier

If other providers need dedicated regression coverage later, they should add separate benchmark modes instead of stretching this one into a generic cross-provider harness.

This benchmark is intended to track three different cost centers separately:

- foreground query latency
- background refresh latency
- FTS rebuild latency after discovery and materialization are already cached

## Workload Model

The benchmark generates a synthetic `CODEX_HOME` corpus inside a temporary directory.

Default corpus:

- `2500` transcripts
- one target transcript containing the cold and warm query keyword
- one separate transcript rewritten later for incremental refresh measurement

The benchmark uses fixed keywords:

- base query: `bench-hit`
- incremental query: `bench-incremental-hit`

This keeps comparisons stable across runs.

## Measured Scenarios

Each scenario uses an isolated temporary workspace so results do not bleed into each other.

### Cold Query

- index enabled
- index initially empty
- measures foreground query latency with index-open overhead but without prior maintenance

### No-Index Query

- `XURL_DISABLE_SEARCH_INDEX=1`
- measures direct-scan latency without any index behavior

### Full Build

- runs `maintain_search_index_for_uri("agents://codex", ...)`
- measures one full background build for the benchmark scope

### FTS Rebuild

- builds the full index first
- deletes only `thread_fts` rows while keeping other sidecar cache state
- runs `maintain_search_index_for_uri("agents://codex", ...)` again
- measures how quickly the worker can rebuild search rows from cached materialization data

### Warm Query

- builds the full index first
- then runs the foreground query
- measures steady-state query latency with a populated index

### Incremental Refresh

- builds the full index first
- rewrites one transcript
- runs `maintain_search_index_for_uri("agents://codex", ...)` again
- measures the incremental maintenance cost

### Incremental Query

- runs immediately after incremental refresh
- measures query latency for the newly indexed keyword

## Isolation Rules

The benchmark must avoid contaminating its own timings.

Rules:

- every sample uses a fresh corpus and fresh index path
- the benchmark exercises `xurl-core` APIs directly
- search-index env overrides are scoped and restored inside the bench process

This keeps the benchmark reproducible while avoiding unrelated CLI process overhead.

It also means:

- `full_build` includes provider discovery, transcript materialization, and FTS insertion
- `fts_rebuild` isolates the FTS insertion path much more closely
- `incremental_refresh` reflects the common case where only a very small subset of threads has changed

## Output Contract

The benchmark prints JSON to stdout.

The output must include:

- benchmark metadata
- corpus size and sample count
- per-sample arrays for each scenario
- summary averages
- derived comparison fields such as cold overhead and warm speedup

The JSON shape is intended to be stable enough for simple diffing in scripts or CI logs.

## Tuning

The benchmark accepts:

- `--threads`
- `--samples`

These exist so tests and faster local runs can shrink the corpus without changing the benchmark logic.

The defaults should remain biased toward signal quality rather than minimal runtime.
