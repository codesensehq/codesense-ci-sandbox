# Plan an epic across repos

_Split one epic into stories that each name the repo their agent works in._

Ledgerline's Checkout v2 epic spans two repos. The cart and refund UI live in `web`, and the money model, tax lines and refund endpoint live in `api`. This guide plans that epic so every story lands in the right repo, while the left nav still shows one plan.

## Before you start

Both repos are on your team's list, and you've attached a local clone of each in the **Source repos** tab (**Go → Source Repos**, ⌥⌘R). See [Multi-repo](../concepts/multi-repo.md).

## 1. Create the epic and list its repos

Make a folder with a main doc, and list the repos the epic spans:

```yaml title="checkout-v2/README.md"
---
type: epic
title: Checkout v2
status: in-progress
repos: [ledgerline/web, ledgerline/api]
---
# Checkout v2

## Purpose
Rebuild checkout on one money model so the cart, the invoice, and refunds agree
to the cent across `web` and `api`.
```

## 2. Write one story per repo

A story names exactly one `source_repo`. Split each piece of work at the repo boundary. A story whose repo isn't in the epic's `repos` list still works, but `validate_story` flags it (`SOURCE_REPO_NOT_IN_EPIC`).

| Story | Repo | Depends on |
|---|---|---|
| LL-201 Money type in bigint cents | `ledgerline/api` | none |
| LL-204 Cart API returns tax lines | `ledgerline/api` | LL-201 |
| LL-231 Cart summary tax breakdown | `ledgerline/web` | LL-204 |
| LL-214 Refund endpoint idempotency keys | `ledgerline/api` | LL-209 |
| LL-233 Refund button in order history | `ledgerline/web` | LL-214, LL-231 |

Some of these links cross repos: LL-231 in `web` waits on LL-204 in `api`. `depends_on` uses bare story ids from one team-wide index, so these links work like any other. See the note at the end of this guide about cross-repo links.

```yaml title="ll-231-cart-summary-tax-breakdown.md"
---
type: story
story_id: LL-231
title: Cart summary tax breakdown
status: review
source_repo: ledgerline/web
depends_on: [LL-204]
---
```

> **One repo per story**
>
> There's no multi-repo story. If a change needs an `api` endpoint and a `web` screen, write two stories. And don't leave `source_repo` out: a story without it can't be opened as a workspace.

## 3. Check the plan

Open `checkout-v2` in the left nav. Every story from both repos is listed in one epic, and its progress counts all of them. Stories whose dependencies are done are marked ready, whichever repo the dependency is in.

To see one repo's share of the epic, add a `csquery` block to the epic's README that filters on `source_repo`:

````md
```csquery
epic: checkout-v2
tag: source_repo=ledgerline/web
view: list
```
````

## 4. Open stories in their repos

Open LL-231's workspace (right-click it in the left nav and choose **Open CLI**). CodeSense creates the worktree in your `web` clone. Do the same for LL-214, and its worktree is created in your `api` clone. You don't have to pick a clone or `cd` anywhere.

If one of the repos isn't attached on your machine, you'll see **No local clone attached**. Choose **Attach local clone**, and the open continues.

A dependency can cross repos, such as LL-231 (web) → LL-204 (api): story ids resolve through one org-wide index, whatever repo a story targets.

The full guide ships in the app's docs: **The model → Source repos**.
