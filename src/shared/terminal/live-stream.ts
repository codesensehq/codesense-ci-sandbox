// SPDX-License-Identifier: MIT
/**
 * Listen before subscribe: a terminal's live stream, opened so that no event
 * can land between the subscribe reply and the listener. The stream id is
 * reserved first, the listener attached on it, and only then is the session
 * subscribed on that id. Events that arrive before the caller has written the
 * snapshot are held; `begin` drops the ones the snapshot already holds (output
 * with `seq <= snapshot.seq`, resizes with `resize_gen <= snapshot.resize_gen`)
 * and applies the rest in arrival order, then every later event as it arrives.
 */
import { acceptsPrivateAuthority } from "@/shared/lib/private-terminal-authority";
import type { TerminalStreamEvent, TerminalSubscription } from "@/tauri/commands";

/** One event of a live stream, as the display applies it. */
export type LiveItem =
  | { kind: "output"; data: string; seq: number }
  | { kind: "resized"; cols: number; rows: number; resizeGen: number };

export interface LiveStreamTransport {
  reserve(): Promise<number>;
  listen(channel: string, onEvent: (event: TerminalStreamEvent) => void): Promise<() => void>;
  subscribe(streamId: number): Promise<TerminalSubscription>;
  /** Detach the stream, or release a reservation that was never bound. */
  unsubscribe(streamId: number): void;
}

export interface LiveStream {
  readonly streamId: number;
  readonly subscription: TerminalSubscription;
  /** The snapshot is written: deliver the held events past it to `sink`, in
   *  arrival order, then each later event as it arrives. Once only. */
  begin(sink: (item: LiveItem) => void): void;
  /** Stop listening and detach. Idempotent. */
  close(): void;
  /** Current exact subscription; false after close or private authority retirement. */
  isCurrent?(): boolean;
}

export function isLiveStreamCurrent(stream: LiveStream): boolean {
  return (
    (stream.isCurrent?.() ?? true) &&
    acceptsPrivateAuthority(stream.subscription.privateAuthorityEpoch)
  );
}

/** The frontend event a stream's output and resizes ride on. */
export function liveStreamChannel(streamId: number): string {
  return `pty-output-stream-${streamId}`;
}

function itemOf(event: TerminalStreamEvent): LiveItem | null {
  if (event.resized) {
    const { cols, rows, resize_gen } = event.resized;
    return { kind: "resized", cols, rows, resizeGen: resize_gen };
  }
  if (typeof event.data === "string" && typeof event.seq === "number") {
    return { kind: "output", data: event.data, seq: event.seq };
  }
  return null;
}

export async function openLiveStream(transport: LiveStreamTransport): Promise<LiveStream> {
  const streamId = await transport.reserve();
  const held: TerminalStreamEvent[] = [];
  let sink: ((item: LiveItem) => void) | null = null;
  let closed = false;
  let epoch: string | null = null;
  const accepts = (event: TerminalStreamEvent) => {
    if (epoch !== null && event.privateAuthorityEpoch !== epoch) return false;
    if (event.privateAuthorityEpoch != null) epoch = event.privateAuthorityEpoch;
    return acceptsPrivateAuthority(event.privateAuthorityEpoch);
  };
  // What the display already holds (the snapshot's position once subscribed):
  // an event at or below it is already on screen.
  let appliedSeq = -1;
  let appliedResizeGen = -1;

  const deliver = (event: TerminalStreamEvent) => {
    if (!accepts(event) || !sink) return;
    const item = itemOf(event);
    if (!item) return;
    if (item.kind === "output") {
      if (item.seq <= appliedSeq) return;
      appliedSeq = item.seq;
    } else {
      if (item.resizeGen <= appliedResizeGen) return;
      appliedResizeGen = item.resizeGen;
    }
    sink(item);
  };

  let unlisten: () => void;
  try {
    unlisten = await transport.listen(liveStreamChannel(streamId), (event) => {
      if (closed || !accepts(event)) return;
      if (sink) deliver(event);
      else held.push(event);
    });
  } catch (error) {
    transport.unsubscribe(streamId);
    throw error;
  }

  let subscription: TerminalSubscription;
  try {
    subscription = await transport.subscribe(streamId);
  } catch (error) {
    closed = true;
    unlisten();
    transport.unsubscribe(streamId);
    throw error;
  }
  if (
    (epoch !== null &&
      subscription.privateAuthorityEpoch != null &&
      epoch !== subscription.privateAuthorityEpoch) ||
    !acceptsPrivateAuthority(subscription.privateAuthorityEpoch) ||
    !acceptsPrivateAuthority(epoch)
  ) {
    closed = true;
    held.length = 0;
    unlisten();
    transport.unsubscribe(streamId);
    throw new Error("private terminal authority retired");
  }
  epoch = subscription.privateAuthorityEpoch ?? epoch;
  subscription = { ...subscription, ...(epoch === null ? {} : { privateAuthorityEpoch: epoch }) };
  appliedSeq = subscription.snapshot.seq;
  appliedResizeGen = subscription.snapshot.resize_gen;

  return {
    streamId,
    subscription,
    isCurrent: () => !closed && acceptsPrivateAuthority(epoch),
    begin(target) {
      if (closed || sink || !acceptsPrivateAuthority(epoch)) return;
      sink = target;
      for (const event of held.splice(0)) deliver(event);
    },
    close() {
      if (closed) return;
      closed = true;
      held.length = 0;
      unlisten();
      transport.unsubscribe(streamId);
    },
  };
}
