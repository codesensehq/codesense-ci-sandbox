# sdlc/all-remote.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `sdlc/all-remote.excalidraw` +
`render_canvas` → `sdlc/all-remote.excalidraw.png`. Phase-8 build page's
third distribution pattern: all five builders on a private-account remote
macbook, driven from the user's laptop.

Export at **1200×700**. See `./all-local.excalidraw.sketch.md` §0 for the
canvas setup, palette tokens, and font — identical across every CodeSense
diagram.

---

## 1 · Shape list

### 1.1 Outer frames

| id | kind | x | y | w | h | label | fill | stroke |
|---|---|---|---|---|---|---|---|---|
| `laptop` | frame | 60  | 60  | 260  | 580 | "Your laptop (viewer)" | surface-2 | `--machine-local` 1.5px        |
| `remote` | frame | 380 | 60  | 760  | 580 | "Remote macbook"       | surface-2 | `--machine-remote` 1.5px dashed |

### 1.2 Laptop side — single viewer tab

Shape `viewer` at `(90, 220)`, `200×260`, fill `surface`, stroke `--machine-local`
1.5px, rounded rect r=12.
Sub-labels:
- top (20px, `--ink`): "CodeSense viewer"
- bottom (14px, `--muted`): "5 tabs · 5 inboxes"

### 1.3 Remote side — five builders

Row at `y=200`, each `140×240`, 10px gap:

| id | x | label | caption |
|---|---|---|---|
| `wt-1` | 400 | "worktree · DEMO-1" | "Amelia 1" |
| `wt-2` | 550 | "worktree · DEMO-2" | "Amelia 2" |
| `wt-3` | 700 | "worktree · DEMO-3" | "Amelia 3" |
| `wt-4` | 850 | "worktree · DEMO-4" | "Amelia 4" |
| `wt-5` | 1000 | "worktree · DEMO-5" | "Amelia 5" |

Fill `surface`, stroke `--story` 1.5px, rounded rect r=12.

### 1.4 Remote inbox hub

Shape `inbox` at `(680, 110)`, `160×50`, fill `surface`, stroke `--ink` 1px
dashed. Label: `builder inbox (remote)`.

### 1.5 Edges

- `inbox` → each `wt-N`: dashed arc, dash 4-3, stroke `--machine-remote`
  1.5px, caption above (14px, `--muted`): `inbox_idle`.
- `viewer` → `inbox`: straight arrow with mid-caption `ssh · host-agent`
  (16px, `--muted`), stroke `--machine-remote` 1.5px dashed 6-6.

### 1.6 Caption bar (bottom)

Centered text at `(600, 580)`, 20px `--ink` + sub-caption at `(600, 605)` 14px
`--muted`:

- title: `All remote`
- sub: `5 remote builders · drive from the laptop · merged work comes home through each PR`

---

## 2 · Visual rules

- Laptop side is deliberately minimal — one shape — to emphasize that it is a
  terminal/driver, not the workhorse.
- Remote side mirrors `all-local.excalidraw` with the colour swapped (local
  blue → remote teal), reinforcing the "same loop, different host" story.

---

## 3 · Owner

Arc H phase-8 content. Pending-slug entry in `../.pending.yml` keeps the
lint happy until the real `.excalidraw` source lands via `apply_canvas`.
