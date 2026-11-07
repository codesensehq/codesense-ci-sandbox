// SPDX-License-Identifier: MIT
import { useMemo, useState } from "react";

import { HUMAN, msgKey, replyTarget, wireColumns } from "../model";
import { Composer } from "./composer";
import { selfOf, useInboxView, type ReplyRequest } from "./inbox-view-context";
import { WireDetail, WireView } from "./wire-view";

/**
 * Wire for one tab (its lifeline first) or one saved agent (you, then its sessions): the
 * selected arrow's detail, then the composer.
 */
export function WireLens({ request }: { request: ReplyRequest | null }) {
  const view = useInboxView();
  const { me, dir, messages, roles } = view;
  const [sel, setSel] = useState<string | null>(null);
  const selected = messages.find((m) => msgKey(m) === sel) ?? null;
  const self = selfOf(view);
  const columns = useMemo(
    () =>
      view.self ? wireColumns(messages, null, [HUMAN, ...view.self]) : wireColumns(messages, me),
    [messages, me, view.self],
  );
  const to = selected ? replyTarget(selected, self, view.self ? view.held : undefined) : null;
  return (
    <div className="ibx-wire">
      <WireView
        columns={columns}
        messages={messages}
        dir={dir}
        roles={roles}
        me={view.self ? null : me}
        selected={sel}
        onSelect={setSel}
      />
      {selected && (
        <WireDetail
          m={selected}
          me={self}
          dir={dir}
          held={view.self ? view.held : undefined}
          onClose={() => setSel(null)}
          onReply={view.replyTo}
          onGo={view.goTo}
        />
      )}
      <Composer defaultTo={to} request={request} />
    </div>
  );
}
