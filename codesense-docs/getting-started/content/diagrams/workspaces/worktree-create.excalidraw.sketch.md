# workspaces/worktree-create.excalidraw — canvas sketch

Authoring spec for the follow-up PR that calls `apply_canvas` to
produce `worktree-create.excalidraw` and `render_canvas` to produce
the sibling `worktree-create.excalidraw.png`. Until those MCP tools land, the
shipped `arc-c-workspaces-and-worktrees/worktree-creation.html` renders the
same shape via an inline SVG stand-in using the same coordinate space, so
swapping to the PNG is a no-layout change.

Export at **1280×720** so the HTML's 16:9 frame math stays trivial.

---

## 0 · Canvas setup

| Prop                | Value                                                       |
| ------------------- | ----------------------------------------------------------- |
| viewBackgroundColor | `#0b0e14` (dark; a light re-export carries the same shapes) |
| gridSize            | 10                                                          |
| exportBackground    | true                                                        |
| exportPadding       | 24                                                          |
| viewportSize        | 1280 × 720                                                  |

Palette tokens (match the arc-c-workspaces-and-worktrees HTML role chroma):

| Token             | Hex (dark) | Role                             |
| ----------------- | ---------- | -------------------------------- |
| `--arc-repo`      | `#b48bff`  | context repo, worktree           |
| `--arc-signal`    | `#5ad1ff`  | story link, viewer flow          |
| `--arc-agent`     | `#f6b454`  | agent / MCP action               |
| `--arc-source`    | `#4fd4bb`  | bound source repo                |
| `--arc-worktree`  | `#9c7cff`  | the new worktree frame           |
| `--ink`           | `#e6ebf3`  | labels, node strokes             |
| `--muted`         | `#8a94a6`  | captions, routing edges          |
| `--surface`       | `#111621`  | node fills                       |
| `--surface-2`     | `#171d2a`  | frame fills                      |
| `--accent`        | `#6ea8f0`  | the "open workspace" call        |

Font: Excalidraw's `Nunito` at 22px for labels, 14px for captions.

---

## 1 · Timeline frame

The diagram reads **left → right** as a four-step timeline. One wide frame
spans the whole canvas; four columns inside it carry the steps.

| id         | kind  | x   | y  | w    | h   | label                            | fill      | stroke       |
| ---------- | ----- | --- | -- | ---- | --- | -------------------------------- | --------- | ------------ |
| `timeline` | frame | 40  | 60 | 1200 | 600 | `Create a story worktree`        | surface-2 | `--muted` 1px dashed |

Column divider hairlines at `x = 340`, `x = 640`, `x = 940`, drawn in
`--muted` 0.5px, from `y = 100` to `y = 620`.

Column captions sit at `(column-center, 90)` in `--muted` 14px, caps:

| x   | caption          |
| --- | ---------------- |
| 190 | `BEFORE`         |
| 490 | `CALL`           |
| 790 | `DURING`         |
| 1090| `AFTER`          |

---

## 2 · Column 1 — BEFORE (x range 80 .. 320)

### 2.1 Story card

| id      | icon       | x   | y   | w   | h  | label        | caption                        | fill    | stroke       |
| ------- | ---------- | --- | --- | --- | -- | ------------ | ------------------------------ | ------- | ------------ |
| `story` | `file-text`| 110 | 180 | 180 | 86 | `DEMO-1`  | `story · depends_on: []`       | surface | `--arc-repo` 1.5px |

### 2.2 Context repo (dim, in background)

A faint rounded rect behind the story:

| id            | x   | y   | w   | h   | fill    | stroke                        |
| ------------- | --- | --- | --- | --- | ------- | ----------------------------- |
| `context-repo`| 90  | 310 | 220 | 90  | surface | `--arc-repo` 1px dashed       |

Label `context repo` at `(105, 350)`, `caption` `(105, 370)` reading
`codesense/docs`.

### 2.3 Caption at bottom of column

`(190, 580)` in `--muted` 14px: `"No worktree yet · nothing on disk"`

---

## 3 · Column 2 — CALL (x range 360 .. 620)

### 3.1 MCP call chip

| id       | icon       | x   | y   | w   | h  | label             | caption                      | fill    | stroke          |
| -------- | ---------- | --- | --- | --- | -- | ----------------- | ---------------------------- | ------- | --------------- |
| `call`   | `plug-zap` | 380 | 200 | 220 | 110| `open_workspace`  | `target: 'DEMO-1'`        | surface | `--arc-agent` 2px|

### 3.2 Launcher caption

`(490, 340)` in `--muted` 14px, centered: `"any agent · any user"`.

Edge **from** `story` right-mid `(290, 223)` **to** `call` left-mid
`(380, 255)` in `--arc-agent` 1.5px with filled triangle arrowhead,
labeled `"handle"` centered.

Caption at bottom of column `(490, 580)`, `--muted` 14px:
`"ensure_story_worktree does the work"`.

---

## 4 · Column 3 — DURING (x range 660 .. 920)

Four stacked step glyphs, each a thin pill `(680, y, 240, 46)` with its
own icon and `step N` tag on the left in `--muted` 11px caps:

| id        | step | y   | icon          | label                        | stroke             |
| --------- | ---- | --- | ------------- | ---------------------------- | ------------------ |
| `d-fetch` | 1    | 140 | `cloud-download` | `git fetch (if reachable)` | `--arc-repo` 1.2px |
| `d-add`   | 2    | 200 | `git-branch` | `git worktree add`           | `--arc-repo` 1.2px |
| `d-link`  | 3    | 260 | `link`       | `link the story`             | `--arc-signal` 1.2px|
| `d-setup` | 4    | 320 | `terminal`   | `run worktree.yaml · setup`  | `--arc-agent` 1.2px|

A vertical connector `(800, 186) → (800, 356)` in `--muted` 1px dashed
threads them.

Below the four steps, a `clock` glyph at `(680, 400)` with caption
`~10-30s` in `--muted` 14px.

Caption at bottom of column `(790, 580)`, `--muted` 14px:
`"codesense runs these git steps for you"`.

### 4.1 Edge from CALL to DURING

Edge from `call` right-mid `(600, 255)` to `d-fetch` left-mid
`(680, 163)` in `--arc-agent` 1.5px with filled arrowhead.

---

## 5 · Column 4 — AFTER (x range 960 .. 1220)

### 5.1 Worktree frame (the headline)

| id         | kind  | x    | y   | w   | h   | label                            | fill      | stroke                    |
| ---------- | ----- | ---- | --- | --- | --- | -------------------------------- | --------- | ------------------------- |
| `worktree` | frame | 980  | 140 | 240 | 320 | `~/codesense-wt/demo-1/`      | surface   | `--arc-worktree` 2px      |

Inside the frame, three stacked chips. Creating a worktree copies no context
into it; context arrives at agent launch or through an explicit hydrate call.

| id              | icon         | x    | y   | w   | h  | label                 | caption                | stroke                      |
| --------------- | ------------ | ---- | --- | --- | -- | --------------------- | ---------------------- | --------------------------- |
| `wt-source`     | `folder-git` | 1000 | 170 | 200 | 50 | `source repo`         | `cloned branch`        | `--arc-source` 1.2px        |
| `wt-link`       | `link`       | 1000 | 230 | 200 | 50 | `story link`          | `no context copied`    | `--arc-signal` 1.2px        |
| `wt-config`     | `file-cog`   | 1000 | 290 | 200 | 50 | `worktree.yaml setup` | `ran · exit 0`         | `--arc-agent` 1.2px         |

A small `check-circle` glyph in `--arc-source` sits at `(1200, 430)`
with caption `ready` in `--muted` 14px below it.

### 5.2 Caption at bottom of column

`(1090, 580)` in `--muted` 14px: `"index entry written · agent attaches"`.

### 5.3 Edge from DURING to AFTER

Edge from `d-setup` right-mid `(920, 343)` to `worktree` left-mid
`(980, 300)` in `--arc-worktree` 1.5px with filled arrowhead,
labeled `"created"` centered just above the edge.

---

## 6 · Legend

Small horizontal cluster at `(60, 650)` in 12px `--muted`:

- `--arc-worktree` swatch · "worktree frame"
- `--arc-source` swatch · "source repo"
- `--arc-signal` swatch · "story link"
- `--arc-agent` swatch · "agent / MCP"

---

## 7 · Export + round-trip

Export the Excalidraw canvas at **1280×720 PNG** at 2x pixel-ratio.

- `apply_canvas` creates the `.excalidraw` with the shape ids above as
  `customData.shapeId`.
- `render_canvas` renders the sibling PNG.
- The HTML embed in `worktree-creation.html` references
  `../diagrams/workspaces/worktree-create.excalidraw.png` with `data-canvas`,
  so a click opens the live canvas tab in-app.

For the first ship, the HTML renders an inline SVG stand-in using the same
coordinate space. The slug `worktree-create` is listed under
`workspaces/worktree-create` in `diagrams/.pending.yml` until `apply_canvas`
and `render_canvas` land.

---

## 8 · Light-theme variant

Re-export both files with `viewBackgroundColor: #fafbfd`, `--surface:
#ffffff`, `--surface-2: #f3f5f9`, labels in `--ink: #0b0e14`. Same shape
coords, same ids.
