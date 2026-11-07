// SPDX-License-Identifier: MIT
import type { Extension, StateEffectType } from "@codemirror/state";
import type { EditorView as CmEditorView, ViewUpdate } from "@codemirror/view";

import type { CmModule } from "../cm-loader";
import { computeLineChanges, type LineChange } from "../line-changes";

/**
 * The change gutter: a thin rail beside the line numbers marking lines added,
 * modified or deleted since the file's committed version, plus an overview
 * ruler down the right edge that places the same marks (and the cursor) along
 * the whole file so off-screen changes are findable at a glance.
 *
 * The baseline arrives through an effect (the host fetches `HEAD` after the
 * file opens), and the diff recomputes on a short debounce after edits — the
 * marks trail typing by a beat rather than costing every keystroke.
 */

export interface ChangeGutter {
  /** The gutter + ruler; compose once per editor. */
  extension: Extension;
  /** Dispatch with the committed text, or `null` to clear the markers. */
  setBaseline: StateEffectType<string | null>;
  /** Read the current markers (tests, status readouts). */
  changesOf: (view: CmEditorView) => readonly LineChange[];
}

interface ChangeState {
  baseline: string | null;
  changes: readonly LineChange[];
}

const RECOMPUTE_DELAY_MS = 150;

// One ChangeGutter per loaded CodeMirror module: effect types and the state
// field must be shared by every editor built from it.
const cache = new WeakMap<CmModule, ChangeGutter>();

export function getChangeGutter(cm: CmModule): ChangeGutter {
  const hit = cache.get(cm);
  if (hit) return hit;

  const setBaseline = cm.StateEffect.define<string | null>();
  const setChanges = cm.StateEffect.define<readonly LineChange[]>();

  const field = cm.StateField.define<ChangeState>({
    create: () => ({ baseline: null, changes: [] }),
    update(value, tr) {
      let next = value;
      for (const effect of tr.effects) {
        if (effect.is(setBaseline)) next = { baseline: effect.value, changes: [] };
        else if (effect.is(setChanges)) next = { ...next, changes: effect.value };
      }
      return next;
    },
  });

  class ChangeMarker extends cm.GutterMarker {
    constructor(readonly kind: LineChange["kind"]) {
      super();
    }
    override eq(other: ChangeMarker) {
      return other.kind === this.kind;
    }
    override toDOM() {
      const el = document.createElement("div");
      el.className = `cm-change-mark cm-change-${this.kind}`;
      el.setAttribute("aria-hidden", "true");
      return el;
    }
  }
  // Reserves the rail's width without drawing a mark.
  class SpacerMarker extends cm.GutterMarker {
    override toDOM() {
      const el = document.createElement("div");
      el.style.width = "3px";
      return el;
    }
  }
  const markers = {
    added: new ChangeMarker("added"),
    modified: new ChangeMarker("modified"),
    deleted: new ChangeMarker("deleted"),
  };

  const recompute = cm.ViewPlugin.fromClass(
    class {
      timer: ReturnType<typeof setTimeout> | null = null;
      constructor(readonly view: CmEditorView) {}
      update(update: ViewUpdate) {
        const baselineChanged = update.transactions.some((tr) =>
          tr.effects.some((e) => e.is(setBaseline)),
        );
        if (!baselineChanged && !update.docChanged) return;
        this.schedule(baselineChanged ? 0 : RECOMPUTE_DELAY_MS);
      }
      schedule(delay: number) {
        if (this.timer) clearTimeout(this.timer);
        this.timer = setTimeout(() => {
          this.timer = null;
          const { baseline } = this.view.state.field(field);
          const changes =
            baseline == null ? [] : computeLineChanges(baseline, this.view.state.doc.toString());
          this.view.dispatch({ effects: setChanges.of(changes) });
        }, delay);
      }
      destroy() {
        if (this.timer) clearTimeout(this.timer);
      }
    },
  );

  const gutter = cm.gutter({
    class: "cm-change-gutter",
    markers: (view) => {
      const { changes } = view.state.field(field);
      const doc = view.state.doc;
      const ranges = [];
      for (const change of changes) {
        if (change.line < 1 || change.line > doc.lines) continue;
        ranges.push(markers[change.kind].range(doc.line(change.line).from));
      }
      return cm.RangeSet.of(ranges, true);
    },
    initialSpacer: () => new SpacerMarker(),
  });

  const ruler = cm.ViewPlugin.fromClass(
    class {
      dom: HTMLElement;
      constructor(readonly view: CmEditorView) {
        this.dom = document.createElement("div");
        this.dom.className = "cm-overview-ruler";
        this.dom.setAttribute("aria-hidden", "true");
        view.dom.appendChild(this.dom);
        this.render();
      }
      update(update: ViewUpdate) {
        if (
          update.docChanged ||
          update.selectionSet ||
          update.geometryChanged ||
          update.startState.field(field) !== update.state.field(field)
        ) {
          this.render();
        }
      }
      render() {
        const { changes } = this.view.state.field(field);
        const lines = Math.max(1, this.view.state.doc.lines);
        const cursorLine = this.view.state.doc.lineAt(this.view.state.selection.main.head).number;
        // Coalesce into percentage rows so a file with thousands of changed
        // lines paints a bounded number of marks.
        const rows = new Map<number, LineChange["kind"]>();
        for (const change of changes) {
          // Changes computed before lines were deleted can point past the end
          // until they are recomputed: no mark below the ruler.
          if (change.line < 1 || change.line > lines) continue;
          const row = Math.floor(((change.line - 1) / lines) * 200);
          const prior = rows.get(row);
          if (
            !prior ||
            change.kind === "deleted" ||
            (prior === "added" && change.kind === "modified")
          ) {
            rows.set(row, change.kind);
          }
        }
        const frag = document.createDocumentFragment();
        for (const [row, kind] of rows) {
          const mark = document.createElement("div");
          mark.className = `cm-overview-mark cm-change-${kind}`;
          mark.style.top = `${row / 2}%`;
          frag.appendChild(mark);
        }
        const cursor = document.createElement("div");
        cursor.className = "cm-overview-cursor";
        cursor.style.top = `${((cursorLine - 1) / lines) * 100}%`;
        frag.appendChild(cursor);
        this.dom.replaceChildren(frag);
        this.dom.hidden = changes.length === 0 && lines < 40;
      }
      destroy() {
        this.dom.remove();
      }
    },
  );

  const result: ChangeGutter = {
    extension: [field, recompute, gutter, ruler],
    setBaseline,
    changesOf: (view) => view.state.field(field, false)?.changes ?? [],
  };
  cache.set(cm, result);
  return result;
}
