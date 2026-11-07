# Attach source repositories

_Add a team's source repos and bind each one to a clone you already have._

Source repos hold the code your team works on. A team's list of source repos is in its context repo, so teammates share it. Where each repo lives on your Mac is yours alone: you attach it to a clone you made with your own git. CodeSense doesn't clone for you, and never commits, pushes or merges in it.

First create or pick a team in your [context repo](workspace.md). A team with no source repos shows **Add source repo**.

## Add a repo

Open the **Source repos** tab: **Go → Source Repos**, or ⌘⌥R. Under **Add a source repo**:

- **Local folder**, then **Choose folder…** to pick a clone you already have, then **Attach folder**. The repo is added to the team and your clone attached in one step. CodeSense reads the folder's git remote to know which repo it is.
- **From a URL**: paste the repository URL or `owner/repo` and choose **Add repo**. This puts the repo on the team's list; it doesn't clone it.

A repo added by URL shows **Not attached — no working copy yet** under **Tracked repos**. Clone it with git or your agent, then choose **Attach local clone** on its row and **Browse for clone directory**.

## What attaching gives you

Once a repo is attached, story worktrees for it go beside your clone, at `../{repo}-wt/{slug}` by default, each on its story's own branch. Your main checkout isn't touched. Agents you start in those worktrees get the context their definition declares, hidden from git. See [Launch your first agent](first-agent.md) and [Worktrees](../concepts/worktrees.md).

Ask your agent, or use git directly, for fetch, pull, push, merge and conflicts in the repo and its worktrees.

## Stop tracking

**Remove** on a repo's row takes it off the team's list. Your clone stays on disk.

## Through MCP

An agent does the same with `list_source_repos` (the team's repos and each one's clone: attached, missing or unattached), `add_source_repo` (by URL, or by path to attach a clone at the same time), `attach_source_repo_path` and `remove_source_repo`.

The full guide ships in the app's docs: **The model → Source repos**.
