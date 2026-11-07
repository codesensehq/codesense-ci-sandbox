# Have an agent build a dashboard

_Have an agent build a live page from your context, routine results and session usage, then publish it to share.codesense.ai for the people you invite._

Use an agent launched from CodeSense so it has the CodeSense MCP tools. For external services, authenticate the agent's own tools; there is no CodeSense service connection to configure.

A dashboard is an HTML file in your team's context. CodeSense opens it as a live page, and the page reads context, routine results, agents and session usage through the in-app page SDK (`window.codesense`). It runs actions only when you click something on it.

## A dashboard inside CodeSense

### Ask for the page

```text
Build a team dashboard showing blocked stories and review requests.
Read get_live_data_contract before implementing it.
Use local context queries for stories. For external review data, configure
an agent-owned collection routine that publishes a routine result.
Read that cached result when the page opens; show its age and errors.
Provide an explicit Refresh action using the documented routine SDK.
Do not launch an agent on page load or put credentials in the page or result.
Validate the page with validate_live_data_doc and fix blocking findings.
```

### What the agent prepares

1. Reads the live-data contract for the current SDK and action verbs.
2. Creates a collection routine using its own service tools and publishes structured data with `routine_publish_result`.
3. Builds a page that reads local context (`codesense.queryContext` or `codesense.subscribeContext`) plus the cached `routineResult`/`routineStatus` data. Missing results have an honest empty state.
4. Adds an explicit `routineRefresh` action. An `openWorkspace` action can open the relevant story.
5. Validates the page and saves it in the team's content folder, for example `dashboards/review-board.html`.

Call `codesense.ready()` once your page has rendered; a pane that doesn't, and never subscribes, falls back after 3 s.

### Open and refresh

Open the page in CodeSense. Opening it shows the cached information without a service request or agent launch. The tab reloads when the file changes on disk. Choose **Refresh** when you want a new collection run, and follow the run under **Agents → Schedule**.

A service error belongs to the collection run. Reauthenticate or repair the agent's tool configuration, then retry the routine. Do not add tokens, authorization headers, or service proxy calls to the dashboard.

## What your agents cost, story by story

CodeSense records the tokens and cost of every Claude Code, Codex and OpenCode session it launches on this Mac, and of every Claude Code and Codex session on the remote machines it connects to. Each session also carries the story it was spent on, its agent and its models. A page reads this with `codesense.sessionUsage`, and an agent with the `session_usage` MCP tool, so a dashboard can set what a story cost against what it shipped.

### Ask for a cost dashboard

```text
Build a dashboard of what our agents cost per story this month.
Read get_live_data_contract first.
Use codesense.sessionUsage for cost and tokens, grouped by session.story
(and by epic), and codesense.queryContext for each story's status.
Show cost per done story and per model. Leave a field for the hours we
estimate each story saved, and say it is our estimate.
Validate the page with validate_live_data_doc.
```

### What the page reads

```js
const { sessions, days, totals } = await codesense.sessionUsage({
  sinceMs: Date.parse("2026-10-01"),
});
// One story only: codesense.sessionUsage({ story: "LL-233" })
```

- `sessions`: one row per session, newest first, with `cli` (`claude`, `codex` or `opencode`), `agent`, `models`, `tokens` (`input`, `output`, `cacheRead`, `cacheWrite`), `costUsd`, and `story` as `{id, title, status, epic}`, or `null` when the session was not tied to a story. A session belongs to the story of its story worktree, or else the story its launch was opened for.
- `days`: totals per local day. `totals`: the whole range.
- `story: "LL-233"` keeps only that story's sessions, and the days and totals cover only them. `sinceMs`, `untilMs`, `sessionId`, `cwd` and `limit` narrow it further.

`costUsd` is `null` when a model has no price on file; `unpricedModels` names those models. Set a price with `codesense.setModelRate({modelPrefix, effectiveFrom, inputPerMtok, outputPerMtok})`. Only sessions CodeSense launched are counted, and CodeSense never measures time saved: any hours on the page are your team's estimate.

## A dashboard published to share.codesense.ai

Publishing a dashboard to share.codesense.ai is invite only: you choose who sees it. The tools are `create_dashboard`, `publish_dashboard`, `share_dashboard` and `unpublish_dashboard`. Parts that are decided but not built yet are marked.

Dashboards saved in synced team content, including dashboards shared from a parent team, appear automatically in **Identity → Dashboards**. Select a team dashboard and choose **Publish**; CodeSense records its folder on this Mac as part of publishing. Use **Open dashboard folder** for a dashboard folder outside your teams' content.

### Ask for it

```text
Build a dashboard of our AI usage that I can share on share.codesense.ai.
Start it with create_dashboard, and read get_live_data_contract (section hosted).
Have a routine write data/usage.json from session_usage every hour.
Then publish it with publish_dashboard and share it with ana@example.com.
```

`create_dashboard` writes a folder that already follows every rule on this page: the manifest with every file listed, an `index.html` and any extra pages, a shared `app.js` and `style.css`, and the data files. It refuses a folder path that is not absolute or that has a `.` or `..` part, and a folder that exists and is not empty. It never writes over an existing file, and it writes nothing when an input is wrong. Its answer lists the files, any warnings and the next steps.

### Live agents

Ask the agent to create a mission-control dashboard in your team's content folder:

```text
Create a dashboard with create_dashboard:
  folder: <absolute team context path>/content/dashboards/mission-control
  slug: mission-control
  title: Mission control
  template: mission-control
  control: true
Then publish it with publish_dashboard.
```

`control: true` is optional. Omit it to make the dashboard view-only, including for its owner. The mission-control starter lists this team's agents on the publishing Mac, puts those needing attention first, and opens the selected agent's live terminal. On a phone, the terminal fills the page; **Back to agents** returns to the list. Use the text-size buttons to enlarge output or **Fit text to width** to fit the columns where possible. Very wide terminals still scroll horizontally at the minimum text size. Scrolling up pauses following; **Jump to latest** resumes it. The terminal stream pauses in the background and resumes when you return.

For a list grouped by open workspace, use `template: workspaces`. Both templates accept `control: true`, share the same terminal view, and write their own page and live block: leave `pages`, `data` and `live` out of the creation request. You can edit the written files afterward.

Mission control writes this live block, with `control` only when requested:

```yaml
live:
  queries: [sessions]
  streams: [terminals, terminal]
  control: owner
```

The agent and terminal streams are restricted to this team. The declared `sessions` query follows the existing catalog: it can return redacted session metadata from other teams on the publishing Mac. If your custom page does not need that query, change `queries` to `[]` before publishing; the agent list and terminals continue to work.

**Only the dashboard's owner can type.** Sign in with a passkey or a password and a second factor, unlock the dashboard, choose an agent, then press **Take control** in the viewer's top bar. **Release** stops control. The page cannot take control itself. Invited viewers can watch, and see when the owner controls a terminal, but can never type or resize it. Terminal snapshots and output are redacted before they leave the Mac, as query answers are. Colour and cursor codes inside a secret don't hide it, and window titles and links are redacted too. Any value after a word like `token:` or `password=` is hidden, however short. Redaction recognises secrets by their shape and labels, so it can't promise to hide every secret: don't print secrets in a terminal you share. Dashboard transport and audit logs omit typed input. The program can echo it into the terminal output everyone watching sees.

The starter ships `cs-live.js`, pinned xterm.js and its stylesheet in the folder, with every file listed in the manifest. The toolkit uses the versioned page API through `postMessage`; the hosted page has no network access and loads no CDN assets. Keep these files listed when customizing the dashboard. Load `vendor/xterm.js`, then `cs-live.js`, then your page script.

- **`CS.ready()` and `CS.role()`** — The shell's role and control capability; role is `null` before hello, then `viewer` or `owner`. In the local app viewer, ready rejects after three seconds and the starter explains that live terminals work in the published dashboard.
- **`CS.query(name, args)`** — One declared query's reply, including `answer`, `source` and timestamps.
- **`CS.subscribe(...)`** — Repeated replies for `name`, `args`, `every_s` and `cb`; returns a function that stops the subscription.
- **`CS.listAgents(cb)`** — This team's local agent rows and stream state; returns a stop function.
- **`CS.watchTerminal(...)`** — Pass `handle`, `element` and `opts` to render the terminal snapshot and live output, with gap recovery, paused and ended states. Returns a view with `dispose()` and `onState(cb)`.
- **`CS.onPresence(cb)`** — Mac status (`mac`, `mac_seen_at`) or per-stream status (`wid`, `watchers`, `controlling`, `mine`); returns a stop function. Check which fields arrived.
- **`CS.viewer(cb)`** — Who is watching and the page's state: `role`, `mac`, `mac_seen_at`, `updated_at`, `stale` and `display` (the signed-in viewer's own email, or `owner`). Called now and on each change; returns a stop function. A hosted dashboard draws no viewer bar by default (`chrome: none`), so the page shows its own Mac status and offline or stale notice. Calling `CS.viewer` tells the viewer the page does this; a page that never calls it gets the viewer's own notice after 3 s.
- **`CS.endOwnerAccess()`** — Owner only, from a click. Ends this browser's owner access on the server, then the page reloads as a viewer. Rejects `not_owner`, `needs_gesture`, `busy`, `failed` or `timeout`.
- **`CS.sendInput(...)` and `CS.resize(...)`** — Pass `handle` and `text` for input, or `handle`, `cols` and `rows` for resize. Owner input only after Take control; otherwise rejects with `not_controlling`.

`CS.launch` and `CS.stop` are reserved names for later; they are not implemented. A page with no viewer bar can imitate CodeSense's own screens: never ask a viewer to type a password or code into a dashboard. Sign-out and the list of dashboards a viewer can open live on the share site's `/account` page, outside the page. Set `chrome: full` in `dashboard.yml` to bring the bar back. Read `get_live_data_contract`, section `hosted`, for the complete API, options and limits.

### Bug triage and declared actions

Use `create_dashboard` with `template: "bug-triage"` and `agent: "<agent_list id>"` to list this team's bugs with **Work it**, **Needs info** and **Won't fix**. Omitting `agent` leaves out Work it; the creation result explains how to add it. The activity feed shows action outcomes and links launched agents to the shared terminal view.

Only the signed-in owner can run actions. Invited viewers can follow the board and watch agents. Terminal input still requires **Take control**. Each action is declared in `dashboard.yml`; callers supply an ID and, optionally, a declared choice, never a path, prompt or command:

```yaml
actions:
  needsInfo:
    verb: setFields
    who: owner
    fields: {status: needs-info}
    match: {type: bug}
    max_per_hour: 5
    max_running: 2
```

`CS.act("needsInfo", {id: "BUG-41"})` returns a promise of the result. `CS.onActions(cb)` follows the last 50 calls and agent/routine progress. Unknown actions, mismatched IDs and extra arguments are refused on the Mac. `launchAgent` declares an existing agent, a fixed `prompt` and `mode` (`headless` or `visible-unfocused`); `runRoutine` declares an existing routine and fixed `vars`. Only `{id}` may be substituted, with templates limited to 500 characters. `choices` maps a choice name to fixed field values. The hard caps are 30 successful calls/hour and four running agents per action, plus 10 calls/minute per owner session. Calls are never retried automatically: after a timeout, check activity before trying again.

### The folder

A dashboard is one folder, inside a team's context repo: under that team's content folder, for example `<context repo>/<team>/content/dashboards/ai-usage/`. CodeSense shows it there, and live `queryContext` answers read that team's context. Outside a team's context repo CodeSense may not show the folder, and every `queryContext` answer fails; `create_dashboard` warns about both.

It can be a small multi-page web app:

```text
ai-usage/
  dashboard.yml
  index.html          the page viewers open first
  runs.html
  app.js
  style.css
  data/summary.json
  notes.md            not listed, so it never leaves the Mac
```

Only the files listed in `dashboard.yml` are uploaded, exactly as they are on disk. What you list is your responsibility: there is no filter on file content.

### The manifest

```yaml
slug: ai-usage
title: AI usage
publish:
  state: draft
  visibility: private
  files:
    - index.html
    - runs.html
    - app.js
    - style.css
    - data/summary.json
live:
  queries: [sessions, runs]
  fields:
    runs: [summary]
```

| Field | Rule |
|---|---|
| `slug` | 3 to 40 characters of `a-z`, `0-9` and `-`. The address is `https://<dashboard host>/d/<slug>`. Unique across publishers. |
| `title` | 1 to 200 characters after trimming. Shown in the viewer's top bar. |
| `publish.state` | `draft` or `published`. `publish_dashboard` and `unpublish_dashboard` set it; do not edit it by hand. |
| `publish.visibility` | `private` or `team`. `team` is reserved and is currently the same as `private`: only invited viewers get in. |
| `publish.files` | Every file the pages use, as relative paths inside the folder: no leading `/`, no `./` or `..`, no backslash, no symlink or hard link, each listed once. |
| `publish.entry` | Decided, not built. The manifest has no entry field yet; the viewer opens `index.html`, or else the first `.html` file in sorted order. Always have an `index.html`. |
| `live` | Optional. See [Live answers](#live-answers). |
| `viewers.session_days` | Optional. 1 to 365; the default is 90. How many days without a visit end a viewer's session. A session ends 90 days after sign-in at the latest. |

Limits: at most 50 files, 5 MB per file, 25 MB in all. Allowed types: `html`, `css`, `js`, `json`, `csv`, `txt`, `png`, `jpg`/`jpeg`, `webp`, `gif`. SVG and font files are not allowed.

### What a hosted page can and cannot do

On share.codesense.ai each page runs in a sandboxed frame with no network access. The viewer around it supplies the page's files:

- **Data.** `fetch("data/summary.json")`, relative to the page, is answered with the listed file. An unlisted file gets a 404, and a web address fails. `XMLHttpRequest`, WebSocket, cookies and `localStorage` do not work.
- **Scripts, styles and images.** `<script src>`, `<link rel="stylesheet">`, `<img src>` and CSS `url()` that point at listed files are inlined. Anything else is dropped: a CDN script, a web font, an unlisted image. Put libraries in the folder and list them; embed fonts as `data:` URLs.
- **No bridge calls.** `publish_dashboard` refuses a page that calls `window.codesense`, `codesense.act` or `codesense.<verb>(`, and names the file and line.
- **Links between pages** are relative (`<a href="runs.html">`) and must point at a listed page. `#anchors` work. A link to an unlisted file shows "That link is not in this dashboard."
- **Outside links** (`http`, `https`) are allowed and open in a new tab. Nothing is fetched for them at publish.
- **Links to other dashboards** are outside links to `https://<dashboard host>/d/<slug>`. They open in a new tab, and the viewer needs their own invite to that dashboard. Publishing one dashboard never publishes another.
- Only a real click follows a link, at most 10 every 5 seconds. A page that navigates away by script is rebuilt, and stopped after three tries.
- Each page load is a fresh document. Keep state in the URL fragment, or read it again from data files.
- Per page, at most 8 file reads run at once and 25 MB can be read in all. Read data once on load; the viewer reloads the page when a new version arrives.

Decided, not built: publish does not yet scan pages for references to unlisted files or to files outside the folder, and it does not yet report linked dashboards that are not published. Until then, such a reference just fails to load in the viewer, so check references before you publish.

The same page works in CodeSense on your Mac, which serves the folder's files, so open `index.html` there to check it before you publish.

### Keeping the data fresh

A dashboard is as fresh as its data files. An agent or a routine rewrites a listed data file, for example a routine that calls `session_usage` and writes `data/usage.json`. There is no built-in usage source.

While CodeSense is open on your Mac, a change to a listed file is pushed about 60 seconds after the last write, and every 15 minutes the Mac checks in. With `chrome: full` viewers see "Page updated N minutes ago" in the viewer bar; by default (`chrome: none`) the viewer draws nothing and the page reads the same time from `CS.viewer` (`updated_at`, `stale`, `mac`). The time is the dashboard's files were last published, measured by the server's clock. A check-in alone does not change it, and neither does a live answer; a live page can show its answers' own `answered_at` time. The viewer flags the page as stale when the Mac has not checked in for 30 minutes: `CS.viewer` says so, and a page that has not called it gets a warning from the viewer after 3 seconds. The viewer looks for a new version every 60 seconds and reloads the page. A new file must also be added to `publish.files`.

### Live answers

A `live` block lets the page ask your Mac read-only questions while it is connected:

- `queries: [sessions, runs]` names catalog queries. `queries: []` only shows whether your Mac is online. `queries: all` allows every catalog query, now and later.
- `fields` shares extra fields per query, for example `runs: [summary]` or the frontmatter keys for `queryContext`.

| Query | What it answers |
|---|---|
| `sessionUsage` | Token use and cost per agent session on your Mac, per day and in total. Not the story each session was spent on or the working folder unless shared: list `fields: { sessionUsage: [story] }` to get `story: {id, title, status, epic}` (or null) on each session and ask for one story with `args: { story: "LL-205" }`. |
| `sessions` | The agent sessions CodeSense is running: name, state, who launched them, story, role, progress. |
| `runs` | Recent agent runs and how each ended. Not the summary or workspace unless shared. |
| `queryContext` | Files in the dashboard's own team context by their metadata, with only the fields the dashboard lists. No file text. |

`get_live_data_contract` (section `hosted`) lists each query's arguments and fields. Secrets are never answered, answers are redacted and capped at 256 KB (see "Answer size" below), each answer has 10 seconds, and a viewer can ask 60 times a minute.

A person decides the `live` block, once, in the CodeSense app. When an agent publishes a new or wider `live` block, `publish_dashboard` publishes the page without live data and returns `live: pending_confirmation`; CodeSense asks the person in a dialog and in the Dashboards tab, and live data stays paused until they confirm. A narrower or removed block takes effect within seconds with no new publish (your Mac saves it and sends the new list to the server, retried once a minute while the server cannot be reached). A person can turn live data off for a dashboard at any time from the Dashboards tab. A live dashboard keeps a connection to share.codesense.ai open while CodeSense runs, and streams the terminals it lists with secrets masked; see [what leaves your Mac](../concepts/security-and-privacy.md#what-leaves-your-mac).

When your Mac is offline, the last answer to the same question comes back marked as cached (`source: "cache"`), with the time it was answered, and the viewer shows "Live data unavailable, showing data from N minutes ago" above the page until a live answer arrives. With no cached answer, the page gets `mac_offline`. Cached answers are deleted when you unpublish, and when a query leaves the list.

**The page API.** The page posts to its own window, not to `parent`:

```js
postMessage({ cs: "live", v: 1, op: "query", id: "q1", query: "runs", args: { limit: 20 } }, "*");
addEventListener("message", (e) => {
  if (e.source !== parent || e.data?.cs !== "live") return;
  // { id, ok: true, source: "live" | "cache", answered_at, server_time, answer }
  // or { id, ok: false, error }, or { op: "presence", mac, mac_seen_at }
});
```

`op: "subscribe"` with `every_s` repeats a query every `max(every_s, 15)` seconds, at most 20 subscriptions per page; `op: "unsubscribe"` with its `id` stops it. An `id` is a string of 1 to 100 characters or a number, and `args` is a plain object. At most 4 requests per page run at once, and up to 64 more wait. Presence arrives when the page has loaded and again at each status check, every 60 seconds. A new page ends the old page's subscriptions. In CodeSense on your Mac nothing answers these messages, so give the page a fallback; the `create_dashboard` starter asks `runs` (or another query that answers wherever the folder is), gives up after 15 seconds, and says so.

| Error | When |
|---|---|
| `query_not_declared` | The query is not in the dashboard's `live` list. |
| `mac_offline` | Your Mac is not connected, and there is no cached answer. |
| `mac_timeout` | Your Mac did not answer within 10 seconds, and there is no cached answer. |
| `query_invalid` | `args` is over 2 KB, or has an argument the query does not take or of the wrong type. |
| `stale_cursor` | A `queryContext` cursor whose index snapshot is gone: a walk's pages read one snapshot, kept 30 seconds after each page and 120 seconds at most, and a cursor from before your Mac's app restarted is stale too. Restart paging without a cursor; `CS.queryAll` and `CS.subscribeAll` do it silently. Never answered from the cache. |
| `answer_failed` | The answer failed, or the request did not get through, and there is no cached answer. |
| `answer_too_large` | One row or value is over 256 KB, and there is no cached answer. Show "That answer is too big for a shared dashboard; narrow the query (fewer rows or a shorter window)." |
| `rate_limited` | Over 60 asks a minute for this viewer, or 4 already waiting for them with no cached answer; or a 21st subscription on the page; or 64 requests already queued in the page. |
| `bad_request` | A malformed request: `v` is not 1, a bad `id`, `op`, `query` name, `args` or `every_s`, or a `subscribe` that reuses the `id` of an active subscription. |
| `access_ended` | The viewer's access ended (session expired, viewer removed, or dashboard unpublished). The page's subscriptions stop and the viewer shows the request form. |
| `post_to_own_window` | The page posted to `parent` instead of its own window. Nothing was asked. |

### Answer size

An answer is at most 256 KB as JSON. The list queries (`sessionUsage`, `sessions`, `runs`, `inbox`, `queryContext`) keep the first rows that fit, measured as sent (after redaction), and cut from the end of their own order: `sessionUsage` newest activity first, `runs` newest first, `inbox` newest first, `sessions` in handle order (not by recency), `queryContext` the sort you asked for or its default. Asking for `limit: 500` is safe. Every list answers as an object, and when CodeSense had to cut rows, by the limit or by size, the object carries one marker: `truncated: true`, `returned` (the rows it sent) and `total` (the rows before the cut). `runs` answers `{runs: [...]}`, `sessionUsage` `{sessions, days, totals}`, `sessions` `{sessions}`, `inbox` `{messages}` and `queryContext` `{rows}`, whose `total` and `truncated` are always present. The `totals` and `days` of `sessionUsage` still cover every session. Test `truncated` and say so on the page. Rows that were dropped cannot be fetched afterwards. Only `sessionUsage` takes a window (`sinceMs`, `untilMs`) to ask for older ones. `runs` takes only `limit`, `sessions` only `launchedBy`, and `inbox` has no time or cursor (narrow it with `scope`, `agentId`, `sessionId` or `role`). `queryContext` narrows with `match`, `filters` or `where`. Say on the page when it shows part of the list.

A `runs` answer, cut at its limit of 50 of 120 runs:

```js
// e.data.answer
{
  runs: [{ outcome: { story_id, run_id, status, ended_at /* … */ }, validated, has_structured_transcript } /* … 50 rows */],
  truncated: true, returned: 50, total: 120,
}
// a list that was not cut has only `runs`
```

Handle one row too big for the cap as the `answer_too_large` error above, not as a cut.

Rough bytes per row: `sessionUsage` 600 to 850 (more with several models), `sessions` 350, `runs` 450, `inbox` 400 plus each message's text when `fields.inbox` lists `text`, `queryContext` 150 plus about 30 per frontmatter field. The totals and days of `sessionUsage` and the rest of the answer are counted first. Only a single row or value over the cap fails, as `answer_too_large`.

### Team inbox

Mission control includes an Inbox view. Other dashboards can declare the query
and stream explicitly:

```yaml
live:
  queries: [inbox]
  streams: [inbox]
  fields:
    inbox: [text]
```

`CS.inbox((messages, state, detail) => { ... })` watches the team's messages and
returns a function that stops watching. The query `CS.query("inbox", args)` takes
`scope` (`all`, `to-human`, `to-agents`), `agentId`, `sessionId`, `role` and `limit`.
The dashboard fixes the team; arguments only narrow it. Up to 500 newest messages
are returned, with fewer complete rows when needed to fit the live answer's
256 KB limit, flagged with `truncated: true` and `returned`. One message larger than that limit returns `answer_too_large`; narrowing
the query or omitting bodies can still show its summary. Reading or watching
never marks messages read.

Rows contain `messageId`, `from`, `fromName`, `summary`, `kind`, `createdAt` (epoch
milliseconds), `unread`, `inReplyTo` and a role delivery's `delivery.state` when
present. Bodies appear only with `fields.inbox: [text]`. Answers are redacted.
Render all message content with `textContent`, including HTML-looking bodies.

The signed-in owner can use `CS.send(to, text, replyTo?)` to send plain text to
`human`, a running session handle in this team, or `role:triage` (substitute the
team's role id). Pass a displayed message's `messageId` as `replyTo` to reply;
the app follows its reply address within the same team. The limit is 2,000
characters and 60 sends per hour. There are no attachments or click actions.
Invited viewers cannot send. The result contains `status` and `messageId`.
Keep a draft on failure; check the inbox before retrying an unconfirmed send.
Audit records contain metadata, never message bodies.

Emoji reactions (tapbacks) need `fields.inbox: [reactions]`. Each row then carries
`reactions: [{emoji, count, mine}]`, where `mine` means you reacted. Who else
reacted is never shared. The signed-in owner can call
`CS.react(messageId, emoji, remove?)` for one of 😂 ❤️ 👍 😮 🎉 🔥 on a message in this
team's inbox. Without `remove` it toggles; `true` takes your reaction back and
`false` makes sure it is there. It resolves with `{on}`, whether your reaction
is on now, and rejects `not_allowed` for viewers and `invalid_args` for any other
emoji, `not_found` for a message outside this team and `rate_limited` after 120
reactions in an hour. A reaction is not unread mail and does not reply. The
inbox stream pushes the new counts to every watcher.

The signed-in owner can mark mail read with `CS.markRead(messageIds)`, as opening
those messages in the app does: 1 to 200 ids per call, each a message to the human
in this team's inbox. It resolves with `{marked}`, how many changed; mail that is
already read is left alone, so repeating a call resolves `{marked: 0}`. One id that is
not a message to the human in this team (another team's, an agent's, a missing
one) refuses the whole call with `not_found` and marks nothing. It rejects
`not_allowed` for viewers, `invalid_args` for an empty list, more than 200 ids or a
bad id, and `rate_limited` past 10 acts a minute (shared with sends and
reactions) or 600 calls an hour. The inbox stream pushes the changed unread flags
and `humanUnread` to every watcher. Viewers cannot mark anything read.

To restrict an owner action to a recipient set, declare `sendMessage` and call
`CS.act("askTeam", {to: "role:triage", text: "Please check the queue."})`:

```yaml
actions:
  askTeam:
    verb: sendMessage
    who: owner
    to: [role:triage, human]
    max_per_hour: 5
```

Its recipient set also applies to the effective reply address. Message text is
the bounded plain-text exception to action arguments; other verbs still accept
only their typed arguments. Declaring `who: viewers` does not enable viewer sends.

### Sharing

`share_dashboard` with `add: [emails]` gives people access; `remove` takes it away and ends their sessions at once; with neither, it changes nothing and only lists the viewers. Every call returns the current viewers. Invite emails are reviewed by the CodeSense operator before they are sent, so one can take a while to arrive. To see the dashboard yourself, add your own address.

### What viewers see

A viewer opens the link in the invite email (valid for 15 minutes, single use) and gets a session for that dashboard only. It ends after `viewers.session_days` (90 by default) without a visit, and 90 days after sign-in at the latest. A viewer who is already signed in to their viewer account on share.codesense.ai (this is separate from the app, which has no sign-in) can choose **Open with your CodeSense account** on the dashboard sign-in page and skip the second email code. The account checks the active share, then hands off through a 60-second, single-use code; the account cookie never reaches the dashboard host. Owners are sent through the existing **Take control** flow, which asks them to confirm it's them. They see the title, "Page updated N minutes ago" (when the files were last published), a warning when your Mac stopped checking in, and the pages. For a dashboard with a `live` block they also see "Owner's Mac: online", "offline since" a time, or "status unknown". Anyone else gets the same not-found page, with a form to ask for a link. Viewers cannot change anything.

### You can

- Build a small multi-page web app: several pages, shared scripts and styles, images, and JSON, CSV or text data.
- Use any JavaScript library you put in the folder and list.
- Keep the data fresh with an agent or routine that rewrites the data files while CodeSense is open.
- Link to any web page, or to another dashboard.
- Show live, read-only answers from your Mac for declared queries, with the last answer when your Mac is offline.
- Take it down with `unpublish_dashboard` and publish it again later. Unpublish deletes every hosted file version and cached live answer, while keeping the slug and share list. The reply includes `hostedFilesDeleted: {versions, files}` (null when unknown); retry a failed takedown to finish deletion.

### You cannot

- Use the in-app page SDK (`window.codesense`, actions) in a published page.
- Reach the network from a hosted page: no requests to web addresses, no CDN scripts or web fonts.
- Use files that are not listed or that live outside the folder.
- Use SVG or font files, more than 50 files, a file over 5 MB, or more than 25 MB in all.
- Write anything as an invited viewer. An owner can run declared actions, including starting the declared agent. Terminal input requires the dashboard to enable control and the owner to take it in the viewer.
- Send invite emails yourself: they are reviewed before they are sent.
- Choose a different first page yet (`publish.entry` is decided, not built).
The full guide ships in the app's docs: **Dashboard primitives → Your first dashboard**.

### Hosted file retention

In `dashboard.yml`, set `publish.retain: current` to keep only the current hosted version, or use an integer from `1` through `5`. The default is `5`. The setting takes effect at the next new version commit, which prunes older committed versions; the current version is always kept. Unpublish deletes all hosted versions regardless of retention. Your local dashboard files stay on your Mac.

### Pin the scope of hosted live queries

A published dashboard can narrow a query to exact argument values in `dashboard.yml`:

```yaml
live:
  queries: [storyChanges, whyLine]
  args:
    storyChanges: {storyId: [DEMO-46, {epic: DEMO}]}
    whyLine: {repo: [acme/app]}
```

Before adding viewers, pin `storyChanges.storyId` and `whyLine.repo`, including when
`queries: all` is declared. `share_dashboard add` refuses a missing pin, and the Mac
refuses viewer asks without these pins even for existing viewers. Private publishing
and owner asks remain allowed without pins. Once set, pins apply to owners too.

Each pinned arg accepts 1–64 literal strings or integers. Values are sorted and
deduped; matching uses the exact JSON type and value (`5` differs from `"5"`). Every
pinned arg must be sent, including a defaulted arg such as `whyLine.rev`. Other args
stay open: pin `path` too if the page should read only one path within a pinned repo.
Unknown queries, undeclared queries, streams, unknown arg names, empty or non-list
pins, nulls and unsupported values are refused. Unknown keys under `live` are refused,
so spell `args` correctly.

The generated starter sends the first literal for each pinned arg of its selected
query, preserving JSON types. A selector-only arg gets a named placeholder such as
`SET_storyId` in `app.js`; the rendered error, creation warnings and `nextSteps` name
the arg to configure and the epic or team it must belong to.

The Mac checks before running a query and returns `arg_not_allowed`. The share server also
checks before relay or cache fallback; a refusal never serves cached data. Edited
pins take effect when the watcher saves the block, without republishing. Until then
asks fail closed. Changed pins delete that query's cached answers. Streams are not
pinned by this block.

Only `storyChanges.storyId` also accepts `{epic: DEMO}` or `{team: platform}`
selectors, alone or mixed with literal ids. A team selector must equal the dashboard's
saved team; selectors cannot be published without a known team scope. The Mac resolves
stories through the context-query engine in that saved scope, using `parent_epic`/`epic`
frontmatter or the epic folder. Another team's stories stay unreachable. Unknown ids,
a folder that now belongs to another team, and an index that is not ready are refused.
Editing a story's epic changes its eligibility without republishing the dashboard.
Requests carry `live_scope`, an opaque SHA-256 fingerprint of the saved org and team.
Republishing under a different scope (or losing a known scope) retires all cached
and in-flight query answers, even when selector text is unchanged; an identical
scope preserves them.

A selector satisfies the viewer pin requirement. The share server checks literal args
and leaves selector args to the Mac. While the Mac is offline, viewers see the last
answer it approved under the current pins, which can still include a story that has
since left the epic; the Mac's next answer replaces it. Changing the pins
still deletes that query's cached answers. Cached and in-flight answers must match
the current pins and the viewer permission computed on the Mac. Online rate limits,
timeouts, query failures and oversized answers never use a selector cache fallback.
If the Mac returns `arg_not_allowed` or `not_approved`, that exact cached answer is
deleted and pending older answers cannot restore it. A later disconnect does not
make the refused answer available again.
