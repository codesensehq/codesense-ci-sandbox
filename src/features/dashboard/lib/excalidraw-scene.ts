// SPDX-License-Identifier: MIT
/**
 * Pure Excalidraw scene parsing — the leaf SC-1's viewer and SC-13's exporter
 * both build on. Deliberately free of any React or `@excalidraw/excalidraw`
 * import: the SC-13 export handler is reached from the eagerly-bundled nav, so
 * importing scene-parsing from here can never drag the lazy engine chunk into
 * the main bundle (SW-SC-13 AC-6 / Finding E). `excalidraw-viewer.tsx`
 * re-exports these symbols so its existing callers are unaffected.
 */

/**
 * The parsed Excalidraw scene a canvas mounts from. Kept structurally loose
 * here (the wrapper never touches Excalidraw's runtime types) — the canvas
 * chunk casts to the real types when it hands them to the engine.
 */
export type ExcalidrawScene = {
  elements: readonly unknown[];
  appState: Record<string, unknown>;
  files: Record<string, unknown>;
};

export const BLANK_SCENE: ExcalidrawScene = { elements: [], appState: {}, files: {} };

/**
 * Parse `.excalidraw` JSON into a scene. An empty / zero-byte file is a
 * valid blank canvas (AC-3); malformed JSON throws and the caller decides
 * how to surface it.
 */
export function parseExcalidrawScene(text: string): ExcalidrawScene {
  if (text.trim() === "") return BLANK_SCENE;
  const parsed: unknown = JSON.parse(text);
  // Valid JSON but not a scene object (e.g. `42`, `"x"`, `null`, an array):
  // treat as malformed so the caller surfaces a toast rather than silently
  // blanking — and overwriting — a file the user may want to recover.
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("excalidraw scene is not a JSON object");
  }
  const json = parsed as Partial<ExcalidrawScene>;
  return {
    elements: Array.isArray(json.elements) ? json.elements : [],
    appState:
      json.appState && typeof json.appState === "object"
        ? (json.appState as Record<string, unknown>)
        : {},
    files:
      json.files && typeof json.files === "object" ? (json.files as Record<string, unknown>) : {},
  };
}

const DATA_IMAGE_URL = /^data:image\//i;

/**
 * The scene a canvas mounts or reloads, with the parts that would reach the
 * network removed. An `iframe` element's `customData.generationData` is HTML
 * that Excalidraw renders in a frame, which loads what it references; without
 * it Excalidraw shows its inert "No generation data" placeholder. A file whose
 * `dataURL` is not a `data:image/` URL would make the canvas fetch it, so it
 * is dropped.
 */
export function sceneForLoad(scene: ExcalidrawScene): ExcalidrawScene {
  return {
    ...scene,
    elements: scene.elements.map(withoutGenerationData),
    files: Object.fromEntries(Object.entries(scene.files).filter(([, f]) => isDataImageFile(f))),
  };
}

function withoutGenerationData(element: unknown): unknown {
  const el = element as { type?: unknown; customData?: unknown } | null;
  if (el?.type !== "iframe" || !el.customData || typeof el.customData !== "object") return element;
  if (!("generationData" in el.customData)) return element;
  const { generationData: _dropped, ...customData } = el.customData as Record<string, unknown>;
  return { ...el, customData };
}

function isDataImageFile(file: unknown): boolean {
  const dataURL = (file as { dataURL?: unknown } | null)?.dataURL;
  return typeof dataURL === "string" && DATA_IMAGE_URL.test(dataURL);
}
