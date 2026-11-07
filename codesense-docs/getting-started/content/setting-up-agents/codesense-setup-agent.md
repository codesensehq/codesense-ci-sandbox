# Recipe: CodeSense setup agent

An agent that sets CodeSense up with you: your workspace, teams, source repos, agents and
routines. It checks what's done, shows what's left, and asks before every change. Safe to run
again at any time.

## What it uses

The `/codesense-setup` skill and these CodeSense MCP tools:

| Step         | Tools                                                                                                    |
| ------------ | -------------------------------------------------------------------------------------------------------- |
| Check        | `describe_setup`                                                                                         |
| Workspace    | `create_local_organization`, `attach_context_repository`, `list_context_attachments`, `rename_workspace` |
| Teams        | `create_team`, `set_active_team`, `rename_team`, `set_team_parent`, `archive_team`, `delete_team`        |
| Source repos | `add_source_repo`, `attach_source_repo_path`, `list_source_repos`, `remove_source_repo`                  |
| Agents       | `agent_list`, `agent_get`, `agent_preview`, `agent_create`, `agent_update`                               |
| Routines     | `routine_create`, `routine_list`                                                                         |

The workspace tools work only for an agent on this Mac, not on a remote machine.

## The files

Two files go into your team in your context repo:

| Copy this                                                                                     | To                                                       |
| --------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| [`examples/skills/codesense-setup/SKILL.md`](../examples/skills/codesense-setup/SKILL.md)     | `<team>/content/.claude/skills/codesense-setup/SKILL.md` |
| [`examples/agents/codesense-setup/agent.yaml`](../examples/agents/codesense-setup/agent.yaml) | `<team>/config/agents/codesense-setup/agent.yaml`        |

`<team>` is your team's folder in the context repo, next to its `content/` and `config/`
folders.

The agent file:

```yaml
id: codesense-setup
name: CodeSense setup
description: Sets up CodeSense with you. Workspace, teams, source repos, agents and routines.
runtime: claude
launch: foreground
startup:
  - CLAUDE.md
  - .claude/skills/codesense-setup/
default_prompt: /codesense-setup
revision: 1
```

For Codex, set `runtime: codex` and put the skill in `.agents/skills/codesense-setup/`
instead, with that path in `startup`.

## Set it up by hand

1. Copy the skill file into your team.
2. Copy the agent file into your team.
3. Open your team in CodeSense. **CodeSense setup** shows in the agent list.
4. Push both files with git so your teammates get it.

## Or ask an agent

Launch any agent from CodeSense (or [your own, with the MCP added](add-codesense-mcp.md)) and
say:

> Set up the CodeSense setup agent from the CodeSense docs, in my team.

The agent should:

1. Read this page from `~/.codesense/users/<your profile>/codesense-docs/`.
2. Call `describe_setup` for your team, and `list_context_attachments` for where its context
   repo is on disk.
3. Copy the skill file into `<team>/content/.claude/skills/codesense-setup/`.
4. Call `agent_list`. If `codesense-setup` already exists, stop and say so.
5. Call `agent_preview` with the fields above, then `agent_create` with the same fields:
   `name`, `id`, `description`, `runtime`, `launch`, `startup` and `defaultPrompt`, plus the
   `org` and `team`. Don't pass `revision`: CodeSense sets it.
6. Tell you what it created and offer to push it with git.

## Use it

Pick **CodeSense setup** when you launch an agent, or type `/codesense-setup` in any agent that
has the skill. It starts by showing a checklist of what's done and what's left.
