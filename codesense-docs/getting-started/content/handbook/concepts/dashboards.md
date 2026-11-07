# Dashboards

_An HTML file in your context is a live page: where it shows, what it reads, how its buttons act, and how external data arrives through routines._

A dashboard is one `.html` file. Put it in your team's context and CodeSense opens it as a live page: it injects `window.codesense`, applies the app's theme as `--cs-*` CSS tokens, and routes its links and buttons to the app. There's no build step and no framework to install.

## Where a page shows

The same kind of page has three places to live:

- **A tab.** Any HTML file in your team's `content/` (or your private space) opens as a tab in the viewer.
- **A right pane.** An HTML file in the team's `panes/` folder, beside `content/` and `config/`, is a custom right pane. The team and its child teams see it. One in your private space's `panes/` is yours alone. Agents write and show panes with the MCP tools `pane_write` and `pane_open`.
- **An Inbox message.** A message an agent sends you with `send_message` and `contentType: "html"` renders in the Inbox like a dashboard file: its scripts run and its buttons work.

## What a page reads

Pages read your local context and app state, never a service directly:

- `codesense.queryContext()` finds context files by their metadata, and `subscribeContext()` keeps the result live. See [Query your context](../guides/query-your-context.md). A page sees team content across your orgs, and private items from its own org only.
- `codesense.context()` says where the page lives (`{org, team, layer, contentRoot}`), so it can scope a query to its own team.
- `sprints()`, `taxonomy()`, `sessions()`, `agents()`, `inbox()`, `routines()` and `worktrees()` read the matching app state.
- `codesense.sessionUsage()` returns tokens and cost per agent session and per day, with the story each session worked on. See [Usage and ROI](../guides/usage-and-roi.md).
- `routineResult()` and `routineStatus()` read a routine's cached result and its status. Show when a result is missing, stale or failed instead of presenting it as current.

The full list is in the [page SDK reference](../../generated/page-sdk.md).

## What a page does

A page calls action verbs such as `openWorkspace`, `openFile`, `setStatus` or `setField` directly. There is nothing to declare and no prompt. Rate guards stop a page that loops: the call fails with a reason such as `denied: write budget`.

Opening a page never starts an agent or a routine on its own. A **Refresh** button can call `routineRefresh` for a configured routine; call it only from a click, never while the page renders.

## External data comes through routines

A page doesn't receive service credentials, and CodeSense doesn't proxy service requests. For external data, a routine runs an agent with its own tools and logins. The agent collects the data and stores the result with `routine_publish_result`, and the page reads the cached copy. A routine can also run on its schedule, separately from anyone opening the page.

## Author and share

Ask an agent to read `get_live_data_contract`, write the page, and check it with `validate_live_data_doc` before saving. Keep credentials out of page files and routine results. Saving a pane's file reloads it within a second.

To share a page with your team, commit and push it with the rest of your context; teammates get it on their next pull. A page snapshot (`codesense.snapshot()`) freezes a view into static HTML. That's different from the routine result that supplies the page's data. See [Have an agent build a dashboard](../guides/agent-builds-dashboard.md) for the workflow.

A dashboard folder with a `dashboard.yml` manifest can also be published to share.codesense.ai for people you invite (`create_dashboard`, `publish_dashboard`, `share_dashboard`, `unpublish_dashboard`). A published page can't use the in-app SDK: it reads the data files published with it, and optionally live answers from your Mac.

The full guide ships in the app's docs: **Dashboard primitives**.

## A live page in one file

Any `.html` file in your context repo is a live page. It reads your stories, sessions and usage, and it’s versioned in git like everything else.

```html title="web/content/dashboards/teams.html"
<!doctype html>
<title>Orgs I can see</title>
<body class="cs-page">
<div id="out">Loading…</div>
<script>
function reload() {
  codesense.invoke("listOrgsTeams", {}).then(function (data) {
    var out = document.getElementById("out");
    out.textContent = data.orgs.map(function (o) { return o.name; }).join(", ");
  });
}
reload();
codesense.subscribeContext({}, reload);   // stay live
</script>
```

Open it in the app and it runs. Style it with the theme tokens (`--cs-bg`, `--cs-fg`, `--cs-accent` and the rest) and it follows the app’s theme.

## The kit

Every live page also gets the dashboard kit, so it looks like the built-in panes with no boilerplate: theme tokens, `cs-` classes, and helpers such as `statusBadge`, `avatar`, `dagView`, `keyed` and `skeleton`.

An agent builds a page from the live-data contract and the kit, and checks it with `validate_live_data_doc`. The docs ship example dashboards, such as a sprint page and a kickoff page, ready to copy into your team.

## A terminal in a dashboard

A page can show a live agent terminal, and you can type into it. On a shared page, only the owner can drive the terminal.

That makes a review station possible: a page with the story, its diff and the agent’s live terminal side by side, so you can answer it without switching tabs. Buttons on a page can open worktrees, message sessions or type into an agent, and delete or move files. Dashboards are your own files, so open only ones you trust.

## On your own servers

> **Coming: the engine on your own servers**
>
> The engine on your own servers: publish your dashboards to your own server instead of share.codesense.ai. Teammates reach them through your own private network layer, such as Tailscale or whatever you already use, from a laptop or a phone, and company data stays on company machines.

> **Coming: CodeSense on your phone, from your own servers**
>
> All of CodeSense on your phone, from your own servers: review plans, chat with your agents, Notes, the pipeline and more. It runs on the CodeSense engine on your own servers, with no CodeSense cloud, and only you can get in.

See the [Roadmap](https://codesense.ai/roadmap) for what’s next and what’s planned.
