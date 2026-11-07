# sdlc/all-local.excalidraw — canvas sketch

Authoring spec for `apply_canvas` → `sdlc/all-local.excalidraw` + `render_canvas`
→ `sdlc/all-local.excalidraw.png`. Shipped by the Arc H phase-8 build page as
one of three distribution patterns for parallel builders.

Export at **1200×700**.

See `../architecture.excalidraw.sketch.md` §0 for the canvas setup, palette
tokens, and font — identical across every CodeSense diagram.

---

## 0 · Canvas setup

| Prop | Value |
|---|---|
| viewBackgroundColor | `#0b0e14` (dark; sibling light variant is a separate re-export, same shapes) |
| gridSize | 10 |
| exportBackground | true |
| exportPadding | 32 |
| viewportSize | 1200 × 700 |

Palette tokens:

| Token | Hex (dark) | Role |
|---|---|---|
| `--machine-local` | `#5ad1ff` | Local Mac frame + builders on it |
| `--machine-remote` | `#4fd4bb` | Remote machine (used for grey-out in this variant) |
| `--story` | `#f6b454` | Story / worktree stroke |
| `--ink` | `#e6ebf3` | Labels |
| `--muted` | `#8a94a6` | Captions |
| `--surface` | `#111621` | Shape fills |
| `--surface-2` | `#171d2a` | Frame fills |

Font: `Nunito`, 20px for shape labels, 14px for captions.

---

## 1 · Shape list

### 1.1 Outer frame

| id | kind | x | y | w | h | label | fill | stroke |
|---|---|---|---|---|---|---|---|---|
| `mac-local` | frame | 60 | 60 | 1080 | 580 | "Your Mac" | surface-2 | `--machine-local` 1.5px |

Frame label sits at `(x+16, y+12)` left-aligned, caption weight.

### 1.2 Builders (5 Amelias, 5 worktrees)

Row at `y=200`, column stride `210`, each shape `180×240`.

| id | x | label | caption |
|---|---|---|---|
| `wt-1` | 90  | "worktree · DEMO-1" | "Amelia 1" |
| `wt-2` | 300 | "worktree · DEMO-2" | "Amelia 2" |
| `wt-3` | 510 | "worktree · DEMO-3" | "Amelia 3" |
| `wt-4` | 720 | "worktree · DEMO-4" | "Amelia 4" |
| `wt-5` | 930 | "worktree · DEMO-5" | "Amelia 5" |

All five shapes: fill `surface`, stroke `--story` 1.5px, rounded rect r=12.
Inside each shape, two sub-labels:
- top (20px, `--ink`): the `label` column above
- bottom (14px, `--muted`): the `caption` column above

### 1.3 Inbox hub

Single shape at `(540, 110)`, `120×50`, fill `surface`, stroke `--ink` 1px dashed.
Label: `builder inbox`.

### 1.4 Edges — inbox → worktree

Dashed arcs from `builder inbox` to each of `wt-1..5`, dash 4-3, stroke
`--machine-local` 1.5px. Caption above each arc (14px, `--muted`):
`inbox_idle`.

### 1.5 Caption bar (bottom)

Centered text at `(600, 580)`, 20px `--ink` + sub-caption at `(600, 605)` 14px
`--muted`:

- title: `All local`
- sub: `5 Amelias × 5 worktrees · bounded by your CPU, fastest feedback`

---

## 2 · Visual rules

- No remote machine shown in this variant — the frame is single-colour (local).
- All five worktrees identical modulo label; emphasize symmetry.
- Inbox hub above; builders below; one direction of arrows (down).
- Reduce visual noise: no CI node, no Scout — those appear on the sibling
  `inbox-flows/` diagrams.

---

## 3 · Owner

Arc H phase-8 content. Pending-slug entry in `../.pending.yml` keeps the
build-time lint happy until the real `.excalidraw` source lands via
`apply_canvas` in a follow-up.
