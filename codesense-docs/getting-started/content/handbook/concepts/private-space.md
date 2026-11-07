# Private

_The one place for what only you see: notes, files, panes, routines and agents kept for each org._

**Private** is where things that are only yours live: notes, a skill you're
still trying out, a pane just for you, a routine or an agent nobody else
needs. It's yours to keep, not a throwaway: nothing in it is shared.

Open it from the menu in the title bar that names your context repo and team:
choose **Private** under **You** (or search "Private" in the command palette).
It lists, for the org you have open:

- **Notes**: your notes for this org. Opening one shows it in the Notes panel.
- **Files**: files that apply in this org's work. In a team's worktrees each
  one replaces the team's file at the same place, and shows a **From private**
  badge.
- **Panes**: right panes kept just for you. Opening one shows it in the side panel.
- **Routines** and **Agents** kept just for you.
- **All orgs**: skills, prompts, snippets and templates that apply in every
  org. Anything else in that folder is listed as not used.

## Only me

When you create a file, a routine or an agent, **Who can use it** asks where it
goes. It starts at the team; pick **Only me** to keep it in Private for the
open org instead.

## Each org keeps its own

Everything is filed under the org you made it in and only ever acts inside
that org's work, whichever org you have open. Only the **All orgs** items
carry across. To keep a team's worktrees free of your items, turn off **Use my
private items in ‹Team›** in that same title-bar menu.

## Share it when it's ready

Each item in Private has a **⋯** menu (a **From private** file in a worktree
has the same menu on right-click):

- **Share with ‹Team›…** moves it to the same place in the team's
  `content/`. It's then an ordinary change in the context repo: you, or an
  agent you ask, commit and push it, and teammates get it on their next pull.
- **Copy to another org…** copies it into your Private for another org.
- **Use in all orgs** moves a skill, prompt, snippet or template to
  **All orgs**. **Only this org** moves it back.
- **Remove…** deletes it.

To go the other way, a team file's **⋯** menu in the left nav has
**Move to…** and **Copy to…**, and Private is one of the places it can go.

> **You back it up**
>
>   Private is a local folder under `~/.codesense/` on this Mac. CodeSense never
>   copies or pushes it anywhere. Back it up yourself, or ask an agent to.

The full guide ships in the app's docs: **The model → Private & All-orgs**.

## Your items in every org

Your all-orgs folder holds the skills, prompts, snippets and templates you want everywhere. They follow you into every org you work in.

They live in your private all-orgs folder, `scratch/all-orgs/`, on your Mac. Keep personal tools here: your review checklist, your favourite prompts. Keep team rules out of it, because teammates never see this folder; put those in team content instead.

> **Caution**
>
> Private and all-orgs files are just as easy to leak as any other file. Keep API keys and tokens out of every layer.
