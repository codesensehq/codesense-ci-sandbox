---
name: agent
description: Set up and manage the user's CodeSense agents — named, reusable workers (a runtime, its args, MCP servers, the content it starts with, and its permissions) the user and their team launch or run from routines. Use when the user says "make an agent that…", "set up a triage agent", "what agents do we have", "let it use Slack but not edit files", or asks to change, copy, archive or restore one. Advisory — the agent_* tools work without it.
---

# Agents

An agent is a named worker defined once and reused: which CLI runs it, the
extra args it gets, the MCP servers it can reach, the content paths it starts
with, its permissions, and an optional default prompt. It lives as
`config/agents/<id>/agent.yaml` in the context repo, revisioned on every save.
An Only me agent is private to the user; a team agent is one every teammate
can use and edit. The `agent_*` tools do all the work; this is how to use them
well.

One call makes a working agent:

```json
agent_create { "name": "Reviewer", "runtime": "claude" }
```

Everything else is optional and has a sensible default. `sessionId` is
optional on every tool: pass your `$CODESENSE_TAB_SESSION_ID` when CodeSense
launched you, so your revisions are credited to you (and `runtime` may then be
left out to mean your own CLI, when CodeSense recorded which); if you have
none, leave it out and always pass `runtime`.

```yaml
id: slack-triage
name: Slack triage
description: Sweeps #support and files actionable issues.
runtime: claude
launch: headless
mcp:
  servers: [slack]
startup:
  - CLAUDE.md
  - .claude/skills/triage/
permissions:
  claude:
    allow: ["Read", "Grep", "Glob", "mcp__slack__*"]
    deny: ["Edit", "Write", "Bash"]
default_prompt: /triage
```

## The starters: Claude, Codex and OpenCode

CodeSense ships three agents everyone has: `builtin/claude` ("Claude"),
`builtin/codex` ("Codex") and `builtin/opencode` ("OpenCode"). OpenCode runs
on this Mac only. Each runs its CLI with its defaults and the user's
own login: no extra args, no MCP server beyond the bridge, the CLI's own
permission config and no default prompt, so the user types the task. Each
starts with what CodeSense needs plus the team's root `CLAUDE.md` (Claude) or
`AGENTS.md` (Codex and OpenCode) when the team has one.

They are read-only: `agent_update`, `agent_delete`, `agent_restore` and
`agent_unarchive` reply `read_only`. To customize one, copy it (below) and
change the copy. A routine (Only me or team) may name either one.

## Compare existing agents first

Before creating one, call `agent_list` and read the names, descriptions and
`startup_count`s. If one already fits, use it. If one nearly fits, update it
with `agent_update` (a team agent is any teammate's to change). Don't make a
near-duplicate.

`scope: "mine"` on `agent_list` only narrows the view to your Only me agents
and the team agents you created. It is not a permission: every agent the
default `all` lists is yours to use and change.

### Copying an agent

There is no copy tool; get it, then create it. If the user wants their own
variant ("copy it to mine"), a starter included:

1. `agent_get {id}` and take its `definition`.
2. `agent_create` with that definition, minus `created_by`, `revision` and
   `archived_at` (CodeSense sets those), with `mcp: {servers: [..]}` passed as
   `mcpServers: [..]`, your change applied, and `scope: "personal"` with the
   `org` it goes in (the default scope is `team`). Keep or change `id`; it only
   has to be free where it goes.

A key the tool doesn't take is refused by name, with what to send instead,
so nothing you meant to copy is silently dropped.

## The fields

- `runtime`: `claude`, `codex` or `opencode`. These are the built-in CLIs. Pass it. It may
  be left out only when you pass `sessionId` and CodeSense knows which CLI
  launched you; otherwise leaving it out is an `invalid` reply naming
  `runtime`, never a guess.
- `args`: extra CLI argv words, one string each (`["--model", "opus"]`),
  never a command line.
- `launch`: `foreground` (a visible tab, the default) or `headless`.
- `mcpServers`: which servers from the team's MCP config (`.mcp.json`, or
  `.codex/config.toml`) the agent gets. The CodeSense bridge is always there,
  so never list it. Default: none beyond the bridge.
- `startup`: the content the agent starts with (below).
- `permissions`: the runtime's own permission config (below).
- `defaultPrompt`: what a launch starts with when the launcher gives nothing.

## Startup: plain content paths

`startup:` is an ordered list of content-relative paths. Each is a file, or a
directory that brings everything under it. There are no `skill:` or
`command:` keys: a skill is its directory, a command is its file.

```yaml
startup:
  [
    CLAUDE.md,
    .claude/skills/bmad-build/,
    .claude/commands/bmad-help.md,
    finish-codesense/02-routines/README.md,
  ]
```

The path rules (a save refuses anything else, naming the path):

- relative to the team's content root: no absolute paths;
- no `..`;
- no globs (`*`, `?`, `[`, `]`);
- not inside a reserved top-level dir (such as `.git`);
- no duplicates, and no entry inside another entry.

A file can name what it needs with optional `requires:` frontmatter. Its
values are content paths too, and they come along whenever the file does:

```yaml
---
name: triage
requires: [.claude/commands/bmad-help.md]
---
```

Default `startup` is empty: the agent gets only what CodeSense itself needs.

A path no content layer has is a manifest error, with one exception: the
runtime's root instructions file (`CLAUDE.md` for claude, `AGENTS.md` for
codex and opencode). Where the team has none, the agent simply starts without it, as the
starters do.

### Slash commands must be in startup

A prompt or a routine task is passed verbatim. If it starts with `/name`, the
agent needs `.claude/commands/name.md` or `.claude/skills/name/` among its
startup paths (codex: `.agents/skills/name/`). Otherwise the run won't find
the command. `agent_get` and `agent_preview` list what the agent `provides`,
and warn when the default prompt names something it doesn't start with.

### When startup takes effect

Named local product launches apply the resolved startup manifest before spawning.
Story, remote, and other launch paths retain their existing context behavior.
`manifest_applies_at_launch` remains false because this capability is not universal.
Existing files remain available; startup selections do not prune old context.
Permissions apply per launch on all these paths.

## MCP servers and permissions

`mcpServers` picks which servers exist for the agent. **Permissions** say what
it may do with them and with its built-in tools. They are the runtime's own
native config, enforced by the CLI and never by CodeSense, and passed through
verbatim. They apply per launch: Claude gets them through its own `--settings`
file, Codex through its own scoped home, so they never leak to another agent
launched in the same folder. CodeSense only adds the allows its own tools
need (`signal_done` and friends); a rule that blocks those is kept as written
and only warned about.

Claude takes `{claude: {allow, ask, deny, additionalDirectories, defaultMode,
disableBypassPermissionsMode}}` with Claude's own rule strings. Codex takes
`{codex: {approvalPolicy, sandboxMode}}`. OpenCode takes `{opencode:
{permission}}`, OpenCode's own permission map. A key the agent's runtime doesn't
have is refused, naming `permissions.<runtime>.<key>`.

A triage agent that is read-only plus Slack:

```json
{
  "name": "Slack triage",
  "runtime": "claude",
  "mcpServers": ["slack"],
  "permissions": {
    "claude": {
      "allow": ["Read", "Grep", "Glob", "mcp__slack__*"],
      "deny": ["Edit", "Write", "Bash"]
    }
  }
}
```

A dev agent that may build and test but not force-push:

```json
{
  "name": "Dev",
  "runtime": "claude",
  "permissions": {
    "claude": {
      "allow": ["Bash(npm run build:*)", "Bash(npm test:*)", "Edit"],
      "deny": ["Bash(git push --force:*)", "Bash(git push -f:*)"]
    }
  }
}
```

The codex equivalent of the dev agent:

```json
{
  "name": "Dev (codex)",
  "runtime": "codex",
  "permissions": { "codex": { "approvalPolicy": "on-request", "sandboxMode": "workspace-write" } }
}
```

With no permissions the CLI's own defaults apply. A headless Codex agent with
no policy gets `approval_policy = "never"` and `sandbox_mode =
"workspace-write"` when unset, so it can't hang on an approval nobody sees.
The bypass flag is never added for you: it applies only if the agent's own
`args` carry it.

Routines can **wake** an agent: type their task into its running session
instead of starting a new one. That needs `launch: foreground` (a headless run
is never woken), and an agent that auto-approves every tool (a bypass flag in
`args`, or `bypassPermissions` mode) is woken only if it also sets
`allowWake: true`. Without it, its
routines start a new session each time.

An agent can **resume after a terminal server update**: with
`resumeAfterRestart: true`, a session the update ended is relaunched as the
same agent, in its workspace and tab, with a resume brief CodeSense builds
from its own records (its last `signal_progress` and `signal_done`, its inbox,
its routine run, the git state and the terminal's last lines; secrets
redacted, at most 8 KB). It never relies on a CLI session id, so it works the
same for Claude, Codex and any other runtime. Off by default. Without it, the
tab reopens as a shell — as it does when the relaunch is refused (its
workspace or launch folder is gone, or a This Mac launch ran outside a
CodeSense-managed folder). A relaunch runs the agent as saved: a launch's own
mode, account or extra arguments are not carried over.

## No limits, only warnings

CodeSense never caps an agent. A long startup list, a huge directory, many
MCP servers or broad permissions are all saved as given. What looks off comes
back in `warnings` (and in `manifest.totals` / `manifest.errors`): an unusually
large entry or manifest, build output or binaries in a startup directory, a
`/name` the agent doesn't start with, or permissions that block CodeSense's
own tools. Pass them on to the user; they decide.

## The account is per user

Which Claude or Codex login runs the agent is not part of the definition.
Each user runs it with their own CLI login (the account they bound it to on
their own Mac, else their active account for the runtime). There is no
account field and no tool input for one: never put an account in
`agent.yaml`. `resolved.account` shows which login the current user would
launch it with; binding a different one is each user's own choice on their
Mac.

## "Make an agent that…"

1. `agent_list`: does one already fit? (See "Compare existing agents first".)
2. `agent_preview` with the fields you will create it with (it takes exactly
   `agent_create`'s fields). Check `manifest.errors`, `warnings`, and that
   `provides` has every `/name` its prompts use. Fix and preview again.
3. `agent_create` with the same fields. It goes to the team by default; pass
   `scope: "personal"` and `org` only when the user wants it for themselves
   (Only me). `dryRun: true` returns the
   same reply and writes nothing.
4. Tell the user its `id`, what it starts with, and any warning.

A validation error is `{ok: false, error: {code: "invalid", message, field}}`
and writes nothing; fix the named `field` and call again.

## Changing, archiving and restoring

- `agent_update` takes the `revision` you last read as `baseRevision`, plus
  only the fields that change. An explicit `[]` clears a list. A
  `permissions` runtime block you send replaces that block whole; `null`
  removes them all.
- A `conflict` reply (`current_revision`) means it changed since you read it.
  Nothing was written: `agent_get` it again and reapply the change.
- `agent_delete` archives it: it drops out of lists and pickers, and its own
  routines (the ones under `config/agents/<id>/routines/`) are archived with it.
  With active routines it is refused `needs_confirm`, listing them: confirm
  with the user, then pass `archiveRoutines: true`. Routines that only wait on
  it (`agent_done from_agent`) keep resolving. `agent_unarchive` brings it
  back and offers its routines. `hard: true` removes it for good, refused
  (`in_use`) while it has active routines or routines wait on it.
- `used_by` and `in_use` list only the routines you can see. A teammate's
  Only me routine can run a team agent and is invisible to you, so a hard
  delete of a team agent may still strand hidden routines: their runs then
  fail `agent_unavailable` until their owner names another agent. Archive a
  team agent instead of hard-deleting it unless the user is sure.
- `agent_restore` makes an old revision current again; `agent_unarchive`
  brings an archived agent back.

Any agent you can see is yours to change, a teammate's team agent included.
Each revision is credited to whoever made it.

## Edits are local until pushed

Every create, update, archive or restore is a local edit to the context repo
on this Mac. Teammates get it when the user's agent pushes it with git, which
is yours to do when the user wants it. CodeSense never commits or pushes it
for you, and creating or changing an agent tells no one.
