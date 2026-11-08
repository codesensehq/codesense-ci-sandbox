# Dependencies and readiness

_How depends_on links stories across epics, and how the left nav marks a story ready._

Each story lists the stories it waits on in `depends_on`. CodeSense resolves those ids across the whole team, and uses them to tell you which stories are ready to start and which are still blocked.

## depends_on

In markdown, `depends_on` is a YAML list of `story_id`s:

```yaml
story_id: LL-233
depends_on: [LL-214, LL-231]
```

In an HTML story it's a comma-separated meta tag: `<meta name="cs:depends-on" content="LL-209, LL-231">`.

The ids are bare `story_id`s, resolved through one index that covers every story in your workspace. So:

- **Links can cross epics.** LL-236 in `checkout-v2` can depend on LL-262 in `invoicing-exports`.
- **Moving a file doesn't break links.** The id stays the same wherever the file lives.
- **An id used by two stories doesn't resolve** until you make it unique.

A story whose dependency doesn't resolve, or isn't done yet, isn't ready.

## Ready in the left nav

The left nav marks a story **ready** when its status is a not-started one (such as Todo) and every story in its `depends_on` is done. Hover the row:

- a ready story says **Ready to start** and how many dependencies are done, or **no dependencies**
- a story that isn't ready says **Waiting on** and lists each dependency with its stage, for example `LL-214 · In review`, or **not found** for an id that doesn't match any story

**Show stories → Ready to start** in the left nav's **View, show and arrange** menu lists only the ready stories.

Readiness is worked out against the whole team's stories, so a dependency in another epic counts even when you've drilled into one folder.

## Epic progress

Each epic row in the left nav counts its done stories out of the total, including its sub-epics. The counts come from the stories' `status` fields, so they're always as current as the files.

## Query dependencies

To list dependencies, query them. The `depends` key in a csquery block lists the stories that depend on one story. An agent can answer "what blocks LL-233?" with the `query_context` MCP tool. See [Query your context](../guides/query-your-context.md).

Dependencies aren't filtered by repo, so an `api` story can block a `web` story.

The full guide ships in the app's docs: **Pipelines → Phase 5 · Story graph**.
