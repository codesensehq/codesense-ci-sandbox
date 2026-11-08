# architecture.excalidraw — canvas sketch

Two variants. Both export at **1600×1000** so the HTML overlay math is trivial.

- `architecture.excalidraw` → `architecture.excalidraw.png` (local-only)
- `architecture-remote.excalidraw` → `architecture-remote.excalidraw.png` (adds the remote mirror)

The shape ids below are the same strings the HTML hitmap uses in `data-shape`. The Excalidraw frames use the id as `customData.shapeId`. The sketch coordinates are the ones the SVG overlay in `mock.html` reflects.

---

## 0 · Canvas setup (both variants)

| Prop | Value |
|---|---|
| viewBackgroundColor | `#0b0e14` (dark; a light variant is a separate re-export, same shapes) |
| gridSize | 10 |
| exportBackground | true |
| exportPadding | 32 |
| viewportSize | 1600 × 1000 |

Palette tokens the shapes reference:

| Token | Hex (dark) | Role |
|---|---|---|
| `--arc-signal` | `#5ad1ff` | HTTP/data edges, viewer flow, "live" |
| `--arc-agent`  | `#f6b454` | MCP, agent-cli, routines-firing edges |
| `--arc-repo`   | `#b48bff` | context-repo, worktree, source-repo |
| `--arc-remote` | `#4fd4bb` | remote-only frames + ssh edge |
| `--ink`        | `#e6ebf3` | labels, strokes on dark |
| `--muted`      | `#8a94a6` | captions |
| `--surface`    | `#111621` | shape fills |
| `--surface-2`  | `#171d2a` | frame fills |

Font: Excalidraw's `Nunito` at 20px for shape labels, 14px for captions. (The embedded PNG uses the font Excalidraw bakes; the HTML page's own type stack is a separate axis.)

---

## 1 · Local variant — shape list

### 1.1 Outer frames

| id | kind | x | y | w | h | label | fill | stroke | note |
|---|---|---|---|---|---|---|---|---|---|
| `machine-local` | frame | 40 | 60 | 940 | 900 | "This Mac" | surface-2 | ink 1.5px | the whole left half |
| `repos-group` | frame | 1020 | 60 | 540 | 460 | "Context repo + worktree" | surface-2 | arc-repo 1.5px dashed | top-right cluster |
| `async-group` | frame | 1020 | 560 | 540 | 400 | "Async spine" | surface-2 | arc-agent 1.5px dashed | bottom-right cluster |

Frame labels sit at `(x+16, y+12)` left-aligned, caption weight.

### 1.2 Shapes inside `machine-local`

Coordinates are canvas-absolute (not relative to the frame).

| id | icon | x | y | w | h | label | caption | fill | stroke |
|---|---|---|---|---|---|---|---|---|---|
| `webview` | `monitor` | 80 | 120 | 420 | 240 | "Tauri webview" | "React viewer · panes · tabs" | surface | arc-signal 1.5px |
| `dashboards` | `layout-dashboard` | 100 | 180 | 180 | 70 | "Dashboards" | "HTML docs, this page" | surface | ink 1px (nested in `webview`) |
| `agent-cli` | `terminal-square` | 300 | 180 | 180 | 70 | "Agent CLI" | "Claude Code · Codex" | surface | arc-agent 1.5px (nested in `webview`) |
| `http-server` | `server` | 540 | 120 | 240 | 240 | "Local HTTP server" | "Axum · localhost:port · injects window.codesense" | surface | arc-signal 1.5px |
| `host-agent` | `bot` | 80 | 420 | 700 | 160 | "App backend" | "The app's Rust backend · MCP · routines · context IO" | surface | arc-agent 1.5px |
| `mcp` | `plug-zap` | 300 | 640 | 480 | 100 | "CodeSense MCP" | "every tool · the agent bridge" | surface | arc-agent 1.5px heavy (2px) |
| `source-code-note` | text | 80 | 780 | 700 | 150 | "Local machine ·  macOS filesystem · ~/.codesense/… + user repos" | — | none | muted caption only |

Nested shape rule: `dashboards` and `agent-cli` are drawn **inside** the `webview` rectangle. Their stroke is one weight lighter than their parent.

### 1.3 Shapes inside `repos-group`

| id | icon | x | y | w | h | label | caption | fill | stroke |
|---|---|---|---|---|---|---|---|---|---|
| `context-repo` | `git-branch` | 1040 | 100 | 500 | 140 | "Context repo" | "git-backed org · teams · content · config" | surface | arc-repo 1.5px |
| `worktree` | `trees` | 1040 | 270 | 500 | 110 | "Worktree" | "per-story · created on open · hydrated on request" | surface | arc-repo 1.5px |
| `source-repo` | `folder-git` | 1040 | 400 | 500 | 100 | "Bound source repo" | "the code being worked on" | surface | arc-repo 1.5px dashed |

### 1.4 Shapes inside `async-group`

| id | icon | x | y | w | h | label | caption | fill | stroke |
|---|---|---|---|---|---|---|---|---|---|
| `inbox` | `inbox` | 1040 | 600 | 500 | 150 | "Inbox" | "per-team threads · messages · role queues" | surface | arc-agent 1.5px |
| `routines` | `clock-9` | 1040 | 770 | 500 | 170 | "Routine engine" | "triggers: cron · manual · events" | surface | arc-agent 1.5px |

### 1.5 Edges (local variant)

Routed 90°, 6px corner radius. Arrowhead = filled triangle 10×8.

| from | to | color | style | label |
|---|---|---|---|---|
| `webview` right-mid (500, 240) | `http-server` left-mid (540, 240) | arc-signal | solid 1.5px, dashed-march on active | "fetch · subscribe" |
| `http-server` bottom-mid (660, 360) | `host-agent` top (660, 420) | arc-signal | solid 1.5px | "ipc" |
| `agent-cli` bottom (390, 250) ↓ via waypoint (390, 620) | `mcp` top (390, 640) | arc-agent | solid 1.5px, dashed-march on active | "tool call" |
| `mcp` bottom (540, 740) ↓ waypoint (540, 500) | `host-agent` bottom (540, 580) via a loop around the right | arc-agent | solid 1.5px | "dispatch" |
| `host-agent` right (780, 500) | `context-repo` left (1040, 170) | arc-repo | solid 1.5px | "read / write files" |
| `host-agent` right (780, 500) | `worktree` left (1040, 325) | arc-repo | solid 1.5px | "create · hydrate" |
| `worktree` bottom-right (1540, 380) → waypoint (1570, 450) | `source-repo` right (1540, 450) | arc-repo | solid 1.5px, label "mounts" | the source repo lives *inside* the worktree |
| `host-agent` right (780, 500) → waypoint (900, 675) | `inbox` left (1040, 675) | arc-agent | solid 1.5px | "post · read" |
| `routines` left (1040, 855) → waypoint (900, 855) | `host-agent` right (780, 540) | arc-agent | solid 1.5px, label "fires" | the engine wakes the host-agent |
| `inbox` top (1290, 600) → waypoint (1290, 540) → waypoint (900, 540) → `routines` top (1290, 770) via back | | arc-agent dashed 1px, label "inbox_idle" | the trigger loop |

Two edges carry the authored-motion dashed-march (viewer↔http, agent-cli↔mcp). The rest are static.

### 1.6 Legend (local)

Small cluster at `(80, 940)`:

- cyan swatch · "data / view flow"
- amber swatch · "agent / MCP"
- violet swatch · "git-backed"

---

## 2 · Remote variant — adds mirror behind SSH

The remote variant is the local variant with the **right half translated left** and a new mirror cluster added. Target canvas stays 1600×1000; shape layout shifts so there is room on the right for the remote frame.

### 2.1 New / changed frames

| id | kind | x | y | w | h | label | fill | stroke |
|---|---|---|---|---|---|---|---|---|
| `machine-local` | frame | 20 | 60 | 720 | 900 | "This Mac" | surface-2 | ink 1.5px |
| `machine-remote` | frame | 1120 | 60 | 460 | 900 | "Remote machine (private account)" | surface-2 | arc-remote 1.5px dashed |
| `repos-group` | frame | 760 | 60 | 340 | 460 | "Shared git truth" | surface-2 | arc-repo 1.5px dashed |
| `async-group` | frame | 760 | 560 | 340 | 400 | "Async spine" | surface-2 | arc-agent 1.5px dashed |

### 2.2 Repositioned local shapes

Same shapes as the local variant, repositioned to fit the narrower left frame. Widths shrink to ~340px; vertical order unchanged.

| id | x | y | w | h |
|---|---|---|---|---|
| `webview` | 40 | 120 | 320 | 240 |
| `dashboards` | 56 | 180 | 140 | 70 |
| `agent-cli` | 212 | 180 | 140 | 70 |
| `http-server` | 380 | 120 | 240 | 240 |
| `host-agent` | 40 | 420 | 580 | 160 |
| `mcp` | 220 | 640 | 400 | 100 |

### 2.3 Repos + async group (centered)

| id | x | y | w | h |
|---|---|---|---|---|
| `context-repo` | 780 | 100 | 300 | 140 |
| `worktree` | 780 | 270 | 300 | 110 |
| `source-repo` | 780 | 400 | 300 | 100 |
| `inbox` | 780 | 600 | 300 | 150 |
| `routines` | 780 | 770 | 300 | 170 |

### 2.4 Remote mirror (inside `machine-remote`)

Teal-tinted copies of three shapes. Smaller, dashed-stroke to indicate they're mirrored over SSH.

| id | icon | x | y | w | h | label | caption | stroke |
|---|---|---|---|---|---|---|---|---|
| `host-agent-remote` | `bot` | 1140 | 420 | 420 | 160 | "Terminal server (remote)" | "terminal server · behind sshd" | arc-remote 1.5px dashed |
| `worktree-remote` | `trees` | 1140 | 270 | 420 | 110 | "Worktree (remote)" | "same worktree rules" | arc-remote 1.5px dashed |
| `source-repo-remote` | `folder-git` | 1140 | 400 | 420 | 100 | "Bound source repo (remote)" | — | arc-remote 1.5px dashed |
| `agent-cli-remote` | `terminal-square` | 1300 | 180 | 220 | 70 | "Remote agent CLI" | "launched via remote tab" | arc-remote 1.5px dashed |
| `mcp-remote` | `plug-zap` | 1200 | 640 | 300 | 100 | "CodeSense MCP (remote)" | "same tools, remote-dispatched" | arc-remote 1.5px dashed |

### 2.5 SSH boundary

A single tall shape — a vertical hairline with a `cloud` icon midway — serves as the identifying boundary.

| id | kind | x | y | w | h | label |
|---|---|---|---|---|---|---|
| `ssh` | boundary | 1100 | 100 | 20 | 820 | "SSH" (rotated 90°, muted) |

Crossing edges carry a small `cloud` glyph where they cross the boundary.

### 2.6 Remote edges

Mirror of the local edges, plus three cross-boundary edges:

| from | to | color | style | label |
|---|---|---|---|---|
| `host-agent` right | `host-agent-remote` left (crosses `ssh`) | arc-remote | dashed 1.5px, cloud glyph | "pair · over sshd" |
| `agent-cli-remote` bottom | `mcp-remote` top | arc-agent | solid 1.5px | "tool call" |
| `mcp-remote` bottom | `host-agent-remote` bottom | arc-agent | solid 1.5px | "dispatch" |
| `host-agent-remote` left (crosses `ssh`) | `context-repo` right | arc-repo | dashed 1.5px, cloud glyph | "sync" |
| `host-agent-remote` left | `worktree` right | arc-repo | dashed 1.5px, cloud glyph | "create over ssh" |
| `host-agent-remote` left | `inbox` right | arc-agent | dashed 1.5px, cloud glyph | "post · read" |

The remote variant's legend adds a fourth swatch — teal · "replicated behind SSH".

---

## 3 · Export + round-trip

Both files export at **1600×1000 PNG** at 2x pixel-ratio.

- `apply_canvas` creates the `.excalidraw` with the shape ids above as `customData.shapeId`.
- `render_canvas` renders the sibling PNG.
- The HTML overlay in `mock.html` declares one JS object per variant mapping `shapeId → {x, y, w, h}` with the same coordinates as above. If the Excalidraw author moves a shape, re-run a round-trip helper (`_shared/open-canvas.js` plus a small export-coords step) and the overlay updates.

For the first ship, the overlay coordinates in `mock.html` are hand-maintained to match this sketch verbatim; the dev who authors the real canvas is instructed to anchor shapes at these coords.

---

## 4 · Open items for the canvas author

- Nested shapes (`dashboards`, `agent-cli` inside `webview`) read better in Excalidraw as children of a `Group` whose first element is the parent rect. Confirm `apply_canvas` honors group parentage; if not, draw them as siblings and trust z-order.
- Decide whether to render the authored-motion dashed-march on the viewer↔http edge directly in the PNG (via a repeating dashed stroke) or purely in the HTML overlay. Default: dashed stroke in the PNG as the "resting look," HTML overlay adds the march on `prefers-reduced-motion: no-preference`.
- Light-theme variant: re-export both files with `viewBackgroundColor: #fafbfd`, `--surface: #ffffff`, `--surface-2: #f3f5f9`, labels in `--ink: #0b0e14`. Same shape coords, same ids.
