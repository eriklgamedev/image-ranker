import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';

// Observer only: never invokes, retries, or keeps a WebDriver body/result.
export function createDecisionTransport(browser,{maximumDecisions=10000,now=()=>performance.now()}={}){
  const decisions=[],issues=[];let active=null,pendingCommand=false,sequence=0,disposed=false,observationMs=0;
  const issue=message=>{if(issues.length<16)issues.push(message);};
  const errorText=error=>String(error?.message??error??'').slice(0,1024);
  const fingerprint=body=>{
    if(typeof body!=='string')throw new Error('Transport observation requires serialized request bytes');
    return {bodyBytes:Buffer.byteLength(body),bodySha256:createHash('sha256').update(body).digest('hex')};
  };
  const guarded=fn=>data=>{const started=now();try{fn(data);}catch(error){issue(errorText(error));}finally{observationMs+=Math.max(0,now()-started);}};
  const handlers={
    command:guarded(data=>{
      pendingCommand=Boolean(active&&data.command==='executeAsyncScript'&&data.method==='POST'&&/\/execute\/async$/.test(data.endpoint));
    }),
    'request.start':guarded(data=>{
      if(!active||!pendingCommand)return;pendingCommand=false;
      if(active.requests.length)issue('More than one execute/async request start for a logical decision');
      if(active.requests.length>=2)return;
      active.requests.push({sequence:++sequence,...fingerprint(data.body),startedAtMs:now(),durationMs:null,
        transportOutcome:'pending',retryCount:null,error:null});
    }),
    'request.retry':guarded(data=>{
      if(!active||!active.requests.length)return;
      active.retries.push({retryCount:data.retryCount,error:errorText(data.error)});
      issue('Automatic transport retry occurred during a measured decision');
    }),
    'request.performance':guarded(data=>{
      if(!active||!active.requests.length||data.request?.method!=='POST')return;
      const observed=fingerprint(data.request.body);
      const request=active.requests.find(item=>item.bodySha256===observed.bodySha256&&item.transportOutcome==='pending');
      if(!request){issue('Unmatched transport completion during a measured decision');return;}
      request.durationMs=Number.isFinite(data.durationMillisecond)?data.durationMillisecond:null;
      request.transportOutcome=data.success?'returned':'error';request.retryCount=data.retryCount;
      request.error=data.error?errorText(data.error):null;
      if(data.retryCount!==0)issue('A measured request reported a nonzero retry count');
    }),
  };
  for(const [event,handler] of Object.entries(handlers))browser.on(event,handler);
  return {
    begin({index,actionId,expectedRevision}){
      assert.ok(!disposed&&!active,'Decision observer already active or disposed');
      assert.ok(decisions.length<maximumDecisions,'Decision observation limit exceeded');
      assert.ok(Number.isSafeInteger(index)&&index>=0&&typeof actionId==='string'&&actionId.length<=256);
      active={index,actionId,expectedRevision,logicalOutcome:'pending',requests:[],retries:[]};decisions.push(active);
    },
    finish(logicalOutcome){
      assert.ok(active,'No observed decision to finish');active.logicalOutcome=logicalOutcome;
      if(active.requests.length!==1)issue('Logical decision did not have exactly one observed execute/async request');
      if(active.requests[0]?.transportOutcome!=='returned')issue('Logical decision returned without a successful transport observation');
      active=null;pendingCommand=false;
      assert.equal(issues.length,0,issues.join('; '));
    },
    interrupt(error){if(active){active.logicalOutcome='ownership-unresolved';active.error=errorText(error);active=null;}pendingCommand=false;},
    snapshot(){return {connectionRetryCount:0,observation:'WDIO request events, not a packet capture; body hashes/sizes only',
      observationMs,issues:[...issues],decisions:structuredClone(decisions)};},
    dispose(){if(disposed)return;disposed=true;for(const [event,handler] of Object.entries(handlers))browser.off(event,handler);},
  };
}
