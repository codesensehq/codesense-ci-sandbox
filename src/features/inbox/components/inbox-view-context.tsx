// SPDX-License-Identifier: MIT
import { createContext, useContext } from "react";

import type { InboxTeam } from "@/tauri/commands";

import type { ConversationMessage, InboxSource, RoleSummary } from "../api";
import type { Directory, RoleScope, Self } from "../model";

/** A Reply action: aim the composer at `key` (answering `inReplyTo` when given) and focus it. */
export interface ReplyRequest {
  key: string;
  nonce: number;
  inReplyTo?: ConversationMessage | null;
}

let replySeq = 0;

/** A new Reply request. Each one is a distinct object, so a repeat Reply to the same key counts. */
export function makeReplyRequest(
  key: string,
  inReplyTo?: ConversationMessage | null,
): ReplyRequest {
  replySeq += 1;
  return { key, nonce: replySeq, inReplyTo: inReplyTo ?? null };
}

const consumed = new WeakSet<ReplyRequest>();

/** An unsent draft: its text and, when one was chosen, the recipient it was written for. */
export interface InboxDraft {
  text: string;
  to: string | null;
  inReplyTo: ConversationMessage | null;
  team?: InboxTeam | null;
}

/**
 * One surface's kept drafts, by thread, with the texts whose send is on its way (never kept,
 * so a surface reopened meanwhile can't offer them again) and a way for a mounted composer to
 * hear a draft come back from a send that failed after its own composer had gone.
 */
export class DraftStore {
  readonly drafts = new Map<string | null, InboxDraft>();
  readonly inFlight = new Map<string | null, string>();
  private readonly listeners = new Set<(key: string | null) => void>();
  get(key: string | null): InboxDraft | undefined {
    return this.drafts.get(key);
  }
  has(key: string | null): boolean {
    return this.drafts.has(key);
  }
  set(key: string | null, draft: InboxDraft): void {
    this.drafts.set(key, draft);
  }
  delete(key: string | null): void {
    this.drafts.delete(key);
  }
  /** Tell mounted composers `key`'s draft came back. */
  announce(key: string | null): void {
    for (const l of this.listeners) l(key);
  }
  subscribe(listener: (key: string | null) => void): () => void {
    this.listeners.add(listener);
    return () => void this.listeners.delete(listener);
  }
}

const draftStores = new Map<string, DraftStore>();

/**
 * The unsent drafts of one inbox surface (a tab's inbox, the docked panel, the rollup, an
 * agent's inbox), by thread. They outlive the composer, so closing the surface — by Escape,
 * ⌘I, the minimize button or a viewer change — and opening it again keeps what was written.
 * A sent or emptied draft is removed.
 */
export function inboxDrafts(namespace: string): DraftStore {
  let store = draftStores.get(namespace);
  if (!store) {
    store = new DraftStore();
    draftStores.set(namespace, store);
  }
  return store;
}

/** Forget every kept draft (tests start clean). */
export function clearInboxDrafts(): void {
  draftStores.clear();
}

/**
 * Claim a Reply request: true only the first time any consumer (a composer, the Mailspool reply
 * line) sees it, including one that mounts holding it (a Reply that opens the conversation).
 * After that it is spent, so a remounted composer or a lens switch never replays an old Reply.
 */
export function takeReplyRequest(
  request: ReplyRequest | null | undefined,
): request is ReplyRequest {
  if (!request || consumed.has(request)) return false;
  consumed.add(request);
  return true;
}

/** What every lens and the composer read: who "me" is and the world around them. */
export interface InboxView {
  /** The tab's session handle, or "human" where the human is looking (the rollup, an agent). */
  me: string;
  /**
   * When the view speaks for several sessions (every session of a saved agent): their handles.
   * Their messages read as "out", and conversations are filed by which of them took part.
   */
  self?: ReadonlySet<string>;
  /**
   * "human": the human's own inbox (the docked panel). Its Threads list shows conversations
   * and role queues. Nothing is opened until the human picks it, since
   * opening a message to the human reads it.
   */
  scope?: "human";
  /** Archive messages to the human (offered only where the human reads their own inbox). */
  archive?: (messages: readonly ConversationMessage[]) => void;
  /** When set, the only recipients the composer offers (an agent's sessions and its #role). */
  targets?: readonly string[];
  dir: Directory;
  messages: ConversationMessage[];
  roles: RoleSummary[];
  /**
   * A saved agent's role inbox: the role an ended session of that agent stands for when the
   * roster names none. The composer offers it instead (only for the agent's own sessions).
   */
  roleScope?: RoleScope;
  /** The roles `me` holds. */
  held: string[];
  /** Where unsent drafts are kept past the composer (see `inboxDrafts`); unset, the
   *  composer keeps them only while it is mounted. */
  drafts?: string;
  /** The team a new message goes to when no earlier message says which. */
  fallbackTeam: InboxTeam | null;
  /** The team explicitly focused in a global role view. */
  roleTeam?: InboxTeam;
  source: InboxSource | null;
  /** Point the composer at `key` and focus it (a Reply action). */
  replyTo: (key: string, inReplyTo?: ConversationMessage | null) => void;
  /** Jump to a session's terminal. */
  goTo: (key: string) => void;
  /** The surface already says the recipient's machine is offline (the inbox chat's header): the
   *  composer's line says where the message goes instead of repeating it. */
  offlineNoted?: boolean;
  /** Open a role's inbox (`#role`) where mail to it can be written: the inbox chat's next step
   *  for an ended agent whose mail would go to its role. */
  openRoleInbox?: (roleKey: string) => void;
}

/** The side `view` speaks for, as the model's helpers take it. */
export const selfOf = (view: Pick<InboxView, "me" | "self">): Self => view.self ?? view.me;

const Ctx = createContext<InboxView | null>(null);

export const InboxViewProvider = Ctx.Provider;

export function useInboxView(): InboxView {
  const v = useContext(Ctx);
  if (!v) throw new Error("useInboxView outside an InboxViewProvider");
  return v;
}
