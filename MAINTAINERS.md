# Maintainers

CodeSense has a single maintainer:

- Trevor Alfstad

The maintainer reviews every change and makes the final call on what is merged
and released. If other people become regular contributors, the project may add
maintainers, and this file will say how.

## Contacts

- **Bugs:** [GitHub issues](https://github.com/codesensehq/codesense/issues).
- **Questions and ideas:**
  [GitHub Discussions](https://github.com/codesensehq/codesense/discussions).
- **Security problems:** [security@codesense.ai](mailto:security@codesense.ai),
  or private vulnerability reporting, as described in [SECURITY.md](SECURITY.md).
- **Conduct problems:** [conduct@codesense.ai](mailto:conduct@codesense.ai), as
  described in the [Code of Conduct](CODE_OF_CONDUCT.md).

## How an outside change gets in

This repository is a release snapshot: CodeSense is developed in the
maintainer's private repository, and each release is synced here as one commit.
We don't accept pull requests yet (see [CONTRIBUTING.md](CONTRIBUTING.md)).
Once we do, an accepted change gets in like this:

1. The maintainer accepts the change in its pull request here.
2. The maintainer applies it as a patch in the private repository, keeping your
   `Signed-off-by:` line and adding a trailer that credits you:

   ```text
   Co-authored-by: Your Name <you@example.com>
   ```

3. The patch goes through a pull request and review in the private repository,
   like every other change.
4. It ships here in the next release sync. This repository gets one commit per
   release, so you're credited by name in that release's `CHANGELOG.md` entry.
   The `Co-authored-by:` trailer stays on the commit in the private history.
5. The maintainer closes your pull request with a note saying which release
   carries it.

Releases are not on a fixed schedule, so a change can take a while to appear.
