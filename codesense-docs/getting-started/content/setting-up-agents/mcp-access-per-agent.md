# Granting MCP servers per agent

MCP access is **per-agent opt-in**. An agent only reaches an MCP server if its own
`mcp.servers` lists that server's key from the team's `.mcp.json`. The CodeSense
bridge is always on; nothing else is.

## Policy: Codex bridge approvals and tool access

CodeSense always auto-approves calls on its generated Codex bridge alias
(`default_tools_approval_mode = "approve"`), including agents using `on-request`
or `untrusted`. Bridge calls do not prompt. The captured bridge grant plus
`mcp.bridge_tools` are the least-privilege controls. Other servers, including
codegraph, keep their existing approval behavior.

Tool restrictions apply to local Codex and OpenCode launches. A remote saved agent with an
explicit `bridge_tools` list, including `[]`, is refused with an unsupported-policy error,
so a restriction is never silently dropped; `agent_preview` reports this as
`bridge_tools_local_only`. A remote agent with no list (omitted or `null`) has the full
tool set. See [How CodeSense trusts agents](https://github.com/codesensehq/codesense/blob/main/SECURITY.md#how-codesense-trusts-agents).

This leaves Codex's filesystem and shell sandbox limits and command approvals
unchanged. Bridge tools execute in CodeSense outside the CLI shell sandbox:
an allowed mutation tool can still change data, even for a read-only agent.

For a Codex agent that needs identity, inbox replies and completion:

```yaml
runtime: codex
permissions:
  codex:
    approval_policy: never
    sandbox_mode: read-only
mcp:
  bridge_tools: [who_am_i, read_messages, reply, signal_done]
```

The list uses exact, unqualified tool names: 1–128 ASCII letters, digits,
underscores or hyphens, without duplicates, namespace prefixes or wildcards.
Omitted or `null` allows all bridge tools; `[]` allows none. It applies to
runtimes that enforce it (today Codex and OpenCode); other runtimes reject an
explicit list. Unknown names warn and grant nothing.
An explicit list never automatically acquires newly introduced tools.

Use `mcpBridgeTools` on `agent_create`, `agent_update` and `agent_preview`.
An update keeps an omitted list; `null` returns it to unrestricted access.
For example, `agent_preview {name:"Inbox consult", runtime:"codex",
mcpBridgeTools:["who_am_i","read_messages","reply","signal_done"]}` writes
nothing and reports the result in `launch.bridge_tools` (the declared restriction)
and `launch.effective_bridge_tools` (the sorted registered tools it permits).
Warnings name unknown tools and excluded `signal_done`, `read_messages` or
`reply`; CodeSense never adds tools to the declared list for you.

The same effective list is applied to Codex `enabled_tools` on the generated
alias and to the captured relay grant. Excluded tools are absent from
`tools/list`; direct `tools/call` requests are refused by the relay independently
of Codex filtering. A by-name call through `codesense_call_tool` needs both its
own name and the trimmed target name enabled. Preview explains this when the
wrapper is listed. Search and schema tools may describe excluded tools; they
cannot execute them.

You do not need to know or override the generated `codesense_scope_<hash>` alias.
The allowlist belongs in the saved agent definition, rather than in CLI `-c` args.

This matters most for the starters CodeSense ships (`builtin/claude`,
`builtin/codex`, `builtin/opencode` and their unattended twins): they carry the bridge and
**nothing else**. If a starter launches and the agent says "I can't reach
Slack," this is why — the launch never had Slack, even if the team's
`.mcp.json` defines it.

## Where to see what's available

- **Agent editor.** Under the MCP line, every server the team's `.mcp.json`
  offers (the bridge aside) shows as a checkbox. Check the ones this agent
  needs; leave it empty for a bridge-only agent.
- **Starter detail.** A starter is read-only, so the detail shows the offered
  set and a note: *This starter has no MCP servers beyond the bridge. The team
  offers: … . Use template to copy it and grant access.*
- **Launch warnings.** When an agent names no servers but the team offers
  some, a non-blocking warning fires on launch and in the editor: the launch
  still runs, and the warning tells the user why only the bridge is reachable.

## How to grant access

1. Open the agent in the editor (for a starter, click **Use template** first,
   which copies it into an editable agent you own).
2. Under *runs … with … and can use*, check the servers the agent needs.
3. Save. The next launch reaches those servers, in addition to the bridge.

A server the team's `.mcp.json` does not define shows as a manifest error
naming it, so a typo or missing config is loud; the launch still runs, but
reaches the server-less launch config. A silent launch — no servers named on
an agent whose team offers some — is a warning, not an error: the launch
runs, and the warning says exactly which servers a copy could grant.

## Which `.mcp.json` is read at launch

Each launch unions **two** `.mcp.json` sources into one scoped config, with
the one nearer the launch overriding the farther one:

1. **The team's content-layer config** — every `.mcp.json` across the agent's
   content layers (the team, admitted parent layers, Private), deep-merged nearest-wins.
   A parent's config participates only when it shares `content/.mcp.json` in
   `config/sharing.yml`, and every intermediate team shares that path onward.
   This is the same set `agent_preview` lists under *offered servers* and the
   same source the editor's MCP checkboxes read.
2. **The worktree's `.mcp.json`** — the file at the launch cwd, if any. The
   Claude Code / Codex native convention. Deep-merged on top of the content
   layer, so a server defined here replaces an entry of the same key on the
   team.

Both sources contribute: a server defined only in the team's content layer
reaches the agent, and a server defined only in the cwd `.mcp.json` reaches
the agent. The CodeSense bridge is always written in last — a stale
`codesense` entry in either source cannot override it.

**Trust surface.** Any teammate with write access to the team's
content-layer `.mcp.json` can set MCP server definitions for every launch on
every teammate's machine — the same trust surface team-shared prompts
already have. Review content-layer changes with that in mind.

If the team's content-layer config cannot be read (parse error, or over
`effective_mcp_config`'s 1 MB / 4 MB limit), the launch degrades to the
worktree's `.mcp.json` alone and the editor shows a non-blocking warning
naming the reason, so no server silently drops without a trail.

## OpenCode reads `opencode.json`, not `.mcp.json`

An OpenCode agent's servers come from the team's **`opencode.json`**, in OpenCode's own
format: servers sit under the `mcp` key, each as
`{ "type": "local", "command": ["server", "arg"], "enabled": true }`, with environment
variables under `environment`. The `.mcp.json` and its `mcpServers` map are the Claude Code
convention, and an OpenCode launch ignores them. The rules above still apply: the agent's
`mcp.servers` must name a server's key, and the file is read from the same content layers
and the launch directory. CodeSense hands the result to OpenCode inline through
`OPENCODE_CONFIG_CONTENT`, with the bridge added last.

If an OpenCode agent cannot reach a server your `.mcp.json` defines, add it to
`opencode.json` under `mcp`. If it cannot reach CodeSense tools at all, update CodeSense: the
bridge is added for you, and a `tools/list` result that OpenCode's MCP client rejects shows
as "codesense — Failed to get tools".

## Why starters stay bridge-only

Starters are the minimum thing that runs on a user's own CLI login. They don't
decide which third-party services a team uses; the team's own agents (owned by
teammates, pushed into the context repo) do. Copying a starter is the point:
each team shapes its own agents on top of what the starters give.

## Related

- [Add the CodeSense MCP to your own agents](add-codesense-mcp.md) — connect
  an outside-of-CodeSense agent to the bridge.
- [MCP-only agents](../arc-e-agents/mcp-only-agents.html) — the opposite
  recipe: lock an agent to the bridge for a scoped, auditable role.
- [agent.yaml, field by field](../arc-e-agents/agent-yaml.html) — the
  reference for `mcp.servers` and every other field.

## Hermes (preview) reads `HERMES_HOME/config.yaml`

Hermes uses `mcp_servers` in `config.yaml`. A managed CodeSense launch writes the
selected servers and its bridge into a private per-session `HERMES_HOME/config.yaml`.
Declare external servers in the team's `config.yaml` and select their keys with
`mcp.servers`. Your personal Hermes home and MCP configuration are left untouched.
The bridge receives the session identity and socket explicitly, including when
Hermes filters the environment of its child servers.

Hermes support is a preview: saved agents and launches refuse `runtime: hermes`,
and its starters are not offered, so the configuration and UI described here stay
inactive. Automatic inbox delivery and activity, usage, the headless runner,
accounts and remote launch are not part of the preview. A hand-typed `hermes` receives tab identity only; it stays unmanaged
and CodeSense does not change its home, MCP config or install an observer/waiter.

Install the pinned [Hermes Agent v0.22.5 source release](https://github.com/NousResearch/hermes-agent/tree/v0.22.5)
(commit `bf0203a794a07aac3ff764972ab9087baa4e4f4e`) using its upstream instructions.
CodeSense does not install Hermes.

For a Hermes agent, a keyless local model uses **Edit → Runs on**, or these non-secret
native settings on the agent (`startupSettings` in the editor/MCP inputs):

```yaml
startup_settings:
  model:
    provider: custom
    base_url: http://127.0.0.1:11434/v1
    default: qwen2.5:3b-instruct
```

The local server must already serve that model. CodeSense does not install or call
it. The saved definition and generated settings are hashed with the launch
manifest. These inline selectors override a selected startup `config.yaml`;
explicit native `--model` args take precedence. No API key is projected or inherited.
A Hermes starter without a resolvable local model endpoint is blocked in preview
and at every managed launch, with guidance to Edit → Runs on.

Managed Hermes permission settings use:

```yaml
permissions:
  hermes:
    approval: ask
    toolsets: [terminal, file]
    deny: ["^write_file$", "^mcp__other_server__delete_.*"]
```

`approval` accepts `ask` (native `approvals.mode: manual`) or `yolo` (native
`off`, marked as escalating). The unattended starter passes `--yolo`. Explicit
`deny` expressions match native tool names, including the inner name of a deferred
MCP call, and remain active under yolo. The native matcher invokes CodeSense’s packaged
`--hook-block hermes` helper before dispatch. `mcp.bridge_tools` is enforced both by
`tools.include` and a native block hook. An empty bridge list excludes every bridge
tool. Changing bridge grants does not alter the agent's explicit deny expressions.

Toolsets default to `terminal` and `file`. Only pinned built-in leaf sets are
accepted; memory, skills, cron, composite and unknown plugin sets are refused.
Memory and user-profile writes, skill creation nudges and shared telemetry are off;
no messaging platforms or cron jobs are configured. An unconditional native tool block
also excludes `memory`, `skill_manage` and `cronjob_manage`, including under `--yolo`
and `-t all` / `--toolsets` overrides. Native toolset arguments remain unchanged; a preview
note identifies a native override, calling composite `all` / `*` selections widened.
Overview uses the final native selection (including accepted option abbreviations),
counts distinct finite selections, and names the permanent blocked tools for overrides. The terminal backend is local.
Headless settings retain ask and deny the approval-gated `terminal`, `write_file`,
`patch` and `computer_use` tools so unattended runs cannot park; an explicitly
escalating launch omits those default denies and retains its saved denies.

Hermes host egress is advisory: it may contact api.github.com,
raw.githubusercontent.com, hermes-agent.nousresearch.com, openrouter.ai and
portal.nousresearch.com. The launch preview warns; CodeSense does not enforce an
egress policy. Overview describes the generated settings for the selected launch mode.

Managed Hermes sessions refuse native safe-mode, ignore-user-config, ignore-rules
and profile arguments, including their accepted native abbreviations. These options
can bypass the managed hooks or home. `--accept-hooks` remains allowed. Inherited
hook-bypass and external TUI gateway variables are stripped. If this Mac has a
policy at `/etc/hermes`, or a nonempty ambient `HERMES_MANAGED_DIR`, preview and
launch explain that admin-managed Hermes policy is not supported. Otherwise
CodeSense pins an empty private managed-policy directory for the session.
