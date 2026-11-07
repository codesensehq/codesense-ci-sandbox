# Diagrams

Every diagram in the shipped HTML docs is an Excalidraw canvas authored through
the CodeSense MCP. Agents produce the `.excalidraw` source directly; readers
click through to the live canvas tab in-app to see the real thing. There is no
per-diagram PNG committed to this tree.

## Why Excalidraw-only

`architecture.html`, `inbox-flows.html`, and the arc content pages all describe
diagrams. Excalidraw is the one standard CodeSense ships end-to-end:

- Agents author canvases through the `apply_canvas` MCP tool. It
  writes the `.excalidraw` source atomically on the context rails OR into a
  registered source-repo attachment / worktree (pre-release docs authoring).
- Agents visually verify a canvas by calling `render_canvas`. It
  returns the PNG bytes INLINE as an MCP `image` content block — the agent
  consumes the render without a file ever landing on disk.
- Users click through the inline SVG stand-in (or the plain live-canvas link)
  in each shipped page to open the live canvas tab and keep editing.

No Mermaid, no D2 in shipped HTML docs. The build-time check in
`scripts/build-codesense-docs.mjs` enforces that half of the convention.

## Authoring pipe

For each diagram you want to ship:

1. **Draft via MCP.** Call `apply_canvas` with the shape skeletons; `path` may
   target either the context rails or a registered source-repo attachment
   (e.g. a `codesense-docs/getting-started/content/diagrams/<slug>.excalidraw`
   path inside the source repo), followed by the `elements` payload.

   ```json
   {
     "path": "codesense-docs/getting-started/content/diagrams/<slug>.excalidraw",
     "elements": [
       /* excalidraw elements */
     ]
   }
   ```

2. **Visually verify.** Call `render_canvas` on the same path. The MCP response
   carries the PNG bytes inline (base64, `image/png`) plus a text block with
   `sourcePath` / `byteLen` / `mimeType`. No sidecar is written.

3. **Commit the source.** Only the `.excalidraw` source is committed. No
   `.excalidraw.png` sidecar belongs in this tree.

4. **Reference from HTML.** Shipped HTML pages embed an inline SVG stand-in in
   the same coordinate space as the sketch (`<slug>.excalidraw.sketch.md`); the
   stand-in is the visual readers see. A click-through from the page is the
   path to the live canvas.

## Build-time rules

The docs bundle generator enforces this convention. The build fails if any
shipped HTML contains a Mermaid fence (` ```mermaid`), a `<div class="mermaid">`,
a D2 fence (` ```d2`), or a `<pre data-d2>` block.

There is no `.excalidraw.png` sibling-source rule: `render_canvas` returns PNGs
inline, and shipped docs embed no per-diagram `<img>` sidecars.

## This directory

Keep diagrams flat: one `<slug>.excalidraw` per concept, named for the concept.
Nested per-arc subfolders (`factory/`, `inbox-flows/`, `sdlc/`, `workspaces/`)
exist today for the authoring specs; follow the pattern of whichever
page is embedding the diagram.

Diagrams land with the content pages that need them (architecture,
inbox-flows, arc-b-model, workspaces, sdlc, factory). The authoring specs (`<slug>.excalidraw.sketch.md`)
remain the source of truth for shape; the `.excalidraw` sources land through
`apply_canvas` as each follow-up PR ships.
