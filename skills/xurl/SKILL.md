---
name: xurl
description: Use xurl to read, discover, and write AI agent conversations through agents:// URIs, and read skills through skills:// URIs.
---

## When to Use

- User gives `agents://...` URI.
- User gives shorthand URI like `codex/...` or `codex?...`.
- User gives `skills://...` URI.
- User names a skill and asks to load or learn it.
- User asks to list/search provider threads.
- User asks to query role-scoped threads like `agents://codex/reviewer`.
- User asks to read or summarize a conversation.
- User asks to read local or GitHub-hosted skill content.
- User task requires capability not covered by current loaded context.
- User asks to discover child targets before drill-down.
- User asks to start or continue conversations for providers.

## Installation

Pick up the preferred ways based on current context:

### Homebrew

Install via Homebrew tap:

```bash
brew tap xuanwo/tap
brew install xurl
xurl --version
```

Upgrade via Homebrew:

```bash
brew update
brew upgrade xurl
```

### Python Env

install from PyPI via `uv`:

```bash
uv tool install xuanwo-xurl
xurl --version
```

Upgrade `xurl` installed by `uv`:

```bash
uv tool upgrade xuanwo-xurl
xurl --version
```

### Node Env

Temporary usage without install:

```bash
npx @xuanwo/xurl --help
```

install globally via npm:

```bash
npm install -g @xuanwo/xurl
xurl --version
```

Upgrade `xurl` installed by npm:

```bash
npm update -g @xuanwo/xurl
xurl --version
```

## Core Workflows

### 1) Query

List latest provider threads:

```bash
xurl agents://codex
# equivalent shorthand:
xurl codex
```

Keyword query with optional limit (default `10`):

```bash
xurl 'agents://codex?q=spawn_agent'
xurl 'agents://claude?q=agent&limit=5'
```

Role-scoped query (session-first, role-fallback):

```bash
xurl agents://codex/reviewer
# equivalent shorthand:
xurl codex/reviewer
```

### 2) Read

```bash
xurl agents://codex/<conversation_id>
# equivalent shorthand:
xurl codex/<conversation_id>
```

### 3) Discover

```bash
xurl -I agents://codex/<conversation_id>
```

Use returned `subagents` or `entries` URI for next step.
OpenCode child linkage is validated by sqlite `session.parent_id`.

### 3.1) Drill Down Child Thread

```bash
xurl agents://codex/<main_conversation_id>/<agent_id>
```

### 4) Write

Create:

```bash
xurl agents://codex -d "Start a new conversation"
# equivalent shorthand:
xurl codex -d "Start a new conversation"
```

Append:

```bash
xurl agents://codex/<conversation_id> -d "Continue"
```

Create with query parameters:

```bash
xurl "agents://codex?cd=%2FUsers%2Falice%2Frepo&add-dir=%2FUsers%2Falice%2Fshared&model=gpt-5" -d "Review this patch"
```

Create with role URI:

```bash
xurl agents://codex/reviewer -d "Review this patch"
```

Payload from file/stdin:

```bash
xurl agents://codex -d @prompt.txt
cat prompt.md | xurl agents://claude -d @-
```

### 5) Read Skills

Read local skill:

```bash
xurl skills://xurl
```

Read GitHub skill:

```bash
xurl skills://github.com/Xuanwo/xurl/skills/xurl
```

Frontmatter only:

```bash
xurl -I skills://xurl
```

### 5.1) Dynamic Load and Learn

Use this protocol when the user references a skill URI, names a skill, or when the current task needs capability not covered by already loaded context.

Step 1: Resolve target URI.

- If user gives full URI, use it directly.
- If user gives only a skill name, build `skills://<skill-name>`.

Step 2: Probe metadata first.

```bash
xurl -I skills://<skill-name>
```

Step 3: Load full skill content.

```bash
xurl skills://<skill-name>
```

Step 4: Extract only execution-critical parts from the loaded skill.

- Trigger conditions (`When to Use`)
- Input requirements
- Workflow steps
- Failure handling and fallback

Step 5: Continue the original task with the extracted rules.

Guardrails:

- Do not use `-d` with `skills://` URIs.
- Do not append query parameters to `skills://` URIs.
- Load minimum required skills only; avoid bulk preloading.
- Deduplicate repeated loads of the same `skills://` URI in the same run.

## Command Reference

- Base form: `xurl [OPTIONS] <URI>`
- `-I, --head`: frontmatter/discovery only
- `-d, --data`: write payload, repeatable
  - text: `-d "hello"`
  - file: `-d @prompt.txt`
  - stdin: `-d @-`
- `-o, --output`: write command output to file
- `--head` and `--data` cannot be combined
- multiple `-d` values are newline-joined
- `--data` is not supported for `skills://` URIs

## URI Reference

URI Anatomy (ASCII):

```text
[agents://]<provider>[/<token>[/<child_id>]][?<query>]
|------|  |--------|  |---------------------------|  |------|
 optional   provider         optional path parts        query
 scheme
```

Component meanings:

- `scheme`: optional `agents://` prefix; omitted form is treated as shorthand
- `provider`: provider name
- `token`: main conversation id or role name
- `child_id`: child/subagent id
- `query`: optional key-value parameters

Token resolution (`agents://<provider>/<token>`):

1. Parse `<token>` as session id first.
2. If session-id parsing fails, treat `<token>` as role.

Common URI patterns:

- `agents://<provider>`: discover recent conversations
- `agents://<provider>/<conversation_id>`: read main conversation
- `agents://<provider>/<role>`: role-scoped thread query or role-based create with `-d`
- `agents://<provider>/<conversation_id>/<child_id>`: read child/subagent conversation
- `agents://<provider>?k=v` with `-d`: create
- `agents://<provider>/<conversation_id>` with `-d`: append

Role create behavior by provider:

- `codex`: supported (`[agents.<role>]` in `~/.codex/config.toml` mapped to `--config`)
- `claude`: supported (`--agent <role>`)
- `opencode`: supported (`--agent <role>`)
- `openclaw`: supported (`--agent <role>`)
- `amp`: returns clear error (non-interactive role create unsupported)
- `gemini`: returns clear error (non-interactive role create unsupported)
- `pi`: returns clear error (role create unsupported)

Skills URI patterns:

- `skills://<skill-name>`: read local skill from `~/.agents/skills/<skill-name>/SKILL.md`
- `skills://github.com/<owner>/<repo>/<skill-dir>`: read remote skill from cloned cache
- `skills://github.com/<owner>/<repo>`: auto-match skill; on ambiguity, `xurl` returns candidate URIs
- `skills://...` does not support query parameters

Query parameters:

- `q=<keyword>`: filter discovery results by keyword. Use when searching conversations by topic.
- `limit=<n>`: cap discovery results (default `10`). Use when you want fewer or more results.
- `<key>=<value>`: in write mode (`-d`), forwarded as `--<key> <value>` to the provider CLI.
- `<flag>`: in write mode (`-d`), forwarded as `--<flag>` to the provider CLI.

## Failure Handling

### `command not found: <agent>`

Install the provider CLI, then complete provider authentication before retrying.

### `multiple skills matched for uri=...`

Pick one URI from candidates and retry with the full candidate URI shown in the error output.

### `skill not found for uri=...`

Verify the skill name/path first, then retry. For GitHub URIs, prefer explicit `<skill-dir>` if repository contains multiple skills.

### `git command failed: ...` or `command not found: git`

Ensure `git` is installed and network access is available, then retry the same `skills://github.com/...` URI.

### `invalid skills uri: ...` or `unsupported skills host: ...`

Use supported forms only:

- `skills://<skill-name>`
- `skills://github.com/<owner>/<repo>[/<skill-dir>]`
