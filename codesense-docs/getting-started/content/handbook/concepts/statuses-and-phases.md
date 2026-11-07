# Statuses and phases

_Per-team statuses: where they live, what their roles mean, and how a story's status changes._

Each team decides its own statuses, which it calls phases. A story's status is the `status:` field in its file, plain frontmatter like any other. CodeSense shows it, sorts by it and lets you change it, but nothing in CodeSense validates, gates or blocks a move from one status to another.

## Statuses (phases)

A team's statuses live in `content/status-taxonomy.yml`. Each state has an `id` (the value written into `status:`), a `label`, a `color`, an optional `icon`, an `order` and a `role`. The role is one of `not_started`, `active`, `review`, `done` or `blocked`, and it's how CodeSense knows what a state means: which stories count as finished, which are waiting on someone, and which are ready to start. A state with no role, or a role CodeSense doesn't know, counts as `active`.

With no file, the defaults are `planning`, `todo`, `in-progress`, `blocked`, `review`, `qa` and `done`.

To edit them, open **Settings › Phases**. You can reorder states, change labels and colours, and mark a done-role state as **Dropped**, which writes `delivered: false`: the work counts as finished but is left out of velocity. To add or remove a state, or change its role, edit the file. The status pickers and status icons follow this list.

## Changing a status

You can set a status from the status icon on a story in the left nav, or from the metadata band above a story. Each of these rewrites `status:` in the story file and nothing else. You, an agent, a routine or a dashboard button can also write the field directly. Every path is equal: there's no gate to pass and no override to file.

A status that isn't in your team's list is still saved as written. CodeSense shows it as drift, so you can see it and fix the value or add the state to your list.

> **Want tests to pass before done?**
>
> Ask your agent to run the tests before it moves the story, or have a routine run them and report the result. The check is your agent's or routine's job; the status is just the field it writes when it's finished.

## When an agent finishes

An agent working on a story calls the CodeSense MCP tool `signal_done` when it's finished. That records the result (success, failed or needs attention) and a short summary, and tells you. It doesn't move the story or run anything. If the agent should also change the status, it writes `status:` like any other edit. See [MCP tools](mcp-tools.md).

The full guide ships in the app's docs: **Pipelines + the full SDLC**.
