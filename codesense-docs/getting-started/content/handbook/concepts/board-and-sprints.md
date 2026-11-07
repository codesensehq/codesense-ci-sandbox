# Stories and sprints

_Status as plain frontmatter, the left nav and its stage views, the sprint registry, adding stories to a sprint, and building a board pane._

A story's status, sprint, points and assignee are plain fields in its frontmatter. CodeSense reads them; you, your teammates and your agents write them. Sprints are a small registry file plus a `sprint:` field on each story. There's no built-in board: when you want one, you build it as a pane that queries your stories.

## Status is a field

`status:` holds one of your team's status ids. With no custom list, the defaults are Planning, Todo, In progress, Blocked, Review, QA and Done. [Statuses and phases](statuses-and-phases.md) covers how to change them.

```yaml
story_id: LL-233
status: todo
sprint: sprint-14
points: 3
assigned_to: sam
```

Edit the field in the file, ask an agent to, or use the app. Each of these writes the same line.

## In the left nav

The left nav shows your epics as folders and your stories inside them:

- **Status icon.** Each story row shows its status. Click the icon (**Change status**) to pick another one. CodeSense rewrites `status:` in the file.
- **Progress.** Each epic row counts its stories that are done out of the total, including sub-epics.
- **Ready.** A not-started story whose dependencies are all done is marked ready. Hover it to see **Ready to start**, or **Waiting on** and the stories it still waits for. See [Dependencies and readiness](dependency-graph.md).

The **View, show and arrange** menu at the top of the left nav changes what it lists:

- **Show stories** narrows the tree to one slice, each with its count: **Needs you**, **Agents on it**, **Ready to start**, **In progress**, **In review**, **Blocked**, **Not started** or **Done**. **Everything** shows them all.
- **Arrange** switches between **Folders** (epics as folders) and **Stage** (one flat list per stage, **Needs you** first and **Done** collapsed last).
- **Stories only** hides every file that isn't a story, keeping the epics as the frame.

## The sprint registry

Sprints live in your team's `config/sprints.yaml`. It's shared through git like the rest of the team's files: when an agent commits and pushes it, teammates get it on their next pull. Here are two sprints from the Ledgerline sample:

```yaml title="billing/config/sprints.yaml"
sprints:
  sprint-14:
    state: active
    goal: "Refunds you can retry, cart tax breakdown"
    focus_epics: [checkout-v2, invoicing-exports, platform-hygiene]
    start: "2026-09-14"
    end: "2026-09-25"
  sprint-15:
    state: planning
    goal: "Wallets, NetSuite export"
    start: "2026-09-28"
    end: "2026-10-09"
```

`state` is `planning`, `active` or `closed`, and it's the only required field. A closed sprint can record `delivered_points` and `carried_points`. A story belongs to a sprint when its frontmatter says `sprint: sprint-14`. Nothing moves on disk.

To plan, start, or close a sprint, edit this file by hand or ask an agent to. [Plan a sprint](../guides/plan-a-sprint.md) walks through the whole cycle.

> **One active sprint at a time**
>
> Marking a sprint active doesn't close the one that's already active. Set the old one to `closed` first. If two sprints end up active, the one with the later start date wins.

## Add stories to a sprint

Right-click a story in the left nav. The menu lists your sprints, the active one first:

- **Add to** a sprint when the story has none, or **Move to** another sprint when it has one.
- **Remove from sprint** clears the field.

Right-click an epic folder and choose **Add to sprint…** to tag its stories into the active sprint. You can add only the stories with no sprint (**Add N unsprinted**) or move them all (**Re-point all**).

Each change writes `sprint:` into the story file.

## Points

`points` is a story field. Use 1, 2, 3, 5, 8 or 13. The agent tool `validate_story` flags other values as `OFF_FIBONACCI_POINTS`.

## Build a board pane

A board is a view over these fields, so you build the one you want. A custom right pane is an HTML file in your team's `panes/` folder (or the `panes/` folder of your private space, just for you), and it can query your stories by their metadata through `codesense.queryContext` (or `codesense.subscribeContext`, to stay live). Group by `status` for one column per status, and filter by epic, `sprint` or `assigned_to`:

```js
codesense.subscribeContext(
  {
    where: { kinds: ["story", "bug", "spike"] },
    filters: [{ key: "sprint", op: "eq", value: "sprint-14" }],
    groupBy: "status",
  },
  (result) => drawColumns(result.groups, result.rows),
);
```

To show only the pane's own team, ask where the pane lives first. `codesense.context()` resolves `{org, team, layer, contentRoot}` (`team` is `null` in your private space):

```js
const { org, team } = await codesense.context();
codesense.subscribeContext(
  { where: { orgs: [org], teams: [team], layers: ["team"], kinds: ["story"] }, groupBy: "status" },
  (result) => drawColumns(result.groups, result.rows),
);
```

A card can open its story and change it in place:

- `codesense.openFile({ path: row.path, newTab })` opens the file in the viewer. The path must be inside one of your synced teams' content; a story in another team switches to that team. `openFile({ team, path })` takes a path relative to that team's content.
- `codesense.setField({ path: row.path, field: "points", value: 5 })` writes one frontmatter key (`points`, `assigned_to`, `sprint`, `rank`, or any other), and `{ path, fields: {…} }` writes several at once. Numbers stay numbers, a list writes a list, and `value: null` removes the key. Only those keys' lines change, and every live query re-runs.
- `codesense.act("setStatus", { path: row.path, status })` moves a card.

Each row's `relationships.parentEpic` is the epic its frontmatter names, and `relationships.folderEpic` is the epic folder it sits in, as the left nav shows it (add `"relationships"` to `select`).

Build yours from the example board in the CodeSense docs: copy `examples/dashboards/story-board.html` into your `panes/` folder and set its epic. Or ask an agent to write one: it can create and open a pane with the MCP tools `pane_write` and `pane_open`. [Query your context](../guides/query-your-context.md) explains the query, and [Dashboards and pages](dashboards.md) covers panes and pages.

For a read-only board inside any Markdown doc, a `csquery` block is enough:

````md
```csquery
tag: sprint=sprint-14
view: board
```
````

See the csquery reference.

The full guide ships in the app's docs: **Pipelines → Phase 6 · Sprint plan**.
