# Pane test harness

Test a pane HTML file in a real browser (chromium and webkit), inside the real host
components, through the real shim and kit. Frames are served cross-origin.

```ts
import { expect, test } from "./harness";

test("my pane", async ({ mountPane }) => {
  const pane = await mountPane("path/to/my-pane.html", {
    sessions: [{ id: "ada", handle: "ada", rowNames: { primary: "Ada" } }],
    inbox: [],
    topics: { rows: [{ id: "r1", label: "One" }] },
    intent: { note: "opened by a test" },
    theme: { id: "github", scheme: "light" },
    // effectiveGrant: ["sessions:read"], // default: the full open grant
  });
  await expect(pane.frame.getByRole("button", { name: "Ada" })).toBeVisible();
  await pane.upsert("rows", { id: "r1", label: "Edited" });
  await pane.show(false); // visibility, then show(true)
  await pane.flush(); // onFlush; also close(), maximize(), setTheme(), sendIntent()
  expect(await pane.verbs()).toEqual([]);
});
```

Run it with `pnpm test:pane-harness`. `example.spec.ts` drives `fixtures/sample-pane.html`.
Type a plain-JS pane with `// @ts-check` and a reference to `/kit/v1/codesense.d.ts`.

Mounting and `pane.setTheme({ id: "dracula", scheme: "dark" })` apply the app's real
bundled tokens, then publish the change through LiveHub. Omitting `id` selects
CodeSense. `theme.vars` can override host CSS variables after the palette is applied.
Frames receive their slot's surface: sidebar for `file-nav`, viewer otherwise.

Architecture: inbox fixtures use the live-hub message envelope. Build one with
`inboxFixtureRow(messageId, text, overrides?, team?)` from
`src/app/mock/inbox-fixtures.ts`. The fake backend derives the separate inbox-read
and rollup DTOs, including conversation summaries, from those same envelopes.
