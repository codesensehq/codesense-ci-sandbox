---
name: codesense-setup
description: Walk the user through setting up CodeSense — workspace, teams, source repos, agents and routines — using the CodeSense MCP tools. Use when the user says "set up CodeSense", "/codesense-setup", "what's left to set up", or "add my repos". Safe to rerun; it reports what is done and what is left.
---

# CodeSense setup

You set CodeSense up with the user by calling the `codesense` MCP tools. CodeSense
only records and shows the setup. You do the work and you check with the user first.

Rules:

- Start by calling `describe_setup`, and call it again after each section. Its result is
  the source of truth. Don't rely on what you remember. If something is already done, say
  so and move on. A rerun only does what is still left.
- Ask before you change anything. Ask before you clone, create, rename or add something,
  and confirm the exact names with the user.
- Get an explicit "yes" before any destructive step. These are `delete_team`,
  `archive_team`, `remove_source_repo` and a hard `agent_delete`. Say exactly what will
  happen first.
- Never ask for a token, password or API key in chat, and never accept one. CodeSense
  stores no service credentials; external services are reached through the user's own
  tools and logins.
- Every tool returns `{status, ...}`. A `validation` or `unauthorizedTeam` status carries
  the reason in plain language. Show it to the user, fix the input, and don't retry
  blindly.
- Keep it short. One section at a time, a line or two per step.

## 1. Discover

Call `describe_setup`. It returns:

- `activeOrg` and `activeTeam`.
- `orgs[]`. Each org has its teams. Each team has its `sourceRepos` (with their
  `attachment` and `localPath`).
- `scratch`. The user's private space (**Private** in the app): things only they use. `scratch.orgs[]` has one
  entry per org with what is filed for that org (notes, files, dashboards, routines,
  agents), and `scratch.allOrgs` counts the skills, prompts,
  snippets and templates that apply in every org. Nothing here needs setting up.

Give the user a short checklist of what's done and what's left, then ask where to start.
Default to the order below.

## 2. Workspace

- **No org in `orgs`.** Nothing else can be set up yet. If your tool list includes
  `create_local_organization`, offer to create a local organization with it; if it
  includes `attach_context_repository`, offer to attach an existing context repo instead.
  Otherwise ask the user to create or attach one in the CodeSense app, then rerun this
  skill.
- **An org lists `loadErrors`.** Report them. Don't edit that org.
- To rename a workspace, call `rename_workspace {org, name}`. The slug stays the same.
- If the user has more than one org, ask which one to work in. Pass that org's `slug` as
  `org` everywhere below.

## 3. Teams

- To create a team, call `create_team {org, name, parent?, activate?}`. The name is a
  lowercase slug. Set `activate: true` when the user wants to work in the new team now.
- To switch teams, call `set_active_team {org, team}`.
- To rename, call `rename_team {org, team, newName}`. To nest or un-nest, call
  `set_team_parent {org, team, parent?}`.
- Archiving with `archive_team {org, team, archived}` can be undone.
  `delete_team {org, team}` removes the team from the registry. Its content stays on disk.
  Confirm both with the user first.

## 4. Source repos

Each team tracks the code repos it works on. For each repo the user names, check the
team's `sourceRepos`:

- **Attached** (`attachment: attached`). Done.
- **Already cloned on this machine.** Call `add_source_repo {org, team, path}` with the
  absolute path of the clone's root. That adds the repo and binds the clone. If the team
  already tracks the repo, use `attach_source_repo_path {org, team, repo, path}` instead.
  `missing` and `unattached` repos are fixed the same way.
- **Not cloned yet.** CodeSense never clones. Ask the user where to put it, run
  `git clone <url> <dir>` yourself, then add it by `path` as above.
- **Track it now, clone later.** Call `add_source_repo {org, team, url}`.
- If the result asks for an `account` (several code-host accounts fit), ask the user which
  account to use and pass its id.
- To check the result, call `list_source_repos`. `remove_source_repo {org, team, repo}`
  stops tracking a repo and leaves the clone on disk. Confirm it first.

## 5. Agents and routines

- Call `agent_list`. Everyone has the two starters, `builtin/claude` and `builtin/codex`,
  so a team needs no agent of its own to start. If the user wants a named worker (a
  reviewer, a triage agent), make it with `agent_create`; its description lists every
  field. Prefer updating or copying an agent that already fits.
- For anything that should run on a schedule ("every weekday at 9…"), call
  `routine_create` with the agent it belongs to and a `mode`: `wake` delivers the task into
  that agent's running session (starting one only if none runs), `fresh` starts a new one
  each time. Its description covers the schedule forms and what the task must say. For a
  one-off reminder in the current session ("check back in 20 minutes"), use
  `schedule_wakeup` instead: a timer that ends with the session.
- Agents and routines go to the team by default, where every teammate can see and change
  them. When the user wants one for themselves, pass `scope: "personal"` and the `org`: it
  lands in their private space for that org (Only me) and never applies in another org.
- Creating either is a local edit to the context repo. The team gets it when the user's
  agent pushes it with git.

For a team with several seats or unclear handoffs, follow the
[agent-design guide](../../../arc-e-agents/agent-design.html) and
[agent-design skill](../agent-design/SKILL.md). Start with discovery and a written
proposal. Preview before saving, and ask before creating or enabling anything.

## 6. Summary

Call `describe_setup` one last time. Report what this run changed, what is still left,
and anything that is waiting on the user in the app, such as a workspace to attach.
