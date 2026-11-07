---
title: "Sample inbox threads"
type: doc
author: "Seeded demo. Four read-only thread shapes that show how CodeSense's agents coordinate: triage dispatch, decision escalation, review feedback, ship confirmation."
---

## Purpose

The four files in this directory are the canonical **thread shapes** that recur in
every CodeSense factory flow. They are read-only on the docs org (`refuse_write_at`). Copy any of them into your own context repo's
`config/inboxes/` to seed your inbox with the same shape — then wire up a routine's
`inbox_idle` trigger to any role in the thread.

| File | Role flow | What it demonstrates |
|---|---|---|
| `01-triage-dispatch.yml` | analyst → builder | A user intent lands in the analyst's inbox; the analyst drafts a story and dispatches it to the builder role. |
| `02-decision-escalation.yml` | builder → resolver | The build hits an ambiguous spec; the builder escalates to the resolver, who clarifies and sends back a resolution message. |
| `03-review-feedback.yml` | reviewer → builder | A review surfaces findings; the reviewer posts a structured message with per-finding links; the builder replies with a fix plan. |
| `04-ship-confirmation.yml` | builder → analyst | After the merge, the builder notifies the analyst with the shipped SHA + the retro link; the analyst closes the loop. |

Each file carries the full message history for one thread, with addresses, timestamps,
content type, and the `on_click` affordance each row should bind to (see
`DESIGN-inbox-ux.md` §3).

## See also

- `agent-inbox-console.html` — the live inbox dashboard.
- `inbox-flows.html` — the four flow shapes explained with visuals.
- `codesense-docs/getting-started/content/examples/sample-epic/README.md` — the fake
  epic these threads reference by story id.
