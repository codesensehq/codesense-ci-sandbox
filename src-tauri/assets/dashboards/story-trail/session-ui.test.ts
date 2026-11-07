// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import {trailTestKit} from './test-kit';
import {mountTrail} from './page.mjs';
import {mountConversation} from './conversation-view.mjs';
import {story,session} from './fixtures.mjs';
import {conversation,turn} from './conversation-fixtures.mjs';
const stops:(()=>void)[]=[];
afterEach(()=>{stops.splice(0).forEach(f=>f());document.body.replaceChildren();});
const settle=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
function api(){return {kit:trailTestKit(),storyChanges:vi.fn().mockResolvedValue(story()),sessionRead:vi.fn().mockResolvedValue({...session('lead'),conversation:conversation()}),sessions:vi.fn().mockResolvedValue([]),openFile:vi.fn().mockResolvedValue({}),gotoAgent:vi.fn().mockResolvedValue({}),subscribe:vi.fn().mockResolvedValue({}),unsubscribe:vi.fn(),onVisibility:(f:(v:boolean)=>void)=>{f(true);return ()=>{};}};}
function mount(cs=api(),options={}){document.body.innerHTML='<main id="trail"></main>';const root=document.querySelector('main')!;const app=mountConversation({root,cs,id:'lead',...options});stops.push(app.stop);return {root,cs,app};}
it('opens real session facts, last-turn excerpt and conversation at its recorded turn',async()=>{
  const cs=api();const facts={...session('lead'),tools:{...session('lead').tools,used:[{name:'Edit',mcpServer:null,calls:1,errors:1,errorUnknown:0}],usedState:'present',usedReason:null},conversation:conversation({turnCount:25,nextCursor:1})};
  cs.sessionRead.mockResolvedValueOnce(facts).mockResolvedValueOnce({...facts,conversation:conversation({turnCount:25,turns:[turn(21),turn(22,{tool_calls:[{name:'Edit',input:'{"file_path":"app.ts","start_line":3,"end_line":4}',is_error:true}]}),turn(23),turn(24)]})});
  document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
  const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();
  expect(cs.sessionRead).toHaveBeenLastCalledWith({sessionId:'lead',include:['conversation'],cursor:21,limit:4});
  expect(document.querySelector('#drill-panel')?.textContent).toContain('Models');expect(document.querySelector('#drill-panel')?.textContent).toContain('Timing');expect(document.querySelector<HTMLElement>('[data-files]')?.hidden).toBe(true);
  document.querySelector<HTMLButtonElement>('[data-panel-key="failure:Edit:"]')!.click();await settle();
  expect(cs.openFile).toHaveBeenCalledWith({path:'story-trail.html#session/lead/turn/22',newTab:true});
});
it.each(['absent','expired','unsupported','unavailable','truncated','present'])('renders the real %s conversation page',async state=>{
  const cs=api();cs.sessionRead.mockResolvedValue({...session('lead'),conversation:conversation({state,reason:['unsupported','unavailable'].includes(state)?'reader reason':null,truncated:state==='truncated'?{droppedTurns:[1,5],droppedTurnCount:5,droppedBytes:150,trimmedFields:1}:null})});
  const h=mount(cs);await settle();const text=h.root.textContent!;
  expect(text).toContain(({absent:'not captured',expired:'removed after 90 days',unsupported:'reader reason',unavailable:'reader reason',truncated:'5 turns left out',present:'Built the change.'})[state]);
});
it('pages earlier/later using nextCursor and keeps lookup-miss reasons',async()=>{
  const cs=api();cs.sessionRead.mockResolvedValueOnce({...session('lead'),conversation:conversation({turns:[turn(0)],nextCursor:20,turnCount:21})}).mockResolvedValueOnce({...session('lead'),conversation:conversation({turns:[turn(20)],turnCount:21})}).mockResolvedValueOnce({...session('lead'),conversation:conversation({turns:[],lookup:{kind:'turn',requested:'99',found:false,turnIndex:99},reason:'turn 99 is not in the stored conversation'})});
  const h=mount(cs);await settle();h.root.querySelector<HTMLButtonElement>('[data-conv-action="later"]')!.click();await settle();expect(cs.sessionRead).toHaveBeenLastCalledWith({sessionId:'lead',include:['conversation'],limit:20,cursor:20});
  h.root.querySelector<HTMLButtonElement>('[data-conv-action="earlier"]')!.click();await settle();expect(cs.sessionRead).toHaveBeenLastCalledWith({sessionId:'lead',include:['conversation'],limit:20,cursor:0});expect(h.root.textContent).toContain('turn 99 is not in the stored conversation');
});
it('jumps, highlights, marks redactions and renders recorded edits as keyboard-operable diff chips',async()=>{
  const cs=api();cs.sessionRead.mockResolvedValue({...session('lead'),conversation:conversation({turns:[turn(37,{text:'A [redacted] value',tool_calls:[{name:'Edit',input:'{"file_path":"app.ts","start_line":3,"end_line":4}',result:'[redacted]',is_error:true}]})],lookup:{kind:'turn',requested:'37',found:true,turnIndex:37}})});
  const h=mount(cs,{turn:37,source:'lines 3–4 of app.ts'});await settle();expect(cs.sessionRead).toHaveBeenCalledWith({sessionId:'lead',include:['conversation'],limit:20,turn:37});
  expect(h.root.querySelector('[aria-current="true"]')?.textContent).toContain('37');expect(h.root.textContent).toContain('You came here from lines 3–4 of app.ts');expect(h.root.querySelectorAll('.rd')).toHaveLength(2);expect(h.root.querySelector('.edit-chip')?.tagName).toBe('BUTTON');expect(h.root.querySelector('.edit-chip')?.getAttribute('data-edit-path')).toBe('app.ts');
  expect(h.root.querySelector('details summary')?.textContent).toContain('failed');h.root.querySelector<HTMLButtonElement>('[data-conv-action="unhighlight"]')!.click();expect(h.root.querySelector('.highlight')).toBeNull();
});
it('shows Jump only while running and uses host forwarding for Back',async()=>{
  const cs=api();cs.sessions.mockResolvedValue([{id:'live-handle',sessionId:'lead',handle:'live-handle',state:'running',displayName:'Builder',launchedBy:null,unread:null,oldestUnreadAt:null}]);
  const h=mount(cs);await settle();expect(h.root.querySelector<HTMLElement>('[data-live-label]')?.hidden).toBe(false);h.root.querySelector<HTMLButtonElement>('[data-conv-action="live"]')!.click();await settle();expect(cs.gotoAgent).toHaveBeenCalledWith({agentId:'lead'});
  h.root.querySelector<HTMLButtonElement>('[data-conv-action="back"]')!.click();await settle();expect(cs.openFile).toHaveBeenCalledWith({path:'story-trail.html#story/SV-X',newTab:false});
  cs.sessions.mockResolvedValue([]);h.app.nudge();await settle();expect(h.root.querySelector('[data-conv-action="live"]')).toBeNull();
});
it.each([true,false])('coalesces hidden conversation nudges; initial read pending=%s',async early=>{
  const cs=api();let resolve:(v:unknown)=>void=()=>{};const facts={...session('lead'),conversation:conversation()};cs.sessionRead.mockImplementationOnce(()=>new Promise(r=>{resolve=r;})).mockResolvedValue(facts);
  const h=mount(cs);await settle();if(!early){resolve(facts);await settle();}h.app.setVisible(false);h.app.nudge();h.app.nudge();if(early){resolve(facts);await settle();}expect(cs.sessionRead).toHaveBeenCalledTimes(1);h.app.setVisible(true);await settle();expect(cs.sessionRead).toHaveBeenCalledTimes(2);
});
it('retains open and closed tool disclosures and reader scroll during refresh',async()=>{
  const cs=api();cs.sessionRead.mockResolvedValue({...session('lead'),conversation:conversation({turns:[turn(37,{tool_calls:[{name:'Read',input:'file',result:'result',native_id:'call-1'}]})]})});
  const h=mount(cs,{turn:37});await settle();let detail=h.root.querySelector('details')!;detail.open=true;h.root.querySelector<HTMLElement>('.thread')!.scrollTop=200;
  h.app.nudge();await settle();detail=h.root.querySelector('details')!;expect(detail.open).toBe(true);expect(h.root.querySelector('.thread')!.scrollTop).toBe(200);
  detail.open=false;h.app.nudge();await settle();expect(h.root.querySelector('details')!.open).toBe(false);
});
it('never offers an unrelated running terminal for a hand-started stored conversation',async()=>{
  const cs=api();cs.sessionRead.mockResolvedValue({...session('native'),session:null,nativeSessionIds:['native'],unavailable:{session:'hand-started'},conversation:conversation({sessionId:'native'})});cs.sessions.mockResolvedValue([{id:'other',handle:'other',state:'running',displayName:'Other',launchedBy:null,unread:null,oldestUnreadAt:null}]);
  const h=mount(cs,{id:'native'});await settle();expect(h.root.querySelector('[data-conv-action="live"]')).toBeNull();
});
it('shows an initial read error without an endless loading state',async()=>{
  const cs=api();cs.sessionRead.mockRejectedValue(new Error('read failed'));const h=mount(cs);await settle();expect(h.root.textContent).toContain('read failed');expect(h.root.querySelector<HTMLElement>('[data-loading]')?.hidden).toBe(true);expect(h.root.querySelector('[data-conv-action="retry"]')).not.toBeNull();
});
it('pages back across a truncated middle interval to retained opening turns',async()=>{
  const cs=api(),truncated={droppedTurns:[1,99],droppedTurnCount:99,droppedBytes:10000,trimmedFields:0};
  cs.sessionRead.mockImplementation(async args=>({...session('lead'),conversation:conversation({state:'truncated',truncated,turns:(args.cursor===0?[turn(0)]:Array.from({length:20},(_,i)=>turn(100+i))),nextCursor:120,turnCount:41})}));
  const h=mount(cs,{turn:100});await settle();h.root.querySelector<HTMLButtonElement>('[data-conv-action="earlier"]')!.click();await settle();expect(cs.sessionRead).toHaveBeenLastCalledWith({sessionId:'lead',include:['conversation'],limit:1,cursor:0});expect(h.root.querySelector('#turn-0')).not.toBeNull();
});
it('retains tool output scroll and summary focus across refresh',async()=>{
  const cs=api();cs.sessionRead.mockResolvedValue({...session('lead'),conversation:conversation({turns:[turn(2,{tool_calls:[{name:'Read',native_id:'call-r',input:'app.ts',result:'long result'}]})]})});
  const h=mount(cs);await settle();const details=h.root.querySelector('details')!;details.open=true;const output=details.querySelectorAll<HTMLElement>('pre')[1];output.scrollTop=150;output.scrollLeft=30;details.querySelector('summary')!.focus();h.app.nudge();await settle();const next=h.root.querySelector('details')!;expect(next.querySelectorAll('pre')[1].scrollTop).toBe(150);expect(next.querySelectorAll('pre')[1].scrollLeft).toBe(30);expect(document.activeElement).toBe(next.querySelector('summary'));
});

it.each(['earlier','later','unhighlight','summary','live'])('restores a stable conversation target when %s disappears',async(kind)=>{
 const cs=api();cs.sessions.mockResolvedValue([{id:'lead',sessionId:'lead',state:'running'}]);cs.sessionRead.mockResolvedValueOnce({...session('lead'),conversation:conversation({turns:[turn(20,{tool_calls:[{name:'Read',native_id:'read-1',input:'app.ts'}]})],nextCursor:40,turnCount:60})}).mockResolvedValue({...session('lead'),conversation:conversation({turns:[turn(kind==='summary'?20:0)],nextCursor:null})});
 const h=mount(cs,{turn:20});await settle();const selector=kind==='summary'?'summary':`[data-conv-action="${kind}"]`;const button=h.root.querySelector<HTMLElement>(selector)!;button.focus();
 if(['earlier','later','unhighlight'].includes(kind))button.click();else{cs.sessions.mockResolvedValue([]);h.app.nudge();}await settle();
 expect(document.activeElement).not.toBe(document.body);
 expect(document.activeElement?.tagName).toBe(kind==='summary'?'ARTICLE':'H1');
});
it('returns panel close to the section when its opener row disappears',async()=>{
 const cs=api();document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();document.querySelector<HTMLElement>('[data-close]')!.focus();cs.storyChanges.mockResolvedValue(story({sessions:[]}));window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();document.querySelector<HTMLButtonElement>('[data-close]')!.click();expect(document.activeElement?.id).toBe('sessions-title');
});
it.each([false,true])('keeps panel Retry focused when its repeated read fails; prior data=%s',async(prior)=>{
 const cs=api();if(!prior)cs.sessionRead.mockRejectedValue(new Error('read failed'));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();if(prior){cs.sessionRead.mockRejectedValue(new Error('read failed'));window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();}const retry=document.querySelector<HTMLButtonElement>('[data-panel-retry]')!;retry.focus();retry.click();await settle();expect(document.activeElement?.matches('[data-panel-retry]')).toBe(true);
});
it('shows known failures independently of the last four turns and distinguishes unknown errors',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>({...session('lead'),tools:{usedState:'present',used:[{name:'Edit',calls:5,errors:1,errorUnknown:0,firstError:{turn:22,toolUseId:'failure-22'}},{name:'shell',calls:5,errors:0,errorUnknown:5,firstError:null}]},conversation:args.toolUseId?conversation({turns:[turn(22,{tool_calls:[{name:'Edit',native_id:'failure-22',is_error:true}]})],lookup:{found:true,turnIndex:22}}):conversation({turnCount:100,turns:[turn(96),turn(97),turn(98),turn(99)]})}));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();expect(document.querySelector('#drill-panel')?.textContent).toContain('errors unknown');expect(cs.sessionRead).toHaveBeenCalledWith({sessionId:'lead',include:['conversation'],toolUseId:'failure-22',limit:1});const failure=document.querySelector<HTMLButtonElement>('[data-turn="22"]');expect(failure).not.toBeNull();failure!.click();await settle();expect(cs.openFile).toHaveBeenCalledWith({path:'story-trail.html#session/lead/turn/22',newTab:true});
});
it('identifies the recorded story, basis and account in the conversation header',async()=>{
 const h=mount();await settle();const header=h.root.querySelector('.conversation-head')!;expect(header.textContent).toContain('SV-X');expect(header.textContent).toContain('A story');expect(header.textContent).toContain('exact');expect(header.textContent).toContain('Work');
});
it('retains the turn rail scroll alongside the thread on live refresh',async()=>{
 const h=mount();await settle();h.root.querySelector<HTMLElement>('.turn-rail')!.scrollTop=120;h.root.querySelector<HTMLElement>('.thread')!.scrollTop=300;h.app.nudge();await settle();expect(h.root.querySelector('.turn-rail')?.scrollTop).toBe(120);expect(h.root.querySelector('.thread')?.scrollTop).toBe(300);
});
it('keeps paging and explicit jumps in the URL so reopening a turn restores it',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>({...session('lead'),conversation:conversation({turnCount:60,turns:[turn(args.turn??args.cursor??0)],nextCursor:(args.turn??args.cursor??0)+20})}));
 window.history.replaceState(null,'','#session/lead/turn/37');const h=mount(cs,{turn:37});await settle();h.root.querySelector<HTMLButtonElement>('[data-conv-action="earlier"]')!.click();await settle();expect(window.location.hash).toBe('#session/lead?from=0');
 h.app.jump(21);await settle();expect(window.location.hash).toBe('#session/lead?from=20&turn=21');
});

it.each(['miss','unavailable'])('omits a failure link when the indexed lookup is %s',async reason=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>({...session('lead'),tools:{usedState:'present',used:[{name:'Edit',calls:1,errors:1,firstError:{turn:22,toolUseId:'failure-22'}}]},conversation:args.toolUseId?conversation({state:reason==='miss'?'present':'unavailable',turns:[],lookup:{found:false},reason}):conversation()}));
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();expect(document.querySelector('[data-panel-key^="failure:"]')).toBeNull();
});
it('keeps busy paging focusable and coalesces repeated clicks',async()=>{
 const cs=api();cs.sessionRead.mockResolvedValue({...session('lead'),conversation:conversation({turns:[turn(20)]})});const h=mount(cs,{turn:20});await settle();let resolve:(v:unknown)=>void=()=>{};h.cs.sessionRead.mockImplementationOnce(()=>new Promise(r=>{resolve=r;}));const later=h.root.querySelector<HTMLButtonElement>('[data-conv-action="earlier"]')!;later.focus();later.click();await settle();expect(later.disabled).toBe(false);expect(later.getAttribute('aria-disabled')).toBe('true');later.click();await settle();expect(h.cs.sessionRead).toHaveBeenCalledTimes(2);expect(document.activeElement).toBe(later);resolve({...session('lead'),conversation:conversation()});await settle();
});

it('links a retained tail failure when the earliest error was dropped',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>({...session('lead'),tools:{usedState:'truncated',used:[{name:'Edit',calls:2,errors:2,firstError:{turn:22,toolUseId:'dropped'}}]},conversation:args.toolUseId?conversation({turns:[],lookup:{found:false}}):conversation({turns:[turn(99,{tool_calls:[{name:'Edit',is_error:true}]})]})}));
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();expect(document.querySelector('[data-panel-key^="failure:"]')?.getAttribute('data-turn')).toBe('99');
});

it.each(['same','facts'])('keeps conversation turn, summary and text nodes during %s updates',async mode=>{
 const cs=api(),facts={...session('lead'),conversation:conversation({turns:[turn(37,{tool_calls:[{name:'Read',native_id:'read-37',input:'app.ts',result:'read result'}]})]})};cs.sessionRead.mockResolvedValue(facts);const h=mount(cs,{turn:37});await settle();const row=h.root.querySelector('#turn-37'),summary=h.root.querySelector('summary'),text=h.root.querySelector('.turn-text')?.firstChild;const rail=h.root.querySelector('[data-jump="37"]');
 cs.sessionRead.mockResolvedValue(mode==='same'?facts:{...facts,conversation:{...facts.conversation,turns:[{...facts.conversation.turns[0],usage:{input_tokens:20}}]}});h.app.nudge();await settle();expect(h.root.querySelector('#turn-37')).toBe(row);expect(h.root.querySelector('summary')).toBe(summary);expect(h.root.querySelector('.turn-text')?.firstChild).toBe(text);expect(h.root.querySelector('[data-jump="37"]')).toBe(rail);
});
it('keeps panel model rows and controls during a live story read',async()=>{
 const cs=api();document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();const close=document.querySelector('[data-close]'),conversation=document.querySelector('[data-panel-key="conversation"]'),model=document.querySelector('.model-facts tbody tr');window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:agent-activity-changed'}}));await settle();expect(document.querySelector('[data-close]')).toBe(close);expect(document.querySelector('[data-panel-key="conversation"]')).toBe(conversation);expect(document.querySelector('.model-facts tbody tr')).toBe(model);
});
it('reapplies an authenticated identical conversation route without reading or remounting',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>pagedSession(args.turn??args.cursor??0,args.limit??20));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';let hash='#session/lead/turn/37';(window as any).__codesenseProxy={token:'test-route-token'};const app=mountTrail({cs,getHash:()=>hash});stops.push(()=>{app.stop();delete (window as any).__codesenseProxy;});await settle();const row=document.querySelector('#turn-37')!,label=row.querySelector('[data-row-label],.turn-head')!;const reveal=vi.fn();label.scrollIntoView=reveal;const reads=cs.sessionRead.mock.calls.length;
 window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:navigate',hash,token:'wrong',nonce:1}}));await settle();expect(reveal).not.toHaveBeenCalled();
 window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:navigate',hash,token:'test-route-token',nonce:2}}));await settle();expect(document.querySelector('#turn-37')).toBe(row);expect(reveal).toHaveBeenCalled();expect(cs.sessionRead).toHaveBeenCalledTimes(reads);
});
it('finds the indexed retained failure before the recent excerpt when the earliest failure was dropped',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>({...session('lead'),tools:{usedState:'truncated',used:[{name:'Edit',calls:2,errors:2,firstError:{turn:22,toolUseId:'dropped'},firstRetainedError:{turn:150,toolUseId:'retained'}}]},conversation:args.toolUseId?conversation({turns:args.toolUseId==='retained'?[turn(150,{tool_calls:[{name:'Edit',native_id:'retained',is_error:true}]})]:[],lookup:{found:args.toolUseId==='retained'}}):conversation({turnCount:200,turns:[turn(196),turn(197),turn(198),turn(199)]})}));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();expect(document.querySelector('[data-panel-key^="failure:"]')?.getAttribute('data-turn')).toBe('150');expect(cs.sessionRead).toHaveBeenCalledWith({sessionId:'lead',include:['conversation'],toolUseId:'retained',limit:1});expect(cs.sessionRead).not.toHaveBeenCalledWith(expect.objectContaining({toolUseId:'dropped'}));
});
it('reattaches the story and conversation nodes across route back after both reads complete',async()=>{
 const cs=api();document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';let hash='#story/SV-X';const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);await settle();const title=document.querySelector('h1'),row=document.querySelector('[data-open="lead"]');hash='#session/lead';app.route();await settle();const turn=document.querySelector('#turn-0');expect(turn).not.toBeNull();hash='#story/SV-X';app.route();await settle();expect(document.querySelector('h1')).toBe(title);expect(document.querySelector('[data-open="lead"]')).toBe(row);hash='#session/lead';app.route();await settle();expect(document.querySelector('#turn-0')).toBe(turn);
});
it('only the current read reveals a target reopened during an older read',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>pagedSession(args.turn??args.cursor??0,args.limit??20));const h=mount(cs);await settle();
 let finishOld:(value:unknown)=>void=()=>{},finishCurrent:(value:unknown)=>void=()=>{};
 cs.sessionRead.mockImplementationOnce(()=>new Promise(resolve=>{finishOld=resolve;})).mockImplementationOnce(()=>new Promise(resolve=>{finishCurrent=resolve;}));
 const reveal=vi.spyOn(HTMLElement.prototype,'scrollIntoView');h.app.jump(37);await settle();h.app.jump(37);await settle();expect(cs.sessionRead).toHaveBeenCalledTimes(3);
 finishOld(pagedSession(20));await settle();expect(h.root.querySelector('#turn-37')).toBeNull();expect(reveal).not.toHaveBeenCalled();expect(cs.sessionRead).toHaveBeenCalledTimes(3);
 finishCurrent(pagedSession(20));await settle();expect(h.root.querySelector('#turn-37')).not.toBeNull();expect(reveal).toHaveBeenCalled();reveal.mockRestore();
});

it('keeps refreshes aligned with the page established by a turn jump',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>({...session('lead'),conversation:conversation({turnCount:60,turns:Array.from({length:20},(_,i)=>turn((args.turn??args.cursor??0)+i)),nextCursor:(args.turn??args.cursor??0)+20})}));const h=mount(cs);await settle();h.app.jump(10);await settle();const before=[...h.root.querySelectorAll('.turn')].map(row=>row.id);expect(before[0]).toBe('turn-0');h.app.nudge();await settle();expect([...h.root.querySelectorAll('.turn')].map(row=>row.id)).toEqual(before);
});
it.each(['resolved','failed'])('keeps a panel session read %s result in its cache while on another route',async outcome=>{
 const cs=api();let resolve:(value:unknown)=>void=()=>{},reject:(error:Error)=>void=()=>{};const pending=new Promise((ok,no)=>{resolve=ok;reject=no;});cs.sessionRead.mockImplementation(args=>args.sessionId==='lead'?pending:Promise.resolve({...session('other'),conversation:conversation()}));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';let hash='#story/SV-X';const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);await settle();document.querySelector<HTMLButtonElement>('[data-open="lead"]')!.click();await settle();hash='#session/other';app.route();await settle();outcome==='resolved'?resolve({...session('lead',{session:{...session('lead').session,agent:'Panel owner done'}}),conversation:conversation()}):reject(new Error('Panel owner failed'));await settle();expect(document.querySelector('main')?.textContent).not.toContain('Panel owner');hash='#story/SV-X';app.route();expect(document.querySelector('#drill-panel')?.textContent).toContain(outcome==='resolved'?'Panel owner done':'Panel owner failed');
});
it.each(['resolved','failed'])('keeps a conversation session read %s result while its view is inactive',async outcome=>{
 const cs=api();let resolve:(value:unknown)=>void=()=>{},reject:(error:Error)=>void=()=>{};cs.sessionRead.mockImplementationOnce(()=>new Promise((ok,no)=>{resolve=ok;reject=no;}));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';let hash='#session/lead';const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);await settle();hash='#story/SV-X';app.route();await settle();outcome==='resolved'?resolve({...session('lead',{session:{...session('lead').session,agent:'Conversation owner done'}}),conversation:conversation()}):reject(new Error('Conversation owner failed'));await settle();expect(document.querySelector('main')?.textContent).not.toContain('Conversation owner');hash='#session/lead';app.route();expect(document.querySelector('main')?.textContent).toContain(outcome==='resolved'?'Conversation owner done':'Conversation owner failed');
});
it('reveals a requested turn when returning before its pending read completes',async()=>{
 const cs=api();let finish:(value:unknown)=>void=()=>{};const pending=new Promise(resolve=>{finish=resolve;});cs.sessionRead.mockImplementation(args=>args.turn===37?pending:Promise.resolve(pagedSession(args.cursor??0,args.limit??20)));document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';let hash='#session/lead/turn/0';const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);await settle();hash='#session/lead/turn/37';app.route();await settle();hash='#story/SV-X';app.route();await settle();hash='#session/lead/turn/37';app.route();await settle();const scroll=vi.spyOn(HTMLElement.prototype,'scrollIntoView');try{finish(pagedSession(37));await settle();expect(document.querySelector('#turn-37')).not.toBeNull();expect(scroll.mock.contexts.some(node=>(node as HTMLElement).matches?.('#turn-37 > .turn-head'))).toBe(true);}finally{scroll.mockRestore();}
});

function pagedSession(start=0,limit=20){
 const turns=Array.from({length:Math.min(limit,42-start)},(_,i)=>turn(start+i,{tool_calls:[]}));
 return {...session('lead'),conversation:conversation({turnCount:42,toolCallCount:0,turns,nextCursor:start+limit<42?start+limit:null})};
}
it.each(['later','rail'])('a passive read cannot swallow a held conversation %s click',async kind=>{
 window.history.replaceState(null,'','#session/lead?cursor=0');
 const cs=api();cs.sessionRead.mockImplementation(async args=>pagedSession(args.turn??args.cursor??0,args.limit??20));const h=mount(cs);await settle();
 let release:(value:unknown)=>void=()=>{},finishUser:(value:unknown)=>void=()=>{};cs.sessionRead.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;}));if(kind==='later')cs.sessionRead.mockImplementationOnce(()=>new Promise(resolve=>{finishUser=resolve;}));
 const control=h.root.querySelector<HTMLElement>(kind==='later'?'[data-conv-action=later]':'[data-jump="1"]')!;
 control.dispatchEvent(new MouseEvent('pointerdown',{bubbles:true,button:0}));h.app.nudge();await settle();
 control.dispatchEvent(new MouseEvent('pointerup',{bubbles:true,button:0}));control.click();await settle();
 expect(window.location.hash).toBe(kind==='later'?'#session/lead?from=20':'#session/lead?from=0&turn=1');
 expect(control.getAttribute('aria-disabled')).toBe(kind==='later'?'true':null);
 const other=h.root.querySelector<HTMLElement>(kind==='later'?'[data-jump="1"]':'[data-conv-action=later]')!;expect(other.getAttribute('aria-disabled')).not.toBe('true');
 release(pagedSession());await settle();if(kind==='later'){expect(control.getAttribute('aria-disabled')).toBe('true');finishUser(pagedSession(20));await settle();}expect(h.root.querySelector('.turn')?.id).toBe(kind==='later'?'turn-20':'turn-0');
});
it.each(['trail','panel'])('a passive read cannot swallow a held %s retry click',async kind=>{
 const cs=api(),read=kind==='trail'?cs.storyChanges:cs.sessionRead;read.mockRejectedValueOnce(new Error('First read failed'));
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';const app=mountTrail({cs,getHash:()=>'#story/SV-X'});stops.push(app.stop);await settle();
 if(kind==='panel'){document.querySelector<HTMLButtonElement>('[data-open=lead]')!.click();await settle();}
 let release:(value:unknown)=>void=()=>{},finishUser:(value:unknown)=>void=()=>{};read.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;})).mockImplementationOnce(()=>new Promise(resolve=>{finishUser=resolve;}));const control=document.querySelector<HTMLElement>(kind==='trail'?'[data-retry]':'[data-panel-retry]')!;
 control.dispatchEvent(new MouseEvent('pointerdown',{bubbles:true,button:0}));cs.subscribe.mock.calls[0][1].onChange();await settle();
 control.dispatchEvent(new MouseEvent('pointerup',{bubbles:true,button:0}));control.click();await settle();
 const result=kind==='trail'?story():{...session('lead'),conversation:conversation()};release(result);await settle();expect(read).toHaveBeenCalledTimes(3);finishUser(result);await settle();expect(read).toHaveBeenCalledTimes(3);
});
it.each(['rail','unhighlight'])('a newer %s action owns the page while paging is pending',async kind=>{
 const cs=api();cs.sessionRead.mockImplementation(async args=>pagedSession(args.turn??args.cursor??0,args.limit??20));const h=mount(cs,{turn:20});await settle();let release:(value:unknown)=>void=()=>{};cs.sessionRead.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;}));
 h.root.querySelector<HTMLButtonElement>('[data-conv-action=later]')!.click();await settle();h.root.querySelector<HTMLElement>(kind==='rail'?'[data-jump="21"]':'[data-conv-action=unhighlight]')!.click();await settle();
 expect(window.location.hash).toBe(kind==='rail'?'#session/lead?from=20&turn=21':'#session/lead?from=20');release(pagedSession(40));await settle();expect(h.root.querySelector('.turn')?.id).toBe('turn-20');
});

// Match reader.rs: filter retained indices at/after the selector, then take limit.
it.each([100,105,37])('Earlier stops before the aligned page containing %s, including a dropped interval',async first=>{
 const cs=api(),gap=first>=100?{droppedTurns:[1,99],droppedTurnCount:99,droppedBytes:10000,trimmedFields:0}:null;
 const stored=gap?[turn(0),...Array.from({length:100},(_,i)=>turn(100+i))]:Array.from({length:70},(_,i)=>turn(i));
 cs.sessionRead.mockImplementation(async ({cursor,turn:index,limit=20})=>{const rest=stored.filter(t=>t.index>=(index??cursor??0)),turns=rest.slice(0,limit);return {...session('lead'),conversation:conversation({state:gap?'truncated':'present',truncated:gap,turnCount:stored.length,turns,nextCursor:rest.length>limit?turns.at(-1)!.index+1:null})};});
 const h=mount(cs,{turn:first});await settle();h.root.querySelector<HTMLButtonElement>('[data-conv-action=earlier]')!.click();await settle();
 const indices=()=>[...h.root.querySelectorAll<HTMLElement>('article.turn')].map(el=>Number(el.id.slice(5)));
 const pageFirst=first>=100?100:20;expect(indices()).toEqual(stored.filter(t=>t.index<pageFirst).slice(-20).map(t=>t.index));
 const response=await cs.sessionRead.mock.results.at(-1)!.value;
 h.root.querySelector<HTMLButtonElement>('[data-conv-action=later]')!.click();await settle();
 expect(cs.sessionRead.mock.calls.at(-1)![0].cursor).toBe(response.conversation.nextCursor);
 expect(indices()[0]).toBe(pageFirst);
});
it('cached cursor routes restore both reading positions while explicit page requests reset them',async()=>{
 const h=mount();await settle();const thread=h.root.querySelector<HTMLElement>('.thread')!,rail=h.root.querySelector<HTMLElement>('.turn-rail')!;
 thread.scrollTop=500;rail.scrollTop=90;h.app.setVisible(false);h.root.remove();document.body.append(h.root);
 h.app.navigate({explicit:false});h.app.setVisible(true);await settle();
 expect(thread.scrollTop).toBe(500);expect(rail.scrollTop).toBe(90);
 h.app.navigate({explicit:true});await settle();expect(thread.scrollTop).toBe(0);expect(rail.scrollTop).toBe(0);
});

it('an aligned from route reopens the same page after remount',async()=>{
 const cs=api();cs.sessionRead.mockImplementation(async ({cursor,limit})=>({...session('lead'),conversation:conversation({turns:Array.from({length:limit},(_,i)=>turn(cursor+i)),nextCursor:cursor+limit})}));
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
 const app=mountTrail({cs,getHash:()=>'#session/lead?from=20'});stops.push(app.stop);await settle();
 expect([...document.querySelectorAll<HTMLElement>('article.turn')].map(el=>el.id)).toEqual(Array.from({length:20},(_,i)=>'turn-'+(20+i)));
});
it('a pending cursor route cannot borrow the previous page reading position',async()=>{
 const cs=api();let release!:(value:unknown)=>void;
 cs.sessionRead.mockImplementation(async args=>pagedSession(args.cursor??0,args.limit??20));
 const h=mount(cs);await settle();const thread=h.root.querySelector<HTMLElement>('.thread')!;thread.scrollTop=500;
 cs.sessionRead.mockImplementationOnce(()=>new Promise(resolve=>{release=resolve;}));h.app.navigate({from:20});await settle();h.app.setVisible(false);h.app.navigate({cursor:20,explicit:false});h.app.setVisible(true);
 release({...session('lead'),conversation:conversation({turns:[turn(20)],nextCursor:40})});await settle();expect(thread.scrollTop).toBe(0);
});
