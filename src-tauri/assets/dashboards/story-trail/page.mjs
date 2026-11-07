// SPDX-License-Identifier: MIT
import {createTrailView} from './trail-view.mjs';
import {createFocusIntents} from './focus.mjs';
import {createPanelView} from './panel-view.mjs';
import {mountConversation} from './conversation-view.mjs';
import {tailCursor,NEEDS_NEWER} from './conversation.mjs';
import {newRateState,transitionRate} from './rate-state.mjs';
import {sessionFacts,rateUsagePriced,lineRefusal,lineRevision,prefillLines,storyFile} from './model.mjs';
import {createIntentReader} from './intent-reader.mjs';

export function mountTrail({cs = window.codesense, doc = document, win = window,
  getHash = () => win.location.hash, navigate = hash => {win.location.hash = hash;}} = {}) {
  const root = doc.getElementById('trail'), panel = doc.getElementById('drill-panel');
  const cache = new Map(), offs = [], collapsed = new Set(), panels = new Map(), conversations=new Map();
  let conversationView=null,routedPopstate=false,hostRoute=null,stopped=false;
  let focusedTask = null;
  let current = null, currentId = '', visible = false;
  let sort = 'when', direction = 1, flat = false, byTask = false, opener = null, rateOpener = null, activeRate = null, lastRate = null, nextRateToken = 0;
  const focus=createFocusIntents({root:doc.body,doc,context:element=>({rowId:element.closest('#rate-form')?.dataset.rowId ?? element.closest('#drill-panel')?.dataset.rowId ?? (element.hasAttribute('data-retry')?lastRate?.rowId:null)})});
  function renderPanel(since=undefined,passive=since!==undefined) {
    if(passive&&view.patches.defer(renderPanel,()=>renderPanel(since,passive)))return;
    const id=current?.selected,lines=current?.lines;
    if(id)panelOwner(id);
    let intent=focus.capture();
    if(since!==undefined)({intent}=focus.completion(intent,{since}));
    panelView.render(lines?{kind:'lines',...lines,storyId:currentId,data:current.data,diffResult:current.diffResult}:id?panels.get(id):null,passive);
    focus.reconcile(intent);
    if(lines)current.linesReader.resume();else if(id)panels.get(id).reader.resume();
  }
  async function readPanel({state}){
      if(typeof cs.sessionRead!=='function')throw new Error(NEEDS_NEWER);
      let result=await cs.sessionRead({sessionId:state.id,include:['conversation'],limit:1});
      if(result?.conversation?.nextCursor!=null && visible)result=await cs.sessionRead({sessionId:state.id,include:['conversation'],cursor:tailCursor(result.conversation),limit:4});
      const failedTurns={};
      await Promise.all((result.tools?.used??[]).map(async tool=>{
        const candidate='firstRetainedError' in tool?tool.firstRetainedError:tool.firstError;if(candidate?.turn==null)return;
        try{const lookup=await cs.sessionRead({sessionId:state.id,include:['conversation'],...(candidate.toolUseId?{toolUseId:candidate.toolUseId}:{turn:candidate.turn}),limit:1});
          if(lookup.conversation?.lookup?.found===true&&lookup.conversation.turns?.some(turn=>turn.index===candidate.turn&&turn.tool_calls?.some(call=>call.name===tool.name&&call.is_error===true&&(!candidate.toolUseId||call.native_id===candidate.toolUseId))))failedTurns[JSON.stringify([tool.name,tool.mcpServer??null])]=candidate.turn;
        }catch{/* Unavailable conversation evidence must not create a dead jump. */}
      }));
      return {result,failedTurns};
  }
  function beginRate(model,rowId,focusKey){
    current.reader.intent();
    rateOpener=focusKey;activeRate=newRateState(rowId,model,++nextRateToken);lastRate=null;render({rowId:activeRate.rowId,controlId:'#rate-form [name=modelPrefix]',sectionId:activeRate.rowId.startsWith('task:')?'tasks-title':'sessions-title'},true);
  }
  async function openConversation(id,turn){
    const since=focus.capture();
    try{await cs.openFile({path:`story-trail.html#session/${encodeURIComponent(id)}${turn==null?'':`/turn/${turn}`}`,newTab:true});}
    catch(error){const state=panels.get(id);if(state){state.error=String(error.message||error);renderPanel(since);}}
  }
  const view=createTrailView({root,panel,kit:cs.kit,doc,panelRenderer:passive=>renderPanel(undefined,passive)});
  const panelView=createPanelView({root:panel,kit:cs.kit,patch:view.patches,doc});
  function panelOwner(id){
    if(panels.has(id))return panels.get(id);
    const state={id,data:null,error:''};panels.set(id,state);
    state.reader=createIntentReader({
      enabled:()=>visible&&current?.selected===id,
      capture:()=>({state,since:focus.capture(),basis:current?.data?.sessions.find(row=>sessionFacts(row).id===id)?.basis??state.data?.basis}),
      read:readPanel,
      commit({result,failedTurns},{basis}){state.data={...result,basis};state.failedTurns=failedTurns;state.error='';},
      fail(error){state.error=String(error.message||error);},
      settled({since}){delete state.actionControl;if(current?.selected===id)renderPanel(since);},
    });
    return state;
  }
  // The story's own ranges, read page by page (bounded) until the file appears. Pages are kept until the
  // story's next read, and paging stops as soon as this pick no longer owns the panel.
  const RANGE_PAGES=10;
  async function storyRanges(owner,lines){
    const cache=owner.ranges??={commits:[],next:undefined,pages:0,pending:null};
    let range=prefillLines(cache,lines.path);
    if(typeof cs.storyChanges!=='function')return {range};
    while(!range&&owner.lines===lines&&cache.next!==null&&cache.pages<RANGE_PAGES){
      cache.pending??=cs.storyChanges({storyId:owner.id,include:['ranges'],...(cache.next?{cursor:cache.next}:{})}).then(page=>{cache.commits.push(...(page.commits??[]));cache.next=page.nextCursor??null;cache.pages++;}).finally(()=>{cache.pending=null;});
      await cache.pending;range=prefillLines(cache,lines.path);
    }
    return {range,capped:!range&&cache.next!==null&&cache.pages>=RANGE_PAGES};
  }
  // One line read per story: a newer pick or Close owns the reply, and a closed panel reads nothing.
  // Only a file this story changed is read, so a panel route cannot reach other files in the repository.
  async function readLines({owner,lines}){
    if(!lines)return null;
    const unavailable=reason=>({range:null,result:{status:'unavailable',reason}});
    if(!storyFile(owner.data,lines.path))return unavailable('Not a file this story changed.');
    if(!owner.data.sourceRepo)return unavailable('The story’s source repository is not configured.');
    const {range,capped}=lines.start==null?await storyRanges(owner,lines):{range:{start:lines.start,end:lines.end}};
    if(owner.lines!==lines)return null;
    if(!range)return {range,result:{status:'pick',capped}};
    const revision=lineRevision(owner.data,lines.path);
    if(!revision)return {range,result:{status:'unavailable',reason:'No merged change or local story branch is recorded for this file.'}};
    if(typeof cs.whyLine!=='function')return unavailable(NEEDS_NEWER);
    return {range,result:await cs.whyLine({repo:owner.data.sourceRepo,rev:revision.rev,path:lines.path,...range})};
  }
  function linesOwner(owner){
    owner.linesReader??=createIntentReader({
      enabled:()=>visible&&current===owner&&!!owner.lines&&!!owner.data,
      // Each pick is a new lines object, so identity says whether a read still owns the panel.
      capture:()=>({owner,lines:owner.lines,since:focus.capture()}),
      read:readLines,
      // Only a prefilled range changes the route; a passive re-read of the same range posts nothing.
      commit(answer,{lines}){if(!answer||owner.lines!==lines)return;const prefilled=lines.start==null;Object.assign(lines,answer.range??{},{result:answer.result,error:''});if(current===owner&&prefilled&&answer.range)linesRoute();},
      fail(error){if(owner.lines)owner.lines.error=String(error.message||error);},
      settled({since}){if(current===owner&&owner.lines)renderPanel(since);},
    });
  }
  function linesRoute(){
    const lines=current?.lines,params=lines?new URLSearchParams({lines:lines.path,...(lines.start==null?{}:{start:lines.start,end:lines.end})}):null;
    const hash=`#story/${encodeURIComponent(currentId)}${params?'?'+params:''}`;
    win.history.replaceState({storyTrail:true},'',hash);
    win.parent.postMessage({type:'codesense:route-changed',hash,token:win.__codesenseProxy?.token},'*');
  }
  function openLines(path,range=null,{route=true}={}){
    const owner=current;owner.reader.intent();owner.selected=null;linesOwner(owner);
    owner.lines={path,start:range?.start??null,end:range?.end??null,result:null,error:'',refused:''};owner.diffResult=null;owner.diffToken=(owner.diffToken??0)+1;
    owner.linesReader.intent({read:true});
    if(route){linesRoute();render({rowId:'file:'+path,controlId:'#drill-panel [data-close]',sectionId:'files-title'},true);}
  }
  function pickLines(form){
    const lines=current?.lines;if(!lines)return;
    const values=new win.FormData(form),start=Number(values.get('linesStart')),end=Number(values.get('linesEnd'));
    lines.refused=lineRefusal(start,end)??'';
    if(!lines.refused){current.lines={...lines,start,end,result:null,error:''};current.diffResult=null;current.linesReader.intent({read:true});linesRoute();}
    renderPanel(undefined,false);
  }
  function trailOwner(id){
    if(cache.has(id))return cache.get(id);
    const state={id,data:null,error:'',selected:null,scroll:0};cache.set(id,state);
    state.reader=createIntentReader({
      enabled:()=>visible&&current===state,
      capture:()=>({since:focus.capture(),rateRequest:current===state?lastRate:state.ui?.lastRate,cursor:state.moreCursor??undefined,reload:state.actionControl?.hasAttribute('data-reload-commits')}),
      read:({cursor})=>typeof cs.storyChanges==='function'?cs.storyChanges({storyId:id,...(cursor?{cursor}:{})}):Promise.reject(new Error(NEEDS_NEWER)),
      commit(data,{rateRequest,cursor,reload}){
        if(cursor){const history=state.commitHistory;if(history){const seen=new Set([...(state.data?.commits??[]),...history.extra].map(row=>row.sha));for(const commit of data.commits??[])if(!seen.has(commit.sha)){history.extra.push(commit);seen.add(commit.sha);}history.nextCursor=data.nextCursor;history.busy=false;}state.moreCursor=null;return;}
        if(reload)state.commitHistory=null;state.ranges=null;syncCommitHistory(state,data);state.data=data;state.error='';state.updated=Date.now();
        const pending=current===state?lastRate:state.ui?.lastRate;
        if(pending?.phase==='pending-acceptance'&&pending.requestToken===rateRequest?.requestToken&&rateUsagePriced(pending,data.sessions)){
          const priced=transitionRate(pending,{type:'priced',requestToken:rateRequest.requestToken});state.ui={...state.ui,lastRate:priced};if(current===state)lastRate=priced;
        }
      },
      fail(error,{cursor}){const message=String(error.message||error);if(cursor&&state.commitHistory){state.commitHistory.error=message;state.commitHistory.busy=false;if(/stale/i.test(message))state.commitHistory.stale=true;state.moreCursor=null;}else state.error=message;},
      settled({since}){delete state.actionControl;if(current===state)render(focus.capture(),false,since);},
    });
    return state;
  }
  function render(intent=focus.capture(),reveal=false,since=undefined){
    if(!current)return;
    if(since!==undefined&&view.patches.defer(render,()=>render(intent,reveal,since)))return;
    if(since!==undefined)({intent,reveal}=focus.completion(intent,{since,reveal}));
    view.render({id:currentId,data:current.data,error:current.error,updated:current.updated,actionControl:current.actionControl,selected:current.selected,sort,direction,flat,byTask,collapsed,focusedTask,activeRate,lastRate,passive:since!==undefined,commitHistory:current.commitHistory,changesError:current.changesError,diffResult:current.lines?null:current.diffResult});
    focus.reconcile(intent,{reveal});
  }
  function nudge(){
    for(const state of cache.values()){state.reader.request();if(state.lines)state.linesReader.request();}for(const state of panels.values())state.reader.request();conversationView?.nudge();
  }
  function syncCommitHistory(owner,data){
    if(!Array.isArray(data.commits))return;
    const history=owner.commitHistory;
    if(!history)owner.commitHistory={baseCursor:data.nextCursor,nextCursor:data.nextCursor,extra:[],stale:false,busy:false,error:''};
    else if(history.baseCursor!==data.nextCursor){
      if(history.extra.length){const seen=new Set(history.extra.map(row=>row.sha));for(const row of owner.data.commits)if(!seen.has(row.sha)){history.extra.push(row);seen.add(row.sha);}history.stale=true;history.error='Commit history changed. Reload commits to continue paging.';}
      else{history.baseCursor=data.nextCursor;history.nextCursor=data.nextCursor;}
    }
  }
  function moreCommits(){
    const history=current?.commitHistory;
    if(!history||history.busy||history.stale||history.nextCursor==null)return;
    current.moreCursor=history.nextCursor;history.busy=true;history.error='';current.reader.intent({read:true});render();
  }
  async function openDecision(){
    const owner=current,id=currentId,since=focus.capture();if(!owner)return;
    try{const context=await cs.context();if(!context?.org)throw new Error('The story’s context org is unavailable.');
      const result=await cs.queryContext({match:{id},text:'Decisions',where:{orgs:[context.org]},limit:2});
      if(result.rows?.length!==1||result.total>1)throw new Error('The story’s Decisions section could not be located uniquely.');
      const row=result.rows[0],anchor=row.textMatches?.find(match=>/^Decisions(?:\s|$)/i.test(match.title))?.anchor;
      await cs.openFile({path:row.path+(anchor?'#'+anchor:''),newTab:true});
    }catch(error){owner.changesError=String(error.message||error);if(!stopped&&current===owner)render(focus.capture(),false,since);}
  }
  function close(){
    if(current?.lines){const path=current.lines.path;current.reader.intent();current.linesReader.intent();current.lines=null;current.diffResult=null;linesRoute();render({rowId:'file:'+path,controlId:`[data-lines-path="${CSS.escape(path)}"]`,sectionId:'files-title'},true);return;}
    current?.reader.intent();const intent={rowId:`session:${opener}`,controlId:`[data-open="${CSS.escape(opener??'')}"]`,sectionId:"sessions-title"};if(current)current.selected=null;render(intent,true);}
  function route(requestedHash=null,explicit=true){
    const hash=typeof requestedHash==='string'?requestedHash:getHash();
    hostRoute=null;
    win.history.replaceState({storyTrail:true},'');
    win.parent.postMessage({type:'codesense:route-changed',hash,token:win.__codesenseProxy?.token},'*');
    if(current){current.scroll=win.scrollY;current.focusIntent=focus.capture();current.ui={sort,direction,flat,byTask,activeRate,lastRate,rateOpener,opener,collapsed:[...collapsed]};}
    panel.hidden=true;activeRate=null;lastRate=null;
    const [routeHash,query='']=hash.split('?'),path=routeHash.split('/'),kind=path[0];
    if(kind==='#session' && path[1]){
      currentId='';current=null;root.classList.add('conversation-view');
      const id=decodeURIComponent(path[1]),params=new URLSearchParams(query),options={turn:path[2]==='turn'&&path[3]!=null?Number(path[3]):params.has('turn')?Number(params.get('turn')):null,source:params.get('source'),from:params.has('from')?Number(params.get('from')):null,cursor:Number(params.get('cursor')??0),explicit};
      let entry=conversations.get(id);
      if(conversationView&&conversationView!==entry?.controller)conversationView.setVisible(false);
      if(!entry){const node=cs.kit.h('div',{class:'conversation-page'});root.replaceChildren(node);entry={node,controller:mountConversation({root:node,cs,focusIntents:focus,patches:view.patches,id,...options,visible})};conversations.set(id,entry);}
      else{if(root.firstChild!==entry.node)root.replaceChildren(entry.node);entry.controller.navigate(options);entry.controller.setVisible(visible);}
      conversationView=entry.controller;
      return;
    }
    conversationView?.setVisible(false);conversationView=null;root.classList.remove('conversation-view');
    if(kind!=='#story'||!path[1]){currentId='';current=null;view.message('Story trail','Open a story’s Trail to see its sessions and tasks.');return;}
    currentId=decodeURIComponent(path.slice(1).join('/'));
    const leaving=current;current=trailOwner(currentId);
    if(leaving&&leaving!==current){leaving.diffResult=null;leaving.diffToken=(leaving.diffToken??0)+1;}current.reader.intent({read:true});focusedTask=current.focusedTask??null;
    const ui=current.ui??{};sort=ui.sort??'when';direction=ui.direction??1;flat=ui.flat??false;byTask=ui.byTask??false;activeRate=ui.activeRate??null;lastRate=ui.lastRate??null;rateOpener=ui.rateOpener??null;opener=ui.opener??null;collapsed.clear();for(const id of ui.collapsed??[])collapsed.add(id);
    const params=new URLSearchParams(query),linesPath=params.get('lines'),start=Number(params.get('start')),end=Number(params.get('end'));
    if(linesPath)openLines(linesPath,lineRefusal(start,end)?null:{start,end},{route:false});
    else if(current.lines){current.linesReader.intent({read:true});linesRoute();}
    if(current.selected)panelOwner(current.selected).reader.intent({read:true});render(current.focusIntent??focus.capture());win.scrollTo?.(0,current.scroll);
  }
  async function openChangeDiff(target){
    const owner=current,id=currentId;if(!owner?.data?.sourceRepo)return;
    // Only the latest click owns the result; an earlier reply never replaces it.
    const token=owner.diffToken=(owner.diffToken??0)+1;owner.diffResult=null;
    owner.reader.intent();render();
    const range=target.dataset.diffStart?{range:{start:Number(target.dataset.diffStart),end:Number(target.dataset.diffEnd)}}:{};
    let result;
    try{if(typeof cs.openDiff!=='function')throw new Error(NEEDS_NEWER);result=await cs.openDiff({repo:owner.data.sourceRepo,rev:target.dataset.diffRev,path:target.dataset.diffPath,storyId:id,...range});}
    catch(error){result={status:'unavailable',reason:String(error.message||error)};}
    if(token!==owner.diffToken)return;
    owner.diffResult=result;
    if(!stopped&&current===owner)render();
  }
  async function openStoryWorkspace(){
    const owner=current,id=currentId;if(!owner)return;
    try{if(typeof cs.openWorkspace!=='function')throw new Error(NEEDS_NEWER);await cs.openWorkspace({storyId:id});}
    catch(error){if(owner.diffResult)owner.diffResult={...owner.diffResult,reason:`${owner.diffResult.reason} · ${String(error.message||error)}`};if(!stopped&&current===owner)render();}
  }
  function click(event){
    // A user gesture starts the next request; an earlier host pairing never consumes it.
    hostRoute=null;
    const target=event.target.closest('button');
    const action=event.target.closest('button,a[href],[role=treeitem],[data-row-focus]');
    if(action&&doc.activeElement===doc.body)focus.restore(focus.capture(action));
    if(event.target.closest('[aria-disabled="true"]')){event.preventDefault();return;}
    if(target?.hasAttribute('data-diff-rev')){void openChangeDiff(target);}
    else if(target?.hasAttribute('data-lines-path')){if(current)openLines(target.dataset.linesPath);}
    else if(target?.hasAttribute('data-lines-retry')){if(current?.lines){current.lines.error='';current.linesReader.intent({read:true});renderPanel(undefined,false);}}
    else if(target?.hasAttribute('data-open-workspace')){void openStoryWorkspace();}
    else if(target?.hasAttribute('data-more-commits')){moreCommits();}
    else if(target?.hasAttribute('data-reload-commits')){if(current){current.moreCursor=null;current.actionControl=target;current.reader.intent({read:true});render();}}
    else if(target?.hasAttribute('data-decision')){current?.reader.intent();void openDecision();}
    else if(target?.hasAttribute('data-conversation')){void openConversation(target.dataset.conversation,target.hasAttribute('data-turn')?Number(target.dataset.turn):null);}
    else if(target?.hasAttribute('data-panel-retry')){const state=panels.get(current?.selected);if(state){state.actionControl=target;state.reader.intent({read:true});renderPanel(undefined,false);}}
    else if(target?.hasAttribute('data-panel-rate')){const model=target.dataset.panelRate,id=current?.selected;close();beginRate(model,`session:${id}`,`session:${id}:rate:${model}`);}
    else if(target?.hasAttribute('data-sort')){current.reader.intent();const key=target.dataset.sort;direction=sort===key?-direction:1;sort=key;render();}
    else if(target?.hasAttribute('data-group')){current.reader.intent();byTask=target.dataset.group==='task';render();}
    else if(target?.hasAttribute('data-flat')){current.reader.intent();flat=!flat;render();}
    else if(target?.hasAttribute('data-retry')){if(!current)return;current.reader.intent();current.actionControl=target;if(lastRate){lastRate=transitionRate(lastRate,{type:'retry'});}render();nudge();}
    else if(target?.hasAttribute('data-close'))close();
    else if(target?.hasAttribute('data-rate')){beginRate(target.dataset.rate,target.dataset.focusKey.split(':rate:')[0],target.dataset.focusKey);}
    else if(target?.hasAttribute('data-cancel-rate')){current.reader.intent();const intent={rowId:activeRate.rowId,controlId:`[data-focus-key="${CSS.escape(rateOpener??'')}"]`};activeRate=null;render(intent,true);}
    else if(!target && event.target.closest('[data-task]')){const item=event.target.closest('[data-task]');focusedTask=item.dataset.task;current.focusedTask=focusedTask;if(item.hasAttribute('aria-expanded')){collapsed.has(focusedTask)?collapsed.delete(focusedTask):collapsed.add(focusedTask);render();}focusTreeTask(focusedTask);}
    else {const row=event.target.closest('[data-session]');if(row && !event.target.closest('a,input')){current.reader.intent();if(current.lines){current.linesReader.intent();current.lines=null;linesRoute();}opener=row.dataset.session;current.selected=opener;panelOwner(opener).reader.intent({read:true});render({rowId:`session:${opener}`,controlId:"#drill-panel [data-close]",sectionId:"sessions-title"},true);}}
  }
  async function submit(event){
    if(event.target.id==='lines-form'){event.preventDefault();pickLines(event.target);return;}
    if(event.target.id!=='rate-form')return;
    event.preventDefault();if(!activeRate||activeRate.phase==='saving')return;
    current.reader.intent();
    const values=new win.FormData(event.target),owner=current,request=activeRate;
    const since=focus.capture()??focus.capture(event.target.querySelector('[type=submit]'));
    activeRate=transitionRate(activeRate,{type:'submit'});render();
    const active=()=>owner===current?activeRate:owner.ui?.activeRate;
    const relevant=()=>active()?.requestToken===request.requestToken;
    try{
      if(typeof cs.setModelRate!=='function')throw new Error(NEEDS_NEWER);
      const changed=await cs.setModelRate({modelPrefix:values.get('modelPrefix'),effectiveFrom:values.get('effectiveFrom'),inputPerMtok:Number(values.get('inputPerMtok')),outputPerMtok:Number(values.get('outputPerMtok'))});
      if(relevant()){
        const result=transitionRate(active(),{type:'resolved',changed,requestToken:request.requestToken});
        owner.ui={...owner.ui,activeRate:null,lastRate:result};
        if(owner===current){lastRate=result;activeRate=null;render({rowId:request.rowId,controlId:'[data-focus-key="'+CSS.escape(request.rowId+':rate:'+request.model)+'"]'},true,since);}
      }
      nudge();
    }catch(error){
      if(relevant()){
        const result=transitionRate(active(),{type:'failed',error:String(error.message||error),requestToken:request.requestToken});owner.ui={...owner.ui,activeRate:result};
        if(owner===current){activeRate=result;render(focus.capture(),false,since);}
      }
    }
  }
  function focusTreeTask(id){
    current.reader.intent();
    focusedTask=id;current.focusedTask=id;
    const next=root.querySelector(`[data-task="${CSS.escape(id)}"]`);
    root.querySelectorAll('[role=treeitem]').forEach(el=>el.tabIndex=el===next?0:-1);focus.restore({rowId:`task:${id}`,controlId:`[data-task="${CSS.escape(id)}"]`,sectionId:"tasks-title"},{reveal:true,revealEl:':scope > [data-row-label]'});
  }
  function treeKey(event){
    const item=event.target.closest?.('[role=treeitem]');
    if(!item || event.target.closest('button'))return;
    if(item.getAttribute('aria-disabled')==='true'){event.preventDefault();return;}
    const items=[...root.querySelectorAll('[role=treeitem]')], index=items.indexOf(item);
    let next=item;
    switch(event.key){
      case 'ArrowDown':next=items[Math.min(index+1,items.length-1)];break;
      case 'ArrowUp':next=items[Math.max(index-1,0)];break;
      case 'Home':next=items[0];break;
      case 'End':next=items.at(-1);break;
      case 'Enter':case ' ':if(item.hasAttribute('aria-expanded')){collapsed.has(item.dataset.task)?collapsed.delete(item.dataset.task):collapsed.add(item.dataset.task);render();next=root.querySelector(`[data-task="${CSS.escape(item.dataset.task)}"]`);}else return;break;
      case 'ArrowRight':if(item.getAttribute('aria-expanded')==='false'){collapsed.delete(item.dataset.task);render();next=root.querySelector(`[data-task="${CSS.escape(item.dataset.task)}"]`);}else if(item.getAttribute('aria-expanded')==='true')next=items[index+1];break;
      case 'ArrowLeft':if(item.getAttribute('aria-expanded')==='true'){collapsed.add(item.dataset.task);render();next=root.querySelector(`[data-task="${CSS.escape(item.dataset.task)}"]`);}else next=item.parentElement.closest('[role=treeitem]')??item;break;
      default:return;
    }
    event.preventDefault();event.stopPropagation();focusTreeTask(next.dataset.task);
  }
  function key(event){if(event.key==='Escape'&&!panel.hidden){event.preventDefault();event.stopPropagation();close();}}
  function on(target,event,fn){target.addEventListener(event,fn);offs.push(()=>target.removeEventListener(event,fn));}
  on(doc,'click',click);on(doc,'submit',submit);on(doc,'keydown',key);on(doc,'keydown',treeKey);
  on(doc,'input',event=>{if(event.target.closest?.('#rate-form'))current?.reader.intent();});
  // Apply traversal intent before deferred replies resume; history restores saved reading.
  // The paired hashchange is consumed, while standalone hashchange remains a fallback.
  // One host open request arrives twice: a Location.href fragment navigation and a codesense:navigate
  // message, in either order. The page side pairs them because the host keeps Location.href for documents
  // that ignore the message. The first channel routes; the other delivery of that hash is consumed.
  function fragment(){const hash=getHash();if(hostRoute?.hash===hash)return;route();hostRoute={hash,message:false};}
  function hostNavigate(hash){
    if(hostRoute?.hash===hash&&!hostRoute.message){hostRoute.message=true;return;}
    // A message ahead of its fragment navigation owns the new history entry, so the previous route survives.
    if(getHash()!==hash)win.history.pushState(null,'',hash);
    route(hash);hostRoute={hash,message:true};
  }
  on(win,'popstate',event=>{routedPopstate=true;if(event.state?.storyTrail===true)route(null,false);else fragment();});
  on(win,'hashchange',()=>{if(routedPopstate){routedPopstate=false;return;}fragment();});
  on(win,'message',event=>{if(event.source===win.parent&&event.data?.type==='codesense:navigate'&&typeof event.data.token==='string'&&event.data.token===win.__codesenseProxy?.token){hostNavigate(event.data.hash);return;}if(event.source===win.parent && ['codesense:agent-activity-changed','codesense:worktrees-changed'].includes(event.data?.type))nudge();});
  const onSessions=()=>nudge();
  cs?.subscribe?.('sessions',{onChange:onSessions}).catch(()=>{});
  offs.push(()=>cs?.unsubscribe?.('sessions',onSessions));
  route();
  if(cs?.onVisibility)offs.push(cs.onVisibility(shown=>{visible=shown;conversationView?.setVisible(shown);if(shown)nudge();}));
  else {visible=doc.visibilityState!=='hidden';on(doc,'visibilitychange',()=>{visible=doc.visibilityState!=='hidden';if(visible)nudge();});current?.reader.resume();}
  function stop(){stopped=true;for(const state of cache.values()){state.reader.stop();state.linesReader?.stop();}for(const state of panels.values())state.reader.stop();for(const entry of conversations.values())entry.controller.stop();view.stop();focus.stop();offs.splice(0).forEach(off=>off?.());}
  on(win,'pagehide',stop);
  return {stop,route,navigate};
}
