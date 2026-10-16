import test from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {createDecisionTransport} from './decision-transport.mjs';
const identity={index:0,actionId:'pair-0',expectedRevision:3};
function start(browser,body){browser.emit('command',{command:'executeAsyncScript',method:'POST',endpoint:'/session/known/execute/async'});browser.emit('request.start',{method:'POST',body});}
function returned(browser,body){browser.emit('request.performance',{request:{method:'POST',body},durationMillisecond:15,success:true,retryCount:0});}
test('metadata binds one request to one logical decision without retaining argument/result payload',()=>{
  const browser=new EventEmitter(),observer=createDecisionTransport(browser);const body=JSON.stringify({script:'save',args:[{secret:'do-not-retain'}]});
  observer.begin(identity);start(browser,body);returned(browser,body);observer.finish('native-ack-verified');
  const value=observer.snapshot(),request=value.decisions[0].requests[0];
  assert.equal(request.sequence,1);assert.equal(request.bodyBytes,Buffer.byteLength(body));assert.match(request.bodySha256,/^[a-f0-9]{64}$/);
  assert.equal(request.durationMs,15);assert.equal(JSON.stringify(value).includes('do-not-retain'),false);assert.equal(value.decisions[0].actionId,'pair-0');
  observer.dispose();assert.equal(browser.listenerCount('request.start'),0);
});
test('unresolved transport remains failure and an automatic retry is retained, never reissued by observer',()=>{
  const browser=new EventEmitter(),observer=createDecisionTransport(browser);observer.begin(identity);start(browser,'{}');
  browser.emit('request.retry',{retryCount:1,error:new Error('target frame detached')});
  browser.emit('request.performance',{request:{method:'POST',body:'{}'},durationMillisecond:30,success:false,retryCount:1,error:new Error('timeout')});
  observer.interrupt(new Error('ownership unresolved'));
  const value=observer.snapshot();assert.equal(value.decisions[0].logicalOutcome,'ownership-unresolved');assert.equal(value.decisions[0].requests[0].transportOutcome,'error');
  assert.equal(value.decisions[0].retries.length,1);assert.ok(value.issues.length);assert.equal(value.decisions.length,1);observer.dispose();
});
test('unrelated requests are ignored; duplicate starts, missing completion and bounds fail visibly',()=>{
  const browser=new EventEmitter(),observer=createDecisionTransport(browser,{maximumDecisions:1});
  start(browser,'ignored');observer.begin(identity);start(browser,'{}');start(browser,'{}');
  assert.throws(()=>observer.finish('native-ack-verified'),/More than one/);assert.equal(observer.snapshot().decisions[0].requests.length,2);
  assert.throws(()=>observer.begin({...identity,index:1}),/limit/);observer.dispose();
});
test('late events after disposal cannot rewrite evidence or retain another request',()=>{
  const browser=new EventEmitter(),observer=createDecisionTransport(browser);observer.begin(identity);start(browser,'{}');observer.interrupt('unresolved');observer.dispose();
  const before=observer.snapshot();returned(browser,'{}');assert.deepEqual(observer.snapshot(),before);
});
