# Setting up agents

CodeSense has no agent library and nothing to install. An agent is a small file in your
context repo: `config/agents/<id>/agent.yaml`, plus the skills it starts with. These pages are
recipes for useful agents. Copy them into your own context repo, or let an agent do it.

Set an agent up once in your **team's** context repo. Your teammates get it when you push, and
they just use it. Make it Private (Only me) only when it's just for you.

## Start here

- [Add the CodeSense MCP to your own agents](add-codesense-mcp.md): connect Claude Code, Codex
  or another MCP client you run yourself outside CodeSense. Agents launched from CodeSense
  already have it, and so does a `claude` or `codex` you type in any CodeSense tab.
- [Granting MCP servers per agent](mcp-access-per-agent.md): MCP access is per-agent opt-in.
  Starters carry only the bridge; copy a starter and check the servers your agent needs.

## Recipes

- [CodeSense setup agent](codesense-setup-agent.md): walks you through your workspace, teams,
  source repos, agents and routines.
- [Machine setup agent](machine-setup-agent.md): gets a remote machine ready for CodeSense and
  adds teammates to it.
- [Build a manager agent](manager-agent.md): an agent that holds a `manager` role queue,
  triages incoming work, launches and tracks workers, and reports to you. CodeSense has no
  built-in controller; this shows how to build one from generic tools.

## Two ways to follow a recipe

**By hand.** Each recipe lists the files to copy and where they go. Copy them into your team,
then push with git.

**Ask an agent.** These docs are plain files on your Mac, in
`~/.codesense/users/<your profile>/codesense-docs/`, so any agent can read them. Say:

> Set up the CodeSense setup agent from the CodeSense docs, in my team.

The agent reads the recipe, copies the skill files into your team's context repo, and creates
the agent with the `agent_preview` and `agent_create` MCP tools. It asks before it changes
anything. Pushing the change with git is up to you or your agent: CodeSense never commits or
pushes.

## What the agents get

- The CodeSense MCP tools, always. You never list them.
- Your team's own `CLAUDE.md` (Claude) or `AGENTS.md` (Codex). The recipes list it first in
  `startup`, so the agent follows your team's rules. If the team has none, the agent starts
  without it.
- Each user's own CLI login. An agent file never holds an account, a token or a key.

The [`agent` example skill](../examples/skills/agent/SKILL.md) explains every field of
`agent.yaml`.
