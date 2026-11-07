// SPDX-License-Identifier: MIT
import { fireEvent } from "@testing-library/react";
import { Window as DocWindow } from "happy-dom";
import { frameEventClass, frameIpc } from "../../../../../tests/support/fixtures/frame";
import { shimSource } from "../../../../../tests/support/page-shim";
import type { PaneListing } from "@/tauri/commands";

const SHIM = shimSource();

export interface PaneDoc {
  sent: Record<string, unknown>[];
  win: DocWindow & { codesense: Record<string, (...a: unknown[]) => Promise<unknown>> };
  doc: Document;
}

/** Run `html` behind the real shim as the document of the pane's iframe. */
export function bootPaneDoc(
  frame: HTMLIFrameElement,
  html: string,
  identity = { origin: "http://f-test.localhost:9322", token: "frame-token" },
  child = false,
  options: { beforeScripts?: (win: DocWindow) => void; shim?: string } = {},
): PaneDoc {
  const PaneMessageEvent = frameEventClass(Number(new URL(identity.origin).port));
  const win = new DocWindow({
    url: frame.getAttribute("src") ?? identity.origin,
    settings: {
      enableJavaScriptEvaluation: true,
      suppressInsecureJavaScriptEnvironmentWarning: true,
    } as never,
  });
  const clone = (data: unknown) => JSON.parse(JSON.stringify(data)) as unknown;
  const sent: Record<string, unknown>[] = [];
  const parentProxy = {
    postMessage: (data: unknown) => {
      sent.push(clone(data) as Record<string, unknown>);
      setTimeout(() =>
        window.dispatchEvent(
          new PaneMessageEvent("message", {
            data: clone(data),
            source: childProxy as unknown as Window,
            origin: identity.origin,
          }),
        ),
      );
    },
  };
  const childProxy = {
    postMessage: (data: unknown) =>
      setTimeout(() =>
        win.dispatchEvent(
          new win.MessageEvent("message", {
            data: clone(data),
            source: parentProxy as never,
            origin: "tauri://localhost",
          }) as never,
        ),
      ),
  };
  Object.defineProperty(frame, "contentWindow", { value: childProxy, configurable: true });
  (win as unknown as { parent: unknown }).parent = parentProxy;
  if (child) Object.defineProperty(win, "top", { value: parentProxy, configurable: true });
  options.beforeScripts?.(win);
  const shim = options.shim ?? SHIM;
  const focusDescriptor = Object.getOwnPropertyDescriptor(win.HTMLElement.prototype, "focus");
  win.document.write(
    `<script>window.__codesenseProxy={token:${JSON.stringify(identity.token)}}</script><script>${child ? shim.split("window.parent").join("window.top") : shim}</script>${html}`,
  );
  // happy-dom DocWindows share HTMLElement.prototype, unlike isolated browser iframe realms.
  if (focusDescriptor) Object.defineProperty(win.HTMLElement.prototype, "focus", focusDescriptor);
  return { win: win as PaneDoc["win"], doc: win.document as unknown as Document, sent };
}

export function listing(options: {
  org: string;
  team: string;
  root: string;
  id: string;
  title: string;
  privateCopy?: boolean;
  surface?: string;
  present?: boolean;
}): PaneListing {
  const { org, team, root, id, title, privateCopy = false, surface, present = true } = options;
  const scope = privateCopy ? "private" : "team";
  const panesDir = `${root}/${privateCopy ? "private" : team}/panes`;
  return {
    contentRoot: `${root}/${team}/content`,
    chain: [team],
    panes: present
      ? [
          {
            id,
            title,
            dashboardRef: { scope, org, ...(privateCopy ? {} : { team }), id },
            scope,
            ...(privateCopy ? { surface } : { team }),
            path: `${panesDir}/${id}.html`,
            panesDir,
            panelId: privateCopy ? `pane:private:${org}/${id}` : `pane:${org}:${team}/${id}`,
          },
        ]
      : [],
  };
}

export function stubTauri(
  cmd: string,
  args: Record<string, unknown> | undefined,
  port: number,
  handle: (cmd: string, args: Record<string, unknown> | undefined) => unknown,
) {
  if (["mint_frame", "resolve_frame_grant", "release_frame"].includes(cmd))
    return handle(cmd, args);
  const frame = frameIpc(cmd, args, port);
  return frame !== undefined ? frame : handle(cmd, args);
}

export const el = (pane: PaneDoc, id: string) => pane.doc.getElementById(id)!;
export function type(pane: PaneDoc, value: string, id = "draft") {
  fireEvent.input(el(pane, id), { target: { value } });
}
export async function pick(
  pane: PaneDoc,
  name: string,
  trigger = "recipient",
  options = "#picker-options button",
) {
  fireEvent.click(el(pane, trigger));
  const option = [...pane.doc.querySelectorAll<HTMLElement>(options)].find((row) =>
    row.textContent?.includes(name),
  );
  if (!option) throw new Error(`No option named ${name}`);
  fireEvent.click(option);
}
