# Write your first story

_Write a story as a markdown file with frontmatter, whether you or your agent authors it._

A story is a markdown file with a few lines of frontmatter. You can write it yourself or ask your agent to. Either way, it's a file in your team's context, and it shows up in the left nav once it's saved.

This guide adds a story to Ledgerline's `checkout-v2` epic: a refund button in order history, in the `web` repo.

## Before you start

- Your team has at least one source repo on its list (see [Attach your repos](../getting-started/attach-repos.md)).
- You know which epic folder the story belongs in. Any folder under the team's `content/` with a `README.md` is an epic. See [The planning model](../concepts/planning-model.md).

## 1. Create the file

In the epic folder, create a new file. In the app, right-click the folder and choose **New file…**, or use your editor. Name it after the story, so the file name is readable:

```
billing/content/planning/checkout-v2/ll-233-refund-button-in-order-history.md
```

## 2. Add the frontmatter

```yaml title="ll-233-refund-button-in-order-history.md"
---
type: story
story_id: LL-233
title: Refund button in order history
status: todo
source_repo: ledgerline/web
depends_on: [LL-214, LL-231]
sprint: sprint-14
points: 3
assigned_to: sam
---
```

What each line does:

- `type: story` makes this a story. Without it, the file is a plain doc, not a story. `bug` and `spike` work too.
- `story_id` is the id other stories use to depend on this one. It must be unique in the org.
- `status` is one of your team's statuses (by default planning, todo, in-progress, blocked, review, qa or done). It sets the status icon in the left nav.
- `source_repo` is the one repo the story's worktree opens in, as `owner/repo`. It must be on your team's list.
- `depends_on` is a list of story ids that must be done first. Always write it as a list, even for one id: `[LL-214]`.
- `sprint`, `points` and `assigned_to` are optional. You can also set them later, or add the story to a sprint from the left nav.

Any other key you add, such as `area: refunds`, is kept as metadata you can query (`query_context` can filter on it).

## 3. Write the body

CodeSense reads the first `## Purpose` or `## Story` section as the story's summary (you can change the headings in Settings, under **Summary section headings**), so start with one:

```md
# LL-233 — Refund button in order history

## Story
As a customer I want a Refund button on a paid order so I don't have to email support.

## Acceptance criteria
- [ ] Button shows only for captured payments
- [ ] Retrying a refund reuses the same idempotency key
```

Everything after that is up to you. The agent that picks up the story reads the whole file.

## 4. Check it

Save the file. LL-233 appears under `checkout-v2` in the left nav with a Todo status icon. Hover it to see that it's waiting on LL-214 and LL-231 until they're done.

If it doesn't appear, check the two usual causes: a missing `type:` line, or frontmatter that isn't valid YAML (for example `depends_on: LL-214, LL-231` without brackets).

## Let your agent write it

Agents with the CodeSense MCP tools can write and check stories themselves. A prompt like this works:

> Add a story to the checkout-v2 epic for a Refund button in order history, in ledgerline/web, depending on LL-214 and LL-231. Call `get_story_format` first, then `validate_story` on the file before you finish.

`get_story_format` gives the agent the story contract and an example. `validate_story` returns findings with stable codes, including:

| Code | Meaning |
|---|---|
| `MISSING_SOURCE_REPO` | No `source_repo`, so the story can't be opened as a workspace |
| `UNKNOWN_SOURCE_REPO` | The repo isn't on the team's list |
| `UNPARSEABLE_FRONTMATTER` | The YAML doesn't parse, so the file reads as a plain doc |
| `DUPLICATE_STORY_ID` | Another story in the org already uses this id |
| `UNRESOLVABLE_DEPENDS_ON` | A `depends_on` id doesn't match any story |
| `MISSING_SUMMARY_HEADING` | No `## Purpose` or `## Story`, so the summary is blank |
| `OFF_FIBONACCI_POINTS` | `points` isn't 1, 2, 3, 5, 8 or 13 |

> **Note**
>
> Prefer HTML? The same story can be an HTML page with `cs:` meta tags. See [Plan in HTML](plan-in-html.md).

You can also right-click the epic folder and choose **New story…**: a form (title, epic, story id, repo, status, dependencies) that writes the same file.

The full guide ships in the app's docs: **Pipelines → Phase 5 · Story graph**.
