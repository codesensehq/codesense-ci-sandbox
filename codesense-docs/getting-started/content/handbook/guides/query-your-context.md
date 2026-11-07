# Query your context

_Find context files by their metadata from an agent, a page or a csquery block, with one query model._

Every file in your context has metadata: a Markdown file's frontmatter, or an HTML page's `cs:*` meta tags. CodeSense keeps an index of all of it, across every org you've synced: each team's context repo, your private space in each org, and your all-orgs files. You can ask that index questions from three places, and they all use the same query:

- An agent calls the `query_context` MCP tool.
- A page calls `codesense.queryContext(query)`, or `codesense.subscribeContext(query, callback)` to stay live.
- A `csquery` block in any Markdown file.

## What a query looks like

```json
{
  "where": { "kinds": ["story"] },
  "relationships": { "parentEpic": "LL" },
  "filters": [
    { "key": "status", "op": "in", "value": ["todo", "in-progress"] },
    { "key": "assigned_to", "op": "eq", "value": "@me" }
  ],
  "sort": ["priority"],
  "groupBy": "status"
}
```

That's "my open stories in epic LL, highest priority first, counted by status". Every part is optional. An empty query `{}` lists the first page of every file, sorted by path.

### Where to look

`where` narrows the search:

| Key | Example | Meaning |
|---|---|---|
| `orgs` | `"all"` or `["acme"]` | Which orgs. The default is every org you've synced. |
| `sameOrgAs` | `"/path/to/README.md"` | Only the org that file is in. |
| `sameRootAs` | `"/path/to/board.html"` | Only the context repo (or private space) that file is in. |
| `teams` | `["default"]` | Team context repos by name. Private and all-orgs files have no team. |
| `layers` | `["private"]` | `team`, `private` or `all-orgs`. |
| `kinds` | `["story", "bug"]` | `story`, `bug`, `spike`, `epic`, `doc`, `dashboard`, `skill` or `other`. |
| `pathPrefix` | `"planning/onboarding"` | A folder, relative to the context repo, or an absolute path. |
| `pathGlob` | `"**/LL-*.md"` | A glob over the path relative to the context repo. |

> **Private stays private**
>
> Your private space is only ever searched for one org at a time. A page or a csquery block sees the private files of the org it lives in, whatever its query says. An agent sees private files only when it names exactly one org (`"orgs": ["acme"]` or `sameOrgAs`); with every org, or several, it gets team and all-orgs files only. All-orgs files, such as your shared skills, match every org.

### Filters

`filters` is a list of `{ key, op, value }`. The key is any frontmatter key, or a built-in: `path`, `relPath`, `fileName`, `id`, `org`, `team`, `layer`, `kind`, `mtime`, `title` and `dir`. Keys ignore case, so `assigned-to`, `assigned_to` and `assignedTo` are the same key.

| `op` | Example | Matches |
|---|---|---|
| `eq` | `{ "key": "team", "op": "eq", "value": "platform" }` | The value, or any item of a list. |
| `in` | `{ "key": "status", "op": "in", "value": ["todo", "review"] }` | Any of the values. |
| `not` | `{ "key": "status", "op": "not", "value": ["done"] }` | None of the values. Files without the key match too. |
| `exists` / `missing` | `{ "key": "branch", "op": "exists" }` | The key is set, or not. |
| `contains` | `{ "key": "tags", "op": "contains", "value": "ux" }` | A list item, or text anywhere in a text value. |
| `range` | `{ "key": "points", "op": "range", "min": 3, "max": 8 }` | Between `min` and `max`. Numbers compare as numbers; dates as text. |
| `prefix` | `{ "key": "story_id", "op": "prefix", "value": "LL-" }` | Text that starts with the value. |

Filters on different keys must all match. Two filters on the same key match if either does. The value `"@me"` stands for you: your profile name, and your entry in the org's people list (`person:<id>`).

`eq`, `in` and `not` compare text exactly, so `"01"` doesn't match `"1"`. Give a JSON number (`"value": 1`) to compare numerically.

`match` is a shorthand: `{ "match": { "status": ["todo", "review"], "team": "default" } }`.

### Relationships

`relationships` follows the links between files. Each value is a path, a story id, a file name or a file name without its extension.

| Key | Matches files that… |
|---|---|
| `dependsOn` | depend on it (`depends_on`). |
| `dependedOnBy` | it depends on. |
| `hasSupportingDoc` | list it in `supporting_docs`. |
| `backlinkedBy` | it lists in its `supporting_docs`. |
| `parentEpic` | belong to that epic: `parent_epic` or `epic` frontmatter, or a folder with that name. |
| `hasDeps`, `isBacklinked` | have any dependencies, or any backlinks (`true` or `false`). |

### Text, fields, order and pages

- `text`: words that must appear in the file. CodeSense checks it after the other filters and reads at most 5,000 files or 64 MB for one query. When it stops early the result says so (`textScanTruncated`, and `totalApproximate` because `total` may be short).
- `select`: the frontmatter keys to return, such as `["status", "priority"]`. Add `"relationships"` to get links too, or `"summary"` to get a story's overview as plain text. Leave it out to get everything.
- `sort`: `["priority", "-mtime"]`. A leading `-` sorts descending. `status` sorts in the built-in workflow order (planning, todo, in-progress, blocked, review, qa, done) unless you pass your team's own order: `{ "key": "status", "order": ["triage", "doing", "shipped"] }`. csquery blocks pass the team's order for you. `priority` sorts p0 first. Otherwise numbers come before dates, dates before text, and text sorts naturally (LL-9 before LL-10).
- `limit` and `cursor`: 200 rows by default, at most 10,000. Pass a result's `nextCursor`, with the same query, to get the next page. If your context changed in between, you get an error asking you to start again from the first page, never skipped or repeated rows.
- `groupBy`: counts every match by the value of one key, for board columns.

### What comes back

```json
{
  "total": 212,
  "rows": [
    {
      "id": "LL-226",
      "path": "/Users/you/…/LL-226-context-query-tool.md",
      "relPath": "stories/LL-226-context-query-tool.md",
      "org": "acme",
      "team": "default",
      "layer": "team",
      "kind": "story",
      "title": "One great query for context repo files by metadata",
      "fields": { "status": "review", "priority": "p1" }
    }
  ],
  "groups": [{ "value": "todo", "count": 140 }, { "value": "review", "count": 72 }],
  "nextCursor": "o:200:17:9f2c41d0a8b3e6f5"
}
```

## From an agent

Ask your agent, for example: "Which stories in LL are still open and assigned to me?" It calls `query_context` with a query like the one above. The tool's description lists every key and operator, so agents can write their own queries.

## From a page

A page can run the same query and keep it live. This board shows the open stories in an epic, one column per status:

```html
<script>
const off = codesense.subscribeContext(
  {
    where: { kinds: ["story"] },
    relationships: { parentEpic: "LL" },
    filters: [{ key: "status", op: "not", value: ["done"] }],
    sort: ["priority"],
    groupBy: "status",
  },
  (result, error) => {
    if (error) return showError(error);
    drawColumns(result.groups, result.rows);
  },
);
window.addEventListener("pagehide", off, { once: true });
</script>
```

To keep a page to its own team, read `const { org, team } = await codesense.context()` and add `where: { orgs: [org], teams: [team] }`. `context()` resolves `null` for a page outside your content.

The callback runs right away and again each time a file changes in the orgs the query covers, including a page's own writes through `codesense.setField` or `setStatus`. The CodeSense docs include a complete version: open `examples/dashboards/story-board.html`.

## From a csquery block

A `csquery` block runs the same query. It searches the org the document is in, unless you write `org:`. Private files are always the document's own org's:

````md
```csquery
epic: LL
status: todo, in-progress
tag: assigned_to=@me
sort: priority
view: board
```
````

`kind:`, `team:`, `layer:`, `depends:` and `text:` work too. The csquery reference lists every key.
