// SPDX-License-Identifier: MIT
import type { HandlerRegistry } from "./mock-router";

/**
 * The mock's event bus: `plugin:event|listen` registrations by event name, and
 * an {@link MockEventBus.emit} that calls them as the Tauri runtime does (each
 * listener's callback with `{ event, id, payload }`). Only surfaces that need
 * motion emit through it (the terminal layout world); everything else stays
 * the static, event-less mock.
 */
export class MockEventBus {
  private readonly observers = new Map<string, Array<(payload: unknown) => void>>();
  private readonly listeners = new Map<string, Map<number, number>>();
  private nextEventId = 1;
  private run: ((callbackId: number, payload: unknown) => void) | null = null;

  /** Where a listener's callback id is invoked (the installed internals). */
  bind(run: (callbackId: number, payload: unknown) => void): void {
    this.run = run;
  }

  handlers(): HandlerRegistry {
    return {
      "plugin:event|listen": (args) => {
        const eventId = this.nextEventId++;
        const event = String(args.event);
        const byId = this.listeners.get(event) ?? new Map<number, number>();
        byId.set(eventId, Number(args.handler));
        this.listeners.set(event, byId);
        return eventId;
      },
      "plugin:event|unlisten": (args) => {
        this.listeners.get(String(args.event))?.delete(Number(args.eventId));
        return undefined;
      },
    };
  }

  emit(event: string, payload: unknown): void {
    for (const observe of this.observers.get(event) ?? []) observe(payload);
    for (const [id, callbackId] of this.listeners.get(event) ?? []) {
      this.run?.(callbackId, { event, id, payload });
    }
  }

  /** Mock source owners observe domain writes before delivering projected deltas. */
  observe(event: string, callback: (payload: unknown) => void): void {
    this.observers.set(event, [...(this.observers.get(event) ?? []), callback]);
  }
}
