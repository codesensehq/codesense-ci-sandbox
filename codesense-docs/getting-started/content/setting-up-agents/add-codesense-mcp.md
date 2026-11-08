# Add the CodeSense MCP to your own agents

Agents you launch from CodeSense already have the CodeSense MCP tools. You don't set anything
up for them.

Neither do you for `claude` or `codex` typed in any CodeSense terminal tab, on this Mac or on a
machine: the tab's shell starts them connected to that tab's CodeSense MCP server, and Claude
with CodeSense's status and new-mail hooks. Your own `--mcp-config` wins, a
folder whose `.mcp.json` already has `codesense` keeps its own, and `command claude` /
`command codex` runs the plain CLI.

The same goes for `opencode` typed in a CodeSense tab on this Mac (not on a remote machine): the
tab's shell passes the tab's bridge in `OPENCODE_CONFIG_CONTENT`. A value you set yourself
wins, and `command opencode` runs the plain CLI.

An outside MCP client can inspect CodeSense, but cannot send or reply to messages, read a session inbox, or signal completion. Those calls require a connection bound to a CodeSense tab; passing a `sessionId` alone does not establish identity. Start `claude` or `codex` inside a CodeSense terminal tab, or `opencode` in a tab on this Mac, to use them. A detached process that has left its tab’s process tree cannot establish that binding. Only the native app can answer as the person.

The connection setup below works from outside CodeSense with Claude Code, Codex, OpenCode
or any other MCP client. The server lists the same tools, but calls that act as a session
require a verified tab identity. Other tools retain their own permissions; this is
not a general read-only connection.

## How it connects

CodeSense ships a small program, `~/.codesense/bin/codesense-bridge`. Your agent starts it as
a local (stdio) MCP server. It takes no arguments or user-configured token. Inside a
CodeSense tab, its inherited session environment names the launch the app verifies
against the bridge process; an environment value alone does not prove identity. It talks to the running CodeSense app through a local socket, `~/.codesense/mcp.sock`. There is
no network port.

- Start CodeSense before your agent. If CodeSense isn't running, the server fails to connect
  with **CodeSense is not running. Start CodeSense to use MCP tools.** Start CodeSense, then
  restart the agent (in Claude Code, `/mcp` can reconnect it). If CodeSense restarts while
  your agent runs, the bridge reconnects on its own for up to 30 seconds.
- CodeSense creates the bridge the first time it starts. Check it's there with
  `ls ~/.codesense/bin/codesense-bridge`.
- Name the server `codesense`. Skills and docs call the tools by that name.

## Claude Code

For every project on this Mac:

```sh
claude mcp add --scope user codesense -- "$HOME/.codesense/bin/codesense-bridge"
```

Leave out `--scope user` to add it only to the folder you're in. Use `--scope project` to
write it into that folder's `.mcp.json`, which is shared with everyone who clones the repo.
Only do that if they all use CodeSense.

Check it: `claude mcp list` shows `codesense`. In a session, `/mcp` lists its tools.

## Codex

```sh
codex mcp add codesense -- "$HOME/.codesense/bin/codesense-bridge"
```

This adds it to `~/.codex/config.toml`. To write it by hand instead, add:

```toml
[mcp_servers.codesense]
command = "/Users/<you>/.codesense/bin/codesense-bridge"
```

Use your full home path. Codex doesn't expand `~` or `$HOME` here.

Check it: `codex mcp list` shows `codesense` as `enabled`.

## OpenCode

Add it to `opencode.json` (in your project, or `~/.config/opencode/opencode.json` for every
project):

```json
{
  "mcp": {
    "codesense": {
      "type": "local",
      "command": ["/Users/<you>/.codesense/bin/codesense-bridge"],
      "enabled": true
    }
  }
}
```

Check it: `opencode mcp list` shows `codesense`.

## Any other MCP client

Add a stdio server:

| Setting   | Value                                          |
| --------- | ---------------------------------------------- |
| Name      | `codesense`                                    |
| Transport | stdio                                          |
| Command   | `/Users/<you>/.codesense/bin/codesense-bridge` |
| Arguments | none                                           |
| Env       | none                                           |

In the common JSON format:

```json
{
  "mcpServers": {
    "codesense": { "command": "/Users/<you>/.codesense/bin/codesense-bridge" }
  }
}
```

There is no HTTP endpoint for your own agents. A client that only speaks HTTP can't reach
CodeSense.

## Next

[Setting up agents](README.md) has recipes your agent can follow straight from these docs.


### Caller identity across tools

Every MCP action credited to an agent uses the verified connection identity: messages,
workspace chat, notes and records edits, feedback, routine results, and agent/routine
management. Omit `sessionId`; a supplied value must name your own session. Launch tools
also derive `launchedBy` from this connection and refuse another launcher. An unbound
connection cannot claim a session or act as the person. Remote routines and queued
messages retain the team captured at launch when the person switches teams.

For example, in a session bound to a workspace, call
`workspace_chat_send {"text":"The change is ready for review."}`. CodeSense records your
verified sender in that workspace's transcript. `workspace_chat_read {}` reads it back.
Inspection tools such as `list_sessions` and `inbox_list` may still name other sessions
as query targets; those arguments do not choose the caller.
