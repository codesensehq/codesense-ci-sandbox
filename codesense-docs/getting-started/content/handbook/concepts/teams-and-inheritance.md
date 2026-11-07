# Teams and inheritance

_How a team chain works: per-file inheritance, nearest wins, and merge marks._

Teams let you write shared standards once, at the top, and tune them lower
down. A team inherits context from its parent team, which can inherit from
its own parent, and so on. That line of teams is the **team chain**.

In Ledgerline, `platform` holds the conventions every product team follows,
and `billing` inherits from it.

## Declare a parent

A team's parent is the `parent_team` key on its entry in `teams.yml`:

```yaml
teams:
  - name: billing
    parent_team: platform
  - name: platform
```

Edit `teams.yml` to set or change a parent; the app shows the result under
the team's name ("inherits from platform"). CodeSense refuses a parent that
doesn't exist, a cycle, or a chain deeper than 64 teams, and points you at
`teams.yml`.

## Nearest wins, per file

A team's context is its own `content/` plus everything its parent teams
hold. CodeSense resolves it **one file at a time**, walking the chain nearest
first: the team itself, then its parent, then the parent's parent. For each
file path, the nearest team that has that file wins. Folders aren't owned by
any team: they're shared, and their contents merge across the chain.

With Ledgerline:

| File | `platform` has it | `billing` has it | `billing`'s context gets |
|---|---|---|---|
| `CLAUDE.md` | yes | yes | `billing`'s `CLAUDE.md` |
| `.claude/skills/review-pr/SKILL.md` | yes | yes | `billing`'s copy |
| `.claude/skills/triage/SKILL.md` | yes | no | `platform`'s, inherited |
| `.claude/skills/add-endpoint/SKILL.md` | no | yes | `billing`'s |
| `AGENTS.md` | no | yes | `billing`'s |

A team that doesn't have a file gets its parent's. So a new team `payments`
with `parent_team: billing` and an empty `content/` gets everything
`billing` resolves to.

## Editing inherited files

In the file tree, a child team shows its parent's files next to its own,
marked as inherited. Opening one says: "This file comes from ‹Parent›.
Changes you make here update it for ‹Parent› and every team that inherits
from it." While you edit, the toolbar reads "From ‹Parent› — edits save to
‹Parent›". Deleting, renaming or moving an inherited file asks you to
confirm first, because it happens in the parent team. To change a file for
your team only, choose **Copy into ‹team›** on it: your team gets its own
copy, and that copy wins from then on.

Folders are shared, not owned: a folder shows every team's files merged.
New files and files you drop into it land in your team, inherited files
can't be dragged, and renaming or deleting a folder only touches your team's
files in it.

## What an agent gets

The team chain is one of several places a file can come from. When an
agent launches in a repo's worktree, each file it asks for comes from the
first of these that has it:

1. Content for that repo only, in `<team>/repos/<host>/<owner>/<repo>/content/`.
2. Your [Private](private-space.md) files for this org.
3. Your **All orgs** folder (skills, prompts, snippets and templates only).
4. The team's own `content/`.
5. The parent teams, nearest first.

Your Private and All orgs files are left out of a team whose **Use my private
items in ‹Team›** is off.

Which files the agent asks for is its `context` (or its runtime's default);
the chain only decides where each one comes from. See
[Context in a worktree](agent-context.md).

## Merge marks

Some files shouldn't replace the copy that's already in the repo. A repo's
own `.claude/settings.json` or `.mcp.json` is the usual case. For those,
mark the team's file as a **merge file**:

1. In the file tree, right-click the file in your team context.
2. Choose **Mark as merge file**. (Choose **Unmark merge file** to undo.)

Only JSON, JSONC, JSON5, YAML and TOML files can be marked. A marked file
shows a badge: "Merges with the source file instead of overwriting it".
The marks live in `<team>/config/merge-marked.yaml`.

When a marked file is written into a checkout:

- Objects merge key by key, all the way down.
- Keys only the team file has are added.
- Where both files set the same key, **the repo's value is kept**.
- Arrays aren't combined: where both files have the same array, the repo's
  array is kept.

To check the result before it lands, open the marked file and switch the
viewer from **Authored** to **Merged preview**. It shows exactly the file a
launch writes into the repo you pick, and what the launch does with it there:
write it, replace a copy nobody edited, or keep an edited one.

To see which team a file comes from, open it from the team that owns it: each
team's own files live in its `content/` folder, and for each file the nearer
team's copy wins.

## Related

- [Skills are plain files](skills.md): a skill is a file in
  your team context; copy an example to start.
- [Private](private-space.md): what only you see, kept per org.

The full guide ships in the app's docs: **The model → Teams, and Content layers**.

## The repo axis

A skill or a `CLAUDE.md` can apply to one code repo only, on top of the team’s. It lives in `<team>/repos/<host>/<owner>/<repo>/content/` and reaches only the agents working in that repo.

Use a repo layer for what’s true of one code repo and nothing else: its build and test commands, its conventions, a skill that only makes sense there. Use team content for what’s true of the team’s work everywhere.

```text title="Tree"
web/
├─ content/CLAUDE.md                                  # every web agent
└─ repos/github.com/acme/checkout/content/
   └─ skills/retry/SKILL.md                           # only agents in acme/checkout
```
