---
name: add-machine-user
description: Give a teammate SSH access to a remote machine for CodeSense — their own Unix account, or their key on a shared account. Use when the user says "add <name> to the build box", "/add-machine-user", "give my teammate access to <host>" or "authorize this key". Safe to rerun; it never adds a key twice.
---

# Add a teammate to a machine

You run on the user's Mac and change the remote machine over SSH, as plain shell. You need the
teammate's **public** key (a line starting `ssh-ed25519` or `ssh-rsa`, usually from their
`~/.ssh/id_ed25519.pub`). A public key is safe to paste in chat.

Rules:

- Never ask for or accept a private key, a password or a token. If the user pastes something
  that isn't a single public key line, stop and say so.
- Ask before creating an account and before any `sudo` command. Show the exact command first.
- Check before you change. Running this twice must not add a second account or a duplicate
  key.
- Never remove an account or a key here unless the user asks, and confirm first.

## 1. Which machine, who, and how

Ask for:

- the SSH host (as in `~/.ssh/config`, or `user@host`);
- the teammate's name for the account (lowercase, such as `sam`);
- their public key;
- **own account** or **shared account**.

Recommend their **own account**. In CodeSense each person connects with a private account:
their own Unix login, their own files and their own agent logins. A **shared account** means
everyone works as the same Unix user and can see and change each other's files. Use it only
if the team already works that way.

Check the machine answers: `ssh -o BatchMode=yes <host> 'uname -s'`.

## 2a. Own account

Check whether it exists: `ssh <host> 'id -u <name>'`.

If it doesn't, ask, then create it (Linux; this needs `sudo`):

```sh
ssh -t <host> 'sudo useradd -m -s /bin/bash <name>'
```

On macOS use **System Settings → Users & Groups** on the machine, or `sudo sysadminctl -addUser
<name>`. Don't set a password: they sign in with their key.

Then add their key, without duplicating it:

```sh
ssh -t <host> 'k="<public key>"; f=~<name>/.ssh/authorized_keys
  sudo install -d -m 700 -o <name> ~<name>/.ssh
  sudo touch "$f" && sudo chown <name> "$f" && sudo chmod 600 "$f"
  sudo grep -qxF "$k" "$f" || echo "$k" | sudo tee -a "$f" >/dev/null'
```

## 2b. Shared account

The account is the one in `<host>`. Add their key, without duplicating it:

```sh
ssh <host> 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && (grep -qxF "<public key>" ~/.ssh/authorized_keys || echo "<public key>" >> ~/.ssh/authorized_keys)'
```

No `sudo` is needed.

## 3. Check and hand over

Confirm the key is there once:
`ssh <host> 'sudo grep -cxF "<public key>" ~<name>/.ssh/authorized_keys'` (own account) or
the same `grep -c` without `sudo` on the shared account. It should print `1`.

You can't test their login from here: it uses their key. Tell the user to send the teammate:

- the host, port and their user name;
- to add it in CodeSense with **Machines → Add machine…**, or to ask their agent to run
  `/setup-machine`, which ends with the `test_machine_connection` check.

If the team shares machines in CodeSense, the machine can also be saved for the team with
`save_machine` and a `team` share, so teammates see it in their list.

## 4. Summary

Say what you created or added, what was already there, and what the teammate does next.

Teams that already manage accounts with Ansible can keep doing that. This skill is just the
plain-shell version.
