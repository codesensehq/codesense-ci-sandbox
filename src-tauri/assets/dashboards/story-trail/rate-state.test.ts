// SPDX-License-Identifier: MIT
import {expect,it} from 'vitest';
import {newRateState,transitionRate} from './rate-state.mjs';
it.each([
  ['idle',{type:'submit'},'saving'],['saving',{type:'resolved',changed:0},'pending-acceptance'],
  ['saving',{type:'resolved',changed:2},'accepted'],['saving',{type:'failed',error:'no connection'},'failed'],
  ['failed',{type:'submit'},'saving'],['pending-acceptance',{type:'priced'},'accepted'],
  ['pending-acceptance',{type:'retry'},'pending-acceptance'],['accepted',{type:'retry'},'idle'],
])('rate transition %s + %j => %s',(phase,event,expected)=>{
  const state={...newRateState('session:a','model',1),phase};expect(transitionRate(state,event).phase).toBe(expected);
});
it('a zero count is conditional request wording, never a native acceptance claim',()=>{
  const state=transitionRate({...newRateState('session:a','model',1),phase:'saving'},{type:'resolved',changed:0});
  expect(state.notice).toContain('If acceptance is required');expect(state.notice).not.toContain('waiting for the app');
  expect(transitionRate(state,{type:'priced'}).notice).toBe('');expect(transitionRate(state,{type:'retry'}).notice).toBe(state.notice);
});
it('ignores stale request tokens and creates a fresh same-model draft',()=>{
  const older={...newRateState('session:a','model',1),phase:'saving'},fresh=newRateState('session:a','model',2);
  expect(fresh.phase).toBe('idle');expect(transitionRate(fresh,{type:'resolved',changed:1,requestToken:older.requestToken})).toEqual(fresh);
});
it('Retry preserves unconfirmed guidance until a priced read',()=>{
  const state=transitionRate({...newRateState('session:a','model',1),phase:'saving'},{type:'resolved',changed:0});
  expect(transitionRate(state,{type:'retry'}).notice).toBe(state.notice);expect(transitionRate(state,{type:'priced'}).notice).toBe('');
});
