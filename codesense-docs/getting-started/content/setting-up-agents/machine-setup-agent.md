# Recipe: Machine setup agent

An agent that gets a remote machine ready for CodeSense and adds your teammates to it. It runs
on your Mac and works on the machine over SSH with plain shell commands. It checks before it
changes anything, so it's safe to run again.

It has two skills:

- **`/setup-machine`** checks for and installs git, the language runtimes your repos need,
  the agent CLIs you use (Claude Code, Codex) and a work folder. It finishes by testing the
  machine from CodeSense with the `test_machine_connection` MCP tool.
- **`/add-machine-user`** gives a teammate access: their own Unix account with their public
  key, or their key on a shared account.

Both ask before any `sudo` command and before creating an account. Neither ever asks for a
password, a token or a private key.

## The files

| Copy this                                                                                   | To                                                        |
| ------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| [`examples/skills/setup-machine/SKILL.md`](../examples/skills/setup-machine/SKILL.md)       | `<team>/content/.claude/skills/setup-machine/SKILL.md`    |
| [`examples/skills/add-machine-user/SKILL.md`](../examples/skills/add-machine-user/SKILL.md) | `<team>/content/.claude/skills/add-machine-user/SKILL.md` |
| [`examples/agents/machine-setup/agent.yaml`](../examples/agents/machine-setup/agent.yaml)   | `<team>/config/agents/machine-setup/agent.yaml`           |

The agent file:

```yaml
id: machine-setup
name: Machine setup
description: Gets a remote machine ready for CodeSense and adds teammates to it.
runtime: claude
launch: foreground
startup:
  - CLAUDE.md
  - .claude/skills/setup-machine/
  - .claude/skills/add-machine-user/
default_prompt: /setup-machine
revision: 1
```

`launch: foreground` matters: the agent asks you questions as it goes, so it needs a visible
tab. For Codex, set `runtime: codex` and put the skills in `.agents/skills/` instead.

## Set it up

By hand: copy the three files into your team, then push them with git.

Or ask an agent:

> Set up the machine setup agent from the CodeSense docs, in my team.

It follows the same steps as the [CodeSense setup recipe](codesense-setup-agent.md#or-ask-an-agent):
copy the skills, then `agent_preview` and `agent_create` with the fields above.

## Use it

- Launch **Machine setup** and name the machine. It runs `/setup-machine`.
- To add a teammate, type `/add-machine-user` and paste their public key (the `.pub` file).

## Already use Ansible?

Keep it. These skills are the plain-shell version for teams without one. You can also ask the
agent to turn what it did into a playbook.
