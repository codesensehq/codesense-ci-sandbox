# MCP tools for agents

_What agents can do through the CodeSense MCP bridge: finding a tool, stories, terminals, the Inbox, pages, usage and machines._

CodeSense runs an MCP server, so the agents it launches can work with the app directly: check a story before saving it, write a dashboard and lint it, open a terminal and run your tests, message you in the Inbox, or ask you a question and wait for the answer.

## How agents connect

The bridge is a small binary, `codesense-bridge`, in `~/.codesense/bin`. It relays MCP over stdio to the running app through a local socket (`~/.codesense/mcp.sock`). There's no network listener.

You don't set it up. Every agent CodeSense launches gets the bridge for that session only, from a machine-local file outside your folders: Claude Code through `--mcp-config` (with its status hooks in a per-launch `--settings` file), Codex through its own launch flags or a per-session `CODEX_HOME`. Nothing is written into the folder the agent runs in for this. An [agent](agents-and-profiles.md) can narrow the other servers it sees, but the CodeSense bridge is always included.

### `claude` and `codex` typed in any CodeSense tab

Type plain `claude` or `codex` in any CodeSense terminal tab, on this Mac or on a machine, and it starts connected to CodeSense, like an agent CodeSense launched: the `codesense` MCP server (your inbox and every tool) and CodeSense's status hooks, so the tab's status dot follows it; for Claude, new-mail hooks too, so new messages reach it. It works in any folder. On a machine, its remote MCP permission must be on, as for any remote agent. Your own arguments pass through unchanged.

The tab's shell does it. CodeSense's shell integration (zsh, bash and fish) defines `claude` and `codex` functions that add `--mcp-config` for Claude and one `-c mcp_servers.codesense=…` override for Codex, and start the CLI through the tab's terminal server, which adds Claude's status and new-mail hooks beside any settings of yours. They add no MCP configuration when:

- you pass your own `--mcp-config` or `--strict-mcp-config` (yours win),
- a `.mcp.json` in the folder or a parent already has a `codesense` server (on this Mac; on a machine the tab's own server replaces it, since a folder's entry can't reach the tab),
- you run a subcommand such as `claude mcp list`, or Codex with `--remote`,
- you already define your own `claude` or `codex` function, or
- you're outside a CodeSense tab.

Run `command claude` or `command codex` for the plain CLI. Tabs opened before CodeSense was updated don't have it; open a new one. On a machine it also needs the terminal server installed by this version of CodeSense.

So that an agent you type yourself in a terminal also connects, the worktrees and workspace folders CodeSense creates for stories, branches, epics and team workspaces carry a `codesense` entry in `.mcp.json` and `.codex/config.toml`, plus the status hooks in `.claude/settings.local.json`. Each such worktree ignores those files on its own, through an exclude file only that worktree reads, so they never show in its `git status` and are never committed, while your main checkout still shows its own `.mcp.json`. That worktree keeps your own `core.excludesFile` patterns too. CodeSense never writes them into your own checkouts, your team's context repo or any other folder you launch an agent in.

If the app isn't reachable, a tool call fails with **CodeSense is not reachable right now** and says whether it was sent. If the app restarts mid-session, the bridge keeps reconnecting for as long as the agent runs; retry the call once it is back.

> **There's no codesense CLI**
>
> `codesense-bridge` is the only CodeSense command-line binary, and it takes no commands of its own. Everything agents do goes through MCP tools.

## Finding a tool

There are a few hundred tools, and every one is listed in `tools/list`, so any MCP client can see and call them. The list doesn't change during a session. Four tools help an agent find its way:

| Tool | Does |
|---|---|
| `codesense_overview` | The first call. One read-only summary: the active org and team, machines, running sessions, saved agents, routines, the inbox and open workspaces, each with the tools to call for more. |
| `codesense_search_tools` | Searches tool names and descriptions. Several terms must all match; name matches rank first. |
| `codesense_get_tool_schema` | Returns one tool's full description and input schema. |
| `codesense_call_tool` | Calls a tool by name, for a host that can't call it directly. |

## What agents can do

The most useful tool groups:

### Stories and plans

| Tool | Does |
|---|---|
| `get_story_format` | Returns the story authoring contract. Agents call it before writing a story. |
| `validate_story` | Checks a story file and returns findings, such as a `type` that isn't `story`, `bug` or `spike`, a missing or unknown `source_repo`, an unknown status, a duplicate `story_id` or a `depends_on` that doesn't resolve. |
| `query_context` | Finds stories and other context files by their metadata. |
| `read_epic_repos` / `set_epic_repos` | Read or rewrite an epic's `repos:` list. |
| `hydrate_story` / `list_hydrated` | Copy a story's epic into a workspace, on this Mac or a machine, and list what's been brought in. |

### Terminals

| Tool | Does |
|---|---|
| `open_terminal_tab` | Opens a terminal tab in a workspace that's already open and returns a handle. |
| `run_in_terminal_tab` | Runs a command in that tab once its prompt is ready. |
| `read_terminal_tab` | Returns the tail of the tab's output. |
| `send_to_session` | Sends text to an agent session opened with `open_workspace`. |

With these, an agent can start `npm run dev` in one tab, run the tests in another, and read both, the way you would.

### Talking to you

| Tool | Does |
|---|---|
| `send_message` | Sends a message to your Inbox or to another agent's. To you, `kind: "needs-you"` lists it under **Needs you**; progress goes under **Updates**. `contentType` is `plain`, `markdown` (the default) or `html`. HTML to you renders as a live page, with buttons that work. |
| `read_messages` / `reply` | Read an agent's own inbox and answer a message. |
| `ask_user` | Asks you a question in the app and waits for your answer. |
| `post_agent_message` | Posts a chat message into the app without waiting. |
| `signal_progress` / `signal_done` | Reports a phase, or records that the task finished with `success`, `failed` or `needs-attention` and a short summary. `signal_done` records and notifies; it doesn't move the story or run any check. |

### Pages, panes and canvases

| Tool | Does |
|---|---|
| `get_live_data_contract` | Returns how to write a live HTML page: the `window.codesense` SDK, action verbs, routine results and a starter page. |
| `validate_live_data_doc` | Lints a page before it's saved (embedded tokens, misaddressed verbs, unsafe `innerHTML` and more). |
| `pane_list` / `pane_write` / `pane_open` | List, write and open custom right panes: HTML files at `<space>/panes/<id>.html`. |
| `apply_canvas` / `render_canvas` | Draw an Excalidraw diagram from an outline, and render it to PNG so the agent can check it. |

`create_dashboard` and the publishing tools (`publish_dashboard`, `share_dashboard`, `unpublish_dashboard`) publish to share.codesense.ai for the people you invite.

### Usage

`session_usage` returns tokens and cost for the agent sessions CodeSense launched, on this Mac and its machines, per session and per day. Each session names its story, agent and models, and you can filter by story, session, folder or date.

### The app itself

`get_viewer_state`, `set_viewer_state` and `open_workspace` let an agent see what's open and open a story workspace (headed, in the background, or headless). `who_am_i` tells an agent its own tab, machine and workspace.

The full, generated list is in the MCP tools reference.

## Machines

Agents can set up and use [remote machines](terminals.md) with the same checks as the **Machines** tab.

### Add a machine

`list_machines` and `list_ssh_sources` show saved machines and your SSH choices. To add one, build one `input` object and pass it to each step:

```json
{
  "mode": "private-account",
  "label": "Development server",
  "host": "dev.example.com",
  "port": 22,
  "username": "dev",
  "signIn": { "method": "agent" }
}
```

1. `test_machine_connection({"input": input})`. Check the returned host fingerprint through a channel you trust, add it as `trustedHostKeyFp`, and test again. A returned fingerprint alone is not approval to trust it.
2. `enroll_private_machine_account`, then `install_terminal_server`, with the same `input`.
3. `save_machine({"input": input, "share": {"kind": "personal"}, "connect": true})`, then `connect_machine({"machineId": …})`. Background reconnect stays off unless `set_machine_auto_connect` turns it on.

For a key file, use `signIn: {"method": "keyFile", "keyPath": "/absolute/path/to/key"}`. For an encrypted key, `capture_machine_passphrase` opens CodeSense's own dialog and returns a reference. Never put a password, private key or passphrase in tool arguments.

### Open a workspace or a tab there

`open_workspace` with `story_id` and `machine_id` opens a story's workspace on a machine. The first open copies your local clone there, so the machine needs no forge credentials. Add `target: "story"` with an `agent` or `initial_prompt` to launch an agent in it. `move_workspace` moves a workspace to another machine, or back to this Mac.

`open_remote_tab` opens a tab in a workspace that's already open. Pass a fresh `requestId` (UUID), `machineId`, `teamKey`, `workspaceId` and an absolute `cwd`. `launch` is `shell` (the default), `claude` or `codex`. To start a saved agent, pass `kind: "agent"` with its `agentKey` and a `preparedWorkspaceHandle`; it runs with the agent's own runtime, permissions and MCP servers. Then use `run_in_terminal_tab` and `read_terminal_tab` with the returned handle. Each read returns the tab's whole text.

A run returns `submitted`, `notSent`, `unknown` or `refused`. `unknown` means the command may have run: read the output and don't replay it. To recover a tab, call `open_remote_tab` with `operation: "attach"` and the original `requestId` (or the `handle`). Attach never starts another process.

### Let agents on a machine use CodeSense

In the **Machines** tab, tick **Allow agents on this machine to use CodeSense** and confirm. It's your own setting and starts off. A remote agent can't grant it to itself. New shells opened after you turn it on get the bridge; existing shells don't. Typing `claude` or `codex` in a new shell is enough (see [above](#claude-and-codex-typed-in-any-codesense-tab)). For other MCP clients, the shell exposes `CODESENSE_MCP_BRIDGE`, `CODESENSE_MCP_CONFIG` and `CODESENSE_CODEX_MCP`.

A remote agent sees only the tools that work from a machine: contracts, the bound team's context, machine setup, workspaces and its own remote tabs. Tools that need this Mac's file paths, local terminals or attaching a context repo are left out. No provider API key or third-party MCP config is copied to the machine.

A saved agent's own MCP servers come with it: started in a remote tab, it can use the local MCP servers its `agent.yaml` names, relayed from your Mac. Credentials and endpoint URLs stay on your Mac. Up to 16 servers per session, CodeSense included.

### Prepare, hydrate and bring work home

- `prepare_remote_workspace` makes a worktree on the machine from a commit your local clone already has. It sends only git objects; tokens and SSH keys stay on your Mac. It copies no context.
- `hydrate_worktree` and `hydrate_story` with `target: {"kind": "remote", "machineId", "workspaceHandle"}` copy context in, from your saved context on this Mac (uncommitted edits included). They never overwrite a file the repo tracks or one edited there.
- `remote_workspace_fetch` brings the machine's commits into your local clone as `refs/remotes/codesense-machine-<machineId>/<branch>`, and creates or fast-forwards the local branch only when that loses nothing. Uncommitted files stay on the machine. Push and open pull requests from your local clone with your own tools.

> **Beta**
>
> `preview_reverse_sync_worktree` and `reverse_sync_worktree` take the same remote target, to publish context edits made on the machine back to the team's content on your Mac. A team file that changed since the copy comes back as a conflict.

Nothing runs between these calls: there's no watcher in either direction.

### Run a task with your local agent login

`start_remote_agent_task` runs Claude Code or Codex on your Mac, with its commands and file edits on the machine. The CLI's sign-in stays on your Mac. Poll `read_remote_agent_task` for output and stop it with `cancel_remote_agent_task`. It needs Codex 0.159.3 or a newer 0.x release on the machine as its runtime, and, on your Mac, Codex 0.159.3 or a newer 0.x release for a Codex task or Claude Code 2.1.141 or newer for a Claude task.

## Repository ownership

CodeSense never commits, pushes or merges. Agents (or you) do the git: clone, fetch, commit, push and open pull requests with your own tools. CodeSense's own git is limited to worktrees, fetches that read history or check a workspace before `move_workspace` moves it, and `remote_workspace_fetch`, which brings a machine's commits into your local clone. `get_publish_set` returns the context repo's local changes for an agent to review before it commits.

## Guardrails

- **Handles, not session ids.** Tools that drive the UI take opaque handles returned by `open_workspace` or `open_terminal_tab`. A raw session id is refused, so an agent can't reach a session it wasn't given.
- **A check in the app.** Tools that open workspaces or send to sessions pass a check inside CodeSense first. It can refuse, and it returns `busy` when the background agent limit is reached.
- **No secrets through tools.** No CodeSense tool accepts a service token. Agents use their own tools and credentials for outside services.

## Other MCP clients

The bridge is a standard stdio MCP server, and the tools are the same whatever calls them. The built-in wiring covers Claude Code, Codex and OpenCode.

## Connect a context repo from a tool

In the app, **Connect a context repo…** in the switcher attaches a context repo folder you already have. Agents on this Mac can do the same:

| Tool | Arguments | Result |
| --- | --- | --- |
| `attach_context_repository` | `{ "clonePath": "/absolute/path/to/context" }` | The org it registered, from its `org.yml` |
| `list_context_attachments` | `{}` | Every attached repo, including missing ones with `availability_error` |
| `detach_context_repository` | `{ "orgSlug": "my-org" }` | Removes the registration only |

CodeSense uses the folder where it is, uncommitted files included. It doesn't copy it, change its remotes, or fetch, pull, push or commit. Attaching the same folder again is safe; a second folder claiming the same org is refused. Detaching never deletes files. If a folder goes missing, the app offers **Locate folder…** and **Disconnect**. These tools aren't available to agents on a machine.

## Outside services

Agents use their own tools and logins to reach outside services, then publish the data as a routine result for dashboards to read. CodeSense doesn't hold service credentials.

The full guide ships in the app's docs: **MCP**.
