// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import {mountTrail} from './page.mjs';
import {trailTestKit} from './test-kit';
import {story,session} from './fixtures.mjs';
import {navigationCases,navigationConversation,runNavigationMatrix} from './navigation-matrix.mjs';
const settle=async()=>{for(let i=0;i<50;i++)await Promise.resolve();};
let app:any;afterEach(()=>{app?.stop();document.body.replaceChildren();});
for(const spec of navigationCases)it(`page identity transition matrix: ${spec.name}`,async()=>{
 let cs:any,savedRoute='';const reading=()=>({thread:document.querySelector('.thread')!.scrollTop,rail:document.querySelector('.turn-rail')!.scrollTop});
 const revealed=vi.spyOn(HTMLElement.prototype,'scrollIntoView').mockImplementation(()=>{});
 try{await runNavigationMatrix(spec,{
  async open(hash:string){app?.stop();document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';history.replaceState(null,'',hash);cs={kit:trailTestKit(),storyChanges:vi.fn().mockResolvedValue(story()),sessionRead:vi.fn(async(args:any)=>({...session('lead'),conversation:navigationConversation(spec,args)})),sessions:vi.fn().mockResolvedValue([]),subscribe:vi.fn().mockResolvedValue({}),unsubscribe:vi.fn(),onVisibility:(f:any)=>{f(true);return ()=>{};}};app=mountTrail({cs});await settle();},
  async snapshot(){return {indices:[...document.querySelectorAll<HTMLElement>('article.turn')].map(node=>Number(node.id.slice(5))),highlight:[...document.querySelectorAll<HTMLElement>('article.turn.highlight')].map(node=>Number(node.id.slice(5))),route:location.hash,reading:reading()};},
  async refresh(){cs.subscribe.mock.calls.at(-1)[1].onChange();await settle();},
  async scroll(thread:number,rail:number){document.querySelector('.thread')!.scrollTop=thread;document.querySelector('.turn-rail')!.scrollTop=rail;return reading();},
  async click(action:string,index?:number){const before=reading();document.querySelector<HTMLButtonElement>(action==='jump'?`[data-jump="${index}"]`:`[data-conv-action="${action}"]`)!.click();await settle();return before;},
  async revealed(index:number){expect(revealed.mock.instances.some((node:any)=>node.closest('article.turn')?.id==='turn-'+index)).toBe(true);},
  async story(){savedRoute=location.hash;history.pushState(null,'','#story/SV-X');app.route();await settle();expect(document.querySelector('[data-open=lead]')).not.toBeNull();},
  async back(){history.replaceState(null,'',savedRoute);app.route(null,false);await settle();},
 },expect);}finally{revealed.mockRestore();}
});
