# Track usage and ROI

_Read what each agent session cost by story, epic, agent and model, and build a dashboard that sets it against what shipped._

CodeSense records the tokens and spend of every Claude Code, Codex and OpenCode session it launches, from each CLI's own reports. Each session also knows the story it was spent on, the agent that ran it and the models it used. That's enough to answer what a feature cost, which agent or model earns its price, and what you got for it.

The usage dashboard is built in. You can also read the numbers from an agent or your own dashboard page.

## 1. Read it

An agent calls the `session_usage` MCP tool. A dashboard page calls `codesense.sessionUsage()`, which returns the same thing. Both are read-only.

```js
const { sessions, days, totals } = await codesense.sessionUsage({
  sinceMs: Date.parse("2026-10-01"),
});
```

- **`sessions`**, newest first. Each has `cli` (`claude`, `codex` or `opencode`), `agent`, `org`, `team`, `models`, `tokens` (`input`, `output`, `cacheRead`, `cacheWrite`), `messages`, `costUsd`, `story` as `{id, title, status, epic}`, and `machineId`: the machine it ran on, as `codesense.machines()` names it (`local` for this Mac).
- **`days`**: tokens and cost per local day.
- **`totals`**: the whole range, with counts of priced and unpriced sessions.

To narrow it, pass `story` (`"LL-233"` keeps only that story's sessions, and the days and totals cover only them), `sinceMs` and `untilMs`, `sessionId`, `cwd` or `limit`.

A session belongs to the story of the worktree it ran in, or else the story its launch was opened for. `story` is `null` for a session that wasn't tied to one, and `agent` is `null` for one launched before CodeSense recorded who ran it.

Usage from sessions on your [remote machines](../concepts/remote-machines.md) is pulled back to your Mac every minute while the machine is connected, and counted with the rest.

> **What isn't counted**
>
> Only sessions CodeSense launched are counted: it switches each CLI's telemetry on at launch. A `claude` you start in a terminal outside CodeSense isn't. `costUsd` is `null` for a session that used a model with no price on file; `unpricedModels` names it, and its tokens still count. An agent can list the prices on file with the `model_rates` MCP tool and set one with `set_model_rate`, using a price you give it; a page does the same with `codesense.modelRates()` and `codesense.setModelRate()`. A new price prices the usage it covers from its day on: costs that were missing are filled in, and costs CodeSense worked out from an earlier price are recalculated. A cost the CLI reported itself is never changed. OpenCode reports its own price for each model, so an OpenCode session on a Go subscription shows that notional price, not what your subscription charged. A price you don't give (cache reads, long prompts) goes back to its default, so to change one price, pass all of them again.

### When your organization manages Claude settings

Some companies install a managed Claude settings file on every Mac (`/Library/Application Support/ClaudeCode/managed-settings.json`) that sends Claude's usage telemetry to the company's own collector. That file wins over what CodeSense sets at launch, so CodeSense doesn't see the tokens or cost of the Claude sessions it starts. Codex and OpenCode sessions aren't affected.

When CodeSense finds that file, it reads the usage of the Claude sessions it launched from Claude's own session log on this Mac instead (under `~/.claude/projects/`). It takes only the token counts, the model, the request and session ids, the time and the working folder from the log. Usage, story cost and spend caps then work as usual, with one difference: a session's usage appears when each turn ends, up to a minute later, not as each call finishes. A spend cap counts that usage once it appears, so the turn in progress can take an agent past its cap before the cap stops it.

Your IT team doesn't need to change anything. If they add CodeSense's local endpoint as a second log destination, usage arrives as each call finishes again. CodeSense picks up a change to the file on the next read, with no restart.

## 2. Group it

The rows are plain data, so group them the way you want to read them:

```js
const byEpic = {}, byAgent = {}, byModel = {};
for (const s of sessions) {
  const cost = s.costUsd ?? 0;
  const epic = s.story?.epic ?? "no epic";
  byEpic[epic] = (byEpic[epic] ?? 0) + cost;
  byAgent[s.agent ?? "unknown"] = (byAgent[s.agent ?? "unknown"] ?? 0) + cost;
  for (const m of s.models) byModel[m] = (byModel[m] ?? 0) + cost / s.models.length;
}
```

Cost is per session, not per model. A session that used two models has its cost split evenly here, so treat the per-model figure as approximate.

## 3. Set cost against what shipped

Cost alone says what you spent. Set it against your context, which already says what got done: `codesense.queryContext()` reads each story's status and kind (see [Query your context](query-your-context.md)).

```js
const done = await codesense.queryContext({
  where: { kinds: ["story", "bug"] },
  match: { status: "done" },
});
const doneIds = new Set(done.rows.map((r) => r.id));
const fixes = new Set(done.rows.filter((r) => r.kind === "bug").map((r) => r.id));

let shipped = 0, fixCost = 0;
for (const s of sessions) {
  if (s.story && doneIds.has(s.story.id)) shipped += s.costUsd ?? 0;
  if (s.story && fixes.has(s.story.id)) fixCost += s.costUsd ?? 0;
}
const costPerStory = shipped / doneIds.size;
const costPerFix = fixCost / fixes.size;
```

Useful figures for the page:

- **Cost per finished story**, overall and per epic.
- **Cost per fix**: what each bug that reached `done` cost.
- **Which model earns its price**: cost per finished story, by model.
- **Spend that didn't ship**: sessions on stories that aren't done, or on no story.

## 4. Add your team's estimate

CodeSense doesn't measure time saved. If you want a return figure, put an input on the page for the hours your team estimates a story would have taken, and an hourly rate, and label the result as your estimate. A page that shows "hours saved" without saying whose estimate it is overstates what the data knows.

## 5. Build the page

Ask an agent for the page, and have it read the contract first:

```text
Build a usage and ROI dashboard for this month.
Read get_live_data_contract first. Use codesense.sessionUsage for cost
and codesense.queryContext for what's done. Show cost per finished story,
cost per fix and cost by epic, agent and model. Add inputs for the hours
we estimate each story saved and our hourly rate, and label that figure
as our estimate. Validate it with validate_live_data_doc.
```

It saves the page as an HTML file in your team's context, for example `dashboards/usage.html`, and CodeSense opens it as a live page. See [Have an agent build a dashboard](agent-builds-dashboard.md) for the rest of the workflow.

## 6. Share it

The page is a file in your team's context, so teammates who open it in CodeSense see their own Mac's usage on it, not yours.

Publish the dashboard to share.codesense.ai and share it with exactly the people you choose. They open the invite link after they sign in, and it stays live while your Mac is connected. Nothing leaves your Mac until you publish, and you can unpublish it any time.
