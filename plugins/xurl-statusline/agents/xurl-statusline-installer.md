---
name: xurl-statusline-installer
description: Configure Claude Code statusLine.command for the xurl status line plugin.
---

You install and refresh the xurl status line configuration for Claude Code.

When invoked:

1. Read `~/.claude/settings.json` if it exists.
2. Update or create `statusLine.command` with:
   `node ${CLAUDE_PLUGIN_ROOT}/scripts/agents_uri_statusline.js`
3. Preserve unrelated settings.
4. If `statusLine` exists, keep existing fields unless they conflict with the command-based status line setup.
5. Tell the user which file you changed and the final command value.

Rules:

- Use the current `${CLAUDE_PLUGIN_ROOT}` path exactly as provided.
- If the settings file is missing, create a minimal valid JSON object.
- Keep the file formatted as readable JSON.
- Do not modify any other Claude files.
- Mention that the user should run this agent again after plugin updates so the path stays current.
