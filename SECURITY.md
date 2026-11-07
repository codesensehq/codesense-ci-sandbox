# Security policy

## Supported versions

Only the latest release is supported. Fixes ship in a new release.

## Reporting a vulnerability

Please don't open a public issue. Report it privately, in either of these ways:

- **GitHub private vulnerability reporting**: open the **Security** tab of this
  repository and choose **Report a vulnerability**.
- **Email** [security@codesense.ai](mailto:security@codesense.ai). Use this if
  the Security tab doesn't offer to report a vulnerability.

Include what you found, the CodeSense version and macOS version, the steps to
reproduce it, and the impact you expect. A proof of concept helps but is not
required.

## What to expect

- We aim to acknowledge reports within 3 business days.
- We aim to have a fix, or a plan for one, within 30 days.
- We prepare fixes privately and aim to release them before the issue is made
  public. We then publish an advisory.
- We aim to keep you updated while we work on it.
- Credit in the advisory, if you want it.

## How CodeSense trusts agents

CodeSense launches and coordinates the agents you choose. It doesn't sandbox
them or police them.

- **Agents run as you.** An agent CodeSense starts runs under your Mac user
  account, with your files, your keys and your network, like any program you
  run yourself. So does anything else running under your account.
- **You choose their limits.** An agent can do what its CLI's permissions or
  sandbox allow, and you set those in the agent's settings. CodeSense's own
  controls, such as spend caps and per-agent MCP tool lists (where the runtime
  supports them), apply to what agents do through CodeSense. They don't limit
  what an agent with unrestricted shell access can do on your Mac. Give an
  agent you don't fully trust sandboxed or restricted permissions.
- **Dashboards are your files.** Dashboards are files in your own context
  repos, and they run with the app's page APIs, so open only dashboards you
  trust.

A way to get past these limits through CodeSense itself, such as an agent
raising its own spend cap over MCP or a page writing outside the content
CodeSense manages, is a vulnerability. Report it as described above.

## Scope

In scope:

- the CodeSense desktop app;
- its update download host, `dl.codesense.ai`;
- the codesense.ai website, including the download page;
- the git-ai binary bundled with the app.

Dashboard sharing at `share.codesense.ai` runs as a separate service and is not
in this repository. Report problems with it the same way.
