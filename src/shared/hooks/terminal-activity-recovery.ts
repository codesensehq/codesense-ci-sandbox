// SPDX-License-Identifier: MIT
import type {
  ActivitySnapshot,
  ActivitySnapshotReply,
  RuntimeMemberTarget,
  TerminalRuntimeEvent,
} from "@/features/viewer/lib/cli-workspace/transport/terminal-activity-source";
import {
  memberKey,
  sessionTarget,
  type TerminalLayoutProjection,
} from "@/shared/lib/terminal-layout-projection";
import { sameActivityIdentity } from "@/shared/stores/agent-state-store";

export function sameActivityTarget(a: RuntimeMemberTarget, b: RuntimeMemberTarget): boolean {
  return (
    memberKey(a.member) === memberKey(b.member) &&
    a.admissionRevision === b.admissionRevision &&
    a.namespace.serverId === b.namespace.serverId &&
    a.namespace.epoch === b.namespace.epoch &&
    a.namespace.generation === b.namespace.generation
  );
}

interface Dependencies {
  layout: () => TerminalLayoutProjection;
  acceptsAuthority: (epoch?: string) => boolean;
  fetch: (target: RuntimeMemberTarget) => Promise<ActivitySnapshotReply>;
  sequence: () => number;
  apply: (snapshot: ActivitySnapshot, sentAt: number, receivedAt: number) => void;
  unavailable: (
    sessions: string[],
    state: "recovering" | "unavailable" | "upgrade-required",
  ) => void;
}
interface MemberRecovery {
  target: RuntimeMemberTarget;
  authority?: string;
  signature: string;
  sessions: string[];
  baseline: Map<string, ActivitySnapshot>;
  pending: { snapshot: ActivitySnapshot; receivedAt: number }[];
  loading: boolean;
  pendingRequest: number | null;
  resyncRequested: boolean;
  ready: boolean;
  retry?: ReturnType<typeof setTimeout>;
  deadline?: ReturnType<typeof setTimeout>;
  attempt: number;
}
const LIMIT = 256;
const RETRY_MS = 3000;

/** Transport recovery only. The host owns state/precedence; this projection
 * admits exact identities and revisions without replaying synthetic hooks. */
export function createActivityRecovery(deps: Dependencies) {
  const members = new Map<string, MemberRecovery>();
  let disposed = false;
  const current = (member: MemberRecovery) =>
    !disposed && members.get(memberKey(member.target.member)) === member;
  const eligible = (member: MemberRecovery, session: string) => {
    const target = sessionTarget(deps.layout(), session);
    return (
      current(member) &&
      deps.acceptsAuthority(member.authority) &&
      target !== undefined &&
      sameActivityTarget(target, member.target)
    );
  };
  function stop(member: MemberRecovery) {
    clearTimeout(member.retry);
    clearTimeout(member.deadline);
    member.attempt++;
  }
  function unavailable(member: MemberRecovery) {
    if (!current(member)) return;
    member.loading = false;
    member.ready = false;
    deps.unavailable(member.sessions, "unavailable");
    clearTimeout(member.retry);
    member.retry = setTimeout(() => fetch(member), RETRY_MS);
  }
  function apply(
    member: MemberRecovery,
    snapshot: ActivitySnapshot,
    sentAt: number,
    receivedAt: number,
  ) {
    if (!eligible(member, snapshot.session)) return;
    member.baseline.set(snapshot.session, snapshot);
    deps.apply(snapshot, sentAt, receivedAt);
  }
  function fetch(member: MemberRecovery) {
    if (!current(member) || member.loading || member.pendingRequest) return;
    clearTimeout(member.retry);
    member.loading = true;
    member.ready = false;
    const attempt = ++member.attempt;
    member.pendingRequest = attempt;
    const sentAt = deps.sequence();
    deps.unavailable(member.sessions, "recovering");
    member.deadline = setTimeout(() => {
      if (current(member) && member.attempt === attempt) {
        member.attempt++;
        unavailable(member);
      }
    }, RETRY_MS);
    void deps
      .fetch(member.target)
      .then(
        (reply) => {
          const receivedAt = Date.now();
          if (member.pendingRequest === attempt) member.pendingRequest = null;
          if (!current(member) || member.attempt !== attempt) return;
          clearTimeout(member.deadline);
          if (
            !sameActivityTarget(reply.target, member.target) ||
            reply.privateAuthorityEpoch !== member.authority ||
            !deps.acceptsAuthority(reply.privateAuthorityEpoch)
          ) {
            unavailable(member);
            return;
          }
          if (reply.availability === "upgrade-required") {
            member.loading = false;
            member.ready = true;
            member.baseline.clear();
            member.pending.length = 0;
            // A connected older owner is not a transport outage. Keep its
            // terminals intact and retry only on an explicit resync/reconnect.
            deps.unavailable(member.sessions, "upgrade-required");
            return;
          }
          member.loading = false;
          member.ready = true;
          member.baseline.clear();
          for (const snapshot of reply.snapshots) {
            // A replacement can emit after the host read its snapshot. Neither
            // identity is installed from contradictory evidence: reread the owner.
            const contradicted = member.pending.some(
              ({ snapshot: event }) =>
                event.session === snapshot.session &&
                !sameActivityIdentity(event.identity, snapshot.identity),
            );
            if (!contradicted) apply(member, snapshot, sentAt, receivedAt);
          }
          // Missing activity means unsupported/uninstrumented, never a process exit.
          deps.unavailable(
            member.sessions.filter((id) => !member.baseline.has(id)),
            "unavailable",
          );
          const pending = member.pending.splice(0);
          for (const { snapshot, receivedAt } of pending)
            update(member, snapshot, true, receivedAt);
        },
        () => {
          if (member.pendingRequest === attempt) member.pendingRequest = null;
          if (!current(member) || member.attempt !== attempt) return;
          clearTimeout(member.deadline);
          unavailable(member);
        },
      )
      .finally(() => {
        if (member.pendingRequest === attempt) member.pendingRequest = null;
        if (
          current(member) &&
          member.resyncRequested &&
          !member.loading &&
          member.pendingRequest === null
        ) {
          member.resyncRequested = false;
          fetch(member);
        } else if (current(member) && !member.ready && !member.loading) {
          clearTimeout(member.retry);
          member.retry = setTimeout(() => fetch(member), RETRY_MS);
        }
      });
  }
  function update(
    member: MemberRecovery,
    snapshot: ActivitySnapshot,
    replaying = false,
    receivedAt = Date.now(),
  ) {
    if (!eligible(member, snapshot.session)) return;
    const previous = member.baseline.get(snapshot.session);
    if (
      !member.ready ||
      !previous ||
      !sameActivityIdentity(previous.identity, snapshot.identity) ||
      snapshot.revision > previous.revision + 1
    ) {
      if (replaying) {
        // A queued old incarnation cannot repeatedly invalidate a fresh snapshot.
        // One bounded follow-up covers replacement after the snapshot was read.
        clearTimeout(member.retry);
        member.retry = setTimeout(() => fetch(member), RETRY_MS);
        return;
      }
      if (member.pending.length >= LIMIT) member.pending.shift();
      member.pending.push({ snapshot, receivedAt });
      member.ready = false;
      fetch(member);
      return;
    }
    if (snapshot.revision <= previous.revision) return;
    apply(member, snapshot, deps.sequence(), receivedAt);
  }
  function reconcile() {
    if (disposed) return;
    const layout = deps.layout();
    const active = new Set<string>();
    for (const [key, held] of Object.entries(layout.members)) {
      const sessions = [
        ...new Set(
          Object.values(held.workspaces).flatMap((ws) => ws.tabs.map((tab) => tab.session)),
        ),
      ];
      const admitted = sessions.filter((id) => {
        const target = sessionTarget(layout, id);
        return target && sameActivityTarget(target, held.target);
      });
      if (!deps.acceptsAuthority(held.privateAuthorityEpoch) || admitted.length === 0) {
        deps.unavailable(sessions, "unavailable");
        continue;
      }
      active.add(key);
      const signature = JSON.stringify([
        held.target,
        held.privateAuthorityEpoch,
        Object.values(held.workspaces).flatMap((ws) =>
          ws.tabs.map((tab) => [tab.session, tab.state]),
        ),
      ]);
      let member = members.get(key);
      if (member?.signature === signature) continue;
      if (member) stop(member);
      member = {
        target: held.target,
        authority: held.privateAuthorityEpoch,
        signature,
        sessions: admitted,
        baseline: new Map(),
        pending: [],
        loading: false,
        pendingRequest: null,
        resyncRequested: false,
        ready: false,
        attempt: 0,
      };
      members.set(key, member);
      fetch(member);
    }
    for (const [key, member] of members) {
      if (active.has(key)) continue;
      stop(member);
      deps.unavailable(member.sessions, "unavailable");
      members.delete(key);
    }
  }
  return {
    reconcile,
    event(event: TerminalRuntimeEvent) {
      const snapshot = event.event.Activity?.snapshot;
      const member = members.get(memberKey(event.target.member));
      if (
        !snapshot ||
        !member ||
        !sameActivityTarget(event.target, member.target) ||
        event.privateAuthorityEpoch !== member.authority ||
        !deps.acceptsAuthority(event.privateAuthorityEpoch)
      )
        return;
      update(member, snapshot);
    },
    resync() {
      for (const member of members.values()) {
        // A read already in flight may predate the resync signal. Coalesce signals
        // into one follow-up without issuing overlapping owner reads.
        if (member.loading || member.pendingRequest !== null) {
          member.resyncRequested = true;
        } else {
          fetch(member);
        }
      }
    },
    dispose() {
      disposed = true;
      for (const member of members.values()) stop(member);
      members.clear();
    },
  };
}
