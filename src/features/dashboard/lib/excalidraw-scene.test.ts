// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";

import { sceneForLoad } from "./excalidraw-scene";

const PNG = "data:image/png;base64,iVBORw0KGgo=";

describe("sceneForLoad", () => {
  it("drops an iframe element's generated HTML and keeps the rest of the element", () => {
    const iframe = {
      id: "f",
      type: "iframe",
      customData: {
        generationData: { status: "done", html: '<img src="https://example.com/a.png">' },
        note: "kept",
      },
    };
    const embed = { id: "e", type: "embeddable", customData: { generationData: "untouched" } };
    const { elements } = sceneForLoad({ elements: [iframe, embed], appState: {}, files: {} });
    expect(elements[0]).toEqual({ id: "f", type: "iframe", customData: { note: "kept" } });
    // Only iframe elements are rendered from generationData.
    expect(elements[1]).toBe(embed);
  });

  it("keeps data: image files and drops any other dataURL", () => {
    const scene = sceneForLoad({
      elements: [],
      appState: {},
      files: {
        png: { id: "png", mimeType: "image/png", dataURL: PNG },
        upper: { id: "upper", mimeType: "image/png", dataURL: "DATA:IMAGE/PNG;base64,AA==" },
        remote: { id: "remote", mimeType: "image/png", dataURL: "https://example.com/b.png" },
        html: { id: "html", mimeType: "image/png", dataURL: "data:text/html,<img src=x>" },
        missing: { id: "missing", mimeType: "image/png" },
      },
    });
    expect(Object.keys(scene.files)).toEqual(["png", "upper"]);
    expect(scene.files.png).toEqual({ id: "png", mimeType: "image/png", dataURL: PNG });
  });
});
