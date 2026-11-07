# Context in a worktree and reverse sync

_How an agent's context reaches its worktree as real files at launch, how an agent asks for more, and how reverse sync (in Beta) brings edits back._

Agents read files. So CodeSense doesn't hand your context to agents through
an API or a plugin: it copies the files an agent needs into the folder it
works in, as ordinary files. Any agent, script or tool that reads the folder
sees the same `CLAUDE.md`, `AGENTS.md` and skills.

Nothing syncs in the background. Creating a worktree, for example when you
open a story, copies no context: it writes only CodeSense's own connection
files (`.mcp.json`, `.codex/config.toml`, `.claude/settings.local.json`).
Context reaches a worktree at two moments:

- **When an agent launches there.** CodeSense compares what the agent's
  definition asks for with what's already in the folder, copies the
  difference in one batch, and lists the copied paths in the repo's
  `.git/info/exclude`.
- **When an agent asks**, by calling a CodeSense tool such as
  `hydrate_worktree` or `hydrate_story`.

Between those moments, editing your team's context changes nothing in a
worktree that's already open.

## What an agent gets

Each agent's definition (`agent.yaml`) lists the context it needs:

```yaml
startup: [CLAUDE.md]          # exact files, checked at launch
context:                      # folders and files to bring in
  - .claude/skills
  - path: runbooks
    dest: docs/runbooks       # optional: land it somewhere else
```

An agent with no `context` gets its runtime's default:

| Runtime | Default context |
|---|---|
| Claude Code | `CLAUDE.md`, `.claude/commands`, `.claude/hooks`, `.claude/agents`, `.claude/skills` |
| Codex | `AGENTS.md`, `.agents/skills` |

An agent launched in a team workspace (a team's own workspace, not a code
repo) defaults to `.claude/commands` and `.claude/skills` instead. Its files
are staged into a separate worker folder that isn't a git repo, so there is
no git exclude to write.

An agent can also have a **per-repo override** that adds or removes paths in
one repo only. It's part of the agent's definition, so it's versioned and
shared with it.

Plans don't come along by default. An agent working on a story pulls the
story and the docs around it with `hydrate_story`.

CodeSense doesn't translate between agent formats. If you run both Claude
Code and Codex, keep both `CLAUDE.md` and `AGENTS.md` (or have one point at
the other).

## Where each file comes from

Each file the agent asks for comes from the first of these that has it:

1. Content for that repo only, in `<team>/repos/<host>/<owner>/<repo>/content/`.
2. Your [Private](private-space.md) files.
3. The team's own `content/`.
4. Its parent teams, nearest first.

This resolves per file, so a folder can hold files from several of these at
once. See [Teams and inheritance](teams-and-inheritance.md).

For Ledgerline's `billing` team, an agent with the Claude Code default gets:

| In the context repo | In a worktree of `ledgerline/api` |
|---|---|
| `billing/content/CLAUDE.md` | `CLAUDE.md` |
| `billing/content/.claude/skills/review-pr/SKILL.md` | `.claude/skills/review-pr/SKILL.md` |
| `platform/content/.claude/skills/triage/SKILL.md` (inherited) | `.claude/skills/triage/SKILL.md` |

CodeSense writes only into worktrees it creates and clones it manages. It
never writes into a checkout you attached from your own folder.

## Hidden from git

Every path CodeSense copies into a worktree is listed in the repo-wide
`.git/info/exclude` (shared by all of the repo's worktrees),
inside a block it manages:

```text
# >>> codesense managed workspace …
/CLAUDE.md
.claude/skills/
# <<< codesense managed workspace …
```

Nothing goes into `.gitignore`, so your repo's tracked files don't change
and the context never ends up in a commit or pull request.

## Your edits are kept

A launch or hydrate never overwrites a file the repo tracks, or a copy that
was edited since CodeSense put it there. It leaves those alone and reports
them as kept (modified). To replace an edited copy with the team's version,
have the agent call `hydrate_worktree` with `overwriteModified: true`. Files
the repo tracks are never touched, even then.

## Secrets stay out

Some files should never leave the context repo. **Settings → Excluded
paths** lists glob patterns that stay in `content/` but are never copied into
a workspace: not at a launch, not by a hydrate, and a publish never writes
them back. The policy is shared by the whole team.

- **Credential defaults** are on out of the box: `**/.env`, `**/.env.*`,
  `**/*.pem`, `**/*.key`, `**/id_rsa`, `**/id_ed25519`, `**/.npmrc`,
  `**/.netrc`, `**/credentials` and more. Untick one to allow it.
- **Custom excludes** take your own patterns, such as `**/*.tfstate`. Start a
  pattern with `!` to allow a path again.

## Getting more into a running worktree

An agent that's already running gets newer or extra context only when it
asks:

- `hydrate_worktree`: copy the files or folders it names (`paths` for exact
  files, `sets` for folders such as `.claude/skills` or `docs`). Called with
  neither, it copies the runtime default set: `CLAUDE.md`,
  `.claude/{commands,hooks,agents,skills}`, `AGENTS.md` and `.agents/skills`.
  For anything else, name the paths or sets.
- `hydrate_story`: bring in a story and the planning docs around it.
- `list_hydrated`: list what CodeSense copied into the worktree, and where
  each file came from.

`hydrate_worktree` only adds and refreshes. It removes a copy only when the
call passes `prune: true`, and then only unedited copies under the paths or
sets it names.

The same tools work for a workspace on a remote machine: the agent passes a
remote target (`machineId` and `workspaceHandle`) instead of a local one.

If you want this to happen at set points, for example before every review,
write your own sync skill that calls these tools. The CodeSense docs ship a
short example to copy, `examples/skills/context-sync/`.

## Reverse sync

> **Beta**
>
>
> If you or an agent edit a context file inside a worktree, for example
> improving `.claude/skills/review-pr/SKILL.md` mid-task, an agent can send
> that edit back to the team's `content/`. It previews the change with
> `preview_reverse_sync_worktree`, then publishes it with
> `reverse_sync_worktree`. A new file goes to the team's content by default,
> or to the repo or personal tier the call names. `route_file_to_context` sends
> one new file to a place in the context repo. Nothing is published unless an
> agent makes one of these calls.
>
> Deletions aren't published unless the call asks for them. If the team's copy
> changed in the meantime too, the publish reports a conflict instead of
> overwriting it.
>
> "Back" means back into your context repo on your Mac. Sharing it with
> teammates is a separate step. See [Sharing through git](sharing.md).
>

> **Renames**
>
>   Renaming a context file or folder isn't carried into worktrees that
>   already have the old name. After a rename, have the agent hydrate again and
>   check what's in its worktree.

The full guide ships in the app's docs: **Sync → Hydration, and Reverse sync**.

## Exactly as written

CodeSense doesn’t generate or translate your files. Mirrored files are kept out of your code repo’s git, and once they’re there any tool reads them, with or without CodeSense.

So you can open the same worktree in Cursor, run Claude Code in your own terminal, or use any other tool, and it reads the same files CodeSense put there. Nothing agent-related is committed to the code repo, so your client’s or company’s repo stays clean.
