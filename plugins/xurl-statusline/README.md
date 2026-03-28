# xurl-statusline

Claude Code plugin that shows the current Claude conversation as an `agents://` URI in the status line.

## Install

```text
/plugin marketplace add Xuanwo/xurl
/plugin install xurl-statusline@xurl
```

## Configure

Ask Claude to use the `xurl-statusline-installer` agent to configure your status line.

Copy and send:

```text
Use the xurl-statusline-installer agent to add the xurl agents URI to my Claude Code status line. Keep my existing status line layout and settings intact, and avoid replacing my current status line unless that is the only safe option.
```

The agent updates `~/.claude/settings.json` for you and should preserve your existing status line layout whenever possible. Run it again after plugin updates so the path stays current.

The script prints:

- `agents://claude/<session_id>` for main threads
- `agents://claude/<main_session_id>/<agent_id>` for Claude sidechain transcripts
