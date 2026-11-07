// SPDX-License-Identifier: MIT
export const newRateState = (rowId,model,requestToken) => ({rowId,model,requestToken,phase:'idle',notice:''});
export function transitionRate(state,event) {
  if(event.requestToken!=null&&event.requestToken!==state.requestToken)return state;
  if(event.type==='submit')return {...state,phase:'saving',notice:''};
  if(event.type==='failed')return {...state,phase:'failed',notice:String(event.error)};
  if(event.type==='retry')return state.phase==='pending-acceptance'?state:{...state,phase:'idle',notice:''};
  if(event.type==='priced')return {...state,phase:'accepted',notice:''};
  if(event.type==='resolved'){
    // set_model_rate(params: SetModelRateParams) -> Result<usize, String>.
    // The number counts changed usage costs. request_rate returns 0 both
    // for queued approval and effective rates with no changed usage costs.
    // This UI phase means confirmation is unobserved, not that native approval
    // is known to be pending; only a priced read establishes the effective rate.
    return event.changed>0 ? {...state,phase:'accepted',notice:'Rate saved.'}
      : {...state,phase:'pending-acceptance',notice:'Rate requested. If acceptance is required, accept it in the affected agent’s or routine’s native Spend cap block, then Retry.'};
  }
  return state;
}
