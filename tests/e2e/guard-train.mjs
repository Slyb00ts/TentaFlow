import { chromium } from 'playwright';
const A_URL='https://localhost:8095/', CREDS={username:'power1',password:'power123'};
const B_ID='3b226fa884a5de60a03602223397975fbc84164de79847e8c4fc3ff4f55f1404';
const PROJECT_ID='955a7407-4911-43df-9e5d-dcbd3b9ab010', DATASET_ID='3ea30e46-256c-41a0-ae1d-aced4d87c102';
const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
async function ev(page,fn,arg){return page.evaluate(async({f,a})=>{const{ApiBinary}=await import('/js/protocol/api-binary-shim.js');return new Function('ApiBinary','arg','return ('+f+')(ApiBinary,arg);')(ApiBinary,a);},{f:fn.toString(),a:arg});}
(async()=>{const b=await chromium.launch({headless:true});const c=await b.newContext({ignoreHTTPSErrors:true});const p=await c.newPage();
try{await p.goto(A_URL,{waitUntil:'domcontentloaded'});
const u=await p.evaluate(async(cr)=>{const{ApiBinary,initTransport}=await import('/js/protocol/api-binary-shim.js');await initTransport();const r=await ApiBinary.action('authLoginRequest',cr);if(r&&r.jwt)await ApiBinary.setJwt(r.jwt);return (await ApiBinary.one('authMeRequest'))?.username;},CREDS);
console.log('login:',u);
const s=await ev(p,(Api,a)=>Api.action('mlStudioFtTrainStartRequest',{projectId:a.pid,datasetId:a.did,baseModel:'Qwen/Qwen2.5-0.5B-Instruct',method:'lora',objective:'sft',targetNodeId:a.bid,numGpus:0,hyperparams:{epochs:2,batchSize:4,gradAccumSteps:1,learningRate:2e-4,maxSeqLen:512,loraR:16,loraAlpha:32,loraDropout:0.05}},{timeoutMs:180000}),{pid:PROJECT_ID,did:DATASET_ID,bid:B_ID});
console.log('train ->',JSON.stringify(s));const rid=s.runId||s.run_id;if(!rid)throw new Error('no runId');
let last='';for(let i=0;i<150;i++){await sleep(8000);const st=await ev(p,(Api,id)=>Api.one('mlStudioFtTrainStatusRequest',{runId:id}),rid).catch(e=>({error:String(e)}));const status=st.status||st.error;
if(status==='syncing'){console.log(`poll ${i}: SYNC ${st.syncBytesSent}/${st.syncBytesTotal}B`);}
else{const l=`status=${status} step=${st.step??''} loss=${st.trainLoss??st.train_loss??''}`;if(l!==last)console.log('poll '+i+': '+l);last=l;}
if(['succeeded','failed','completed','error'].includes(String(status))){console.log('FINAL:',status,'err=',st.error||'(none)');if(status==='succeeded')console.log('GUARD_MODEL_RUN='+rid);break;}}
console.log('GUARDTRAIN_DONE');}catch(e){console.error('ERR:',e?.message||e);process.exitCode=1;}finally{await b.close();}})();
