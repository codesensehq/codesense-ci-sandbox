# Dashboard uses

_Six dashboards teams build first, and where each gets its data. Each is an HTML file in your context repo: a tab, a right pane or an Inbox message._

## A bug bot, with its ROI

A routine reads your error tracker each morning through its MCP server and posts a live report to your Inbox, with a button to open a worktree per fix. Add what it found, fixed and cost, and what to improve next.

```text title="routine_create"
{
  "name": "Morning bug bot",
  "agent": "triage",
  "when": "weekdays 08:30",
  "task": "Read new errors with the tracker's MCP tools. File a story for each real one. Then send_message the human an html report with an Open worktree button per fix, and signal_done."
}
```

The triage agent needs your error tracker’s MCP server in its `mcp.servers`. Put the cost of each run in the report with `story_totals`, and the bot shows its own ROI.

## An issue tracker board

Your GitHub, Jira or Linear issues next to the stories and worktrees working on them. Connect any issue tracker with a dashboard.

A routine pulls issues through your tracker’s MCP server or CLI and files them as records or files in your context repo, with a link back to each issue. The board is a page that queries those files next to the stories that work on them.

## Sprint and pipeline view

Every story by status, sprint and dependency, across repos, updating as agents move them. The pipeline view, the Stories dashboard and the Journey dashboard ship in the app; build your own on the same query when you want a different cut.

```text title="A board, grouped by status"
codesense.subscribeContext(
  { where: { kinds: ["story", "bug", "spike"] }, match: { sprint: "2026-11" }, groupBy: "status" },
  function (result) { /* one column per result.groups[i].value */ }
);
```

## Cost per story

What each story, epic, agent and model cost, from your own rates.

One `codesense.storyTotals` call gives cost, sessions and time for up to 500 stories. Set your rates with `set_model_rate`. A live AI-usage dashboard also ships in the app.

## Agent activity

Who is working on what, on which machine, and who is waiting on you.

`codesense.sessions()` lists the live sessions and `onSessions` keeps the page current. Add a button that jumps to an agent’s terminal, or one that stops it.

## Client progress

A live progress page for a client, published to invited viewers on share.codesense.ai.

Publish the folder with `publish_dashboard` and list in its `dashboard.yml` which queries the hosted copy may read. Hosted pages read sessions, usage and story data, with paging. Your client opens it from an email invite, on a laptop or a phone.
