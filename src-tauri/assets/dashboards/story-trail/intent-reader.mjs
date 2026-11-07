// SPDX-License-Identifier: MIT
// Every view uses the same read ownership and coalescing rule.
export function createIntentReader({enabled=()=>true,capture=()=>undefined,read,commit,fail,settled=()=>{}}){
  let intent=0,pending=false,queued=false,stopped=false;
  const inFlight=new Set();
  function schedule(){
    if(stopped||queued||!pending||!enabled()||inFlight.has(intent))return;
    queued=true;
    Promise.resolve().then(()=>{
      queued=false;
      if(!stopped&&pending&&enabled()&&!inFlight.has(intent))run(intent);
    });
  }
  function request(){if(stopped)return;pending=true;schedule();}
  function ensureCurrent(){if(!stopped&&!inFlight.has(intent))run(intent);}
  async function run(owner){
    pending=false;inFlight.add(owner);
    const context=capture();
    let outcome;
    try{outcome={data:await read(context)};}
    catch(error){outcome={error};}
    try{
      if(owner===intent){
        if('error' in outcome)fail(outcome.error,context);else commit(outcome.data,context);
        settled(context);
      }else ensureCurrent();
    }finally{inFlight.delete(owner);schedule();}
  }
  return {
    intent({read=false}={}){if(stopped)return;intent++;if(read)pending=true;schedule();},
    request,resume:schedule,
    stop(){stopped=true;intent++;pending=false;},
  };
}
