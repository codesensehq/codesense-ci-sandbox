# Override a parent team's skill

_Give your team its own version of a skill a parent team ships._

Ledgerline's `platform` team ships a `review-pr` skill and shares
`content/.claude/skills/**` in its `config/sharing.yml`, so every product team inherits it. The `billing` team wants
the same review plus money and idempotency checks, so it gives itself its
own copy. Every skill is plain git content, so you maintain that copy.

## Give your team its own copy

Inheritance works per file: for each path, the nearest team that has the
file supplies it. So `billing` replaces just `review-pr/SKILL.md`, and keeps
every other skill `platform` shares.

Editing `platform`'s `review-pr` from `billing`'s file tree edits
`platform`'s file, for every team below it. To change it for `billing` only,
choose **Copy into billing** on the inherited file, or create the file
yourself:

1. In `billing/content/.claude/skills/`, create `review-pr/SKILL.md` with
   your version:

   ```md
   ---
   name: review-pr
   description: Billing PR review — adds money and idempotency checks on top of the platform review.
   ---
   Check the diff for correctness and missing tests, and that every new
   handler uses AppError and the error envelope. Then check: amounts are
   bigint cents, every POST/PATCH honours Idempotency-Key, and refunds never
   exceed the captured amount.
   ```

   Write out the parent's steps you still want. The parent's `SKILL.md`
   isn't in the worktree for your version to refer to.

2. Leave `platform`'s other skills alone. If `platform` also has
   `.claude/skills/triage/`, `billing` still inherits it.

3. Launch an agent for a `billing` story (or have a running one call
   `hydrate_worktree`) and check its worktree:

   ```sh
   head -4 .claude/skills/review-pr/SKILL.md
   ls .claude/skills
   ```

Your team's copy wins because it is nearer in the chain. See
[Teams and inheritance](../concepts/teams-and-inheritance.md).

> **Your copy doesn't follow the parent**
>
>   When `platform` changes its `review-pr` later, `billing`'s copy doesn't
>   change and nothing flags it. Review the parent's history now and then.

The full guide ships in the app's docs: **The model → Content layers**.
