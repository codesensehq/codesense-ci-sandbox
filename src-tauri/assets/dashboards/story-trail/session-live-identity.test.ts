// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import {trailTestKit} from './test-kit';
import {mountConversation} from './conversation-view.mjs';
import {session} from './fixtures.mjs';
import {conversation} from './conversation-fixtures.mjs';
import {locateAgentByAgentId} from '../../../../src/features/weft/lib/locate-agent';
const host=vi.hoisted(()=>({sessions:[] as any[],show:vi.fn().mockResolvedValue({shown:'tab'})}));
vi.mock('@/features/viewer/lib/agent-list-projection',()=>({useAgentListProjectionStore:{getState:()=>({sessions:host.sessions})}}));
vi.mock('@/features/viewer/lib/show-agent',()=>({showAgent:host.show}));
const settle=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
let stop=()=>{};
afterEach(()=>{stop();document.body.replaceChildren();});
it('keeps Jump to live terminal bound to the conversation session across handle reuse',async()=>{
 document.body.innerHTML='<main></main>';const root=document.querySelector('main')!;
 const original={id:'worker',handle:'worker',sessionId:'lead',alive:true,startedAtMs:1793178000000};
 host.sessions=[original];
 const facts={...session('lead'),conversation:conversation()};
 const cs={kit:trailTestKit(),sessionRead:vi.fn().mockResolvedValue(facts),sessions:vi.fn().mockResolvedValue([{id:'worker',handle:'worker',sessionId:'lead',state:'running'}]),gotoAgent:({agentId})=>locateAgentByAgentId(agentId),openFile:vi.fn()};
 const app=mountConversation({root,cs,id:'lead'});stop=app.stop;await settle();
 const jump=root.querySelector<HTMLButtonElement>('[data-conv-action=live]')!;expect(jump).not.toBeNull();
 // An ordinary completed worker is relaunched under its reusable handle. The host
 // knows the replacement before the conversation's passive refresh completes.
 host.sessions=[{...original,alive:false},{...original,sessionId:'replacement',startedAtMs:1793178060000}];
 cs.sessionRead.mockImplementationOnce(()=>new Promise(()=>{}));
 cs.sessions.mockResolvedValue([{id:'worker',handle:'worker',sessionId:'replacement',state:'running'}]);app.nudge();await settle();
 jump.click();await settle();
 expect(host.show).toHaveBeenCalledTimes(1);
 expect(host.show.mock.calls[0][0].sessionId).toBe('lead');
});
