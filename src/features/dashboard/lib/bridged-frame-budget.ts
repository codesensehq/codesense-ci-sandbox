// SPDX-License-Identifier: MIT
/** Host mount accounting only. Rust's FrameStore remains the identity owner. */
interface Activity {
  visible: boolean;
  unsaved: boolean;
  privateSpace: boolean;
}
interface Entry extends Activity {
  flush: () => unknown;
  evict: () => void;
  lastVisible: number;
}
const isPinned = (entry: Activity) => entry.visible || entry.unsaved || entry.privateSpace;

export class BridgedFrameBudget {
  private entries = new Map<string | symbol, Entry>();
  private clock = 0;
  private warned = false;
  constructor(
    private limit: number,
    private warn: (pinned: number) => void,
  ) {}
  get size() {
    return this.entries.size;
  }
  register(id: string | symbol, entry: Omit<Entry, "lastVisible">): () => void {
    this.entries.set(id, { ...entry, lastVisible: entry.visible ? ++this.clock : 0 });
    this.trim();
    return () => {
      this.entries.delete(id);
      this.trim();
    };
  }
  update(id: string | symbol, patch: Partial<Activity>) {
    const entry = this.entries.get(id);
    if (!entry) return;
    if (patch.visible && !entry.visible) entry.lastVisible = ++this.clock;
    Object.assign(entry, patch);
    this.trim();
  }
  private trim() {
    const pinned = [...this.entries.values()].filter(isPinned).length;
    if (pinned > this.limit) {
      if (!this.warned) {
        this.warned = true;
        this.warn(pinned);
      }
      return;
    }
    this.warned = false;
    const candidates = [...this.entries]
      .filter(([, entry]) => !isPinned(entry))
      .sort((a, b) => a[1].lastVisible - b[1].lastVisible);
    for (const [id, entry] of candidates) {
      if (this.entries.size <= this.limit) break;
      this.entries.delete(id);
      try {
        const flushed = entry.flush();
        if (flushed instanceof Promise) void flushed.catch(() => {}).then(entry.evict);
        else entry.evict();
      } catch {
        entry.evict();
      }
    }
  }
}
