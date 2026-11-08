# How CodeSense works

_A guide for agents. Read it to answer questions about CodeSense from any project, with only the
CodeSense MCP tools. Each concept names the tools that show or change it._

## Start here: by task

| To do this | Tools | Read |
| --- | --- | --- |
| Build a pane and choose its slot | `pane_write`, `pane_open`, `get_live_data_contract` | [Slots and pane shapes](../arc-i-dashboard-primitives/pane-shapes.html#slots) |
| Build a file tree pane | `pane_write`, `pane_open` | [File tree panes](../arc-i-dashboard-primitives/linking.html#file-tree) |
| Build a fast, light dashboard (cached list, visible-only totals, per-section loading) | `get_live_data_contract` | [Fast, light dashboards](../arc-i-dashboard-primitives/fast-light-dashboards.html) |
| Customize the Inbox pane | `pane_write`, `pane_open` | [Customize built-in panes](../arc-i-dashboard-primitives/customize-built-in-panes.html) |
| Fire a routine on a metadata status change | `routine_create`, `routine_update` | [Metadata trigger cookbook](../arc-g-routines/trigger-cookbook.html#metadata_change) |
| Fire a routine on inbox mail or agent completion | `routine_create`, `routine_update` | [Event trigger cookbook](../arc-g-routines/trigger-cookbook.html#events) |
| Run a routine for one role queue or every queue in reach | `routine_create`, `routine_update` | [Role queue triggers](../arc-g-routines/trigger-cookbook.html#role_pending) |
| Run a command and chain routines | `routine_create`, `routine_runs` | [Command routines](../arc-g-routines/command-routines.html) |
| Cancel a queued or running routine run | `routine_runs`, `routine_cancel`, `routine_run` | [Cancel one run](../arc-g-routines/index.html#cancel-run) |
| Design an agent team | `agent_preview`, `agent_create`, `role_save` | [Agent design](../arc-e-agents/agent-design.html) |
| Nest teams and declare shared content | `set_team_parent`, `list_orgs_teams` | [Teams and inheritance](../arc-b-model/teams.html#set-team-parent) |
| Sync context with git | `context_repo_status`, `report_context_sync` | [Agent-driven git](../arc-k-sync/agent-driven-git.html) |
| Add a machine | `save_machine`, `connect_machine` | [Machine setup](../arc-d-terminals-and-machines/setup-machine.html) |
| Share a dashboard | `codesense_search_tools`, `codesense_get_tool_schema` | [Sharing dashboards](../handbook/concepts/dashboards.md) |
| Read run evidence | `list_runs`, `read_run_evidence`, `read_run` | [Run tools reference](../generated/mcp-tools.json) |
| Read what a story changed: sessions, commits, files, tasks, cost and time | `story_changes` | [Story contributions](../arc-e-agents/mcp-only-agents.html#tool-story_changes) |
| Inspect a story's sessions and conversation | `story_changes`, `session_read` | [Story trail](../arc-i-dashboard-primitives/index.html#story-trail) |
| Check docs freshness | `codesense_overview`, `update_codesense_docs` | [Docs freshness](README.md#docs-freshness) |
| Store and query records | `records_create`, `records_list`, `records_write` | [Records](../arc-i-dashboard-primitives/records.html) |
| Configure the roles registry | `role_save`, `roles_list`, `role_state` | [Role configuration](../arc-f-inboxes-and-triage/roles.html#configuration) |
| Group workspaces | `workspace_group_create`, `workspace_group_add`, `workspace_group_list` | [Workspace group tools](../generated/mcp-tools.json) |
| Create and verify a canvas | `apply_canvas`, `render_canvas` | [Canvas tools](../generated/mcp-tools.json) |
| Send a GIF, meme or document attachment; configure remote media | `send_message`, `reply` | [Send a GIF or image](../arc-f-inboxes-and-triage/messages.html#send-media) |
| Keep a badge fresh while hidden | `get_live_data_contract` | [Live pane badges, with the Inbox example](../arc-i-dashboard-primitives/reacting-to-state.html#live-badge) |
| See which Claude or Codex account a launch uses, or why one was refused | `list_agent_accounts`, `resolve_agent_account`, `agent_get` | [Agent accounts](../arc-e-agents/agent-accounts.html) |
| Set or review an agent or fresh routine's spend cap | `agent_update`, `routine_update` | [Spend caps](../arc-e-agents/spend-caps.html) |
| See what a task was for, its targets and how it went; add a sub-task or a verdict | `task_list`, `task_read`, `task_start`, `task_update` | [Task records](../arc-e-agents/tasks.html) |

Hosted publishing depends on the installed build. Discover its available tools and read
their schemas before trying to share a dashboard.

## Start here

1. **`codesense_overview`** answers "what is going on right now" in one read-only call: active
   org and team, machines, sessions, saved Agents, routines, inboxes, role queues, open
   workspaces and where these docs are. Every section lists in `detail` the tools that return
   its full data.
2. Drill in with the tools each section names.
3. The tool list carries every tool. To find one by what it does, use
   `codesense_search_tools`, read its contract with `codesense_get_tool_schema`, then call it by
   name. Tool descriptions are the contract: read them before calling.

Answer from what the tools return, and name the tool that showed it. When a section reports
`error`, say so instead of guessing.

## Build and verify a custom pane

Read [Add a custom pane with a live list and a command](../arc-i-dashboard-primitives/custom-pane-live-list.html).
Use `validate_live_data_doc` → `pane_write` → `pane_open` → `pane_list` →
`read_rendered_document`, then `command_list`/`command_run` and a second rendered read.
The recipe links the shipped SDK, declarations, capabilities, slots, LiveHub and token contracts.
For collection-backed panes, read [Records](../arc-i-dashboard-primitives/records.html).

## Docs freshness

`codesense_overview.docs` includes the canonical profile docs `root`, `bundledCommit`,
`placedCommit`, `appVersion`, `state` and one-line `action`, alongside the README and
concepts-guide links. States are `current`, `behind`, `modified`, `behind-and-modified` or
`missing`; an unreadable source reports `error`. “Behind” means the placed commit differs
from the running app's bundle, not a comparison with a remote repository.

Call `update_codesense_docs` for an explicit offline refresh. Before selecting the bundle's
HEAD and index, it preserves local history, staged content, working edits, tracked deletions
and nonignored untracked files on the returned `preservationBranch`. Its `changedPaths` lists
local changes. Ignored machine state remains; replacement that would overwrite or delete unsaved local
content is refused.
A current clean checkout is a no-op. Untouched older checkouts still refresh at app startup;
modified checkouts need the explicit tool. The manual Docs helper routine runs from the docs
team's content directory; the tool resolves the canonical docs root independently of cwd.
No private remote, network access or credentials are needed. If another branch or detached
HEAD is active, the update selects a detached bundled HEAD and leaves existing branch refs
untouched, including during a later automatic refresh.

To run the manual helper from any project, call `routine_list {}` and choose the row whose
`org` is `codesense/docs`, `team` is `getting-started` and `name` is `Update CodeSense docs`.
Use that row's returned `id` for `routine_set_enabled {id, enabled:true}` (explicit opt-in
on this Mac), then `routine_run_now {id}`. The `each_mac` routine starts disabled and has no
schedule. `routine_run {runId}` only reads an existing run.

If docs and a live tool description disagree, trust the tool description and check
`docs.state`. Claude and Codex callers use their verified connection identity for messages, notes and records edits,
workspace chat, feedback, agent/routine management and routine results. Omit the session field
(`{session_id}` or `{sessionId}`, as each schema names it); a supplied value must name that same
caller. Launch tools derive the launcher field (`{launched_by}` or `{launchedBy}`) the same way.
Unbound MCP calls cannot claim another session or act as the person.

## Orgs, teams and context repos

- An **org** is a **context repo**: a plain git repo of team context (instructions, skills,
  plans, stories, dashboards). It is identified by an `owner/repo` slug.
- An org has **teams**, listed in its `teams.yml`. A team can have a parent team; the line of
  parents is the **team chain**. A child inherits only paths its ancestors explicitly share.
  When several levels share the same path, the nearest copy wins.
- Each team holds `content/` (what people and agents read), `config/` (Agents in
  `config/agents/<id>/agent.yaml`, and each Agent's routines under it in
  `config/agents/<id>/routines/<rid>/routine.yml`) and
  `panes/` (custom right panes).
- A team tracks **source repos**, the code repos its work lands in. A source repo is bound to
  a local clone on this Mac.
- One team is **active** in the app at a time. Team-scoped tools use it unless you pass
  `org` and `team`.
- CodeSense never commits, pushes or merges for you. Sharing a change with the team is a git push
  that you, the agent, make.
- A context repo you attach in place reports its checkout's origin (with any credentials
  removed) in `describe_setup` and `list_context_attachments`. CodeSense shows that origin
  but never fetches, pulls or pushes the checkout.

Tools: `describe_setup` (orgs, teams, source repos and their binding status),
`list_orgs_teams`, `set_active_team`, `create_team`, `add_source_repo`,
`attach_source_repo_path`, `list_context_attachments`.

## Declaring what child teams inherit

Use `set_team_parent` to nest teams; read [team inheritance](../arc-b-model/teams.html#set-team-parent).

Edit the sharing team's `<team>/config/sharing.yml` in its context repo:

```yaml
share:
  - content/handbook/**
  - content/.claude/skills/review/**
  - content/.mcp.json
  - panes/board.html
  - "!content/handbook/drafts/**"
```

Paths are relative to that team's folder. Only `content/` and `panes/` are
shareable; `config/` (including agent and routine definitions) and private space
are excluded. A leading `!` excludes a path. A middle team must share a path
onward for its children to receive it, even if an earlier ancestor shares it.

A missing declaration shares nothing. Invalid YAML or globs share nothing and
produce a diagnostic; child launches using their own files still work and show
a warning. CodeSense does not create share entries for you. Commit and push the
file using your usual git tools when you want teammates to receive the change.

The inherited nav, pane list, startup context, MCP server merge and hydration
use this declaration. Explicit cross-team tools retain their existing scopes.
The org checkout still contains its teams' files, so this is not filesystem
isolation or a promise of confidentiality.

## The context repo's git state and history

The left nav's status button shows the context repo's branch, how far it is from GitHub,
conflicts, and how the last sync went; its History view draws the commits on branch lanes.
The same reads are tools, so you can answer "what changed" or "what is on which branch"
without the app's code:

- `context_repo_status`: the checked-out branch (or detached), its upstream, commits ahead
  and behind, a merge or rebase in progress, conflicted files with both sides and the agent
  that last wrote each, and the sync state.
- `context_repo_history`: commits newest first with parents, branch and tag labels, the PR a
  commit merged, open PRs (from PR head refs fetched into the clone), the agent that made a
  commit (its `Actor-Name` or agent `Co-Authored-By` trailer), and whether GitHub has it yet.
  Filter by branch (`main`, a branch name), by `team`, or by `paths`; page with `cursor`.
- `report_context_sync`: when you sync the context repo (pull, push, open or merge a PR), say
  how it went (`syncing`, `ok`, `failed` with a reason, `signin`, `retrying` n of max). The
  human's button shows it. CodeSense does not sync this repo itself.

Uncommitted edits are `get_publish_set`.

## Content layers

Where a file lives decides who sees it:

| Layer             | Where                                                 | Who sees it                                              |
| ----------------- | ----------------------------------------------------- | -------------------------------------------------------- |
| Team content      | `<team>/content/` in the context repo                 | The team; child teams receive declared shares                |
| Repo tier         | `<team>/repos/<host>/<owner>/<repo>/content/`         | Agents working in that one source repo                   |
| Private (Only me) | Your private space for one org, under `~/.codesense/` | You, on this Mac; never shared                           |
| All orgs          | Your private all-orgs folder                          | You, in every org (skills, prompts, snippets, templates) |

Tools: `query_context` (find files by frontmatter, path or text across every synced org;
`layer` is `team`, `private` or `all-orgs`), `list_context_content`, `move_between_contexts`,
`copy_between_contexts`, `remove_from_context`.

File navigation also exposes `list_context_content({scope: "team", dir?})`,
`read_content_file({path, maxBytes?})`, `create_content_file({path, content?, onCollision?})`,
`move_content_file` and `delete_content_file`. Listing by `context` keeps the mobility
format; listing by `scope` returns file rows. Story scope uses the default `hydrate_story`
file set; agent scope uses the agent's resolved context want set. Private scope reads
your own private space. A child team's session or page receives only the files its
ancestors declare in `config/sharing.yml`, including when it explicitly selects the
parent team or supplies an absolute parent path. Unshared files behave like missing
files. Agent and routine definitions under `config/` are not shareable; human cross-team
management views still show them.

For example, a session launched in `child` can call
`query_context({where: {teams: ["parent"]}})` and pass a returned row's `path` to
`read_content_file({path})`. The query and file read use the session's child scope;
switching the app's active team does not widen that scope. A shared parent pane running
in the child uses the child's scope for `queryContext` and `openFile` too.
For agent workspace launches, omitting `team` selects the session's own team, even
when a person has switched the app to its parent. Human launches still default to
the active team.

Two current limits apply to child agents:

- A child agent can't open a workspace on a shared parent story. A human can.
- A child agent can't publish, unpublish or share a parent's dashboard, even if its
  files are shared. It can copy shared files into its own team and publish that copy;
  a human can publish, unpublish or share the parent's dashboard.

See [File navigation panes](../arc-i-dashboard-primitives/pane-shapes.html#file-nav)
for the matching `files.*` page methods and pushed directory updates.

## Stories, workspaces and worktrees

- A **story** is a markdown file with frontmatter (`type: story`, `story_id`, `status`,
  `source_repo`, optional `branch`, `depends_on`). Most other content is a plain doc and needs
  no frontmatter. Before writing one, call `get_story_format` and `validate_story`.
- A **workspace** is a work area in the app with its own terminal tabs. Kinds include a
  story workspace, an epic workspace (assembles the repos listed in the epic doc's `repos:`),
  a team workspace (its own folder with team content attached) and generic workspaces.
- A story workspace gets a **worktree**: a separate git checkout of the story's source repo
  on the story's branch, so parallel agents never share files. The first open creates the
  worktree.
- Team context reaches a worktree by **hydration**, and only at two moments: when an agent
  starts there (it gets what its definition's `context` declares) and when an agent calls
  `hydrate_worktree` or `hydrate_story`. Edits go back only when an agent calls
  `reverse_sync_worktree`. Nothing syncs in the background.

Tools: `open_workspace` (open a story workspace, or launch an agent in one; also on a
machine), `get_viewer_state` (its `terminalLayout` shows every open workspace and its tabs;
`sections: ["terminalLayout"]` returns just that part),
`read_epic_repos`, `set_epic_repos`, `read_worktree_config`, `hydrate_worktree`,
`hydrate_story`, `preview_reverse_sync_worktree`, `reverse_sync_worktree`, `list_hydrated`,
`get_sync_status`.

## Machines and remote tabs

- A **machine** is a host your terminals run on. "This Mac" is local. Others are reached
  over SSH and listed per team.
- A **private machine** (mode `private-account`, the only mode that opens terminal tabs) runs
  CodeSense's terminal server under your own account there. Terminals on it survive the app
  closing.
- A remote tab runs a shell, `claude` or `codex` on the machine. Claude or Codex there needs
  the machine's remote MCP permission turned on (`set_machine_remote_mcp_control`, off by
  default) and a login on that machine.
- **Remote agents have full tool parity with local agents**: an agent on a machine calls every
  CodeSense MCP tool a local agent can, through a relay to the desktop app, inbox and
  `who_am_i` included (its `$CODESENSE_TAB_SESSION_ID` is its remote tab's session id). It
  drives only its owner's terminals.
- Files on this Mac (including these docs) are not on the machine. A remote agent reads this
  guide with `codesense_overview` `{"guide": true}`.

Tools: `list_machines` (needs `orgSlug`, optional `team`; shows connection state), `connect_machine`,
`disconnect_machine`, `retry_machine`, `save_machine`, `prepare_remote_workspace`,
`open_remote_tab`, `open_workspace {machine_id}`, `remote_workspace_fetch`.

## Sessions and handles

- A **session** is one terminal process CodeSense runs: an agent or a shell, local or remote.
- Each session has a **handle**, its stable address for messaging and driving it. Never pass a
  raw PTY id; tools refuse them.
- In any CodeSense tab, local or on a machine, plain `claude` and `codex` start connected to
  that tab's CodeSense MCP server (inbox and tools) and, for Claude, CodeSense's status and
  new-mail hooks, like an agent CodeSense launched. The tab's shell adds the flags; your own
  `--mcp-config` wins, and `command claude` / `command codex` runs the plain CLI.
  On a machine this needs its remote MCP permission and an up-to-date terminal server.
- An agent learns its own session id from the environment variable
  `$CODESENSE_TAB_SESSION_ID`. Pass it in the tool's session field (`{session_id}` or `{sessionId}`,
  as its schema names it) wherever a tool asks who you are.
- `who_am_i` `{session_id}` tells a session who it is: its handle, its tab's current name
  (`displayName`), machine, workspace and launcher. A tab someone opened by hand is a session
  too: its handle is its session id.
- A session's name is its tab's current name. Its messages show under that name, and
  `rename_terminal_tab` renames it everywhere from then on.
- Session `state`: `running`, `idle` (turn finished), `needs-you` (waiting for a permission, or
  stuck on a startup screen), `exited`, `offline` (its machine is disconnected; it may still run there), `unknown`.
- `launchedBy` is the handle of the session that launched it (null when a person did). That is
  how to answer "who started whom" and "who is waiting on whom".

`list_sessions` also exposes `effectiveStatus`, distinct from terminal `state`: `running`,
`thinking` and `tool-running` mean working; `paused` means paused or awaiting permission,
`blocked` means blocked, and `awaiting-answer` means a question needs a reply. `idle` means
the turn has finished. **Stalled** (`stalled`) means an otherwise working session has had
no activity for at least 10 minutes **with no open subagents**; `unknown` means activity is
unobserved. For both Claude and Codex, an otherwise working session with open subagents
stays working (`running`, `thinking` or `tool-running`), even when the parent is inactive;
attention and completion states still take precedence. `completed` means reported success,
`done` means the agent or session ended, `dropped` means it needs attention (a row's
`needsYouKind` says why; `startup` means a CLI that reports readiness itself has not become
ready 3 minutes after launch, usually a first-run or trust screen, and clears by itself once
it is), `failed` means
reported failure, `crashed` means it crashed, and `errored` means a session or launch error.

A crashed session may include `endReason` alongside its `crashId`. This is the host's
explanation, also shown in the terminal's existing ended bar. For example, if an
agent's engine stops and detached recovery would lose CodeSense's verified identity,
the host ends the session, clears inbox readiness, and explains that you should
relaunch to continue. Keep the ended tab to read its scrollback, then use **Relaunch**;
`dismiss_agent_mark` with the row's `crashId` dismisses the mark without relaunching.

Tools: `who_am_i` (your own session), `list_sessions` (every session, local and remote, with state, machine, workspace,
role, launcher and unread mail), `read_terminal_tab` (what a tab shows now),
`send_to_session` (type into a live agent), `open_terminal_tab`, `run_in_terminal_tab`,
`rename_terminal_tab`, `signal_progress`, `signal_done`.

## Agents

- A saved **Agent** is a reusable launch definition: runtime (`claude`, `codex` or
  `opencode`; OpenCode runs on this Mac only), args,
  MCP servers, startup files, permissions and a default prompt. It lives in a team (shared
  with teammates) or in your private space (Only me).
- CodeSense ships read-only starters: `builtin/claude`, `builtin/codex`,
  `builtin/opencode` and their `-unattended` twins (permission checks skipped). Copy one to customize it.
- CodeSense has no special Agent. A "manager" or "controller" is an Agent you design: its
  prompt, skills and routines decide what it triages, which workers it launches and what it
  reports to you. An Agent named `controller` behaves like any other.
- A **plain launch** (a raw `claude`, `codex` or `opencode` in a tab) has no saved definition: it gets the
  CLI's own defaults, no declared startup files and no declared permissions.
- Any teammate can edit any team Agent. There are no owner checks; `created_by` is
  attribution only.

Tools: `agent_list`, `agent_get` (definition, revisions and what a launch would resolve to),
`agent_preview`, `agent_create`, `agent_update`, `agent_delete`; launch with
`open_workspace` (`agent`) or `open_remote_tab` (`agentKey`).

For "set up my agents" or "design a team", start with the
[agent-design guide](../arc-e-agents/agent-design.html) and the
[agent-design skill](../examples/skills/agent-design/SKILL.md). They cover discovery,
patterns, previews, approval and proof. The
[solo-founder starter](../examples/agents/solo-founder/README.md) has three seats and
a role-queue wake.

## Messaging: conversations, role inboxes and progress notes

Send a GIF or meme with `send_message` or `reply`, using `media: {url, alt}` or
`media: {path, alt}` (exactly one source; alt required). A GIPHY page ending in
`<slug>-<id>` maps to `https://i.giphy.com/<id>.gif`. The host caches verified GIF,
WebP, PNG or JPEG bytes up to 8 MB; the Inbox never fetches an external image.
Local paths resolve the sender's own team content first and then that local session's
captured workspace; other sessions' workspaces and arbitrary home paths are refused.
Remote sessions cannot read files from their remote workspace with `path`.
Team `config/inbox.yml` controls `inbox.media` through `media: {remote: auto|ask|off,
hosts: [...]}`. Auto requires an allowlisted host and a session on your own admitted
machine; other cases ask, and off prevents remote media. OS Reduce Motion shows a
still with play/pause. Use images for light moments, never `needs-you`, and avoid
flashing GIFs. `attachFile` separately attaches a logical team-content document.
See [Send a GIF or image](../arc-f-inboxes-and-triage/messages.html#send-media).

**Instance inbox.** Every session with a handle has an inbox, kept in its team's store.

- `send_message` `{to, text, session_id}` sends to a handle from `list_sessions`, or to
  `"human"`. `reply` answers a message to whoever sent it. `read_messages` drains your own
  unread mail (it marks it read).
- A reaction is not mail. `react {message_id, emoji, remove?}` puts one emoji on a message,
  once per reactor and emoji; repeating it takes it off. The sender hears about it once,
  through `reactions` in `read_messages`, and it never wakes them.
- CodeSense binds the MCP connection to its tab at launch, for Claude and Codex, including
  a CLI started by hand inside a CodeSense terminal. Omit `{session_id}` to use that identity.
  If supplied, it must name the same tab; another session or `human` is refused. Messages
  show the tab's current name. A connection outside CodeSense has no sender identity and
  is refused; passing a session id cannot bind it. Only the native app answers as the person.
- For example, a bound worker can call `send_message {to:"human", text:"Checks passed"}`,
  `read_messages {}`, `reply {message_id:7, text:"Done"}`, and
  `signal_done {status:"success", summary:"Checks passed"}` without a session id.
- A Claude recipient that CodeSense launched or that runs in a CodeSense tab has an **inbox
  waiter**: new mail wakes it at once (an idle agent starts a turn, a busy one hears at its
  next step) with "You have N new messages. Call read_messages…", and nothing is typed into
  its terminal. A Codex recipient there (Codex 0.159.3 or newer) runs on its own engine
  (a Codex app-server CodeSense starts for that session, with the Codex terminal attached
  to it), and is told the same notice through that engine: while it is idle, its host
  starts it a turn with the notice, headed by `[inbox <token>]`, as if it were a prompt;
  while it works, its own hooks hand the notice over at its next tool call or prompt (mail
  after its last tool call goes as a turn once that turn ends). Nothing is typed into its
  terminal, so a draft or a person typing there is never disturbed. Either decides when to
  read. Each batch of mail is told once.
- Inbox mail comes from people and agents that the user's setup lets reach an agent, so anyone
  who can message an agent is someone the user trusts. An agent answers a message with `reply`. How it
  acts on its mail (what it takes on, when it answers, what it asks first) is up to its prompt
  or its Agent definition.
- A Codex that has not had a turn yet runs no hook, so its host cannot see it at its prompt: it
  reads `inboxWaiter: false`, and its mail is held and offered once its first turn ends.
  Launched with a starting prompt, it has that turn at once.
- An agent CodeSense cannot tell is told nothing (`inboxWaiter: false` in `list_sessions`).
  That is one whose CLI has no wake path, a Codex older than 0.159.3, or one whose process
  CodeSense cannot name, as for one started outside a CodeSense tab, under tmux, or detached.
  Wake it with `send_to_session`, telling it to call `read_messages`, or give its Agent a
  wake routine on the inbox-idle event.
- `inbox_list` reads inbox history without marking anything read.

**Why can't agent Y message agent Z?** Check, in order:

1. Is Z in `list_sessions`, and is `to` exactly Z's `handle`? An unknown recipient returns
   `{status: "invalid"}`.
2. Is Y's MCP connection bound to its CodeSense tab? `who_am_i {}` shows that identity.
   If binding is unavailable, start the CLI inside a CodeSense terminal tab. An explicit
   `sessionId` must name that same tab; it cannot supply a missing identity.
3. Does Z have an inbox waiter? With `inboxWaiter: false` in `list_sessions` (its CLI has no
   wake path, or CodeSense cannot name its process), the mail is stored but nothing tells Z: see its `unread` and
   wake it with `send_to_session`. With a waiter, `mailNotice: "told"` shows Z was told; it
   decides when to read.
4. Is Z's `unread` null? Its team is not synced on this Mac, so its inbox can't be read here.
5. Was the text blank? Blank text is refused.

**Role inbox.** A role is any name the agents agree on, usually an Agent id; no name is
special. Mail to a role is one queue shared by every live holder of that role.

- Send with `send_role_message`. A session joins with `register_role` (pass its `sessionId`
  and CodeSense keeps the registration alive while the session runs; `exclusive: true` allows
  only one live holder, for a single triage desk or manager), takes deliveries with
  `claim_role_messages`, records a disposition with `accept_role_message` (`work` or
  `no_work`), finishes with `ack_role_message`, and leaves with `release_role` (unaccepted
  claims return to pending).
- Delivery states: `pending` → `claimed` → `accepted` → `acknowledged`. "Unclaimed work" means
  `pending` deliveries.
- `role_state` shows each role's registrations and deliveries. `codesense_overview`'s
  `roleQueues` shows the counts for the active team.

**A controller is just an agent you design.** CodeSense decides nothing about who is in
charge: there is no built-in manager or controller. A "manager" is a saved Agent that holds a
one-holder role (`register_role {role, sessionId, exclusive: true}`), works that role's queue,
launches workers with `open_workspace {launched_by}`, tracks them with `list_sessions`, and
asks workers to send questions with `replyTo: "role:manager"` so answers come back to its
queue. The [manager recipe](../setting-up-agents/manager-agent.md) builds one, with a skill and
two routines that wake it and restart it.

**The human.** `send_message` to `"human"` lands in a conversation in the Inbox.
Its badge counts all unread human mail, of either kind, excluding archived mail.
`ask_user` asks a blocking question and waits for the answer. `inbox_post` creates an
attention item for agent views and dashboards. `get_human_status` says whether
the human is `online`, `away`, `busy` or `offline`.

**Message kind and progress notes.** Set `kind: "needs-you"` on a question or decision
for the human and `kind: "update"` on a report. Kind marks whether mail waits on the
human for agent views and dashboards; both kinds stay in normal conversations.
Without a kind a message is an update, unless it carries a `replyTo` naming someone
other than the human, its sender or a role its sender holds. The send result echoes
the `kind`, with a `hint` when an unmarked question defaults to update.
`inbox_list {kind}` filters mail without marking it read.
Waiting terminals and progress remain visible in agent workspaces and do not add
to the human Inbox badge. `post_agent_message` progress notes do not appear in the
native human Inbox; dashboards can read them with `list_agent_updates` or
`codesense.updates()`. Use `send_message {to: "human", kind: "update"}` for a report
the human should receive as mail.

## Metadata triggers and command routines

A metadata trigger fires when a selected document enters a field value, with an optional
previous value. [The trigger cookbook](../arc-g-routines/trigger-cookbook.html#metadata_change)
shows a status-change example. A command routine runs an argv command without an agent;
`after` chains a same-space routine from its result. Read [command routines](../arc-g-routines/command-routines.html).

## Routines

- An **agent routine** is a saved Agent's standing task: by hand, on a schedule (`when`: presets
  such as `daily 09:00` or a 5-field cron) or on an event (an agent finished; a session sits
  idle with unread mail). It belongs to that Agent and lives under it, in
  `config/agents/<id>/routines/<rid>/routine.yml`. Each fire is recorded in its run history.
- **Mode** says what a fire does. **wake** types the task into the Agent's running session
  when it is idle, so it keeps its context, and never starts a second one; if none is
  running it starts one with the task. **fresh** ("start a new" in the app) starts a new
  instance every time. Wake needs a saved, non-starter Agent; built-in starters are fresh
  only, and an Agent that skips permission checks needs `allowWake: true`.
- **Machine** says where: this Mac (local, the default), a machine id, or any connected
  machine. A remote wake is delivered by that machine, so it lands when the session is idle
  even if the desktop drops meanwhile.
- A wake run is queued while it waits for the agent to be idle, then ends delivered,
  coalesced (folded into one already waiting), undelivered (with a reason such as machine
  offline or session ended) or unknown. Unknown is never re-sent:
  check the terminal first. A woken session finishes its run with `routine_done`, which
  leaves the session running; a fresh run ends when the agent exits, calls `signal_done`, or
  hits `maxRuntime`.
- A team routine runs on exactly one Mac (`runs_on: one_mac`; `armed_here` says whether that
  is this one) or on each Mac that turns it on (`each_mac`). Routines never deliver results
  by themselves: the task must say where to send or write them.
- A routine can publish a structured result (`resultSchema`, then `routine_publish_result` and
  `signal_done`); dashboards read the cached result without launching anything.
- A **metadata trigger** watches a selected document enter a field value, e.g. a story
  moving from in-progress to review. `routine_create`/`routine_update` take
  `metadataTriggers: [{selection:{where:{kinds:["story"]}}, target:{key:"status",op:"eq",value:"review"}, previous:{key:"status",op:"eq",value:"in-progress"}}]`.
  Omit previous for any entry into the target. Discovery and restart baseline existing
  matches; they never replay them. Metadata triggers cannot use each_mac.
- A **command routine** uses `action: command` and an argv array such as
  `command: [python3, scripts/check.py]`. It has no agent, task or mode, runs locally,
  and lives under `config/agents/_command/routines/<id>/routine.yml`. No implicit shell.
  Exit 0 succeeds; nonzero fails. `after: {routine: check, on: succeeded}` chains a
  same-space routine; `finished` continues after succeeded, failed, timed_out,
  needs_you or ended. A cancelled run does not fire a chain. The next task or argv
  reads stdout with `{{trigger.result_text}}`, and why the run ended with `{{trigger.detail}}`.
  `{{trigger.result_text}}` is what the agent produced; `detail` is CodeSense's explanation of why
  the run ended (for wake, machine and admission runs that explanation is in `{{trigger.result_text}}`). See [command routines](../arc-g-routines/command-routines.html).
- For inbox_idle, agent_done and role_pending, use `events` in `routine_create` or
  `routine_update`; see the [event cookbook](../arc-g-routines/trigger-cookbook.html#events).
  role_pending with no `role` watches every queue in reach; add `role: reviewer`
  for reviewer mail only. Creation supports `dryRun:true` with resolved triggers.
- `routine_arm_on` moves the routine between this Mac and a machine safely. Read
  placement with routine_get; do not hand-edit armed_host, armed_machine or arm_epoch.
  The [trigger cookbook](../arc-g-routines/trigger-cookbook.html) includes MCP dry runs
  and complete YAML for all six trigger kinds.
- In the app, routines live in the **Agents** tab: each Agent's **Routines** section, and the
  **Agents | Schedule** view of upcoming fires and recent runs across Agents.

Tools: `routine_list` (enabled, next run, last run, needs you), `routine_get`, `routine_runs`,
`routine_result`, `routine_create`, `routine_update`, `routine_set_enabled`,
`routine_run_now`, `routine_arm_on`, `routine_context` and `routine_done` (inside a run); `agent_get` lists an
Agent's routines.

**Timers** are temporary routines for one running session, saved Agent or not, on any
machine. "Add a routine for your session to check the build in 20 minutes" is a timer:
`schedule_wakeup` (`in`, `at`, `every`), `list_wakeups`, `cancel_wakeup`, always with the
caller's own session id. A timer types its prompt into that same session when it is idle,
never starts or reaches anything else, is not written to git, and ends with the session.
"Make a routine for Barb to do this every weekday" is a routine on Barb. People set timers
with **Remind this agent…** on a terminal tab, and turn one into a routine with
**Keep this…**.

## Notes

- **Notes** are the person's private Markdown notes for one org: plans, prompts, todos and
  findings, shown in the app's Notes pane. They stay on this Mac (a local git folder that is
  never pushed) and one org's notes are never visible from another.
- Agents read and change them with the notes tools. Each write is credited to the calling
  session (its tab name, as `who_am_i` shows it; pass your `sessionId`) in the note's
  `updatedBy` and in its history, and the open Notes pane updates at once.
- Every change is a revision: an agent write at once, the person's typing after about 30
  seconds of quiet. `notes_history` shows who changed a note and when, `notes_read_at` reads
  an old version, and `notes_restore` brings one back as a new change, so nothing is lost.
- CodeSense only stores notes and their history. How notes are organised (tidying, merging,
  checking off tasks) is up to agents and the person; a routine can do it on a schedule.

Tools: `notes_list`, `notes_read`, `notes_search`, `notes_create`, `notes_write` (whole body,
`append`, or an exact `patch`), `notes_history`, `notes_read_at`, `notes_restore`.

## Dashboards and the `window.codesense` bridge

To customize Inbox, Notes or the agent list, use the [built-in pane recipe](../arc-i-dashboard-primitives/customize-built-in-panes.html). It links the shipped sources, worked changes, selection controls and rollback steps.

- A **dashboard** is an HTML file in team content, opened in CodeSense, or a **pane**: one HTML
  file in `<team>/panes/<id>.html` shown in the right pane.
- Its scripts get `window.codesense`, a bridge to live app data and actions. Examples:
  `codesense.queryContext(q)` and `subscribeContext(q, cb)` (context files by metadata),
  `codesense.sessions()` (live sessions), `codesense.agents()`, `codesense.routineResult({key})` (`key` is a `routines()` row's `id`),
  and `codesense.act(verb, args)` (`openWorkspace`, `runPrompt`, `runSkill`, `sendToSession`,
  `openTab`, ...). Every call returns a Promise.
- Style with the app's theme variables (`--cs-bg`, `--cs-fg`, `--cs-accent`, ...), in a pane
  or a centre tab. Never call external services from the page; configured agents and
  routines do that.
- Every live page also gets the **dashboard kit**, so it can look like the built-in panes
  with no boilerplate: spacing, radius, type and status tokens (`--cs-space-*`,
  `--cs-state-running`, ...), opt-in `cs-` classes (`cs-app` on `<body>`, `cs-card`,
  `cs-table`, `cs-badge`, `cs-btn`, `cs-tabs`, `cs-dot`, `cs-empty`, `cs-toolbar`,
  `cs-drawer`) and `window.codesense.kit`: `icon(name)` (Lucide), `statusDot(state)`,
  `statusBadge(state)`, `avatar({face, name})`, `layoutDag(nodes, edges)` and `dagView(el,
{renderNode})` for live, pannable graphs (grouped by epic or machine if you like), `keyed()`
  for flicker-free lists, `skeleton()`, `h()` for safe DOM, `call(verb)` and `live(render,
{events})`.
  The `kit` section of `get_live_data_contract` lists it all.

**How to build a dashboard that shows X:** read `get_live_data_contract` for the full bridge
reference, write the page with `pane_write` (then `pane_open`) or as an `.html` file in team
content, and check it with `validate_live_data_doc`. For data that needs an agent (an external
service, an expensive analysis), make a routine with a `resultSchema` and read it on the page
with `codesense.routineResult`. Examples ship in these docs under `examples/dashboards/`,
including reference rebuilds of built-in panes: `agent-dag.html` (who launched whom, live),
`pipeline-graph.html` (story dependencies) and `routines-console.html` (routines and runs).

**HTML messages are live dashboards.** A `send_message` to the human with
`contentType: "html"` is meant to render in the Inbox as a live dashboard: scripts, the
`window.codesense` bridge and live events, like a dashboard file. Use it to send the human a
live view instead of a static report. Give it a `summary`: one plain-text line (at most 280
characters) that the Inbox and other lists show as the message's preview, since a preview
derived from HTML markup reads badly. A long markdown message deserves one too. Check the
current `send_message` description for what the installed version renders.

## Panes and slots

A pane declares where it fits (right pane or dock), its capabilities and its commands.
Use `pane_write` and `pane_open`, and adapt to slot context. Read
[slots and pane shapes](../arc-i-dashboard-primitives/pane-shapes.html#pane-manifest)
and the linked declaration reference before authoring one.

## Records

Records are structured, revisioned items in a named collection. `records_create`,
`records_list` and `records_write` let agents and dashboards share that data.
Read [records](../arc-i-dashboard-primitives/records.html) for collection and field examples.

## Roles registry

The role registry stores queue display names, icons and default handlers; a saved
agent's display role does not itself register a queue holder. Use `role_save` and
`roles_list` for configuration, `register_role` and `role_state` for live holders.
Read [roles](../arc-f-inboxes-and-triage/roles.html#configuration).

## Workspace groups

A workspace group collects related workspaces for navigation. Use `workspace_group_create`,
`workspace_group_add` and `workspace_group_list`; the
[generated tool reference](../generated/mcp-tools.json) gives their arguments.

## Canvas

A canvas is an editable Excalidraw scene. `apply_canvas` writes the whole scene;
`render_canvas` returns its current rendered PNG so you can inspect it.
Read the [generated tool reference](../generated/mcp-tools.json) before replacing a scene.

## Copy between contexts

`copy_between_contexts` copies an item by its stable logical name to another content
context. Use `list_context_content` to get that name first. This is an explicit copy, separate
from inherited paths. The [generated tool reference](../generated/mcp-tools.json)
spells out source and destination contexts.

## Updates, network calls and removing CodeSense

How the app updates itself (Settings → General → Updates, Check now, Restart to update),
what it sends on its own and how to remove it and its files are on
[Updates, network calls and removing CodeSense](../arc-n-reference/updates-and-uninstall.html#updates).
Read the page rather than guessing: update behaviour isn't visible through the MCP tools.
`PRIVACY.md`, at the root of the CodeSense source, lists every network call the app makes.

## These docs

These docs are a read-only context repo, org `codesense/docs`, team `getting-started`, that
every CodeSense install places on its Mac (its path is in `codesense_overview`'s `docs`
section). Find a page with `query_context` `{"where": {"orgs": ["codesense/docs"]},
"text": "..."}`; rows carry the file path. The handbook is under `handbook/`, examples under
`examples/`.

Docs metadata can narrow discovery without text: `query_context`
`{"where":{"orgs":["codesense/docs"]},"filters":[{"key":"cs:topic","op":"eq","value":"routines"}]}`.
Select `cs:topic`, `cs:task`, `cs:tools` or `cs:audience` to return the canonical list fields.
Text searches rank titles and headings, return heading anchors, and report a default
`generated/` exclusion. Set an explicit `where.pathPrefix: "generated/"` to search references.
