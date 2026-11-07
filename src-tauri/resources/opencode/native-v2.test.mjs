// SPDX-License-Identifier: MIT
import assert from 'node:assert/strict';
import { test } from 'node:test';
import plugin from './codesense.js';

test('OpenCode 2 registers native domain hooks and owns its subscription', async () => {
  const hooks = new Map();
  let signal;
  const domain = (name) => ({ hook: (event, handler) => { hooks.set(`${name}.${event}`, handler); return { dispose: async () => {} }; } });
  const cleanup = await plugin.setup({
    app: { version: '2.1.4' },
    session: domain('session'), tool: domain('tool'), shell: domain('shell'),
    event: { subscribe: (options) => { signal = options.signal; return { async *[Symbol.asyncIterator]() {} }; } },
  });
  assert.equal(plugin.id, 'codesense');
  assert.deepEqual([...hooks.keys()], ['session.prompt', 'tool.execute.before', 'tool.execute.after', 'shell.create.before']);
  assert.equal(signal.aborted, false);
  await cleanup();
  assert.equal(signal.aborted, true);
});
