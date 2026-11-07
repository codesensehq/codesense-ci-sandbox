# Start here: by task

| To do this | Tools | Read |
| --- | --- | --- |
| Build a pane and choose its slot | `pane_write`, `pane_open`, `get_live_data_contract` | [Slots and pane shapes](arc-i-dashboard-primitives/pane-shapes.html#slots) |
| Build a file tree pane | `pane_write`, `pane_open` | [File tree panes](arc-i-dashboard-primitives/linking.html#file-tree) |
| Build a fast, light dashboard (cached list, visible-only totals, per-section loading) | `get_live_data_contract` | [Fast, light dashboards](arc-i-dashboard-primitives/fast-light-dashboards.html) |
| Customize the Inbox pane | `pane_write`, `pane_open` | [Customize built-in panes](arc-i-dashboard-primitives/customize-built-in-panes.html) |
| Fire a routine on a metadata status change | `routine_create`, `routine_update` | [Metadata trigger cookbook](arc-g-routines/trigger-cookbook.html#metadata_change) |
| Fire a routine on inbox mail or agent completion | `routine_create`, `routine_update` | [Event trigger cookbook](arc-g-routines/trigger-cookbook.html#events) |
| Run a routine for one role queue or every queue in reach | `routine_create`, `routine_update` | [Role queue triggers](arc-g-routines/trigger-cookbook.html#role_pending) |
| Run a command and chain routines | `routine_create`, `routine_runs` | [Command routines](arc-g-routines/command-routines.html) |
| Cancel a queued or running routine run | `routine_runs`, `routine_cancel`, `routine_run` | [Cancel one run](arc-g-routines/index.html#cancel-run) |
| Design an agent team | `agent_preview`, `agent_create`, `role_save` | [Agent design](arc-e-agents/agent-design.html) |
| Nest teams and declare shared content | `set_team_parent`, `list_orgs_teams` | [Teams and inheritance](arc-b-model/teams.html#set-team-parent) |
| Sync context with git | `context_repo_status`, `report_context_sync` | [Agent-driven git](arc-k-sync/agent-driven-git.html) |
| Add a machine | `save_machine`, `connect_machine` | [Machine setup](arc-d-terminals-and-machines/setup-machine.html) |
| Share a dashboard | `codesense_search_tools`, `codesense_get_tool_schema` | [Sharing dashboards](handbook/concepts/dashboards.md) |
| Read run evidence | `list_runs`, `read_run_evidence`, `read_run` | [Run tools reference](generated/mcp-tools.json) |
| Read what a story changed: sessions, commits, files, tasks, cost and time | `story_changes` | [Story contributions](arc-e-agents/mcp-only-agents.html#tool-story_changes) |
| Inspect a story's sessions and conversation | `story_changes`, `session_read` | [Story trail](arc-i-dashboard-primitives/index.html#story-trail) |
| Check docs freshness | `codesense_overview`, `update_codesense_docs` | [Docs freshness](concepts/README.md#docs-freshness) |
| Store and query records | `records_create`, `records_list`, `records_write` | [Records](arc-i-dashboard-primitives/records.html) |
| Configure the roles registry | `role_save`, `roles_list`, `role_state` | [Role configuration](arc-f-inboxes-and-triage/roles.html#configuration) |
| Group workspaces | `workspace_group_create`, `workspace_group_add`, `workspace_group_list` | [Workspace group tools](generated/mcp-tools.json) |
| Create and verify a canvas | `apply_canvas`, `render_canvas` | [Canvas tools](generated/mcp-tools.json) |
| Send a GIF, meme or document attachment; configure remote media | `send_message`, `reply` | [Send a GIF or image](arc-f-inboxes-and-triage/messages.html#send-media) |
| Keep a badge fresh while hidden | `get_live_data_contract` | [Live pane badges, with the Inbox example](arc-i-dashboard-primitives/reacting-to-state.html#live-badge) |
| See which Claude or Codex account a launch uses, or why one was refused | `list_agent_accounts`, `resolve_agent_account`, `agent_get` | [Agent accounts](arc-e-agents/agent-accounts.html) |
| Set or review an agent or fresh routine's spend cap | `agent_update`, `routine_update` | [Spend caps](arc-e-agents/spend-caps.html) |
| See what a task was for, its targets and how it went; add a sub-task or a verdict | `task_list`, `task_read`, `task_start`, `task_update` | [Task records](arc-e-agents/tasks.html) |

Hosted publishing depends on the installed build. Discover its available tools and read
their schemas before trying to share a dashboard.
