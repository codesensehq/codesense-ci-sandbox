# Attribution and cost

_Which session wrote a line, the turn behind it, and what each story cost, per agent and per model, readable through MCP._

## Every line, traced

CodeSense installs and pins git-ai, which records the session, model and tools behind each line. `why_line` adds the cost and the conversation turn that wrote it, offline.

```text title="why_line"
{
  "repo": "acme/checkout",
  "path": "src/retry.ts",
  "start": 40,
  "end": 52,
  "story": "WEB-42"
}
```

The answer names each range’s basis: **exact** from git-ai’s notes, **associated** through the story or PR, or **unknown**, with the reason. An exact range points to the session, the model, the tools and the turn of the conversation that made the edit. It runs offline, on your copy of the repo, and never fetches.

Per story, you also see how much of the change agents wrote and how much people did.

> **Note**
>
> Attribution notes are local. They never leave your machine unless you choose to share them. Lines are traced for Claude Code and Codex agents that run on your Mac.

## The session record

Every session records who ran it, which agent, model, tools and account, the story it served, and its active and idle time. Attribution notes stay on your machine unless you choose to share them.

Each managed session also keeps its whole conversation, with secrets redacted and tool calls indexed. `session_read` pages through it, so you, or an agent, can read exactly what was asked and what was done.

The Story trail puts it together for one story: its agents, runs, conversations, commits and diffs, in one view.

> **Caution**
>
> A guard keeps credential files out of managed Claude sessions’ ordinary reads. It is not a sandbox: give an agent you don’t fully trust sandboxed permissions.

## Cost per story

`story_totals` returns a story’s sessions, models, time and cost in one read. Cost is worked out from your own rates per model, with cache and long-prompt tiers.

```text title="story_totals"
{ "storyIds": ["WEB-42", "WEB-43"] }
```

Each story comes back with its `sessions`, `commits`, `files`, `lines`, `costUsd`, and its wall, active and idle time, plus the CLIs that reported usage and its merged PRs. A story still being indexed says `pending`; ask again rather than reading it as zero. `story_changes` breaks one story down by session, task and commit.

Set your own price per model with `set_model_rate`, including cache and long-prompt tiers; `model_rates` lists them. Cost rolls up by session, story, epic, agent and model, and sessions on remote machines count the same as local ones.

## Your own cost dashboard

The usage dashboard is built in, and the same data is readable through MCP, so you can show it however you want in your own dashboard.

A page gets the same numbers with one call, for any set of stories.

```html title="cost.html"
<script>
codesense.storyTotals({ storyIds: ["WEB-42", "WEB-43"] }).then(function (r) {
  // r.stories["WEB-42"].totals.costUsd
});
</script>
```

The [Dashboards](dashboards.md) guide covers the rest of the API, and [Dashboard uses](../guides/dashboard-uses.md#cost-per-story) has a cost-per-story page to start from.
