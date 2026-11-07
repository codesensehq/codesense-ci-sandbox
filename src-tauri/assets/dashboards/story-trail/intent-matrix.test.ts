// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import {mountTrail} from './page.mjs';
import {trailTestKit} from './test-kit';
import {story,session,task} from './fixtures.mjs';
import {conversation,turn} from './conversation-fixtures.mjs';
import {navigationConversation} from './navigation-matrix.mjs';

type Held={value:any;done:boolean;resolve:(value:any)=>void;reject:(error:Error)=>void};
const cleanup:(()=>void)[]=[];
afterEach(()=>{cleanup.splice(0).forEach(stop=>stop());document.body.replaceChildren();vi.restoreAllMocks();});
// Drain promise work without advancing a clock; replies advance only when released below.
const flush=async()=>{for(let i=0;i<60;i++)await Promise.resolve();};
const transitions=[
 {name:'navigate turn',view:'conversation',read:true,from:40,highlight:47,focus:'#outside'},
 {name:'Remove highlight',view:'conversation',read:false,from:20,highlight:null,focus:'.conversation-head h1'},
 {name:'re-click loaded turn',view:'conversation',read:false,from:20,highlight:27,focus:'[data-jump="27"]'},
 {name:'Earlier',view:'conversation',read:true,from:0,highlight:null,focus:'.conversation-head h1'},
 {name:'Later',view:'conversation',read:true,from:40,highlight:null,focus:'[data-conv-action="later"]'},
 {name:'story switch',view:'trail',read:true,focus:'#outside'},
 {name:'sort',view:'trail',read:false,focus:'[data-sort="cost"]'},
 {name:'selection',view:'panel',read:true,focus:'#drill-panel [data-close]'},
] as const;
const pendingStates=[{name:'none',kind:'none',error:false},{name:'passive',kind:'passive',error:false},{name:'passive error',kind:'passive',error:true},{name:'user',kind:'user',error:false},{name:'user error',kind:'user',error:true}] as const;

for(const transition of transitions)for(const pending of pendingStates)it(`${transition.name} × ${pending.name}: one current intent owns facts, route and focus`,async()=>{
 vi.spyOn(document,'hasFocus').mockReturnValue(true);
 vi.spyOn(HTMLElement.prototype,'scrollIntoView').mockImplementation(()=>{});
 document.body.innerHTML='<button id="outside">Host</button><main id="trail"></main><aside id="drill-panel" hidden></aside>';
 const q=(selector:string)=>document.querySelector<HTMLElement>(selector)!;
 const click=(selector:string)=>{const node=q(selector);expect(node,selector).not.toBeNull();node.focus();node.click();};
 let epoch=1,hold=false,failNext=false,nudge=()=>{};
 const held:Held[]=[],reads:any[]=[];
 const watched=transition.view==='trail'?'story':'session';
 function fetch(kind:string,args:any,value:any){
  if(kind!==watched)return Promise.resolve(value);
  reads.push(args);
  if(failNext){failNext=false;return Promise.reject(new Error('Previous read failed'));}
  if(!hold)return Promise.resolve(value);
  return new Promise((resolve,reject)=>held.push({value,done:false,resolve,reject}));
 }
 function facts(id:string,storyId:string){
  const base=session(id);return {...base,session:{...base.session,storyId,agent:`Facts ${epoch}`},story:{...base.story,id:storyId,title:`${storyId} facts ${epoch}`}};
 }
 const cs={
  kit:trailTestKit(),
  storyChanges:vi.fn((args:any)=>{const id=args.storyId,root=task(`story:${id}`,null,{storyId:id,goal:`${id} facts ${epoch}`,taskType:'story'}),lead=task('lead',root.taskId,{storyId:id});return fetch('story',args,story({storyId:id,tasks:[root,lead],sessions:[{...facts('lead',id),tasks:[lead]},facts('second',id)]}));}),
  sessionRead:vi.fn((args:any)=>{
   const value=facts(args.sessionId,'SV-A');
   if(transition.view==='panel')return fetch('session',args,{...value,conversation:conversation({state:'absent',turns:[],turnCount:0,nextCursor:null})});
   const from=Math.floor((args.turn??args.cursor??20)/20)*20;
   return fetch('session',args,{...value,conversation:conversation({turnCount:80,turns:Array.from({length:20},(_,i)=>turn(from+i,{text:`Facts ${epoch} turn ${from+i}`,tool_calls:[]})),nextCursor:from+20<80?from+20:null,lookup:args.turn==null?null:{kind:'turn',requested:String(args.turn),found:true,turnIndex:args.turn}})});
  }),
  sessions:vi.fn().mockResolvedValue([]),openFile:vi.fn().mockResolvedValue({}),gotoAgent:vi.fn().mockResolvedValue({}),
  subscribe:vi.fn((_topic:string,options:any)=>{nudge=options.onChange;return Promise.resolve({});}),unsubscribe:vi.fn(),
  onVisibility:(show:(value:boolean)=>void)=>{show(true);return ()=>{};},
 };
 history.replaceState(null,'',transition.view==='conversation'?'#session/lead/turn/27':'#story/SV-A');
 const app=mountTrail({cs});
 cleanup.push(()=>{app.stop();for(const reply of held)if(!reply.done){reply.done=true;reply.resolve(reply.value);}});
 const route=(hash:string)=>{q('#outside').focus();history.replaceState(null,'',hash);app.route(hash);};
 await flush();
 if(transition.view==='panel'){click('[data-open="lead"]');await flush();}
 // A user-owned pending read uses the ordinary Retry entry in the story/panel.
 if(pending.kind==='user'&&transition.view!=='conversation'){
  failNext=true;nudge();await flush();
  expect(q(transition.view==='panel'?'[data-panel-retry]':'[data-retry]')).not.toBeNull();
 }
 reads.length=0;
 if(pending.kind!=='none'){
  epoch=2;hold=true;
  if(pending.kind==='passive')nudge();
  else if(transition.view==='conversation')route('#session/lead/turn/67');
  else click(transition.view==='panel'?'[data-panel-retry]':'[data-retry]');
  await flush();expect(held).toHaveLength(1);
 }
 epoch=pending.kind!=='none'||transition.read?3:1;
 switch(transition.name){
  case 'navigate turn':route('#session/lead/turn/47');break;
  case 'Remove highlight':click('[data-conv-action="unhighlight"]');break;
  case 're-click loaded turn':click('[data-jump="27"]');break;
  case 'Earlier':click('[data-conv-action="earlier"]');break;
  case 'Later':click('[data-conv-action="later"]');break;
  case 'story switch':route('#story/SV-B');break;
  case 'sort':click('[data-sort="cost"]');break;
  case 'selection':click('[data-open="second"]');break;
 }
 await flush();
 const beforeRelease=reads.length;
 if(pending.kind!=='none'){
  const previous=held[0];previous.done=true;
  pending.error?previous.reject(new Error('Superseded reply failed')):previous.resolve(previous.value);
  await flush();
  // New-page reads already own the current token. UI-only changes need one trailing read.
  expect(reads.length-beforeRelease).toBe(transition.read?0:1);
 }
 hold=false;
 for(const reply of held)if(!reply.done){reply.done=true;reply.resolve(reply.value);}
 await flush();
 expect(reads).toHaveLength(pending.kind!=='none'?2:transition.read?1:0);
 if(transition.view==='conversation'){
  expect(q('.conversation-head h1').textContent).toBe(`Facts ${epoch}`);
  expect(location.hash).toBe(`#session/lead?from=${transition.from}${transition.highlight==null?'':`&turn=${transition.highlight}`}`);
  expect([...document.querySelectorAll('.turn.highlight')].map(node=>Number(node.id.slice(5)))).toEqual(transition.highlight==null?[]:[transition.highlight]);
 }else{
  const id=transition.name==='story switch'?'SV-B':'SV-A';
  expect(location.hash).toBe(`#story/${id}`);
  if(transition.view==='panel'){
   expect(q('#drill-panel h2').textContent).toBe(`Facts ${epoch}`);
   expect([...document.querySelectorAll('[data-session][aria-selected="true"]')].map((node:HTMLElement)=>node.dataset.session)).toEqual(['second']);
  }else{
   expect(q('h1').textContent).toBe(`${id} facts ${epoch}`);
   expect(document.querySelector('[data-session][aria-selected="true"]')).toBeNull();
  }
 }
 expect(document.activeElement).toBe(q(transition.focus));
 expect(document.body.textContent).not.toContain('Superseded reply failed');
});

// One navigation fact per transition: the page on screen, the aligned page start and one host channel.
// Each row releases deferred replies and dispatches events in the order it names; no clock moves.
type Facts={indices:number[];held:{value:any;resolve:(value:any)=>void}[];reads:any[];hold:boolean;app:any;nudge:()=>void};
async function conversationHarness(hash:string,total:number):Promise<Facts>{
 vi.spyOn(document,'hasFocus').mockReturnValue(true);
 vi.spyOn(HTMLElement.prototype,'scrollIntoView').mockImplementation(()=>{});
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
 const facts:Facts={indices:Array.from({length:total},(_,i)=>i),held:[],reads:[],hold:false,app:null,nudge:()=>{}};
 const cs={
  kit:trailTestKit(),storyChanges:vi.fn().mockResolvedValue(story({storyId:'SV-A'})),
  sessionRead:vi.fn((args:any)=>{
   const value={...session('lead'),conversation:navigationConversation({indices:[...facts.indices],gap:null},args)};
   facts.reads.push(args);
   return facts.hold?new Promise(resolve=>facts.held.push({value,resolve})):Promise.resolve(value);
  }),
  sessions:vi.fn().mockResolvedValue([]),openFile:vi.fn().mockResolvedValue({}),gotoAgent:vi.fn().mockResolvedValue({}),
  subscribe:vi.fn((_topic:string,options:any)=>{facts.nudge=options.onChange;return Promise.resolve({});}),unsubscribe:vi.fn(),
  onVisibility:(show:(value:boolean)=>void)=>{show(true);return ()=>{};},
 };
 history.replaceState(null,'',hash);
 facts.app=mountTrail({cs});
 cleanup.push(()=>{facts.app.stop();for(const reply of facts.held.splice(0))reply.resolve(reply.value);});
 await flush();
 return facts;
}
const $=(selector:string)=>document.querySelector<HTMLElement>(selector)!;
const shown=()=>[...document.querySelectorAll<HTMLElement>('article.turn')].map(node=>Number(node.id.slice(5)));
const reading=()=>({thread:$('.thread').scrollTop,rail:$('.turn-rail').scrollTop});
const release=async(facts:Facts)=>{const reply=facts.held.shift()!;reply.resolve(reply.value);await flush();};
const locationEvent=()=>new Promise<void>(resolve=>{const done=()=>{removeEventListener('popstate',done);removeEventListener('hashchange',done);resolve();};addEventListener('popstate',done);addEventListener('hashchange',done);});
const traverseBack=async()=>{const moved=locationEvent();history.back();await moved;await flush();};
// The host applies one open request as a Location.href fragment navigation plus an authenticated message. The fragment navigation is spelled out with WebKit's event order (popstate, then hashchange when
// the fragment changed); happy-dom's own Location fragment events differ.
const hostLocation=(hash:string)=>{const changed=location.hash!==hash;history[changed?'pushState':'replaceState'](null,'',hash);dispatchEvent(new PopStateEvent('popstate',{state:null}));if(changed)dispatchEvent(new HashChangeEvent('hashchange'));};
const hostMessage=(hash:string)=>window.dispatchEvent(new MessageEvent('message',{source:window.parent,data:{type:'codesense:navigate',hash,token:'row-token',nonce:1}}));

const factRows=[
 // A: Earlier and Later target the page on screen, not a committed reply whose render is held by the gesture.
 ...([['later','earlier',0],['earlier','later',40]] as const).map(([pending,clicked,from])=>({
  name:`${pending} pending × pointer-down on ${clicked} × reply mid-gesture × click → from=${from}`,
  async run(){
   const facts=await conversationHarness('#session/lead?from=20',70);
   facts.hold=true;$(`[data-conv-action="${pending}"]`).click();await flush();expect(facts.held).toHaveLength(1);
   const target=$(`[data-conv-action="${clicked}"]`);target.dispatchEvent(new MouseEvent('pointerdown',{bubbles:true,button:0}));
   await release(facts);
   expect(shown()[0]).toBe(20);
   facts.reads.length=0;facts.hold=false;target.focus();target.click();await flush();
   expect(location.hash).toBe(`#session/lead?from=${from}`);
   expect(document.querySelectorAll('.turn.highlight')).toHaveLength(0);
   expect(shown()).toEqual(Array.from({length:20},(_,i)=>from+i));
   expect(facts.reads).toHaveLength(1);
  },
 })),
 // B: reading positions belong to the aligned page start, so a growing last page keeps them.
 {name:'the last page grows by N turns between leave and history back → reading equals saved',async run(){
  const facts=await conversationHarness('#session/lead?from=40',46);
  expect(shown()).toEqual([40,41,42,43,44,45]);
  $('.thread').scrollTop=500;$('.turn-rail').scrollTop=90;const saved=reading();
  history.pushState({storyTrail:true},'',location.hash);
  $('[data-conv-action="earlier"]').click();await flush();expect(shown()[0]).toBe(20);expect(reading()).toEqual({thread:0,rail:0});
  facts.indices=Array.from({length:51},(_,i)=>i);
  await traverseBack();
  expect(shown()).toEqual(Array.from({length:11},(_,i)=>40+i));
  expect(reading()).toEqual(saved);
 }},
 {name:'a passive refresh while hidden, then setVisible(true) → reading equals saved',async run(){
  const facts=await conversationHarness('#session/lead?from=40',46);
  $('.thread').scrollTop=500;$('.turn-rail').scrollTop=90;const saved=reading();
  facts.indices=Array.from({length:51},(_,i)=>i);facts.hold=true;facts.nudge();await flush();expect(facts.held).toHaveLength(1);
  history.pushState(null,'','#story/SV-A');facts.app.route();await flush();
  await release(facts);
  history.replaceState(null,'','#session/lead?from=40');facts.app.route(null,false);await flush();
  expect(shown()).toEqual(Array.from({length:11},(_,i)=>40+i));
  expect(reading()).toEqual(saved);
 }},
 // C: one host open request routes once, whichever channel the page hears first.
 ...(['location first','message first'] as const).map(order=>({
  name:`host navigate to a turn from the story with the entry loaded, ${order} → back returns to the story, one read`,
  async run(){
   (window as any).__codesenseProxy={token:'row-token'};cleanup.push(()=>{delete (window as any).__codesenseProxy;});
   const facts=await conversationHarness('#session/lead/turn/27',70);
   history.pushState(null,'','#story/SV-A');facts.app.route();await flush();expect($('h1').textContent).toBe('A story');
   facts.reads.length=0;const hash='#session/lead/turn/47';
   // postMessage is its own task: the first channel's read is in flight when the second arrives.
   facts.hold=true;
   const [first,second]=order==='location first'?[hostLocation,hostMessage]:[hostMessage,hostLocation];
   first(hash);await flush();expect(facts.held).toHaveLength(1);second(hash);await flush();
   facts.hold=false;await release(facts);
   expect(location.hash).toBe('#session/lead?from=40&turn=47');
   expect([...document.querySelectorAll<HTMLElement>('.turn.highlight')].map(node=>node.id)).toEqual(['turn-47']);
   // One read: the turn lookup and its aligned page.
   expect(facts.reads.map(({turn,cursor})=>({turn,cursor}))).toEqual([{turn:47,cursor:undefined},{turn:undefined,cursor:40}]);
   await traverseBack();
   expect(location.hash).toBe('#story/SV-A');expect($('h1').textContent).toBe('A story');
  },
 })),
 // D: re-clicking the loaded turn keeps one history entry.
 {name:'re-click a loaded turn 3×, then back once → the previous route',async run(){
  await conversationHarness('#session/lead/turn/27',70);
  const before=location.hash,length=history.length;
  for(let i=0;i<3;i++){$('[data-jump="25"]').click();await flush();}
  expect(location.hash).toBe('#session/lead?from=20&turn=25');expect(history.length).toBe(length+1);
  await traverseBack();
  expect(location.hash).toBe(before);
  expect([...document.querySelectorAll<HTMLElement>('.turn.highlight')].map(node=>node.id)).toEqual(['turn-27']);
 }},
];
for(const row of factRows)it(`navigation fact: ${row.name}`,()=>row.run());
