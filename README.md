# xURL

`xURL` is a CLI that reads, queries, and writes AI agent conversations through a unified `agents://` URI scheme.

> Also known as **Xuanwo's URL**.

## What xURL Can Do

xURL gives you one URI scheme (`agents://`) to **read**, **query**, **discover**, and **write** conversations across multiple AI agent CLIs.

- **Read** a conversation as markdown — `xurl agents://codex/<id>`
- **Query** threads by provider, keyword, local path, or role — `xurl 'agents://codex?q=refactor'`
- **Discover** child targets and metadata before drilling down — `xurl -I agents://codex/<id>`
- **Write** to start or continue a conversation — `xurl agents://codex -d "hello"`

## Providers

<table>
  <tr>
    <td align="center"><img src="https://ampcode.com/amp-mark-color.svg" alt="Amp" width="36" height="36" /><br /><code>agents://amp</code></td>
    <td align="center"><img src="https://cdn.simpleicons.org/claude" alt="Claude" width="36" height="36" /><br /><code>agents://claude</code></td>
    <td align="center"><img src="https://avatars.githubusercontent.com/u/14957082?s=200&v=4" alt="Codex" width="36" height="36" /><br /><code>agents://codex</code></td>
    <td align="center"><img src="https://cdn.simpleicons.org/githubcopilot" alt="GitHub Copilot" width="36" height="36" /><br /><code>agents://copilot</code></td>
    <td align="center"><img src="https://www.cursor.com/favicon.ico" alt="Cursor" width="36" height="36" /><br /><code>agents://cursor</code></td>
  </tr>
  <tr>
    <td align="center"><img src="https://cdn.simpleicons.org/googlegemini" alt="Gemini" width="36" height="36" /><br /><code>agents://gemini</code></td>
    <td align="center"><img src="https://avatars.githubusercontent.com/u/129152888?s=200&v=4" alt="Kimi" width="36" height="36" /><br /><code>agents://kimi</code></td>
    <td align="center"><img src="https://avatars.githubusercontent.com/u/208539476?s=200&v=4" alt="OpenCode" width="36" height="36" /><br /><code>agents://opencode</code></td>
    <td align="center"><img src=".github/assets/pi-logo-dark.svg" alt="Pi" width="36" height="36" /><br /><code>agents://pi</code></td>
  </tr>
</table>

## Installation

Install as an agent skill:

```bash
npx skills add Xuanwo/xurl
```

Or install the standalone CLI:

```bash
brew tap xuanwo/tap && brew install xurl   # Homebrew
cargo install xurl-cli                      # Cargo
uv tool install xuanwo-xurl                 # Python / uv
npm install -g @xuanwo/xurl                 # npm
```

## Quick Start

Ask your agent to summarize a thread:

```text
Please summarize this thread: agents://codex/xxx_thread
```

## Usage

> **Note:** The `agents://` scheme prefix is optional — `codex/...` is equivalent to `agents://codex/...`.

### Read

```bash
xurl agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
xurl agents://copilot/688628a1-407a-4b4e-b24a-1a250ebf864f
```

Save output to a file:

```bash
xurl -o /tmp/conversation.md agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
```

### Query

By provider:

```bash
xurl agents://codex
xurl 'agents://codex?q=spawn_agent'
xurl 'agents://claude?q=agent&limit=5'
xurl 'agents://copilot?q=resume&limit=5'
```

By local path:

```bash
xurl agents:///Users/alice/work/xurl
xurl 'agents:///Users/alice/work/xurl?q=refactor&limit=5'
xurl 'agents://.?q=refactor&providers=codex,claude'
xurl 'agents://~/work/xurl?providers=opencode'
```

By role:

```bash
xurl agents://codex/reviewer
```

Query results include reduced thread metadata when available, so you can inspect fields like `payload.git.branch` without opening each thread individually.

### Discover

```bash
xurl -I agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592
```

Frontmatter includes provider metadata flattened into readable key-value lines (e.g. `payload.git.branch = ...`), and skips oversized instruction-like fields.

Drill down into a discovered child target:

```bash
xurl agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592/019c87fb-38b9-7843-92b1-832f02598495
```

### Write

Start a new conversation:

```bash
xurl agents://codex -d "Draft a migration plan"
```

Start with a role URI:

```bash
xurl agents://codex/reviewer -d "Review this patch"
xurl agents://copilot/research -d "Investigate the failing integration test"
```

Continue an existing conversation:

```bash
xurl agents://codex/019c871c-b1f9-7f60-9c4f-87ed09f13592 -d "Continue"
```

Pass extra parameters to the provider CLI via query string:

```bash
xurl "agents://codex?cd=%2FUsers%2Falice%2Frepo&add-dir=%2FUsers%2Falice%2Fshared&model=gpt-5" -d "Review this patch"
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

## Error Output

`xurl` writes actionable stderr errors for agents:

- unsupported providers and unsupported capabilities include `requested_uri`, suggested `next_steps`, and the GitHub issue link for requesting support
- missing local data includes evidence such as `searched_roots` so the next recovery step is explicit
- provider CLI failures include the command, exit code, and concrete retry guidance

## URI Reference

### Agents URI

```text
[agents://]<provider>[/<token>[/<child_id>]][?<query>]
|------|  |--------|  |---------------------------|  |------|
 optional   provider         optional path parts        query
 scheme
```

- `scheme`: optional `agents://` prefix. If omitted, `xurl` treats input as an `agents` URI shorthand.
- `provider`: target provider name, such as `amp`, `claude`, `codex`, `copilot`, `cursor`, `gemini`, `kimi`, `opencode`, `pi`.
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
agents://codex/<conversation_id>
agents://codex/reviewer
agents://codex?cd=%2FUsers%2Falice%2Frepo&add-dir=%2FUsers%2Falice%2Fshared
```
