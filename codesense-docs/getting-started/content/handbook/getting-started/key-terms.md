# Key terms

_Context repos, teams, stories, worktrees, agents, routines and the rest, in one page._

| Term | Meaning |
| --- | --- |
| Local profile | Whose local workspace CodeSense opens on this Mac. Profiles keep their context repos, files and routines separate. It is not a CodeSense account; there isn't one. |
| Context repo (org) | A git repo you own that holds your teams and their content: agents, routines, epics, stories, docs and dashboards. CodeSense reads and writes its files and never commits, pushes or merges there. |
| CodeSense Docs | The read-only context repo of these docs that ships with every install and opens on first launch. |
| Team | A named group inside a context repo, with its own content, source repos and an optional parent team. The closest team wins. |
| Private | Your own space in each context repo. Only you see it. |
| Source repo | A code repository on a team's list. You attach it to a clone you made with git. |
| Epic | A folder of stories. |
| Story | A markdown or HTML file with a few metadata fields: `type`, `status`, `source_repo`, `depends_on` and any of your own. |
| Worktree | A git worktree of a source repo for one story, on its own branch, beside your clone. The first open creates it; it stays until you remove it. |
| Workspace | Where you work on one story or task: its terminal tabs and tiles, its worktree when it has one, and the diff, editor, Story HUD and Inbox overlays. |
| Context hydration | Copying team context into a workspace: when an agent launches there, or when an agent calls `hydrate_worktree` or `hydrate_story`. Never in the background. |
| Agent | A saved definition, `config/agents/<id>/agent.yaml`: which CLI (Claude Code, Codex or OpenCode) runs, what it starts with, its MCP servers and permissions. Each CLI uses its own login. Open **Agent definitions** from the context repo menu. |
| Machine | This Mac, or a remote machine you reach over SSH with your own account. Open **Machines** from the context repo menu. |
| Routine | An agent's task that runs on a schedule, on an event such as a message waiting for a role, or on demand. Shown under **Schedule**. |
| Routine result | A stored result a routine publishes. Dashboards read it without launching an agent. |
| Inbox | Messages between agents and you, in two lanes: **Needs you** and **Updates**. |
| Dashboard | An HTML file in your context that opens as a live page over your stories, sessions and routine results, with actions it calls directly. |

Start with [Create or connect a context repo](workspace.md) or [Have an agent build a dashboard](../guides/agent-builds-dashboard.md).

The full guide ships in the app's docs: **Reference → Glossary**.
