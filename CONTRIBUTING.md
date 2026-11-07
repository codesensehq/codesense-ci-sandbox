# Contributing to CodeSense

Thanks for your interest. **We don't accept pull requests yet.** Issues and
GitHub Discussions are welcome:

- **Bugs:** open an [issue](https://github.com/codesensehq/codesense/issues/new/choose).
- **Questions and ideas:** start a
  [discussion](https://github.com/codesensehq/codesense/discussions).
- **Security problems:** report them privately, as described in
  [SECURITY.md](SECURITY.md). Don't open a public issue.

This repository is a release snapshot. CodeSense is developed in the
maintainer's private repository, and each release is synced here as one commit.
A pull request opened here can't be merged as it stands, so please don't spend
time on one yet. When that changes, this file will say so.

## Issues

Open a GitHub issue with what you did, what you expected and what happened,
plus your macOS version and the CodeSense version. The bug report form asks for
these. Don't post secrets, tokens or private repository contents. Search the
open issues first.

## When pull requests open

Nothing in this section applies yet. It describes how pull requests will work
once we accept them.

- Keep a change focused on one thing, and say what it does and why.
- If users would notice the change, add a line under `## [Unreleased]` in
  `CHANGELOG.md`. Write it for the people who use CodeSense.
- Sign off every commit (`git commit -s`) to certify the
  [Developer Certificate of Origin](https://developercertificate.org/). See
  [Sign-off](#sign-off-dco) below.
- There is no CI on this repository. Run the [local checks](#local-checks)
  yourself and say in the pull request which ones you ran.

### Sign-off (DCO)

The DCO applies once pull requests open. The sign-off is a line at the end of
each commit message that says you wrote the change, or otherwise have the right
to submit it under this project's licence:

```text
Signed-off-by: Your Name <you@example.com>
```

`git commit -s` adds it, using the `user.name` and `user.email` in your git
config. Use your real name. If you forgot, add it to your last commit with
`git commit --amend -s --no-edit`. To add it to every commit on your branch,
point a remote at this repository once, then rebase onto it:

```bash
git remote add upstream https://github.com/codesensehq/codesense.git
git fetch upstream
git rebase --signoff upstream/main
```

A pull request with unsigned commits won't be accepted.

There is no CLA. Contributions come in under the MIT licence, the same licence
the project goes out under, and the sign-off is all we need. A CLA would only
come if the project were ever relicensed.

### How a pull request will land

Because this repository is a release snapshot, an accepted pull request won't
be merged here directly. The maintainer will apply your change to the private
repository, credit you with a `Co-authored-by:` trailer and keep your sign-off,
and it will appear here with the next release, crediting you by name in that
release's `CHANGELOG.md` entry. The maintainer will then close
the pull request with a note saying which release carries it. A change can take
a while to show up, because releases are not on a fixed schedule.
[MAINTAINERS.md](MAINTAINERS.md) describes the process in full.

## Local checks

First follow the [Build from source](README.md#build-from-source) steps in the
README. They fetch `src-tauri/resources/git-ai`, build the terminal emulator
files, and build the sidecar binaries the app runs, which the checks need. Then
run:

```bash
pnpm lint              # formatting, ESLint and the repository's own checks
pnpm build             # type-checks and builds the frontend
pnpm exec vitest run   # frontend tests
pnpm test:rust         # Rust tests
```

`pnpm exec prettier --write <file>` fixes the formatting of a file you changed.

If a step fails on a clean checkout, please open an issue.

## Who maintains CodeSense

CodeSense has a single maintainer, who reviews every change and makes the final
call on what is merged and released. [MAINTAINERS.md](MAINTAINERS.md) says who
that is and how to reach them.

What you can expect:

- Issues and discussions are triaged weekly, on a best-effort basis. There is
  no promised response time for bugs, features or questions.
- We aim to acknowledge security reports within 3 business days (see
  [SECURITY.md](SECURITY.md)).
- Only the latest release is supported.
- Questions and ideas go to
  [GitHub Discussions](https://github.com/codesensehq/codesense/discussions);
  bugs go to issues.

## AI assistance

AI agents help the maintainer triage issues, review changes and write code,
including changes ported from an outside contribution. The maintainer reviews and is
responsible for what is released.

You're welcome to use AI tools for your own contributions. You remain
responsible for what you submit, and your sign-off certifies the DCO for it as
for any other change.

## Conduct

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
