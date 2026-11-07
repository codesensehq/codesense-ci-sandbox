// SPDX-License-Identifier: MIT
/**
 * Read a Blob into a base64 string with the `data:<mime>;base64,` prefix
 * stripped — the shape `writeBinaryFile` / `writeBinaryContentFile` expect.
 * Shared by the canvas's derived-render save (SW-SC-2) and the right-click
 * PNG/SVG export (SW-SC-13); engine-free so either side can import it.
 */
export function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onloadend = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      const comma = result.indexOf(",");
      resolve(comma >= 0 ? result.slice(comma + 1) : result);
    };
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read blob"));
    reader.readAsDataURL(blob);
  });
}
