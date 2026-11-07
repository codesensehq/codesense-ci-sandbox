# `<CodeEditor>` — shared code editor (CodeMirror 6)

Shared infrastructure for any codesense surface that needs to view or edit a
file. First consumer: the install management workbench (SS-10). Future
consumers anticipated: main viewer in-place editing, story editor.

> This file is the consumer contract. The full story-level context lives in
> `SS-14-shared-code-editor.md`.

## Quick start

```tsx
import { CodeEditor } from "@/shared/components/code-editor";

function MyEditor() {
  const [value, setValue] = useState("# Hello\n");
  return (
    <CodeEditor
      value={value}
      onChange={setValue}
      path="README.md"
      onSave={async (next) => {
        await senseSyncWriteToLayer({ /* ... */, content: encodeUtf8(next) });
      }}
      dirty={value !== savedValue}
    />
  );
}
```

## Public API

See `index.ts` for the canonical types. Key props:

- **`value: string`** — sync-trigger semantics (see D-15 in story). The editor
  syncs to external `value` changes via a CodeMirror transaction; use the
  imperative `handle.setValue()` + `handle.clearHistory()` when you want to
  bypass undo history (e.g. disk-reload after a conflict).
- **`onChange(next)`** — fires per keystroke.
- **`onSave(value): Promise<void>`** — Cmd+S handler. Rejection → host
  responsible for error UX (toast, retry). Concurrent Cmd+S during in-flight
  save returns the same promise (in-editor dedup).
- **`onLoadError?(err)`** — fires if CM6 chunk load fails. Editor also renders
  an inline "Editor failed to load. [Retry]" UI inside its bounds.
- **`readOnly`** — applies CM6 readOnly extension AND reflects `aria-readonly`
  on the root; an `aria-live` region announces transitions.
- **`theme: "light" | "dark" | "auto"`** — default `"auto"` subscribes to
  `next-themes` `resolvedTheme`. Explicit values force-override.
- **`gutters?: boolean`** — default `true`. Set `false` for chromeless
  markdown editing.
- **`fontSize?: number`** — default reads `codeEditorFontSize` from
  `viewer-store` via `font-sizes.ts`; explicit prop wins.
- **`customExtensions?: Extension[]`** — surface-specific CM6 extensions.
  Composed AFTER SS-14's defaults so consumers can override behavior.

## Imperative handle (via `ref`)

```ts
interface CodeEditorHandle {
  getValue(): string;
  setValue(next: string): void;
  clearHistory(): void;

  // Per-tab state preservation (the load-bearing API for hosts with tabs).
  // getState() returns null when the editor is mid-load, has errored, or
  // has been unmounted — host must gate before passing into setState.
  getState(): EditorState | null;
  setState(state: EditorState): void;

  focus(): void;
  blur(): void;
  getCursor(): { line: number; ch: number };
  setSelection(range: {
    from: { line: number; ch: number };
    to: { line: number; ch: number };
  }): void;

  getScrollPos(): number;
  scrollTo(pos: number): void;

  checkConflict(diskVersion: string): boolean;
}
```

### Imperative + controlled-prop contract

`<CodeEditor>` is _not_ a fully controlled component in the React sense.
The `value` prop is a sync-source — whenever it changes, the editor
syncs. The imperative `setValue` / `setState` calls are escape hatches
for one-off writes that bypass the prop pipeline (e.g., disk-reload
after a conflict, tab-switch state restore).

**Important pairing rule:** when you call `handle.setValue(x)` or
`handle.setState(s)`, you should _also_ update your component's `value`
prop to match (`setState((v) => x)` or whatever your host's pattern
is). Otherwise the editor's next render will run the value-sync effect,
see that `value` differs from the new content, and dispatch the _old_
`value` prop on top — silently reverting your imperative write.

```ts
// Disk-reload pattern (after a conflict)
const bytes = await readLayerFile({
  /* ... */
});
const next = decodeUtf8(bytes);
handle.setValue(next); // imperative replace (no undo)
handle.clearHistory(); // wipe pre-reload undo entries
setLocalValue(next); // keep the controlled prop in lockstep
```

### Tab-switch pattern (for hosts like the workbench)

The workbench keeps one `<CodeEditor>` and swaps state per tab. Note
the null guard — the editor may not be ready when a switch fires (e.g.,
mid-load):

```ts
// on leaving tab A
const snapshot = handle.getState();
if (snapshot) tabStateStash.set(tabA.id, snapshot);

// on entering tab B
const stashed = tabStateStash.get(tabB.id);
if (stashed) handle.setState(stashed);
else /* host builds a fresh state via host-side EditorState.create */ ;
handle.focus(); // autoFocus is mount-only; re-focus is host's job
```

`EditorState` carries content, undo history, cursor, and selection.
**Scroll position is NOT part of EditorState** — it lives on the
per-view scrollDOM. Hosts that want scroll-preserving tab switches
stash scroll separately:

```ts
// on leaving tab A
tabScrollStash.set(tabA.id, handle.getScrollPos());

// on entering tab B
const scroll = tabScrollStash.get(tabB.id) ?? 0;
queueMicrotask(() => handle.scrollTo(scroll));
```

## Theming contract

Themes (`themes/light.ts`, `themes/dark.ts`) reference codesense CSS variables
at runtime so theme swaps are automatic. The editor reads:

- `var(--background)`, `var(--foreground)`, `var(--muted)`, `var(--border)`,
  `var(--accent)` for chrome.
- (Syntax tokens: defined inline in themes today; future codesense additions
  should land in `src/index.css`.)

The editor root also sets `color-scheme: light` or `color-scheme: dark` so
native scrollbars / selection / caret follow the theme without a frame of
mismatch.

## Save / conflict / dirty contract

- **Cmd+S** (focused editor) — fires `onSave(currentValue)`. Returns the
  in-flight save promise if one is unresolved (dedup).
- **Blur** — fires `onSave(currentValue)` IFF `dirty === true`.
- **Conflict detection** — host owns the watcher subscription. When a
  `layer-content-change` arrives, host calls `handle.checkConflict(diskBytes)`
  which returns `true` if `diskBytes !== getValue()`. Host renders banner.
- **Dirty indicator** — host renders it. The editor reads `dirty` only to
  gate blur-save.

## Surfaces NOT in scope (and where to add them)

- **Read-only banner UI** — host owns it (e.g. workbench's "🔒 CodeSense Core
  — read-only. [Fork to ...]" banner). The editor only reflects ARIA state.
- **Find / Replace** — `@codemirror/search` is not installed. Consumers
  needing in-editor find/replace add it via `customExtensions`.
- **Diff / merge view** — shipped by SS-15 as `<CodeMergeView>`. See below.
- **LSP / IntelliSense / Linting on save** — not warranted yet.

## Caveats

- **Empty buffers** — surfaces that may open empty files should always pass
  a `placeholder`; otherwise users see a blank pane and can't tell whether
  it loaded.
- **Large pastes** — no paste-size guard. CM6 handles multi-MB pastes but
  typing latency degrades. Surfaces with very large clipboard content should
  warn or pre-truncate.

## `<CodeMergeView>` — side-by-side merge view

Shared merge component for conflict resolution. Uses `@codemirror/merge`'s
split `MergeView` (disk on the left, local on the right). First consumer:
the install workbench's conflict-resolution flow (SS-15).

### Quick start

```tsx
import { CodeMergeView } from "@/shared/components/code-editor";

<CodeMergeView
  diskValue={diskContent}
  localValue={localContent}
  onSave={async (merged) => {
    /* write merged content */
  }}
  path="skills/foo.md"
/>;
```

### Public API

- **`diskValue: string`** — read-only left pane (the "theirs" / disk side).
- **`localValue: string`** — editable right pane (the "mine" / local side).
  This is the resolved buffer that gets saved.
- **`onSave?(mergedValue): Promise<void>`** — fires on Cmd+S. Receives the
  editable side's current doc. Rejection is swallowed (host owns error UX).
- **`onLoadError?(err)`** — fires on CM6 chunk-load failure. Renders
  "Merge view failed to load. [Retry]" inline.
- **`path?`** — drives language detection for syntax highlighting.
- **`theme?: "light" | "dark" | "auto"`** — default `"auto"`.
- **`fontSize?: number`** — same fallback chain as `<CodeEditor>`.
- **`gutters?: boolean`** — default `true`.

### Imperative handle

```ts
interface CodeMergeViewHandle {
  getLocalValue(): string;
  getDiskValue(): string;
  setLocalValue(next: string): void;
  setDiskValue(next: string): void;
  goToNextChunk(): boolean;
  goToPreviousChunk(): boolean;
  getChunkCount(): number;
}
```

All methods are null-safe after unmount (reads return defaults, writes no-op).

### Render states

1. **Loading** — skeleton with `aria-busy="true"`.
2. **Error** — "Merge view failed to load. [Retry]" + `onLoadError` callback.
3. **Ready** — live `MergeView` with per-chunk revert arrows (disk → local).

### MergeView configuration

- `orientation: "a-b"` — disk left, local right.
- `revertControls: "a-to-b"` — per-chunk arrows copy disk chunks INTO local.
- `highlightChanges: true`, `gutter: true`.
- `collapseUnchanged: { margin: 3, minSize: 4 }`.
- Keyboard: `Alt-ArrowDown` / `Alt-ArrowUp` for chunk navigation.

### Save flow

Save routes through the host's existing `handleSave` callback — no parallel
write path. The merge view exposes the editable side's value; the host reads
it via `handle.getLocalValue()` or receives it in `onSave(mergedValue)`.

### Tab-switch / Cancel / Wave behavior

- **Tab switch** during merge = silent Cancel (merge work lost, same as
  dirty-editor tab switch in the regular editor).
- **Cancel** returns to the regular editor with the conflict banner
  re-appearing (pending conflict NOT cleared).
- **New wave** during merge updates the disk pane in place via
  `handle.setDiskValue()`; surfaces an informational toast.

## Extension authoring

Surface-specific behaviors compose via `customExtensions`:

```tsx
import { EditorView } from "@codemirror/view";
import { highlightSelectionMatches } from "@codemirror/search";

<CodeEditor
  // ...
  customExtensions={[
    highlightSelectionMatches(),
    EditorView.theme({ ".cm-selectionMatch": { backgroundColor: "#ff05" } }),
  ]}
/>;
```

The CM6 extension model composes left-to-right; SS-14's defaults run first,
then `customExtensions`, so consumers can override theme rules / keybindings
that conflict.
