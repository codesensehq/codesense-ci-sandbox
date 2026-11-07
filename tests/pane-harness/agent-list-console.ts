// SPDX-License-Identifier: MIT
import type { Page } from "@playwright/test";

export function capturePaneErrors(page: Page) {
  const errors: string[] = [];
  const messages: Array<{ type: string; text: string; url: string }> = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    const text = message.text();
    messages.push({ type: message.type(), text, url: message.location().url });
    // WebKit logs early host messages aimed at the still-about:blank cross-origin frame.
    // Keep those browser diagnostics in the capture; all script errors still fail.
    const initialOriginWarning =
      /^Unable to post message to http:\/\/(?:127\.0\.0\.1|f-[a-f0-9]{32}\.localhost):\d+\. Recipient has origin http:\/\/localhost:\d+\.\s*$/.test(
        text,
      );
    if (message.type() === "error" && !initialOriginWarning) errors.push(text);
  });
  return { errors, messages };
}
