// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import { buildConnectivityView } from "./build-connectivity-view";
import type { DocSummary, RelRef } from "../doc-query/types";

const A = "/ctx/epic-a";
const B = "/ctx/epic-b";

function ref(path: string, resolved = true): RelRef {
  const fileName = path.split("/").pop()!;
  return { id: fileName, path, fileName, resolved };
}

function doc(
  path: string,
  o: {
    documentType?: string | null;
    status?: string | null;
    forward?: RelRef[];
    backlinks?: RelRef[];
  } = {},
): DocSummary {
  const fileName = path.split("/").pop()!;
  return {
    id: fileName,
    path,
    fileName,
    tags: {},
    status: o.status ?? null,
    documentType: o.documentType ?? "story",
    mtime: null,
    sourceRepo: null,
    sprint: null,
    points: null,
    assignedTo: null,
    relationships: {
      supportingDocs: o.forward ?? [],
      backlinks: o.backlinks ?? [],
      dependsOn: [],
    },
  };
}

describe("buildConnectivityView", () => {
  it("returns one row per in-scope doc; unknown scope → empty (AC-1)", () => {
    const docs = [doc(`${A}/a1.md`), doc(`${B}/b1.md`)];
    const v = buildConnectivityView(docs, A);
    expect(v.rows.map((r) => r.story.path)).toEqual([`${A}/a1.md`]);
    expect(v.scope).toBe(A);

    const empty = buildConnectivityView(docs, "/ctx/nope");
    expect(empty.rows).toEqual([]);
    expect(empty.orphans).toEqual([]);
    expect(empty.summary.storyCount).toBe(0);
  });

  it("forward + in-scope backlinks; a mutual link renders once as both (AC-2)", () => {
    const docs = [
      doc(`${A}/a1.md`, { forward: [ref(`${A}/a2.md`)], backlinks: [ref(`${A}/a2.md`)] }),
      doc(`${A}/a2.md`, { forward: [ref(`${A}/a1.md`)], backlinks: [ref(`${A}/a1.md`)] }),
    ];
    const v = buildConnectivityView(docs, A);
    const a1 = v.rows.find((r) => r.story.fileName === "a1.md")!;
    expect(a1.forward).toHaveLength(1);
    expect(a1.forward[0]).toMatchObject({ fileName: "a2.md", direction: "both" });
    // The mutual edge lives under forward, NOT duplicated under backlinks.
    expect(a1.backlinks).toHaveLength(0);
  });

  it("HTML story with an HTML backlink resolves (AC-2 golden)", () => {
    const docs = [
      doc(`${A}/center.html`, { documentType: "story" }),
      doc(`${A}/neighbor.html`, { forward: [ref(`${A}/center.html`)] }),
    ];
    // neighbor lists center → center is backlinked by neighbor.
    docs[0].relationships.backlinks = [ref(`${A}/neighbor.html`)];
    const v = buildConnectivityView(docs, A);
    const center = v.rows.find((r) => r.story.fileName === "center.html")!;
    expect(center.backlinks.map((l) => l.fileName)).toEqual(["neighbor.html"]);
  });

  it("health keys off resolved forward only: desert / thin / healthy (AC-4)", () => {
    const docs = [
      // 0 forward but HAS a backlink → still context-desert (built blind).
      doc(`${A}/desert.md`, { backlinks: [ref(`${A}/healthy.md`)] }),
      // 1 resolved forward → thin (< default threshold 2).
      doc(`${A}/thin.md`, { forward: [ref(`${A}/arch.md`, true)] }),
      // 2 resolved forward → healthy.
      doc(`${A}/healthy.md`, { forward: [ref(`${A}/desert.md`), ref(`${A}/arch.md`)] }),
      // only a DEAD ref → 0 resolved forward → desert (+1 unresolved).
      doc(`${A}/broken.md`, { forward: [ref(`${A}/ghost.md`, false)] }),
    ];
    const v = buildConnectivityView(docs, A);
    const h = (n: string) => v.rows.find((r) => r.story.fileName === n)!.health;
    expect(h("desert.md")).toBe("context-desert");
    expect(h("thin.md")).toBe("thin");
    expect(h("healthy.md")).toBe("healthy");
    expect(h("broken.md")).toBe("context-desert");
    expect(v.summary.contextDesertCount).toBe(2);
    expect(v.summary.thinCount).toBe(1);
    expect(v.summary.healthyCount).toBe(1);
    expect(v.summary.unresolvedCount).toBe(1);
  });

  it("orphan detection: island, linked, and out-of-scope-only (AC-5)", () => {
    const docs = [
      // island: links nothing in-scope, nothing links it.
      doc(`${A}/island.md`),
      // linked: nothing forward, but backlinked in-scope → NOT an orphan.
      doc(`${A}/linked.md`, { backlinks: [ref(`${A}/linker.md`)] }),
      doc(`${A}/linker.md`, { forward: [ref(`${A}/linked.md`)] }),
      // only forward-links OUT of scope → still an orphan FOR THIS SCOPE.
      doc(`${A}/outbound.md`, { forward: [ref(`${B}/b1.md`)] }),
    ];
    const v = buildConnectivityView(docs, A);
    const orphanNames = v.orphans.map((o) => o.fileName).sort();
    expect(orphanNames).toEqual(["island.md", "outbound.md"]);
    expect(v.summary.orphanCount).toBe(2);
  });

  it("scope bounds the backlink fan (AC-6)", () => {
    const docs = [
      doc(`${A}/a4.md`, { backlinks: [ref(`${A}/a1.md`), ref(`${B}/b1.md`)] }),
      doc(`${A}/a1.md`, { forward: [ref(`${A}/a4.md`)] }),
      doc(`${B}/b1.md`, { forward: [ref(`${A}/a4.md`)] }),
    ];
    const v = buildConnectivityView(docs, A);
    const a4 = v.rows.find((r) => r.story.fileName === "a4.md")!;
    // b1 is in sub-epic B → NOT counted as an in-scope backlink for a4.
    expect(a4.backlinks.map((l) => l.fileName)).toEqual(["a1.md"]);
  });

  it("full-file forward target (a leaf) counts as context + groups by type (AC-4/7)", () => {
    const docs = [
      doc(`${A}/a1.md`, {
        forward: [ref(`${A}/diagram.excalidraw`), ref(`${A}/mock.png`)],
      }),
    ];
    const v = buildConnectivityView(docs, A);
    const a1 = v.rows[0];
    expect(a1.forwardCount).toBe(2); // both resolved non-story files
    expect(a1.health).toBe("healthy");
    expect(Object.keys(a1.groups).sort()).toEqual(["canvas", "image"]);
    expect(a1.groups.canvas[0].fileName).toBe("diagram.excalidraw");
  });

  it("respects a custom thinThreshold", () => {
    const docs = [doc(`${A}/a.md`, { forward: [ref(`${A}/x.md`), ref(`${A}/y.md`)] })];
    expect(buildConnectivityView(docs, A, { thinThreshold: 3 }).rows[0].health).toBe("thin");
    expect(buildConnectivityView(docs, A).rows[0].health).toBe("healthy");
  });
});
