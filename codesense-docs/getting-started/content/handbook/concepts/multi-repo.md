# Multi-repo

_A team's repo list, source_repo on each story, epic repos and how launches route to the right repo._

One team can work across many repos. The team's context reaches every repo you attach. The plan (stories, dependencies and sprints) covers all of them at once. And each story's workspace opens in that story's own repo.

## The team's repo list

A team lists its source repos in the org's `teams.yml`. The Ledgerline `billing` team works in two:

```yaml title="org/teams.yml"
teams:
  - name: billing
    parent_team: platform
    source_repos:
      - {owner: ledgerline, repo: web}
      - {owner: ledgerline, repo: api}
```

You don't have to edit this by hand. Open the **Source repos** tab from **Go → Source Repos** (⌘⌥R). Under **Add a source repo**, choose **Local folder** to add a clone you already have, or **From a URL** to track a repo by its URL or `owner/repo`. There's no cap on the number of repos.

The list says which repos the team works in. Where each repo lives on your machine is a separate, per-user setting: attaching a clone links the repo to a checkout you made with your own git. CodeSense doesn't clone for you. See [Attach your repos](../getting-started/attach-repos.md).

## Context reaches every repo

The same team context serves every repo on the list. When an agent launches in a worktree of any of them, CodeSense copies in the context that agent's definition asks for, as files that stay out of git. Content you keep for one repo only (`<team>/repos/<host>/<owner>/<repo>/content/`) wins over the team-wide copy in that repo. Your own attached clone is never written to. See [Context in a worktree and reverse sync](agent-context.md).

## Each story names one repo

Every story carries `source_repo`, written `owner/repo`. It must exactly match a repo on the team's list:

```yaml
story_id: LL-214
source_repo: ledgerline/api
```

```yaml
story_id: LL-231
source_repo: ledgerline/web
```

A story names exactly one repo. When work touches `api` and `web`, split it into one story per repo and link the two with `depends_on`. For example, LL-233 (web) depends on LL-214 (api).

> **No source_repo, no launch**
>
> A story without `source_repo` is left out of the story index. It can't be opened as a workspace, and other stories can't find it by id. The agent tool `validate_story` flags it as `MISSING_SOURCE_REPO`, or as `UNKNOWN_SOURCE_REPO` when the value isn't on the team's list.

## One plan across repos

Dependencies, readiness and sprints all read one team-wide index. A single sprint can hold `api` and `web` stories side by side, and queries can filter and group on `source_repo`.

## Launches land in the story's repo

When you open a story's workspace, CodeSense looks up the story's `source_repo`, finds your attached clone of that repo, and creates the story's worktree beside it, at `../<repo>-wt/<slug>`, on the story's `branch_name` or `feat/<story_id>`. Later opens reattach the same worktree. You don't `cd` or pick a clone. The same routing applies whether the workspace is opened from the app, from a dashboard or by an agent through the MCP tool `open_workspace`.

If you haven't attached that repo yet, CodeSense asks: **No local clone attached**, with **Attach local clone**. Once you attach it, the open goes ahead.

## Epic repos

An epic's main doc can list the repos the epic spans:

```yaml title="checkout-v2/README.md"
repos: [ledgerline/web, ledgerline/api]
```

In the app, **Configure epic repos** in the epic's terminal bar opens **Configure repos**, where you add or remove team repos. It opens on its own the first time you open an epic with no repos. Agents can do the same with the MCP tools `read_epic_repos` and `set_epic_repos`. Writing the list works for markdown epic docs only.

The full guide ships in the app's docs: **The model → Source repos**.
