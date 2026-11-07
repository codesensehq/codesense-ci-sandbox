# Put a csquery table in a README

_Add a live table of stories to any markdown doc with a csquery block._

A `csquery` block turns a fenced code block in any markdown doc into a live table of your stories. There's no HTML and no script. When a story changes on disk, the table updates.

This guide adds an "in flight" table to the Ledgerline `checkout-v2` epic's `README.md`.

## 1. Add the block

Open `planning/checkout-v2/README.md` and add a fenced block with the language `csquery`:

````md
## In flight

```csquery
status: in-progress, review
tag: source_repo=ledgerline/web
columns: id, name, status
sort: id asc
```
````

Each line is one `key: value` pair. Keys aren't case-sensitive, and blank lines and `#` comments are ignored.

## 2. View it

Open the README in CodeSense's markdown viewer. The block renders as a table of every `web` story that's in progress or in review, such as LL-231 "Cart summary tax breakdown". Click a row to open that story.

> **It renders in CodeSense**
>
> A csquery block is live in CodeSense's viewer. On GitHub or in another editor it shows as a plain code block.

## The keys

| Key | Example | Meaning |
|---|---|---|
| `status` | `status: todo, in-progress` | Any of these statuses. Repeated `status` lines add up. |
| `tag` | `tag: ui` or `tag: source_repo=ledgerline/api` | A bare value matches the doc's `tags:` list. `key=value` matches any frontmatter field. Lines on different keys must all match; lines on the same key match either. |
| `epic` | `epic: LL` | Docs in this epic. |
| `depends` | `depends: LL-225` | Docs that depend on this one. |
| `kind` | `kind: story, epic` | Kinds to list. Default `story, bug, spike`. |
| `org` | `org: all` | Orgs to search. Default: the org this doc is in. |
| `team` | `team: billing, web` | Only these teams' context. |
| `layer` | `layer: private` | `team`, `private` or `all-orgs`. |
| `text` | `text: idempotency` | Words that must appear in the doc's body. |
| `related` | `related: <doc>` | Docs that have this doc as a supporting doc. |
| `backlink` | `backlink: <doc>` | Docs backlinked by this doc. |
| `scope` | `scope: <path>` | Only docs under this path. |
| `view` | `view: board` | `table` (default), `list`, `board` or `connectivity`. |
| `group` | `group: status` | Group rows or board columns by a field. A board groups by `status` by default. |
| `columns` | `columns: id, name, status, type` | Table columns. Default `name, status`. `id`, `name`, `status`, `type` and `path` are built in; any other name reads that tag. |
| `limit` | `limit: 20` | At most this many rows. Default 200. |
| `sort` | `sort: priority` | Sort by any field, ascending by default. `name` is the file name and `id` the story id. |

For all keys other than `tag` and `status`, the last line wins, and CodeSense shows a warning.

## Variations

A board of every doc tagged `ui`, one column per status:

```csquery
tag: ui
view: board
```

A short list of what's waiting on review:

```csquery
status: review
view: list
limit: 5
```

## Blocks are read-only

A csquery block only shows data. Action keys such as `setstatus`, `open` or `run` are dropped with a warning. To act on stories from a page, use an [HTML page](../concepts/dashboards.md). The same query engine powers `codesense.queryContext()` in HTML pages and the `query_context` MCP tool: see [Query your context](query-your-context.md).

The full syntax is in the csquery reference.
