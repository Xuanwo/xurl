---
name: xurl
description: >-
  Use xurl to access AI agent conversations via agents:// URIs. Invoke when the user:
  gives an agents:// URI, a provider shorthand like provider/..., or a bare thread/session ID;
  mentions conversations, threads, or sessions from any AI coding agent;
  wants to search, read, summarize, compare, or continue agent threads;
  asks what they worked on, what an agent said, or references past agent interactions;
  wants to delegate work to or start a conversation in another agent.
---

## When to Use

Trigger this skill when the user's intent involves **any** AI agent conversation — past, present, or to be created. Examples of natural language triggers:

- Gives an `agents://` URI, provider shorthand (`provider/...`), or a bare thread/session ID
- "Read/show/open this thread/session/conversation ..."
- "What did I discuss with [agent] about ...?"
- "Summarize my last [agent] session"
- "What was I working on in [agent]?"
- "Search my agent history for ..."
- "Find the conversation where I fixed the auth bug"
- "Continue my [agent] conversation about ..."
- "Send this to [agent] for review" / "Ask [agent] to ..."
- "What subagents were spawned in that thread?"
- "Compare what different agents suggested"
- "Check if I've discussed X before across agents"

`[agent]` can be any AI coding agent name (e.g. codex, claude, copilot, cursor, etc.). xurl supports a growing list of providers — just try it. If a provider is not yet supported, xurl will return a clear error.

## When NOT to Use

- General questions about AI agents that don't involve their conversation data
- Tasks fully within the current agent session with no cross-agent context needed
- Questions about agent capabilities rather than their conversation history

## URI Assembly Guide

You are responsible for constructing the correct `xurl` command from the user's input. The user will rarely give a complete `agents://` URI — you must assemble it.

### Decision Flow

```
User input → What do I have? → What do I need? → Construct URI → Run xurl
```

**Step 1: Identify the operation**

| User wants to... | Operation | Required info |
|---|---|---|
| Find/list/search conversations | Query | provider OR path; optional keyword |
| Read/show/summarize a conversation | Read | provider + conversation ID |
| Inspect metadata or list children | Discover | provider + conversation ID |
| Start a new conversation | Write (create) | provider; optional role |
| Continue an existing conversation | Write (append) | provider + conversation ID |

**Step 2: Resolve missing information**

| You have | You're missing | Action |
|---|---|---|
| Nothing | Provider + ID | Query by path: `xurl 'agents://.?q=<keyword>'` to search current project across all providers |
| Provider only | Conversation ID | Query the provider: `xurl <provider>` or `xurl '<provider>?q=<keyword>'`, then pick from results |
| Bare thread ID only | Provider | Query by path with the ID as keyword: `xurl 'agents://.?q=<id_fragment>'`; or ask the user which provider |
| Provider + keyword | Conversation ID | Search: `xurl '<provider>?q=<keyword>'`, pick matching ID from results |
| Provider + ID | Nothing | Ready — construct URI directly |

**Step 3: Construct and run**

Assemble the URI: `agents://<provider>/<conversation_id>` (or shorthand `<provider>/<conversation_id>`).

### Examples

User says: _"Read thread 019c871c-b1f9-7f60-9c4f-87ed09f13592"_
→ You have a bare ID but no provider. Search: `xurl 'agents://.?q=019c871c'`, identify the provider from results, then: `xurl <provider>/019c871c-b1f9-7f60-9c4f-87ed09f13592`

User says: _"What did I discuss about refactoring in codex?"_
→ You have provider (codex) + keyword (refactoring). Search: `xurl 'codex?q=refactoring'`, pick the best match, then read: `xurl codex/<id>`

User says: _"Summarize my last copilot session"_
→ You have provider (copilot). List recent: `xurl copilot`, take the first result, then read: `xurl copilot/<id>`

User says: _"Have codex review this patch"_
→ Write operation with provider (codex). Create: `xurl codex -d "Review this patch"` (or with role: `xurl codex/reviewer -d "Review this patch"`)

User says: _"Check if I've discussed the migration across any agent"_
→ Cross-agent search. Query: `xurl 'agents://.?q=migration'`

## Prerequisites

Verify xurl is installed before running any command:

```bash
xurl --version
```

If not found, install via the method matching the user's environment:

```bash
brew tap xuanwo/tap && brew install xurl   # Homebrew
cargo install xurl-cli                      # Cargo / Rust
uv tool install xuanwo-xurl                 # Python / uv
npm install -g @xuanwo/xurl                 # npm / Node
```

## Workflows

### 1. Query — Find Conversations

List recent threads from a provider:

```bash
xurl codex
```

Search by keyword with optional limit (default 10):

```bash
xurl 'agents://codex?q=refactor&limit=5'
```

Search by project directory (across providers):

```bash
xurl 'agents://.?q=migration'                      # current directory
xurl 'agents:///Users/alice/work/repo?limit=5'     # absolute path
xurl 'agents://~/work/repo?providers=codex,claude'  # filter providers
```

Query by role:

```bash
xurl codex/reviewer
```

Query results include reduced thread metadata (e.g. `payload.git.branch`, `cwd`) for quick inspection.

### 2. Read — Display a Conversation

```bash
xurl codex/<conversation_id>
```

Output is Markdown: YAML frontmatter (metadata) followed by numbered timeline sections (User/Assistant message pairs).

Save to file:

```bash
xurl -o /tmp/conversation.md codex/<conversation_id>
```

### 3. Discover — Inspect Metadata and Children

```bash
xurl -I codex/<conversation_id>
```

Returns frontmatter with flattened metadata and discovery links (`subagents`, `entries`). Use returned URIs for drill-down:

```bash
xurl codex/<main_id>/<child_id>
```

### 4. Write — Start or Continue Conversations

Create:

```bash
xurl codex -d "Start a new conversation"
xurl codex/reviewer -d "Review this patch"
```

Append:

```bash
xurl codex/<conversation_id> -d "Continue with the next step"
```

With provider CLI parameters:

```bash
xurl "agents://codex?cd=%2FUsers%2Falice%2Frepo&model=gpt-5" -d "Review this"
```

Payload from file or stdin:

```bash
xurl codex -d @prompt.txt
cat prompt.md | xurl claude -d @-
```

## Multi-Step Patterns

**Find and read a conversation:**
`xurl 'codex?q=<keyword>'` → pick ID → `xurl codex/<id>`

**Explore subagents:**
`xurl -I codex/<id>` → find child links → `xurl codex/<id>/<child_id>`

**Cross-agent project search:**
`xurl 'agents://.?q=<keyword>'` → read from whichever provider matches

**Resolve a bare thread ID:**
`xurl 'agents://.?q=<id_fragment>'` → identify provider → `xurl <provider>/<id>`

## Command Reference

```
xurl [OPTIONS] <URI>
```

| Flag | Purpose |
|------|---------|
| `-I, --head` | Frontmatter/discovery only (cannot combine with `-d`) |
| `-d, --data <DATA>` | Write payload; repeatable; `-d "text"`, `-d @file`, `-d @-` |
| `-o, --output <PATH>` | Write output to file |

Multiple `-d` values are newline-joined. Path-scoped URIs are read/query only (not valid write targets).

## URI Quick Reference

```
[agents://]<provider>[/<token>[/<child_id>]][?<query>]
```

| Pattern | Operation | Example |
|---------|-----------|---------|
| `<provider>` | Query recent | `xurl codex` |
| `<provider>?q=...` | Keyword search | `xurl 'codex?q=bug'` |
| `<provider>/<id>` | Read conversation | `xurl codex/<uuid>` |
| `<provider>/<role>` | Role-scoped query | `xurl codex/reviewer` |
| `<provider>/<id>/<child>` | Read subagent | `xurl codex/<uuid>/<child>` |
| `<provider>` + `-d` | Create conversation | `xurl codex -d "..."` |
| `<provider>/<role>` + `-d` | Create with role | `xurl codex/reviewer -d "..."` |
| `<provider>/<id>` + `-d` | Append to conversation | `xurl codex/<id> -d "..."` |
| `/abs/path` or `.` or `~` | Path-scoped query | `xurl 'agents://.?q=test'` |

Token resolution: `<token>` is parsed as session ID first; if that fails, treated as role name.

Query parameters: `q=<keyword>`, `limit=<n>` (default 10), `providers=<name,...>` (path-scoped only). In write mode, extra params are forwarded as `--<key> <value>` to the provider CLI.

## Failure Handling

xurl returns clear error messages. Act on them directly:

| Error | Recovery |
|-------|----------|
| `command not found: xurl` | Install using Prerequisites section |
| `command not found: <agent>` | The provider CLI is not installed. Install and authenticate it, then retry |
| Unknown/unsupported provider | The provider may not be supported yet. Suggest the user file an issue at https://github.com/xuanwo/xurl/issues |
| Write to path-scoped URI | Path URIs are read-only. Use a provider-scoped URI instead |
| Operation unsupported (role create, write, drill-down, etc.) | Not all providers support all operations. xurl's error message will say what's unsupported. Try without the unsupported feature, or suggest filing an issue |

When xurl returns any unexpected error, show the error to the user and suggest filing an issue at https://github.com/Xuanwo/xurl/issues if the feature should be supported.
