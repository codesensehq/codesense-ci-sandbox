# sdlc/hybrid.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `sdlc/hybrid.excalidraw` + `render_canvas`
→ `sdlc/hybrid.excalidraw.png`. Phase-8 build page's second distribution
pattern: two builders local, three on a private-account remote.

Export at **1200×700**. See `./all-local.excalidraw.sketch.md` §0 for the
canvas setup, palette tokens, and font — identical across every CodeSense
diagram.

---

## 1 · Shape list

### 1.1 Outer frames (two side by side)

| id | kind | x | y | w | h | label | fill | stroke |
|---|---|---|---|---|---|---|---|---|
| `mac-local`  | frame | 40  | 60 | 500 | 580 | "Your Mac"             | surface-2 | `--machine-local` 1.5px        |
| `mac-remote` | frame | 580 | 60 | 580 | 580 | "Private-account remote" | surface-2 | `--machine-remote` 1.5px dashed |

### 1.2 Builders

Local (2 Amelias), row at `y=200`, each `180×240`, 10px gap:

| id | x | label | caption |
|---|---|---|---|
| `wt-1` | 80  | "worktree · DEMO-1" | "Amelia 1" |
| `wt-2` | 280 | "worktree · DEMO-2" | "Amelia 2" |

Remote (3 Amelias), row at `y=200`, each `160×240`:

| id | x | label | caption |
|---|---|---|---|
| `wt-3` | 600 | "worktree · DEMO-3" | "Amelia 3" |
| `wt-4` | 780 | "worktree · DEMO-4" | "Amelia 4" |
| `wt-5` | 960 | "worktree · DEMO-5" | "Amelia 5" |

All five shapes: fill `surface`, rounded rect r=12. Stroke `--story` 1.5px on
all five.

Each shape carries two sub-labels (same as all-local):
- top (20px, `--ink`): the `label` column above
- bottom (14px, `--muted`): the `caption` column above

### 1.3 Shared inbox hub (above both frames)

Shape at `(540, 110)`, `120×50`, fill `surface`, stroke `--ink` 1px dashed.
Label: `builder inbox`.

### 1.4 Edges — inbox → worktree

Dashed arcs from `builder inbox` to each of `wt-1..wt-5`, dash 4-3,
stroke 1.5px:

- `wt-1`, `wt-2`: stroke `--machine-local`
- `wt-3`, `wt-4`, `wt-5`: stroke `--machine-remote`

Caption above each arc (14px, `--muted`): `inbox_idle`.

### 1.5 Ssh seam (between frames)

Vertical dashed line at `x=560`, from `y=60` to `y=640`, stroke `--machine-remote`
1.5px, dash 6-6. Caption at midpoint (14px, `--muted`): `ssh · host-agent`.

### 1.6 Caption bar (bottom)

Centered text at `(600, 580)`, 20px `--ink` + sub-caption at `(600, 605)`
14px `--muted`:

- title: `Hybrid`
- sub: `2 local + 3 remote · keep touchy diffs on your Mac, offload long builds`

---

## 2 · Visual rules

- The two frames must read as "same shape, two colours." Keep layouts
  symmetric; only the stroke colour + the frame label distinguish them.
- The seam between them (`ssh · host-agent`) is where the parity breaks live,
  not inside either side.

---

## 3 · Owner

Arc H phase-8 content. Pending-slug entry in `../.pending.yml` keeps the
lint happy until the real `.excalidraw` source lands via `apply_canvas`.
