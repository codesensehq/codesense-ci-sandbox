// SPDX-License-Identifier: MIT
import {lineOwnership,lineRevision,lineSpan} from './model.mjs';
import {createSessionDom} from './session-dom.mjs';

// "Who changed these lines" in the shared side panel: the same leaf patches, gesture gate and focus rule.
export function createLinesView({kit,patch,doc}) {
  const {h,text,set,template,diffStatus,updateDiffStatus}=createSessionDom({kit,patch,doc});
  const node=h('div');
  node.append(template('<header class="panel-head"><h2 id="panel-title">Who changed these lines</h2><p class="mono sub" data-lines-file></p></header><form id="lines-form" class="lines-form"><label>From line <input class="cs-input" name="linesStart" type="number" min="1" step="1" required></label><label>To line <input class="cs-input" name="linesEnd" type="number" min="1" step="1" required></label><button class="cs-btn" type="submit" data-panel-key="lines:show">Show</button></form><p class="fail" role="status" data-lines-refused hidden></p><p class="muted" data-lines-hint hidden>Pick lines to see who changed them.</p><div class="lines-diff"><button class="cs-btn" data-panel-key="lines:diff" data-lines-diff>Open diff</button> <span class="sub" data-lines-rev></span></div>'));
  node.append(diffStatus());
  node.append(template('<div data-lines-loading aria-label="Loading line attribution" aria-busy="true"></div><p class="fail" role="status" data-lines-error hidden><span></span> <button class="cs-btn" data-panel-key="lines:retry" data-lines-retry>Retry</button></p><p class="note" data-lines-unavailable hidden></p><p class="subh" data-lines-summary hidden></p><div data-lines-groups></div><div data-lines-associated hidden><p class="note"><b>Who changed each line isn’t recorded yet</b><br><span></span></p><div class="own"></div></div><div class="own" data-lines-other></div>'));
  node.querySelector('[data-lines-loading]').append(kit.skeleton({rows:3}));
  const q=selector=>node.querySelector(selector);
  // A passive render keeps what the person is typing; only a new committed value replaces it.
  const value=(input,next)=>{next=next==null?'':String(next);if(input.dataset.applied!==next){input.value=next;input.dataset.applied=next;}};
  const conversation=(container,id,turn,passive)=>patch.keyed(container,id?[{id,turn}]:[],{key:item=>`${item.id}:${item.turn??''}`,render:()=>h('button',{class:'disc'}),update:(button,item)=>{set(button,'data-conversation',item.id);set(button,'data-turn',item.turn);set(button,'data-panel-key',`lines:conversation:${item.id}:${item.turn??''}`);text(button,item.turn==null?'Open conversation':`Open conversation at turn ${item.turn}`);}},passive);
  function render(state,passive){
    const {path,start,end,result,error,refused}=state,picked=start!=null,revision=lineRevision(state.data,path);
    text(q('h2'),picked?`Who changed ${lineSpan({start,end}).toLowerCase()}`:'Who changed these lines');text(q('[data-lines-file]'),path);
    value(q('[name=linesStart]'),start);value(q('[name=linesEnd]'),end);
    text(q('[data-lines-refused]'),refused);q('[data-lines-refused]').hidden=!refused;
    q('[data-lines-hint]').hidden=picked||!result;text(q('[data-lines-hint]'),result?.capped?'Pick lines to see who changed them — ranges not all loaded.':'Pick lines to see who changed them.');
    const diffRev=revision?.diffRev,diff=q('[data-lines-diff]');diff.hidden=!picked||!diffRev||!state.data?.sourceRepo;set(diff,'data-diff-rev',diffRev);set(diff,'data-diff-path',path);set(diff,'data-diff-start',start);set(diff,'data-diff-end',end);text(q('[data-lines-rev]'),revision?.label??'');
    updateDiffStatus(q('[data-diff-result]'),state.diffResult);
    const unavailable=result?.status==='unavailable'?result.reason:'',answered=!!result&&!unavailable&&!!result.ranges;
    q('[data-lines-loading]').hidden=!!result||!!error||!picked;
    const fail=q('[data-lines-error]');fail.hidden=!error;text(fail.firstChild,error);
    text(q('[data-lines-unavailable]'),unavailable);q('[data-lines-unavailable]').hidden=!unavailable;
    const owners=answered?lineOwnership(result,state.storyId,state.data?.sessions):{changed:0,groups:[],associated:[],candidates:[],other:[]};
    const summary=q('[data-lines-summary]');summary.hidden=!owners.changed;text(summary,owners.changed?`This story changed ${owners.changed} of these ${end-start+1} lines.`:'');
    patch.keyed(q('[data-lines-groups]'),owners.groups,{key:group=>group.key,render:()=>h('div',{class:'own',role:'group'},h('div',{class:'oh'},h('span',null,h('b'),h('small'))),h('div')),update:(el,group)=>{set(el,'data-lines-group',group.key);set(el,'aria-label',`${group.agent} · ${group.detail}`);const [name,detail]=el.firstChild.firstChild.children;text(name,group.agent);text(detail,group.detail);
      patch.keyed(el.lastChild,group.ranges,{key:range=>range.key,render:()=>h('div',{class:'or','data-lines-range':''},h('div',{class:'ot'}),h('div',{class:'oc'})),update:(row,range)=>{text(row.firstChild,range.text);conversation(row.lastChild,group.sessionId,range.turn,passive);}},passive);}},passive);
    const shared=q('[data-lines-associated]');shared.hidden=!owners.associated.length;text(shared.querySelector('span'),owners.associated.length?`These sessions edited this file. One of them may have changed ${owners.associated.map(range=>lineSpan(range).toLowerCase()).join(', ')}.`:'');
    shared.lastChild.hidden=!owners.candidates.length;
    patch.keyed(shared.lastChild,owners.candidates,{key:item=>item.id,render:()=>h('div',{class:'or','data-lines-candidate':''},h('div',{class:'ot'},h('b'),h('span')),h('div',{class:'oc'})),update:(row,item)=>{text(row.firstChild.firstChild,item.agent);text(row.firstChild.lastChild,' · associated');conversation(row.lastChild,item.id,null,passive);}},passive);
    const other=q('[data-lines-other]');other.hidden=!owners.other.length;
    patch.keyed(other,owners.other,{key:item=>`${item.range.start}:${item.range.end}:${item.range.commit}`,render:()=>h('div',{class:'or','data-lines-range':''},h('div',{class:'ot'})),update:(row,item)=>text(row.firstChild,item.text)},passive);
  }
  return {node,render};
}
