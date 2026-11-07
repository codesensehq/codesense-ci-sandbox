# The context repo

_Orgs, teams and the files-in-git model: what lives where in your context repo._

Everything your team hands to agents lives in one git repo, the **context
repo**. It isn't a database or a hosted service: it's folders and text
files you can read, edit and review in any editor, and share with `git`.
CodeSense reads it, resolves it per team, and writes the result into your
code.

## Orgs and teams

An **org** (a workspace, in the app) is one context repo. Inside it:

- **Teams** each have their own folder, their own context, and their own
  list of source repos. Ledgerline has `platform` and `billing`.
- A team can name a **parent team** and inherit its context. See
  [Teams and inheritance](teams-and-inheritance.md).

You can create, archive and restore teams in the **Teams** tab (**Go → Teams**, ⌘⌥T).

On first launch, the switcher holds one org: the read-only CodeSense docs. **Create a new context repo…** makes your own, and **Connect a context repo…** adds one you already have.

## What lives where

A shared org is its context repo: a plain git folder wherever you cloned or
created it. CodeSense records where it is in
`~/.codesense/users/<id>/context-attachments.yml`. Here's Ledgerline's, trimmed:

```text
ledgerline/codesense-context/
├── org.yml                 # the org's slug, display name and creation time
├── teams.yml               # teams, parents and source repos
├── people.yml
├── platform/
│   └── content/
│       ├── CLAUDE.md
│       └── .claude/skills/review-pr/SKILL.md
└── billing/
    ├── config/             # app-managed settings, hidden in the file tree
    │   ├── agents/          saved agents and their routines
    │   └── sprints.yaml
    └── content/            # what agents draw their context from
        ├── CLAUDE.md
        ├── AGENTS.md
        ├── .claude/skills/  add-endpoint, review-pr, write-migration, …
        ├── planning/        epics and stories
        └── dashboards/      HTML pages
```

### `teams.yml`

One entry per team, with its parent and the repos it works in:

```yaml
teams:
  - name: billing
    parent_team: platform
    source_repos:
      - {owner: ledgerline, repo: web}
      - {owner: ledgerline, repo: api}
  - name: platform
    source_repos:
      - {owner: ledgerline, repo: web}
      - {owner: ledgerline, repo: api}
```

### `<team>/content/`

The team's context, laid out the way it should appear in a repo. A file
lands at the same path at the root of a worktree: `content/CLAUDE.md` becomes
`CLAUDE.md`, and `content/.claude/skills/` becomes `.claude/skills/`. Which
files land is up to each agent: its `context` (or its runtime's default) is
copied in when it launches, and nothing else moves until an agent asks. See [Context in a worktree and reverse sync](agent-context.md).

Anything you put here goes: instructions for any agent (`CLAUDE.md`,
`AGENTS.md`, `.codex/…`), skills, docs, plans and dashboards.

### `<team>/config/`

Settings the app manages for the team, kept out of `content/` so they're
never written into your repos:

- `agents/<id>/agent.yaml`: the team's saved agents, with each agent's
  routines under `agents/<id>/routines/`.
- `sprints.yaml`: the team's sprints.
- `merge-marked.yaml`: the files you've marked to merge instead of replace.
- `projection-excludes.yml`: glob patterns that are never copied into a
  workspace. A pattern matches any letter case (`**/*.tfstate` also hides
  `Prod.TFSTATE`). A pattern starting with `!` brings a path back, but only
  in the exact case it's written in.

### `<team>/repos/<host>/<owner>/<repo>/content/`

Context for one repo only, for example
`billing/repos/github.com/ledgerline/api/content/`. The app shows it as
**billing · api** in the context picker. Content here wins over the
team-wide content for that repo.

## Files in git

Because it's a git repo, the usual things work: history, blame, branches and
review. CodeSense adds its own views on top: the left nav's **Changes** view
lists what you've changed, and **History** shows the repo's commits.
CodeSense never commits, pushes or merges: you or your agents do the git, and
teammates get a change on their next pull. See
[Sharing through git](sharing.md).

> **Upgrades are automatic**
>
>   When a new version of CodeSense changes the on-disk layout, it migrates
>   your context repo when it starts. You don't need to do anything.

The full guide ships in the app's docs: **The model → Orgs**.

## Two axes

What an agent sees is decided on two axes at once. On the **team axis**, layers stack from the parent team down to your private layer. On the **repo axis**, a code repo can add its own files on top. For each file, the nearest layer wins, so company rules, team rules and your own tweaks stack without copying.

For one story in the `acme/checkout` repo, an agent gets:

- `CLAUDE.md`, from the `web` team layer;
- `.claude/skills/retry/`, from the `acme/checkout` repo layer;
- `.claude/skills/snippets/`, from your private layer.

| Layer | Where it lives | Who sees it |
|---|---|---|
| Team content | `<team>/content/` | The team and its child teams, once pushed |
| Repo layer | `<team>/repos/<host>/<owner>/<repo>/content/` | Agents working in that one code repo |
| Private | Your private space for one org, on your Mac | You, never shared |
| All orgs | Your private all-orgs folder | You, in every org |

The [Context layers](teams-and-inheritance.md) guide goes deeper: which layer wins when, and how to move a file from your private layer to the team.

## Through MCP

Agents read the context repo with the same tools the app uses. One query finds files by frontmatter, path or text across every synced org.

```text title="query_context · open stories for web, by status"
{
  "where": { "teams": ["web"], "kinds": ["story", "bug", "spike"] },
  "match": { "status": ["todo", "in-progress"] },
  "groupBy": "status"
}
```

Related tools: `describe_setup` lists orgs, teams and code repos; `list_context_content` browses a team; `move_between_contexts` moves a file from your private layer to the team.
