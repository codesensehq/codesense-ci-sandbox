# Your first hour

_A timed tutorial from a fresh install, with no account, to an agent working on a story in your own repo._

This is the shortest path from a fresh install to an agent working on a story
in your own repo, with your team's context in its worktree. You don't need an
account for any of it. The times are a guide; each step links to the page
with the details.

**You need:** a Mac with Apple Silicon, a repo you work in, and Claude Code,
Codex or OpenCode installed.

## Minutes 0–5: open CodeSense in its own docs

Install CodeSense and open it. There's no sign-in and no welcome screen: the
app opens straight into **CodeSense Docs**, a context repo that ships with every
install. Its **getting-started** team is open with its README. See
[Install CodeSense](install.md).

This context repo is these docs, set up the way a team would set up its own:
a routine you can open and run, and `examples/` with skills (such as
`/routine`, `/agent` and `/codesense-setup`) and dashboards you can copy into
your own team. Nothing in `examples/` is installed; the CodeSense MCP tools
describe everything an agent needs on their own. It's read-only and has a book icon in the context repo menu at the
top left. It's a real git clone: to update it, ask an agent to run the
`update-codesense-docs` skill, which fetches the latest docs with git. CodeSense never commits, pushes
or merges for you.

## Minutes 5–10: connect your context repo

Open the context repo menu (the **CodeSense Docs** name at the top left, or ⌘⇧G).
At the bottom:

- **Connect a context repo…** if your team already has one. Clone it with git,
  choose its folder, and confirm **Connect**. CodeSense reads and writes files
  in that folder; you or your agents run git.
- **Create a new context repo…** to start one. Give it a name; it goes in
  `~/CodeSense/<name>` unless you choose another location. Turn on **Start from
  the CodeSense example** to begin with an editable copy of the docs' setup.

CodeSense switches to it. A new, empty context repo asks you to **Create a
team**. From now on, CodeSense opens the context repo you used last, and the
CodeSense docs stay one switch away.

Then connect your code. Open **Source repos** (**Go → Source Repos**, or
⌘⌥R), choose **Local folder**, then **Choose folder…**, pick your clone and
choose **Attach folder**. Your main checkout stays untouched: agents work in
worktrees next to it. See
[Attach your repos](attach-repos.md).

Then give the team some context. In the team's content folder, add a
`CLAUDE.md` (or `AGENTS.md`, if you use Codex) with the few rules every
agent on this code should follow. Ledgerline's starts like this:

```md
# Billing team — agent instructions

## Commands
- api: `npm test`, `npm run db:migrate`, `npm run contract`

## Rules
- Amounts cross the wire as integer cents in a string (`"1299"`), never floats.
- Migrations are forward-only. Use the `write-migration` skill.
```

Attach a second repo the same way if you like. Agents you launch there get
the same `CLAUDE.md`: one context, two repos, nothing copied by hand.

## Minutes 10–15: run an agent on your repo

Right-click an epic folder in the left nav and choose **New story…**. Give it
a title and set the epic, the story id and the repo, then choose **Create story**.

Right-click the story in the left nav and, under **Launch agent**, choose your agent.
CodeSense creates its worktree on its own branch and starts your agent there
in the terminal with a prompt to read the story and complete it. Before the
agent starts, CodeSense copies in the context its definition asks for. If you
added a `CLAUDE.md`, it's in the folder:

```sh
ls CLAUDE.md          # the team context is here
git status --short    # clean: context paths are excluded from git
```

See [Launch your first agent](first-agent.md).

Stories are plain files, so you can also write them yourself or have your
agent write them. A story file needs `type: story` to be treated as a story,
and a `source_repo` (`owner/repo`, one of the team's source repos) to get a
worktree. See
[Write your first story](../guides/first-story.md).

## Minutes 15–20: open a dashboard

Any `.html` file in a context repo opens as a live page. In the CodeSense docs,
open `examples/dashboards/sprint-analysis.html` to see one reading stories live.
Copy it into your own context repo, or start your context repo from the
CodeSense example, to have it read yours. See
[Dashboards and pages](../concepts/dashboards.md).

## Minutes 20–30: run several agents side by side

Add two or three more stories and start an agent on each: right-click a story
in the left nav and, under **Launch agent**, choose your agent. Each story gets
its own branch and worktree, so agents never share files. Split the terminal
area into tiles to watch them together.

Your own scripts and CLIs run in any worktree's terminal too, next to the
context the agent launched with.
See [Run your own tools and scripts in a worktree](../guides/your-own-tools.md).

## Minutes 30–40: build a skill once

Add `.claude/skills/<name>/SKILL.md` to the team's content folder. The next
Claude Code agent you launch, on a story in any repo you attached, gets the
skill in its worktree, hidden from git. Nothing syncs into worktrees on its
own: an agent that's already running picks it up when it calls
`hydrate_worktree`.
See [Write a skill once for every repo](../guides/skill-once.md).

> **Beta**
>
>   Improve the skill from inside a worktree, then ask the agent to send the
>   change back to the team's content folder with `reverse_sync_worktree`
>   (after a preview with `preview_reverse_sync_worktree`).

## Minutes 40–50: finish a story

Ask the agent to run the repo's tests in the story's worktree, fix what
fails, and move the story to `review` when they pass. A story's status is
a plain field in its file, so the agent changes it like any other edit,
and you can change it from the left nav. When it's finished, the agent
calls `signal_done` and the result shows up in the app. See
[Statuses and phases](../concepts/statuses-and-phases.md).

## Minutes 50–60: write your own page

Any `.html` file in your context opens as a live page that can read your
stories. Copy one of the CodeSense docs' pages and change it, or start from
the [page SDK reference](../../generated/page-sdk.md).

> **Beta**
>
>   You can also have an agent write the page. It gets CodeSense's authoring
>   contract and a validator through its MCP tools, and writes the page into
>   your context.
>   See [Have an agent build a dashboard](../guides/agent-builds-dashboard.md).

## Where to go next

- [Create or connect a context repo](workspace.md): share
  your context with teammates.
- [The context repo](../concepts/context-repo.md) and
  [Teams and inheritance](../concepts/teams-and-inheritance.md): how your
  team's context is organized.
- [Stories and sprints](../concepts/board-and-sprints.md): planning the work
  your agents pick up.
- [Sharing through git](../concepts/sharing.md): getting your context to
  teammates.
