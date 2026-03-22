# xURL

`xURL` is a client for AI agent URLs.

> Also known as **Xuanwo's URL**.

## What xURL Can Do

- Read an agent conversation as markdown.
- Query recent threads and keyword matches for a provider.
- Query conversations by local path across providers.
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

Query conversations by path:

```bash
xurl agents:///Users/alice/work/xurl
xurl 'agents:///Users/alice/work/xurl?q=refactor&limit=5'
xurl 'agents://.?q=refactor&providers=codex,claude'
xurl 'agents://~/work/xurl?providers=opencode'
```

Query role-scoped threads:

```bash
xurl agents://codex/reviewer
# equivalent shorthand:
xurl codex/reviewer
```

Query results include the same reduced thread metadata used by `--head` when it is available, so you can inspect fields like `payload.git.branch` without opening each thread individually.

Discover child targets:

```bash
xurl -I agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
```

Frontmatter includes the first provider metadata record flattened into readable key-value lines such as `payload.git.branch = ...`, and skips oversized instruction-like fields.

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

- `-I, --head`: output frontmatter/discovery info only, including the first provider metadata record flattened into key-value lines when available.
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

### Path-Scoped Query URI

```text
agents:///abs/path[?<query>]
agents://.[?<query>]
agents://./subdir[?<query>]
agents://..[?<query>]
agents://../repo[?<query>]
agents://~[?<query>]
agents://~/repo[?<query>]
```

- `agents:///abs/path`: canonical local path query form.
- `agents://.` / `agents://./subdir`: query relative to the current working directory.
- `agents://..` / `agents://../repo`: query relative to the parent of the current working directory.
- `agents://~` / `agents://~/repo`: query relative to the home directory.
- path-scoped query always returns a conversation list.

### Agents Query

- `q=<keyword>`: filters discovery results by keyword. Use when you want to find conversations by topic.
- `limit=<n>`: limits discovery result count (default `10`). Use when you need a shorter or longer result list.
- `providers=<name[,name...]>`: restricts a path-scoped query to selected providers.
- `<key>=<value>`: in write mode (`-d`), `xurl` forwards as `--<key> <value>` to the provider CLI.
- `<flag>`: in write mode (`-d`), `xurl` forwards as `--<flag>` to the provider CLI.

Examples:

```text
agents://codex?q=spawn_agent&limit=10
agents:///Users/alice/work/xurl?q=refactor&providers=codex,claude
agents://.?q=refactor&providers=codex
agents://codex/threads/<conversation_id>
agents://codex/reviewer
agents://codex?cd=%2FUsers%2Falice%2Frepo&add-dir=%2FUsers%2Falice%2Fshared
```
