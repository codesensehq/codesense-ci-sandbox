# Create or connect a context repo

_Create a new context repo, or connect one your team already shares, then add your first team._

A **context repo** is a git repo that holds your team definitions and shared context: agents, routines, epics, stories, docs and dashboards. A **team** is a named group inside it. No account is needed for any of this.

Both ways in start from the context repo menu: the name at the top left, or ⌘⇧G.

## Create a new context repo

Choose **Create a new context repo…**. Type a **Name**; the **Location** follows it as `~/CodeSense/<name>` until you type another path or use **Choose…**. The folder must not exist yet: CodeSense never writes into an existing folder. Turn on **Start from the CodeSense example** to begin with an editable copy of the CodeSense Docs' setup, or leave it off to start empty. Then choose **Create and connect**.

CodeSense creates the folder with `org.yml`, `people.yml` and `teams.yml`, starts a git repo on an unborn `main`, and switches to it. It doesn't stage files, commit, set an author or add a remote. The repo's id comes from the name (`Acme Billing` becomes `acme-billing`), and a name already in use is refused.

A new, empty context repo has no teams yet, so it asks you to **Create a team**. Then [attach your source repos](attach-repos.md).

If the folder is created but can't be connected, the error names the folder. Use **Connect a context repo…** with it to retry.

## Connect one your team already has

Clone the context repo with git (or have your agent do it), then choose **Connect a context repo…** and pick the clone's top folder, the one with `.git/` and `org.yml`. CodeSense asks **Connect &lt;name&gt;?** (with the repo's name), says it will read and write files there and won't commit, push or merge, and switches to it when you choose **Connect**. It records where the folder is and doesn't move or copy it.

A folder that isn't a git checkout's top folder, or has no valid `org.yml`, is refused: "This folder isn't a CodeSense context repo. Pick the folder you cloned, or create a new one."

From then on CodeSense opens the context repo you used last. The CodeSense Docs stay in the menu, one switch away.

If you move a connected repo's folder, CodeSense shows it as missing: locate the folder or disconnect it. Disconnecting forgets the folder and keeps every file.

Your agent and git own commits, remotes, credentials, fetch, push and sharing.

## Set up through MCP

A local agent can do all of this with the same services the app uses:

| Operation | Tool and arguments |
| --- | --- |
| Create a context repo | `create_local_organization({displayName, destinationPath})` (always starts empty) |
| Connect / list / disconnect | `attach_context_repository({clonePath})`, `list_context_attachments({})`, `detach_context_repository({orgSlug})` |
| Create a team | `commit_registry_edit_local({orgSlug, edit: {type: "CreateTeam", team_name: "billing"}})` |
| Manage teams | The same tool renames, archives and deletes teams, sets parent teams and adds source repos. |
| Add a source repo to a team | `commit_registry_edit_local({orgSlug, edit: {type: "AddSourceRepoToTeam", team_name, owner, repo}})` |
| Attach a source checkout | `save_source_repo_attachment({repoPath})` reads the checkout's origin locally. |
| Discover and switch | `describe_setup()`, `list_orgs_teams()`, `set_active_team({org, team})` |

None of these needs an active team. They act for the selected local profile; a caller can't name another one. Registry edits update the app straight away.

## Profiles and local state

**Settings → Local profile** chooses whose local workspace to open. Profiles keep their context repos, files and routines separate. Switching saves pending notes and the workspace selection first, then reloads the app with the chosen profile. If saving fails, CodeSense stays in the current profile and shows the error.

The full guide ships in the app's docs: **First touch → Create a context repo, and Connect a context repo**.
