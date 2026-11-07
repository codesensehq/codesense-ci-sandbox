// SPDX-License-Identifier: MIT
import {afterEach,expect,it,vi} from 'vitest';
import {mountTrail} from './page.mjs';
import {trailTestKit} from './test-kit';
import {contributionStory} from './contribution-fixtures.mjs';
import {session} from './fixtures.mjs';
import {conversation,turn} from './conversation-fixtures.mjs';
const stops:(()=>void)[]=[];
afterEach(()=>{stops.splice(0).forEach(stop=>stop());document.body.replaceChildren();});
const flush=async()=>{for(let i=0;i<80;i++)await Promise.resolve();};
function mount(initial='#story/SV-X'){
 document.body.innerHTML='<main id="trail"></main><aside id="drill-panel" hidden></aside>';
 let hash=initial;
 const call={name:'Edit',native_id:'edit-1',input:JSON.stringify({file_path:'/repo/worktree/src/app.ts',start_line:2,end_line:3}),result:'Done',is_error:false};
 const cs={kit:trailTestKit(),storyChanges:vi.fn().mockResolvedValue(contributionStory({sourceRepo:'example/project'})),sessionRead:vi.fn().mockResolvedValue({...session('lead'),conversation:conversation({turns:[turn(0,{tool_calls:[call]})]})}),sessions:vi.fn().mockResolvedValue([]),openDiff:vi.fn().mockResolvedValue({status:'opened'}),openFile:vi.fn().mockResolvedValue({}),subscribe:vi.fn().mockResolvedValue({}),unsubscribe:vi.fn(),onVisibility:(show:(v:boolean)=>void)=>{show(true);return ()=>{};}};
 const app=mountTrail({cs,getHash:()=>hash});stops.push(app.stop);
 return {cs,focus:()=>{if(document.activeElement===document.body)[...document.querySelectorAll<HTMLElement>('button,a[href],[tabindex]')].find(node=>node.tabIndex>=0&&!node.closest('[hidden]')&&!node.matches(':disabled'))?.focus();},route:(next:string)=>{hash=next;app.route(next);}};
}
function click(selector:string){const node=document.querySelector<HTMLElement>(selector);expect(node,selector).not.toBeNull();node!.click();}
it('files row opens its explicitly chosen recorded commit',async()=>{
 const h=mount();await flush();click('[aria-label="Files changed"] [data-diff-rev="'+ 'a'.repeat(40)+'"]');await flush();expect(h.cs.openDiff).toHaveBeenCalledWith({repo:'example/project',rev:'a'.repeat(40),path:'src/app.ts',storyId:'SV-X'});
});
it('the commit SHA itself opens the recorded first file, not a guessed revision',async()=>{
 const h=mount();await flush();const sha=[...document.querySelectorAll<HTMLElement>('[data-commits] button')].find(node=>node.textContent?.trim()==='aaaaaaa');expect(sha,'a keyboard-operable commit SHA').toBeTruthy();sha!.click();await flush();expect(h.cs.openDiff).toHaveBeenCalledWith({repo:'example/project',rev:'a'.repeat(40),path:'src/app.ts',storyId:'SV-X'});
});
it('a conversation edit chip opens the story WORKTREE at its recorded after range',async()=>{
 const h=mount('#session/lead?from=0');await flush();click('button.edit-chip');await flush();expect(h.cs.storyChanges).toHaveBeenCalledWith({storyId:'SV-X'});expect(h.cs.openDiff).toHaveBeenCalledWith({repo:'example/project',rev:'WORKTREE',path:'src/app.ts',storyId:'SV-X',range:{start:2,end:3}});
});
it('case18: a late edit context read only updates its inactive owner',async()=>{
 const h=mount('#session/lead?from=0');await flush();let finish!:(value:unknown)=>void;h.cs.storyChanges.mockImplementationOnce(()=>new Promise(resolve=>{finish=resolve;}));click('button.edit-chip');await flush();h.route('#story/SV-B');await flush();finish(contributionStory({sourceRepo:'example/project'}));await flush();expect(h.cs.openDiff).not.toHaveBeenCalled();expect(document.querySelector('[data-open=lead]')).not.toBeNull();
});

it('host entry keeps the story primary control ahead of metadata links',async()=>{
 const h=mount();await flush();h.focus();expect(document.activeElement).toBe(document.querySelector('[data-flat]'));expect(document.querySelector<HTMLAnchorElement>('[data-prs] a')?.tabIndex).toBe(0);
});
it('conversation host entry keeps the default after leaving a story',async()=>{
 const h=mount();await flush();h.route('#session/lead?from=0');await flush();h.focus();expect(document.activeElement).toBe(document.querySelector('[data-conv-action=back]'));
});
it('the header carries contribution facts as text with no focusable elements',async()=>{
 mount();await flush();const header=document.querySelector('[data-contribution-meta]')!;expect(header.textContent).toMatch(/Merged.*PR #17.*branch feat\/SV-X/s);expect(header.querySelectorAll('a,button,input,select,textarea,[tabindex]')).toHaveLength(0);
});
