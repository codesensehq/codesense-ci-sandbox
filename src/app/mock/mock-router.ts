// SPDX-License-Identifier: MIT
/**
 * The mock backend's single dispatch point. Every `invoke(cmd, args)` the app
 * makes in mock mode arrives here (routed through the `window.__TAURI_INTERNALS__`
 * shim). Two kinds of command land:
 *
 * - **Business commands** (snake_case, e.g. `load_orgs`, `create_team`) carry the
 *   real backend's behavior. An unmapped one is a LOUD stub — it throws
 *   {@link NotMockedError} naming the command — so an author notices the gap and
 *   fills one fixture, rather than a silent `undefined` faking success.
 * - **Plugin surfaces** (`plugin:*`, e.g. `plugin:window|set_title`) are Tauri
 *   runtime boundaries — window, webview, opener, dialog, event. These are
 *   intentionally inert in mock mode (the honest-boundaries contract), so an
 *   unmapped one is a benign no-op, never a loud stub.
 */

export type CommandHandler = (args: Record<string, unknown>) => unknown;
export type HandlerRegistry = Record<string, CommandHandler>;

/** The single seam every backend request funnels through: `window.__TAURI_INTERNALS__.invoke`. */
export type InvokeRouter = (cmd: string, args?: Record<string, unknown>) => Promise<unknown>;

export class NotMockedError extends Error {
  readonly command: string;
  constructor(command: string) {
    super(`not mocked: ${command}`);
    this.name = "NotMockedError";
    this.command = command;
  }
}

/** Benign defaults for the few plugin surfaces whose callers expect a concrete
 *  return value. `plugin:event|listen` must resolve to a numeric event id or the
 *  listener wrapper rejects; everything else is fine resolving `undefined`. */
let nextListenerId = 1;
const pluginDefaults: HandlerRegistry = {
  "plugin:event|listen": () => nextListenerId++,
  // `homeDir()` (the CodeSense home is `${home}/.codesense`); the mock never touches disk.
  "plugin:path|resolve_directory": () => "/mock",
};

export function createInvokeRouter(handlers: HandlerRegistry): InvokeRouter {
  return (cmd, args = {}) => {
    try {
      const handler = handlers[cmd];
      if (handler) return Promise.resolve(handler(args));

      if (cmd.startsWith("plugin:")) {
        const pluginDefault = pluginDefaults[cmd];
        return Promise.resolve(pluginDefault ? pluginDefault(args) : undefined);
      }

      throw new NotMockedError(cmd);
    } catch (err) {
      return Promise.reject(err);
    }
  };
}
