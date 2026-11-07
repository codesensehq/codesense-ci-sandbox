# Panes and slots

_Any HTML file in your context repo can fill a slot in the app: the right pane, a centre tab, the dock or an Inbox message. No plugin build, no app update._

## The slots

The app has slots: the right pane, the centre tab, the dock and the Inbox. Any HTML file in your context repo can fill one, with no plugin build and no app update.

| Slot | Where it shows | Good for |
|---|---|---|
| Right pane | Beside your terminals, in the Panels menu | A team board, a live checklist |
| Centre tab | A full tab, like a terminal | A dashboard you work in |
| Dock | Docked into the app’s layout | Something you always want open |
| Inbox message | Inside a message to you | A report that keeps itself current |

## One file, three shapes

The same page opens as a centre tab, a docked right pane or an Inbox message. A pane is one HTML file in `<team>/panes/<id>.html`.

```text title="pane_write"
{
  "id": "bugs-board",
  "html": "<!doctype html><title>Bugs</title>…"
}
```

`pane_write` writes the file and the pane appears in the right pane’s Panels menu at once; `pane_open` shows it. Save the file and an open pane reloads within a second. Teammates get it with your next push, like any other team file.

## Restyle the built-ins

Built-in panes are HTML you can customize. The Inbox is an HTML pane: fork it into your team’s content and restyle it, and every teammate gets your version. If yours fails to load, the app falls back to the built-in. Themes such as Dracula and Catppuccin come from one Appearance picker, and your panes follow them through the `--cs-*` tokens.

## Fenced in

Each pane runs in an isolated frame with its own capability token, and a pane that fails falls back to the built-in.

A pane reads and acts only through `window.codesense`: queries, sessions, routines and a fixed list of actions such as opening a workspace or messaging an agent. Dashboards are your own files, so open only ones you trust; see [How CodeSense trusts agents](https://github.com/codesensehq/codesense/blob/main/SECURITY.md#how-codesense-trusts-agents). A runaway loop is rate-limited. Errors show as a banner in the pane, never a blank pane.

## Shortcuts in ⌘K

Your own panes register their shortcuts with CodeSense and show up in the command palette.

## Ask an agent for a pane

An agent reads the live-data contract with `get_live_data_contract`, writes the page and checks it with `validate_live_data_doc` before handing it over.

> **Tip from the designer**
>
> Ask for the result, not the code: “a right pane that lists this sprint’s stories by status, with a button to open each one’s worktree.” The agent knows the contract; tell it what you want to see.
