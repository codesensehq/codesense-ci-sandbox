// SPDX-License-Identifier: MIT
/** The page shim every live doc gets, read from the files the viewer server injects. */
import { readFileSync } from "node:fs";
import { join } from "node:path";

const ROUTES = join(process.cwd(), "src-tauri/src/http/routes");

/** A raw-string script constant in viewer.rs, e.g. `LINK_INTERCEPTOR_JS`. */
export function viewerScript(name: string, root = process.cwd()): string {
  const viewer = readFileSync(join(root, "src-tauri/src/http/routes/viewer.rs"), "utf8");
  const found = viewer.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`));
  if (!found) throw new Error(`${name} not found in viewer.rs`);
  return found[1];
}

/** The shim, data reads, live hub and verb methods, in injection order. */
export function shimSource(root = process.cwd()): string {
  const routes = join(root, "src-tauri/src/http/routes");
  return [
    viewerScript("LINK_INTERCEPTOR_JS", root),
    viewerScript("DATA_READS_JS", root),
    readFileSync(join(routes, "live_hub.js"), "utf8"),
    readFileSync(join(routes, "verb_methods.generated.js"), "utf8"),
  ].join("\n");
}

/** The kit scripts `kit_tags()` links after the shim. */
function kitSource(): string {
  return ["kit.js", "virtual-list.js"]
    .map((file) => readFileSync(join(ROUTES, "dashboard_kit", file), "utf8"))
    .join("\n");
}

/**
 * Runs the shim (and the kit) in this window with a stub parent. `deliver` posts a
 * host message to the page as the parent would.
 */
export function loadShim(options: { kit?: boolean } = {}) {
  const posted: Record<string, unknown>[] = [];
  const parent = { postMessage: (message: Record<string, unknown>) => posted.push(message) };
  Object.defineProperty(window, "parent", { value: parent, configurable: true });
  const win = window as unknown as Record<string, unknown>;
  win.__codesenseProxy = { token: "shim-test" };
  delete win.codesense;
  new Function(shimSource() + (options.kit ? `\n${kitSource()}` : ""))();
  const deliver = (data: Record<string, unknown>) =>
    window.dispatchEvent(
      new MessageEvent("message", {
        data: { token: "shim-test", ...data },
        source: parent as unknown as Window,
      }),
    );
  return { codesense: win.codesense as Record<string, unknown>, posted, deliver };
}
