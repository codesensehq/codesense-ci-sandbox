// SPDX-License-Identifier: MIT
import {conversationState,editChips,hasRedaction} from './conversation.mjs';

// Shared leaf and turn patches for the panel excerpt and full conversation.
export function createSessionDom({kit,patch,doc=document}) {
  const h=kit.h,text=patch.text;
  const set=(node,name,value)=>{if(value==null)node.removeAttribute(name);else if(node.getAttribute(name)!==String(value))node.setAttribute(name,String(value));};
  const template=html=>{const t=doc.createElement('template');t.innerHTML=html;return t.content;};
  function recorded(node,value,passive){
    const parts=String(value??'').split(/(\[redacted\])/).filter(Boolean).map((value,index)=>({value,index,redacted:value==='[redacted]'}));
    patch.keyed(node,parts,{key:p=>p.index,version:p=>p.redacted?'redacted':'text',render:p=>h('span',{class:p.redacted?'rd':''}),update:(el,p)=>text(el,p.value)},passive);
  }
  const note=page=>page?.truncated?`Conversation truncated${page.truncated.droppedTurnCount?` · ${page.truncated.droppedTurnCount} turns left out`:''}${page.truncated.trimmedFields?` · ${page.truncated.trimmedFields} fields shortened`:''}.`:'';
  function tool(){return h('details',{class:'tool'},h('summary',null,h('span',{class:'tool-name mono'}),h('span',{class:'tool-input'}),h('span',{class:'tool-status'})),h('pre',{class:'tool-output'}),h('pre',{class:'tool-output'}));}
  function updateTool(node,item,passive){const call=item.call,summary=node.firstChild,status=call.is_error===true?'failed':call.is_error===false?'ok':'';set(node,'data-tool-key',item.key);set(node.children[1],'data-scroll-key',item.key+':input');set(node.children[2],'data-scroll-key',item.key+':result');text(summary.children[0],call.name);recorded(summary.children[1],call.input?.slice(0,160),passive);summary.children[1].hidden=!call.input;text(summary.children[2],status);summary.children[2].className='tool-status '+status;summary.children[2].hidden=!status;recorded(node.children[1],call.input,passive);recorded(node.children[2],call.result,passive);node.children[1].hidden=!call.input;node.children[2].hidden=!call.result;}
  function diffStatus(action=null){return h('div',{'data-diff-result':'',role:'status'},h('p'),h('input',{readonly:'','aria-label':'Fetch command'}),h('button',{class:'cs-btn is-sm',...(action?{'data-conv-action':action}:{'data-open-workspace':''})},'Open in workspace'));}
  function updateDiffStatus(node,result){node.hidden=!result||result.status==='opened';text(node.firstChild,result?.reason??'');node.children[1].hidden=result?.status!=='commitNotLocal';set(node.children[1],'value',result?.fetchCommand??'');node.children[2].hidden=result?.status!=='commitNotLocal';}
  function turn(){const node=h('article',{'data-row-focus':'',tabindex:'-1',class:'turn'},h('header',{class:'turn-head','data-row-label':''},h('b'),h('span'),h('span',{class:'muted'})),h('div',{class:'turn-text'}),h('div',{'data-tools':''}),h('div',{'data-chips':''}));return node;}
  function updateTurn(node,item,passive){
    const t=item.turn,index=t.index,meta=[t.model,t.ts_ms==null?'':new Date(t.ts_ms).toLocaleString([],{dateStyle:'short',timeStyle:'short'})].filter(Boolean);
    if(t.usage?.input_tokens!=null)meta.push(`${t.usage.input_tokens.toLocaleString()} input tokens`);if(t.usage?.output_tokens!=null)meta.push(`${t.usage.output_tokens.toLocaleString()} output tokens`);
    set(node,'data-row-focus',`turn:${index??item.key}`);set(node,'id',index==null?null:`turn-${index}`);set(node,'aria-label',hasRedaction(t)?`Turn ${index??''}, contains redacted content`:null);node.className=`turn ${t.role==='user'?'request':''} ${index===item.highlight?'highlight':''}`;
    text(node.firstChild.children[0],t.role==='user'?'Request':item.agent??'Agent');text(node.firstChild.children[1],index==null?'':`Turn ${index}`);text(node.firstChild.children[2],meta.join(' · '));recorded(node.children[1],t.text,passive);node.children[1].hidden=!t.text;
    patch.keyed(node.children[2],(t.tool_calls??[]).map((call,i)=>({call,key:`${index??item.key}:${call.native_id??i}`})),{key:x=>x.key,render:tool,update:(el,x)=>updateTool(el,x,passive)},passive);
    patch.keyed(node.children[3],editChips(t).map((chip,i)=>({...chip,key:JSON.stringify([chip.path,chip.start,chip.end,i])})),{key:x=>x.key,render:()=>h('button',{type:'button',class:'edit-chip mono'}),update:(el,c)=>{set(el,'data-edit-path',c.path);set(el,'data-edit-start',c.start);set(el,'data-edit-end',c.end);text(el,c.path+(c.start==null?'':` · lines ${c.start}–${c.end}`));}},passive);
  }
  return {h,text,set,template,recorded,note,turn,updateTurn,conversationState,diffStatus,updateDiffStatus};
}
