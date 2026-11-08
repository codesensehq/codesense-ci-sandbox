# The planning model

_Epics are folders and stories are files, in markdown or HTML, with frontmatter or cs meta tags._

CodeSense keeps your plan in plain files, not in a database. Each epic is a folder and each story is a file. The left nav, dependencies, sprints and your queries all read those files, and when you change a status in the app, CodeSense writes it back into the file.

The files live in your team's context repo, under the team's `content/` folder. They travel through git like everything else there, and agents can read and write them directly.

## Epics are folders

Every folder under `content/` shows up as a group in the tree. A folder becomes an **epic** when it has a main doc. CodeSense looks for these names, in this order and ignoring case: `README.md`, `index.md`, `README.html`, `index.html`, `README.htm`, `index.htm`.

Epics nest to any depth, so a folder inside an epic is a sub-epic. For example, a `billing` team:

The `planning/` folder is just how this example is laid out. CodeSense doesn't require it. The left nav skips folders whose names start with `.`.

An epic's main doc can carry its own frontmatter. The field CodeSense acts on is `repos:`, the list of source repos the epic spans (see [Multi-repo](multi-repo.md)):

```yaml title="checkout-v2/README.md"
---
type: epic
title: Checkout v2
status: in-progress
repos: [ledgerline/web, ledgerline/api]
---
```

## Stories are files with a type

A file is a **story** when its metadata says `type: story`, `type: bug` or `type: spike`. The match ignores case. Anything else is treated as a plain document: no `type:`, or any other value such as `epic` or `retro`. A document still shows in the file tree, but it never gets a status, readiness or a place in a sprint.

```yaml title="ll-231-cart-summary-tax-breakdown.md"
---
type: story
story_id: LL-231
title: Cart summary tax breakdown
status: review
source_repo: ledgerline/web
depends_on: [LL-204]
sprint: sprint-14
points: 3
assigned_to: sam
---

# LL-231 — Cart summary tax breakdown

## Story
As a buyer I want to see subtotal, tax per jurisdiction, and total before I pay.
```

The fields that drive the app:

| Field | What it does |
|---|---|
| `type` | `story`, `bug` or `spike`. Required, or the file is a doc. |
| `story_id` | The id other stories use in `depends_on`. It must be unique across your workspace. |
| `status` | Where the story stands. Values come from your team's statuses (see [Statuses and phases](statuses-and-phases.md)). |
| `source_repo` | The one repo this story's agent works in, as `owner/repo`. It must be one of your team's configured repos. |
| `depends_on` | A YAML list of `story_id`s this story waits on. |
| `supporting_docs` | Relative paths to documents that go with the story, such as a design or a spec. |
| `sprint`, `points`, `assigned_to` | Planning metadata (see [Stories and sprints](board-and-sprints.md)). |

The full list of registered fields, such as `tags`, `priority` and `parent_epic`, is in [Story and epic fields](../../generated/story-fields.md). Any other key, such as `area` or `risk`, is kept as metadata: you can filter and group on it in queries and dashboards, and an agent can filter on it with `query_context`. `validate_story` flags such a key with the advisory `UNREGISTERED_FIELD` until your team declares it in `content/field-schema.yml`.

CodeSense reads the first `## Purpose` or `## Story` section as the story's summary (in HTML, `<section data-cs="purpose">` or `<section data-cs="story">`). Without one, the summary is blank.

> **Two mistakes the left nav doesn't explain**
>
> - **A missing `type:`** turns the story into a plain doc. It loses its status and readiness in the left nav, and the nav doesn't say why. Open the file: if it still has a `story_id`, the metadata band above it lists the problem.
> - **Frontmatter that doesn't parse as YAML** does the same. For example, `depends_on: LL-204, LL-201` must be written as a list, `depends_on: [LL-204, LL-201]`. The same goes for an unquoted value that contains `: ` (colon, space).

> **Every story needs a source_repo**
>
> A story without `source_repo` is left out of the workspace's story index. It can still appear in the tree, but it can't be launched, and other stories can't find it by id. Give every story exactly one `owner/repo` that is in your team's repo list.

## Markdown or HTML, per file

A story or an epic main doc can also be an HTML page. In HTML, the same fields are `<meta name="cs:…">` tags in the page head, with dashes in place of underscores: `cs:story-id`, `cs:source-repo`, `cs:depends-on`, `cs:assigned-to`. List values are comma-separated. The title comes from `<title>`, or from the first `<h1>` if there's no title.

When you change the story's status in the app, CodeSense rewrites the matching meta tag in place. [Plan in HTML](../guides/plan-in-html.md) walks through one.

## One index across the workspace

CodeSense keeps an index of every story's metadata across all the teams in an org. That is what lets `depends_on` use bare ids: a story can move to another folder and its links still resolve. If two stories share a `story_id`, neither one resolves until you fix the duplicate.

## Checking a story

Agents can check their own work. The CodeSense MCP tools `get_story_format` and `validate_story` return the story contract and a list of findings, such as `MISSING_SOURCE_REPO`, `UNPARSEABLE_FRONTMATTER`, `DUPLICATE_STORY_ID` or `MISSING_SUMMARY_HEADING`. When you open a story, the metadata band above it shows the same findings, each marked **Blocking** or **Advisory**. See [Write your first story](../guides/first-story.md).

You can also create a story from **New story…** on an epic's right-click menu in the left nav. The form asks for a title, epic, story id, repo, status and dependencies, checks for problems as you type, and writes the file into the epic.

The work-item types are story, bug and spike. Your folders, statuses, phases and metadata are yours to shape.

The full guide ships in the app's docs: **Pipelines → Phase 5 · Story graph**.

## Frontmatter that matters

A story needs `type` (story, bug or spike), `story_id`, `status` and a configured `source_repo`; `branch` and `depends_on` are optional. Agents check one with `validate_story` before saving it.

| Field | Needed | What reads it |
|---|---|---|
| `type` | Yes | The pipeline: `story`, `bug` or `spike` |
| `story_id` | Yes | Links, dependencies, worktrees and cost |
| `status` | Yes | The pipeline, boards and routines that wait for a value |
| `source_repo` | Yes | Which code repo the worktree opens in. It must be a configured repo, or the story is left out of the pipeline |
| `depends_on` | No | Readiness: a story is ready when what it depends on is done |
| `branch` | No | The worktree’s branch |
| Anything else | No | Your own queries, boards and routines |

## The pipeline view

The pipeline view draws every story as a graph of its dependencies, so you can see what’s ready, what’s blocked and what unblocks the most. It updates the moment a field changes, whether you changed it or an agent did. Dependencies can cross repos: an API story can block a web story, and the map draws that link like any other.

Two dashboards ship in the app: **Stories** shows every story at a glance, and **Journey** follows one story end to end. Build your own on the same queries when you want a different cut.

## From a note to a PR

Notes are your private Markdown notes for one org, and they open in a full-screen HTML Notes pane by default. Send a note to an agent and it turns it into a story, opens a worktree, does the work and comes back with a PR.

Agents can read and write notes, too, with the `notes_*` tools, so a quick thought becomes a story without leaving the app.
