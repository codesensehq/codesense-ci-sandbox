// SPDX-License-Identifier: MIT
import {createFocusIntents} from './focus.mjs';
import {sessionFacts} from './model.mjs';
import {conversationState,runningSession,conversationPageRequest,editDiffRequest,NEEDS_NEWER} from './conversation.mjs';
import {createPatches} from './patches.mjs';
import {createSessionDom} from './session-dom.mjs';
import {createIntentReader} from './intent-reader.mjs';

export function mountConversation({root,cs,id,turn=null,source=null,from=null,cursor=0,visible=true,focusIntents=null,patches=null}) {
  let data=null,pageFrom=null,rows=[],error='',stopped=false,pendingDiff=null,diffOutcome=null;
  let request=turn==null?{cursor:from??cursor,limit:20}:{turn},highlight=turn,origin=source,scrollTarget=turn??'page',readingPending=true,actionControl=null;
  const readingPositions=new Map();
  const openedTools=new Set();
  const focus=focusIntents??createFocusIntents({root});
  const patch=patches??createPatches({root:root.ownerDocument.body,kit:cs.kit});
  const {h,text,set,template,note,turn:turnNode,updateTurn,diffStatus,updateDiffStatus}=createSessionDom({kit:cs.kit,patch,doc:root.ownerDocument});
  root.append(template('<header class="conversation-head"><div class="conversation-title"><div class="grow"><h1>Conversation</h1><p data-facts></p><p class="muted" data-context></p><p class="muted mono" data-id></p></div><span class="live" data-live-label hidden>Live</span><div data-live></div></div><div data-back></div></header><div data-error></div><section data-loading aria-label="Loading conversation" aria-busy="true"></section><p class="note" data-truncation hidden></p><section class="note" data-state hidden><h2></h2><p></p></section><div class="conversation-body" hidden><nav class="turn-rail" data-scroll-key="conversation-rail" aria-label="Conversation turns"><ol></ol></nav><div class="thread" data-scroll-key="conversation-thread"><div class="turns-inner"><div data-origin></div><div data-earlier></div><div data-turns></div><div data-later></div></div></div></div>'));
  const q=selector=>root.querySelector(selector);
  q('[data-error]').after(diffStatus('workspace'));
  q('[data-loading]').append(cs.kit.skeleton({rows:4}));
  text(q('[data-id]'),id);
  function actions(container,items,passive){patch.keyed(container,items,{key:x=>x.action,render:()=>h('button',{class:'cs-btn is-sm'}),update:(node,item)=>{set(node,'data-conv-action',item.action);set(node,'data-from',item.from);text(node,item.label);}},passive);}
  function render(since=undefined){
    if(stopped)return;
    if(since!==undefined&&patch.defer(render,()=>render(since)))return;
    let intent=focus.capture();if(since!==undefined)({intent}=focus.completion(intent,{since}));
    const passive=since!==undefined,facts=data?sessionFacts(data):null,page=data?.conversation,state=conversationState(page),live=runningSession(rows,data,id),present=!!data&&['present','truncated'].includes(state);
    text(q('h1'),facts?.agent??'Conversation');text(q('[data-facts]'),facts?[facts.model,facts.time,page?`${page.turnCount} turns`:null,facts.cost].filter(Boolean).join(' · '):'');text(q('[data-context]'),data?[data.story?[data.story.id,data.story.title].filter(Boolean).join(' · '):null,data.basis,facts.account].filter(Boolean).join(' · '):'');
    actions(q('[data-live]'),live?[{action:'live',label:'Jump to live terminal'}]:[],passive);q('[data-live-label]').hidden=!live;
    actions(q('[data-back]'),data?.story?.id?[{action:'back',label:'Back to story trail'}]:[],passive);
    patch.keyed(q('[data-error]'),error?[error]:[],{key:()=> 'read-error',render:()=>h('p',{class:'fail',role:'status'},h('span'),h('button',{class:'cs-btn','data-conv-action':'retry'},'Retry')),update:(node,value)=>text(node.firstChild,value)},passive);
    q('[data-loading]').hidden=!!data||!!error;set(q('[data-loading]'),'aria-busy',!data&&!error?'true':null);text(q('[data-truncation]'),note(page));q('[data-truncation]').hidden=!page?.truncated;q('[data-state]').hidden=!data||present;text(q('[data-state] h2'),state);text(q('[data-state] p'),page?.reason&&page.reason!==state?page.reason:'');q('.conversation-body').hidden=!present;
    const turns=present?[...new Map(page.turns.map((turn,i)=>[turn.index??`unindexed:${i}`,turn])).values()]:[];
    patch.keyed(q('.turn-rail ol'),turns.filter(t=>t.index!=null),{key:t=>t.index,render:()=>h('li',null,h('a',null,h('span'),h('span'))),update:(node,t)=>{const link=node.firstChild;set(link,'href',`#session/${encodeURIComponent(id)}?from=${pageFrom??0}&turn=${t.index}`);set(link,'data-jump',t.index);set(link,'aria-current',t.index===highlight?'true':null);text(link.firstChild,t.index);text(link.lastChild,t.tool_calls?.[0]?.name??(t.role==='user'?'Request':'Reply'));}},passive);
    patch.keyed(q('[data-origin]'),present&&highlight!=null?[highlight]:[],{key:()=> 'origin',render:()=>h('p',{class:'origin-note'},h('span'),h('button',{class:'cs-btn is-sm','data-conv-action':'unhighlight'},'Remove highlight')),update:node=>text(node.firstChild,origin?'You came here from '+origin:`Turn ${highlight}`)},passive);
    // Paging targets are stamped from the page this render shows; a gesture-held render keeps them with the page on screen.
    const gap=page?.truncated?.droppedTurns,first=turns[0]?.index;
    actions(q('[data-earlier]'),first>0?[{action:'earlier',label:'Show earlier turns',from:conversationPageRequest(gap&&first===gap[1]+1?gap[0]-1:first-1,gap).cursor}]:[],passive);
    // The reader alone supplies the forward cursor, including a cursor inside a dropped interval.
    actions(q('[data-later]'),present&&page.nextCursor!=null?[{action:'later',label:'Show later turns',from:page.nextCursor}]:[],passive);
    patch.keyed(q('[data-turns]'),turns.map((turn,i)=>({turn,key:turn.index??`unindexed:${i}`,highlight,agent:facts?.agent})),{key:x=>x.key,render:item=>{const node=turnNode();updateTurn(node,item,passive);for(const detail of node.querySelectorAll('details')){detail.open=openedTools.has(detail.dataset.toolKey);}return node;},update:(node,item)=>updateTurn(node,item,passive)},passive);
    updateDiffStatus(q('[data-diff-result]'),diffOutcome);
    markBusy();
    if(root.isConnected)focus.reconcile(intent);
    reveal();
  }
  function rememberToggle(event){const detail=event.target;if(detail.matches?.('details[data-tool-key]')){detail.open?openedTools.add(detail.dataset.toolKey):openedTools.delete(detail.dataset.toolKey);}}
  function rememberScroll(event){const pre=event.target,detail=pre.closest?.('details[data-tool-key]');if(detail&&pre.matches('pre'))rememberReading();}
  // A live last page grows; its aligned start alone identifies the reading position.
  const readingKey=()=>JSON.stringify({sessionId:id,from:pageFrom});
  function rememberReading(){if(data)readingPositions.set(readingKey(),focus.captureReading(root));}
  function reveal(){
    if(!visible||!root.isConnected||!readingPending||request)return;
    const target=scrollTarget==='page'?q('.thread'):scrollTarget!=null?q(`#turn-${scrollTarget}`)?.querySelector('[data-row-label]'):null;
    if(scrollTarget!=null&&!target)return;
    focus.restoreReading(root,{target,saved:readingPositions.get(readingKey())??[]});
    scrollTarget=null;readingPending=false;
  }
  // A passive read never disables controls; only the pending user action owns its busy control.
  function markBusy(){root.querySelectorAll('[data-jump],[data-conv-action],[data-edit-path]').forEach(el=>set(el,'aria-disabled',el===actionControl?'true':null));}
  const reader=createIntentReader({
    enabled:()=>visible&&root.isConnected,
    capture:()=>({requested:request??(pageFrom==null&&highlight!=null?{turn:highlight}:conversationPageRequest(pageFrom??0,data?.conversation?.truncated?.droppedTurns)),since:focus.capture(),diff:pendingDiff}),
    async read({requested,diff}){
      if(typeof cs.sessionRead!=='function')throw new Error(NEEDS_NEWER);
      const [initial,currentRows]=await Promise.all([cs.sessionRead({sessionId:id,include:['conversation'],limit:20,...requested}),cs.sessions().catch(()=>[])]);
      let result=initial;const page=result.conversation,first=page?.turns?.find(turn=>turn.index!=null)?.index;
      if(first!=null&&page.lookup?.found!==false){
        const aligned=conversationPageRequest(requested.turn??first,page.truncated?.droppedTurns);
        if(first!==aligned.cursor||page.turns.some(turn=>turn.index>=aligned.cursor+aligned.limit))result=await cs.sessionRead({sessionId:id,include:['conversation'],...aligned});
      }
      let diffTarget;
      if(diff){
        try{if(typeof cs.storyChanges!=='function')throw new Error(NEEDS_NEWER);diffTarget=diff.storyId?editDiffRequest(await cs.storyChanges({storyId:diff.storyId}),diff,diff.storyId):{status:'unavailable',reason:'This session has no linked story'};}
        catch(failure){diffTarget={status:'unavailable',reason:String(failure.message||failure)};}
      }
      return {result,currentRows,diffTarget};
    },
    commit({result,currentRows,diffTarget},{requested}){data=result;pageFrom=result.conversation?.turns?.find(turn=>turn.index!=null)?.index??null;request=null;rows=currentRows;error='';saveRoute(true,pageFrom??conversationPageRequest(requested.turn??requested.cursor??0,result.conversation?.truncated?.droppedTurns).cursor);
      if(diffTarget){pendingDiff=null;if(diffTarget.params){if(visible&&root.isConnected)void openDiff(diffTarget.params);}else diffOutcome=diffTarget;}
    },
    fail(failure){error=String(failure.message||failure);},
    settled({since}){actionControl=null;render(since);},
  });
  async function openDiff(params){
    try{if(typeof cs.openDiff!=='function')throw new Error(NEEDS_NEWER);diffOutcome=await cs.openDiff(params);}catch(failure){diffOutcome={status:'unavailable',reason:String(failure.message||failure)};}
    if(!stopped&&visible&&root.isConnected)render();
  }
  const nudge=()=>reader.request();
  function saveRoute(replace=true,from=pageFrom){
    if(!visible||!root.isConnected||from==null)return;
    const hash=`#session/${encodeURIComponent(id)}?from=${from}${highlight==null?'':`&turn=${highlight}`}`;
    // Re-selecting the loaded turn keeps one history entry.
    window.history[replace||window.location.hash===hash?'replaceState':'pushState']({storyTrail:true},'',hash);
    window.parent.postMessage({type:'codesense:route-changed',hash,token:window.__codesenseProxy?.token},'*');
  }
  function navigate({turn=null,from=null,cursor=0,source=null,explicit=true,replace=true,control=null}={}){
    reader.intent();pendingDiff=null;diffOutcome=null;
    if(visible&&root.isConnected)rememberReading();
    const gap=data?.conversation?.truncated?.droppedTurns,aligned=conversationPageRequest(turn??from??cursor,gap);
    const retained=pageFrom===aligned.cursor&&(turn==null||data.conversation.turns.some(row=>row.index===turn));
    const pendingTurn=readingPending&&turn!=null&&scrollTarget===turn;
    highlight=turn;origin=source;scrollTarget=explicit?(turn??'page'):pendingTurn?turn:null;readingPending=true;
    actionControl=retained?null:control;
    if(retained){request=null;saveRoute(replace);render();return;}
    request=turn==null?{cursor:from??cursor,limit:aligned.limit}:{turn};if(data)saveRoute(replace,aligned.cursor);markBusy();reader.request();
  }
  function jump(index,newSource=null,control=null){navigate({turn:index,source:newSource,replace:false,control});}
  async function click(event){
    const actionTarget=event.target.closest('button,a[href],summary');if(actionTarget&&root.ownerDocument.activeElement===root.ownerDocument.body)focus.restore(focus.capture(actionTarget));
    if(event.target.closest('[aria-disabled="true"]')){event.preventDefault();return;}
    const link=event.target.closest('[data-jump]');if(link){event.preventDefault();jump(Number(link.dataset.jump),null,link);return;}
    const chip=event.target.closest('[data-edit-path]');
    if(chip){pendingDiff={path:chip.dataset.editPath,start:chip.hasAttribute('data-edit-start')?Number(chip.dataset.editStart):null,end:chip.hasAttribute('data-edit-end')?Number(chip.dataset.editEnd):null,storyId:data?.story?.id};diffOutcome=null;actionControl=chip;reader.intent({read:true});render();return;}
    const action=event.target.closest('[data-conv-action]')?.dataset.convAction;
    if(!action){if(event.target.closest('summary'))reader.intent();return;}
    if(action==='retry'){actionControl=actionTarget;reader.intent({read:true});markBusy();return;}
    if(action==='unhighlight'){reader.intent();request=null;actionControl=null;highlight=null;origin=null;scrollTarget=null;readingPending=false;saveRoute();render();return;}
    if(action==='earlier'||action==='later'){navigate({from:Number(actionTarget.dataset.from),control:actionTarget});return;}
    const since=focus.capture();
    try{if(action==='back')await cs.openFile({path:`story-trail.html#story/${encodeURIComponent(data.story.id)}`,newTab:false});
      else if(action==='workspace'){if(typeof cs.openWorkspace!=='function')throw new Error(NEEDS_NEWER);await cs.openWorkspace({storyId:data.story.id});}
      else if(action==='live'){const live=runningSession(rows,data,id);if(typeof cs.gotoAgent!=='function')throw new Error(NEEDS_NEWER);if(live)await cs.gotoAgent({agentId:live.sessionId});}}
    catch(failure){error=String(failure.message||failure);render(since);}
  }
  function setVisible(shown){const changed=shown!==visible;if(!shown&&visible)rememberReading();visible=shown;if(shown&&changed){reveal();reader.request();}}
  root.addEventListener('click',click);root.addEventListener('toggle',rememberToggle,true);root.addEventListener('scroll',rememberScroll,true);render();reader.request();
  return {nudge,jump,navigate,setVisible,stop(){stopped=true;reader.stop();if(!focusIntents)focus.stop();if(!patches)patch.stop();root.removeEventListener('click',click);root.removeEventListener('toggle',rememberToggle,true);root.removeEventListener('scroll',rememberScroll,true);}};
}
