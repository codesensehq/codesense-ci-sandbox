---
name: setup-machine
description: Get a remote machine ready for CodeSense over SSH — git, language runtimes, agent CLIs and a work folder — then test it from CodeSense. Use when the user says "set up my machine", "/setup-machine", "get this server ready for CodeSense" or "what's missing on <host>". Safe to rerun; it only installs what is missing.
---

# Set up a machine

You run on the user's Mac and set up a remote machine over SSH. You check first, install only
what's missing, and ask before anything that needs `sudo` or creates an account.

Rules:

- Every step is plain shell run as `ssh <host> '<command>'`. Check before you change:
  `command -v git` before installing git, `test -d ~/work` before `mkdir -p ~/work`. Running
  this skill twice must change nothing the second time.
- Ask before any command that uses `sudo`, and show the exact command first. If the user says
  no, print the command for them to run themselves and carry on.
- Never create, change or delete a Unix account here. That's the `add-machine-user` skill.
- Never ask for a password, token or key in chat, and never accept one. If `sudo` needs a
  password, ask the user to run that command in their own terminal.
- Keep it short: a line or two per step.

## 1. Connect

Ask which machine: the SSH host (an alias from `~/.ssh/config` or `user@host`) and the port
if it isn't 22. Check it answers:

```sh
ssh -o BatchMode=yes <host> 'uname -sm; . /etc/os-release 2>/dev/null; echo "$ID $VERSION_ID"'
```

If this fails, stop and help the user fix SSH first (their key, the host name, the port). Use
the OS and package manager you found (`apt-get`, `dnf`, `brew`) for every install below.

## 2. Check what's there

Run one check and show the user a short list of what's present and what's missing:

```sh
ssh <host> 'for c in git curl node npm python3 claude codex; do printf "%s: " "$c"; command -v "$c" || echo missing; done; test -d ~/work && echo "work: ~/work" || echo "work: missing"; printf "codex sandbox userns: "; cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo n/a'
```

Ask which runtimes their repos need (Node, Python, Go, Rust…) and which agent CLIs they use
(Claude Code, Codex, or both). Only install those.

## 3. Install what's missing

One item at a time, each only if its check said missing:

- **git and curl.** System packages, for example
  `sudo apt-get update && sudo apt-get install -y git curl`. Needs `sudo`: ask first.
- **Runtimes.** Prefer a per-user install that needs no `sudo` when the team uses one (a
  version manager such as `nvm`, `pyenv` or `rustup`). Otherwise the system package, after
  asking.
- **Agent CLIs.** Use each CLI's official install instructions. With Node present:
  `npm install -g @anthropic-ai/claude-code` and `npm install -g @openai/codex`. If a global
  npm install would need `sudo`, ask first or set a user prefix.
- **Codex's command sandbox (Linux, when Codex is used).** Codex runs each command in a
  sandbox built on user namespaces (bubblewrap). Ubuntu 23.10 and newer block those by
  default, and Codex then can't run any command ("Operation not permitted"). Check with
  `cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns`. If it prints `1`, allow
  them, and keep it across reboots, with
  `echo 'kernel.apparmor_restrict_unprivileged_userns = 0' | sudo tee /etc/sysctl.d/60-codex-sandbox-userns.conf && sudo sysctl --system`.
  This needs `sudo`, so ask first. Then check with `cd /tmp && codex sandbox sh -c 'echo ok'`.
- **Work folder.** `mkdir -p ~/work`, or the folder the user names.

Each user signs in to their agent CLI themselves, on the machine. Never copy a login or an
API key onto it.

After each install, run its check again and report the version.

## 4. Test it from CodeSense

Call the `test_machine_connection` MCP tool with the machine:

```json
{
  "input": {
    "label": "<a short name>",
    "host": "<host or IP>",
    "port": 22,
    "username": "<user>",
    "signIn": { "method": "agent" }
  }
}
```

Pass `machineId` too when the machine is already in CodeSense (`list_machines` shows it).

The first test returns the machine's host key fingerprint. Show it to the user and ask them to
confirm it matches the machine. Only then call it again with that value as
`trustedHostKeyFp`. Report each step of the result in plain words, and the install plan if it
returns one.

If the machine isn't in CodeSense yet, tell the user to add it with **Machines → Add machine** in CodeSense, or
offer to save it with `save_machine` after they say yes.

## 5. Summary

List what was already there, what you installed, anything the user still has to run
themselves, and the test result.

Teams that already manage machines with Ansible can keep doing that. This skill is just the
plain-shell version of the same checks.
