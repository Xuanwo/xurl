# Agent Guidelines

## Workspace Responsibilities
- `xurl-core`: all URI parsing, provider resolution, raw file reading, and markdown rendering lives here. It owns provider-specific parsers for Codex, Claude, and OpenCode plus the shared service layer (`resolve_thread`, `render_thread_markdown`, `render_thread_head_markdown`).
- `xurl-cli`: thin CLI that parses `xurl <uri>` arguments with `clap`, wires up `ProviderRoots::from_env_or_home`, emits metadata warnings to `stderr`, and prints rendered markdown (`render_thread_markdown`).

## CLI Parameter & Provider Behavior Matrix
- The CLI accepts a single `<uri>` and an optional `-I/--head` flag for frontmatter-only output.
- `ProviderRoots::from_env_or_home` sources the base directories using:
  - Codex: `CODEX_HOME` then `~/.codex`
  - Claude: `CLAUDE_CONFIG_DIR` then `~/.claude`
  - OpenCode: `XDG_DATA_HOME/opencode` then `~/.local/share/opencode`
- `render_thread_markdown` converts provider payloads into a markdown thread view with frontmatter metadata plus user/assistant-focused content; warnings from `resolved.metadata.warnings` are emitted to `stderr` before the primary output.

## Design Docs
- Design documents live under `docs/`.
- Before proposing, implementing, or revising URI behavior, query behavior, or other user-facing semantics, read the relevant design docs in `docs/` first and align with them.
- If implementation diverges from an existing design doc, update the design doc in the same change or explain clearly why the divergence is intentional.

## Error Handling & Exit Contract
- `main` maps any `xurl_core::Result` failure to a non-zero exit code (1) and prints `error: <message>` on `stderr`; successes return exit code 0.
- Common failure cases include invalid URI syntax, missing provider roots, unresolved session IDs, unreadable files, empty files, and non-UTF-8 payloads (`read_thread_raw` explicitly guards against empty and non-UTF8 data).
- Metadata warnings and diagnostics are printed on `stderr` but do not change the exit code, making it clear that only `Err` results trigger failure.

## Style, Lint & Test Constraints
- The workspace enforces `cargo clippy` with `all = warn` and `pedantic = warn` at the root (`Cargo.toml` workspace lints); follow Rust formatting conventions and keep identifiers/comments in English.
- Tests are scoped to the crates (`xurl-cli/tests/cli.rs` for argument coverage and markdown output, `xurl-core` unit tests for file-reading edge cases). Run `cargo test` when touching parsing, rendering, or CLI behaviors.

## Provider Fixture Policy
- Use the same fixture method for Codex, Claude, OpenCode, and Gemini integration tests: start from local real thread files, then sanitize them for repository fixtures.
- Keep the original record structure unchanged (`JSON/JSONL` object shape, event layout, field presence, scalar types). Randomize string content only.
- Preserve parser-critical discriminator values and keys (for example event/type names and stable schema keys) so runtime parsing behavior stays representative.
- Replace sensitive or identifying string values with deterministic random placeholders, including nested serialized JSON inside string fields such as `arguments` and `output`.
- Preserve cross-record and cross-file linkage consistency after sanitization (for example main thread id, subagent id, `call_id`, and parent-child references).
- Store provider fixtures under `xurl-cli/tests/fixtures/<provider>_real_sanitized/` with a small `manifest.json` that documents fixture ids used by tests.
- Prefer medium-size real samples that still cover target behavior (for example subagent lifecycle for Claude/Codex, message/parts for OpenCode, session/message variants for Gemini) to keep repository size reasonable.
- Every new sanitized fixture must be exercised by at least one CLI integration test in `xurl-cli/tests/cli.rs`.

## Documentation Sync Requirement
- Any new feature, behavior change, provider support, URI rule update, or command usage change must update both:
  - `README.md`
  - `skills/xurl/SKILL.md`
- A change is not complete if runtime behavior and skill/readme docs diverge.

## Documentation Audience & Style
- Treat documentation as user-facing by default. Focus on capabilities and usage, not implementation details.
- Do not frame docs as "what xurl is not". State what users can do and how to do it.
- Keep wording task-oriented and action-first (for example: read, discover, start, continue, save).
- Prefer conversation-oriented language in user docs. Use `thread/session` only when required by literal CLI syntax, URI forms, or field names.
- `README.md` is for human users:
  - prioritize quick onboarding, command examples, option semantics, provider capability boundaries, and troubleshooting.
  - avoid internal architecture, backend adapter design, and code-structure explanations unless strictly necessary.
- `skills/xurl/SKILL.md` is for agent users:
  - prioritize execution workflow (`when to use`, `read/discover/write steps`, command rules, failure handling).
  - provide concise decision rules that help agents choose the right command without background theory.
- Keep `README.md` and `skills/xurl/SKILL.md` consistent in terminology, capability matrix, and command behavior.

## Minimal Change Strategy for Agents
- Keep patches as small as possible: touch only the crate that owns the behavior, avoid cross-crate refactors unless the fix explicitly requires both `xurl-core` and `xurl-cli`.
- Unless the user asks for new optional behavior, do not add new dependencies, features, or files; if you see adjacent concerns, surface them as follow-up items instead of folding them into the current change.
- Document any deviation from the existing light-touch approach (e.g., introducing new public interfaces or broader renders) so reviewers know why the scope expanded.

## Branch Safety Rule
- If the user is working on `main`, do not create a new branch and do not switch branches unless the user explicitly asks for it.
- If branch-based workflow could help, mention it as an optional follow-up instead of doing it by default.

## Release Rule
- When the user asks for a release, create a GitHub Release together with the version tag; do not stop at creating only a tag.
- Fill the release changelog based on the actual changes included in that release.
