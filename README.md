# xURL

`xURL` is a client for AI agent URLs.

> Also known as **Xuanwo's URL**.

## What xURL Can Do

- Read an agent conversation as markdown.
- Query recent threads and keyword matches for a provider.
- Query role-scoped threads with `agents://<provider>/<role>`.
- Discover subagent/branch navigation targets.
- Start a new conversation with agents.
- Continue an existing conversation with follow-up prompts.

## Quick Start

1. Add `xurl` as an agent skill:

```bash
npx skills add Xuanwo/xurl
```

2. Start your agent and ask the agent to summarize a thread:

```text
Please summarize this thread: agents://codex/xxx_thread
```

## Providers

| Provider | Query | Create | Role Create |
| --- | --- | --- | --- |
| <img src="https://ampcode.com/amp-mark-color.svg" alt="Amp logo" width="16" height="16" /> Amp | Yes | Yes | No |
| <img src="https://avatars.githubusercontent.com/u/14957082?s=24&v=4" alt="Codex logo" width="16" height="16" /> Codex | Yes | Yes | Yes |
| <img src="https://www.anthropic.com/favicon.ico" alt="Claude logo" width="16" height="16" /> Claude | Yes | Yes | Yes |
| <img src="https://www.google.com/favicon.ico" alt="Gemini logo" width="16" height="16" /> Gemini | Yes | Yes | No |
| <img src=".github/assets/pi-logo-dark.svg" alt="Pi logo" width="16" height="16" /> Pi | Yes | Yes | No |
| <img src="https://opencode.ai/favicon.ico" alt="OpenCode logo" width="16" height="16" /> OpenCode | Yes | Yes | Yes |

## Usage

Read an agent conversation:

```bash
xurl agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
# equivalent shorthand:
xurl codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
```

Query provider threads:

```bash
xurl agents://codex
xurl 'agents://codex?q=spawn_agent'
xurl 'agents://claude?q=agent&limit=5'
# equivalent shorthand:
xurl codex
xurl 'codex?q=spawn_agent'
```

Query role-scoped threads:

```bash
xurl agents://codex/reviewer
# equivalent shorthand:
xurl codex/reviewer
```

Discover child targets:

```bash
xurl -I agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
```

Frontmatter includes provider metadata flattened into readable key-value lines such as `payload.git.branch = ...`, alongside discovery fields.

Drill down into a discovered child target:

```bash
xurl agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592/019c87fb-38b9-7843-92b1-832f02598495
```

Start a new agent conversation:

```bash
xurl agents://codex -d "Draft a migration plan"
# equivalent shorthand:
xurl codex -d "Draft a migration plan"
```

Start a new conversation with role URI:

```bash
xurl agents://codex/reviewer -d "Review this patch"
```

Continue an existing conversation:

```bash
xurl agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592 -d "Continue"
```

Create with query parameters:

```bash
xurl "agents://codex?cd=%2FUsers%2Falice%2Frepo&add-dir=%2FUsers%2Falice%2Fshared&model=gpt-5" -d "Review this patch"
```

Save output:

```bash
xurl -o /tmp/conversation.md agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
```

## Command Reference

```bash
xurl [OPTIONS] <URI>
```

- `-I, --head`: output frontmatter/discovery info only, including provider metadata flattened into key-value lines when available.
- `-d, --data <DATA>`: write payload (repeatable).
  - text: `-d "hello"`
  - file: `-d @prompt.txt`
  - stdin: `-d @-`
- `-o, --output <PATH>`: write command output to file.

## URI Reference

### Agents URI

```text
[agents://]<provider>[/<token>[/<child_id>]][?<query>]
|------|  |--------|  |---------------------------|  |------|
 optional   provider         optional path parts        query
 scheme
```

- `scheme`: optional `agents://` prefix. If omitted, `xurl` treats input as an `agents` URI shorthand.
- `provider`: target provider name, such as `codex`, `claude`, `gemini`, `amp`, `pi`, `opencode`.
- `token`: main conversation identifier or role name.
- `child_id`: child/subagent identifier under a main conversation.
- `query`: optional key-value parameters, interpreted by context.

### Agents Query

- `q=<keyword>`: filters discovery results by keyword. Use when you want to find conversations by topic.
- `limit=<n>`: limits discovery result count (default `10`). Use when you need a shorter or longer result list.
- `<key>=<value>`: in write mode (`-d`), `xurl` forwards as `--<key> <value>` to the provider CLI.
- `<flag>`: in write mode (`-d`), `xurl` forwards as `--<flag>` to the provider CLI.

Examples:

```text
agents://codex?q=spawn_agent&limit=10
agents://codex/threads/<conversation_id>
agents://codex/reviewer
agents://codex?cd=%2FUsers%2Falice%2Frepo&add-dir=%2FUsers%2Falice%2Fshared
```
