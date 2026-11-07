// SPDX-License-Identifier: MIT
import type { Frame, Page } from "@playwright/test";
import { toHexColor } from "../../src/shared/lib/color-contrast";

/** Resolve CSS colours in the document that rendered them, retaining alpha. */
export async function hex(context: Frame | Page, colors: string[], under?: string) {
  const channels = await context.evaluate(
    ({ colors, under }) => {
      const canvas = document.createElement("canvas");
      canvas.width = canvas.height = 1;
      const ctx = canvas.getContext("2d")!;
      return colors.map((value) => {
        ctx.clearRect(0, 0, 1, 1);
        if (under) {
          ctx.fillStyle = under;
          ctx.fillRect(0, 0, 1, 1);
        }
        ctx.fillStyle = value;
        ctx.fillRect(0, 0, 1, 1);
        return [...ctx.getImageData(0, 0, 1, 1).data];
      });
    },
    { colors, under },
  );
  return channels.map(
    (c) =>
      toHexColor(c.slice(0, 3) as [number, number, number]) + c[3].toString(16).padStart(2, "0"),
  );
}
