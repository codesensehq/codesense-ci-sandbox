import type { FrameGrant, FrameRecord, MintFrameArgs } from "../../../src/tauri/commands";

export function frameFixture(args: MintFrameArgs, port = 9123): FrameRecord {
  return {
    ...(args.parentFrame ? { parentFrame: args.parentFrame } : {}),
    kind:
      args.message || args.slot === "message"
        ? "message"
        : args.builtinTeam ||
            args.documentPath.startsWith("builtin:") ||
            args.documentPath.startsWith("virtual:")
          ? "builtin"
          : "private",
    frameId: "test",
    origin: `http://f-test.localhost:${port}`,
    token: "frame-token",
    grant: {
      frameId: "test",
      principal: "human",
      dashboard:
        args.slot === "right-pane" || args.slot === "dock"
          ? `dashboard:private:fixture/${args.documentPath
              .split("/")
              .pop()
              ?.replace(/\.html$/, "")}`
          : args.documentPath,
      slot: args.slot,
      team: args.builtinTeam ?? null,
      scope: "open",
    },
    contentRoot: args.contentRoot,
    documentPath: args.documentPath,
    assetFolder: args.documentPath.slice(0, args.documentPath.lastIndexOf("/")),
    policy: {},
    message: args.message ?? null,
  };
}
export function frameCommands(port = 9123) {
  let grant: FrameGrant;
  return {
    mintFrame: async (args: MintFrameArgs) => {
      const frame = frameFixture(args, port);
      grant = frame.grant;
      return frame;
    },
    releaseFrame: async () => {},
    liveHubViewKey: async () => "all",
    resolveFrameGrant: async () => grant,
  };
}
const MessageEventBase: new (type: string, init?: MessageEventInit) => MessageEvent = MessageEvent;
export class FrameMessageEvent extends MessageEventBase {
  constructor(type: string, init: MessageEventInit = {}) {
    super(type, {
      origin: "http://f-test.localhost:9123",
      ...init,
      data: { token: "frame-token", ...init.data },
    });
  }
}

export function frameIpc(
  command: string,
  args: Record<string, unknown> | undefined,
  port: number,
): unknown {
  if (command === "mint_frame")
    return Promise.resolve(frameFixture(args?.args as MintFrameArgs, port));
  if (command === "release_frame") return Promise.resolve();
  if (command === "live_hub_view_key") return Promise.resolve("all");
  if (command === "resolve_frame_grant")
    return Promise.resolve(
      frameFixture({ documentPath: "", contentRoot: "", slot: "centre" }, port).grant,
    );
  return undefined;
}
export function frameEventClass(port: number) {
  return class extends FrameMessageEvent {
    constructor(type: string, init: MessageEventInit = {}) {
      super(type, { origin: `http://f-test.localhost:${port}`, ...init });
    }
  };
}
