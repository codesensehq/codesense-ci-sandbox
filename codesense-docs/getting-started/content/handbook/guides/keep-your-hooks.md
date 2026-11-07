# Keep your own Claude Code hooks

_Your repo's .claude/settings.json hooks keep working next to CodeSense's status hooks and your team's settings._

Your repo may already commit a `.claude/settings.json` with hooks, such as
a formatter after every edit or a guard before shell commands. This guide
shows how to keep them working in CodeSense worktrees, alongside CodeSense's
own hooks and any hooks your team adds.

## CodeSense's status hooks never touch your settings

> **Beta**
>
> Both Claude Code and Codex drive the status dot on a terminal tab, launched
> by CodeSense or typed by hand, on your Mac or on a machine. The shell runs
> the CLI through CodeSense's activity helper, which adds CodeSense's hooks
> to a private copy of the CLI's settings. Your own `--settings` are kept,
> and nothing in the worktree changes.

Claude Code also runs an older status hook, `codesense-notify.sh`, which
CodeSense installs with itself beside its activity helper and runs as
`$CODESENSE_ACTIVITY_HOST/codesense-notify.sh`. Nothing is written into your
worktree or your team's content, and your team ships no hook file. The
settings that run it go **only** into files CodeSense owns:

- A launched agent gets them in a private settings file for that session.
- A hand-typed `claude` in a worktree CodeSense made gets them in
  `.claude/settings.local.json`. Existing keys and hooks there are kept,
  and the file is kept out of git.
- If your `settings.local.json` is tracked by git, is a symlink, or isn't
  valid JSON, CodeSense leaves it alone.
- If your committed `settings.json` already runs
  `$CODESENSE_ACTIVITY_HOST/codesense-notify.sh` for an event, CodeSense
  doesn't add it again.

Claude Code runs hooks from every settings file, so yours and CodeSense's
all fire. Your repo's `.claude/settings.json` is never written.

## When your team ships a `settings.json` too

A team can put `.claude/settings.json` in its content, for example to run
a check whenever an agent stops. Say Ledgerline's `billing` team adds
`billing/content/.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      { "hooks": [{ "type": "command", "command": "npm run lint --silent" }] }
    ]
  }
}
```

What lands in a worktree depends on the repo:

- **The repo commits its own `.claude/settings.json`.** A launch never
  writes a file the repo tracks, so the repo's file is used as it is and
  the team's file doesn't land there. Keep your hooks in the repo; they
  always win.
- **The repo has none.** The launch copies the team's file into the
  worktree and keeps it out of git.
- **You edited the copy in a worktree.** The next launch keeps your edit
  and reports it, rather than replacing it.

## Combining team files

By default the nearest team's file wins whole: a child team's
`.claude/settings.json` replaces its parent team's. To combine them
instead, mark the file as a merge file:

1. In the file tree for the team, right-click `.claude/settings.json`.
2. Choose **Mark as merge file**. The file gets a badge: "Merges with the
   source file instead of overwriting it".
3. Open the file and switch the viewer from **Authored** to **Merged
   preview**, then pick a repo to see exactly what a launch there would
   write, and whether it would write it at all.

How the merge decides:

- Objects merge key by key, all the way down.
- A key only one team's file has is added.
- Where two teams set the same key, **the nearer team's value wins**.
- Lists aren't combined unless the mark names a key to match items by.
  Otherwise, if both files have hooks for the **same event**, the nearer
  team's list for that event replaces the other.

Check the **Merged preview** for each repo when in doubt.

## Related

- [Teams and inheritance](../concepts/teams-and-inheritance.md): merge marks
  and the other rules for how team files combine.
- [Launch your first agent](../getting-started/first-agent.md): the status
  dot these hooks drive.

The full guide ships in the app's docs: **Agents → The Claude notify hook**.
