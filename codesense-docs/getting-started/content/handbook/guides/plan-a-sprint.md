# Plan a sprint

_Add a sprint to sprints.yaml, tag stories into it, estimate them, and roll the old sprint over._

This guide plans Ledgerline's next sprint: create it, fill it with stories, estimate them, and then roll sprint 14 into it. Everything here is a file edit. Sprints are recorded in the team's `config/sprints.yaml`, and each story's membership is its `sprint:` field. You can make each edit yourself or ask an agent to.

## 1. Add the sprint

Add an entry to `config/sprints.yaml` in your team's context with `state: planning`:

```yaml title="billing/config/sprints.yaml"
sprints:
  sprint-14:
    state: active
    goal: "Refunds you can retry, cart tax breakdown"
    start: "2026-09-14"
    end: "2026-09-25"
  sprint-15:
    state: planning
    goal: "Wallets and NetSuite export"
    start: "2026-09-28"
    end: "2026-10-09"
```

The key (`sprint-15`) is the sprint id that stories point at. Only `state` is required.

Or ask an agent in any worktree:

> Add sprint-15 to our team's config/sprints.yaml: state planning, goal "Wallets and NetSuite export", two weeks starting the day after sprint-14 ends.

## 2. Add stories to it

- **One story:** right-click it in the left nav and choose **Add to sprint-15** (or **Move to sprint-15** if it's already in a sprint).
- **A whole epic:** right-click the epic folder and choose **Add to sprint…**. This targets the **active** sprint. You can add only the stories with no sprint (**Add N unsprinted**) or move them all (**Re-point all**).
- **By hand or by agent:** set `sprint: sprint-15` in each story's frontmatter.

**Remove from sprint** on a story's menu clears the field.

## 3. Estimate

Set `points:` on each story to 1, 2, 3, 5, 8 or 13. An agent can do this in bulk, for example: "Estimate every story tagged sprint-15 and write `points:` into each file."

To see the sprint as a board, with points per column, [build a board page](../concepts/board-and-sprints.md#build-a-board-pane) that queries `sprint: sprint-15` and groups by `status`.

## 4. Close the current sprint

When sprint 14 ends:

1. Re-tag its unfinished stories to `sprint-15`, or clear their `sprint:` to send them back to the backlog. Finished stories keep `sprint: sprint-14`, so the history stays in the files.
2. Set sprint 14 to `state: closed`. You can record what it delivered:

```yaml
  sprint-14:
    state: closed
    delivered_points: 21
    carried_points: 5
```

An agent can do both steps: "Close sprint-14. Move its unfinished stories to sprint-15, then record delivered and carried points in sprints.yaml."

## 5. Start the next sprint

Set sprint 15 to `state: active`. The nav's sprint menus now list it first, and an epic's **Add to sprint…** targets it.

> **Close before you activate**
>
> Marking a sprint active doesn't close the one that's already active. Close the old sprint first (step 4). If two sprints are ever active, the one with the later start date wins.

## Sprint focus

To record which epics a sprint is about, add `focus_epics`:

```yaml
  sprint-15:
    state: planning
    focus_epics: [checkout-v2, invoicing-exports]
```

Your board page or an agent can read it to narrow what it shows.

The full guide ships in the app's docs: **Pipelines → Phase 6 · Sprint plan**.
