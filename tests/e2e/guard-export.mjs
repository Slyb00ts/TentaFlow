import { chromium } from 'playwright';
const A_URL='https://localhost:8095/', CREDS={username:'power1',password:'power123'};
const MODEL_ID='b9323d8b-310e-4ac9-8f65-cc3a25e4023f';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function ev(p,fn,arg){return p.evaluate(async({f,a})=>{const{ApiBinary}=await import('/js/protocol/api-binary-shim.js');return new Function('ApiBinary','arg','return ('+f+')(ApiBinary,arg);')(ApiBinary,a);},{f:fn.toString(),a:arg});}
(async()=>{const b=await chromium.launch({headless:true});const c=await b.newContext({ignoreHTTPSErrors:true});const p=await c.newPage();
try{await p.goto(A_URL,{waitUntil:'domcontentloaded'});
await p.evaluate(async cr=>{const{ApiBinary,initTransport}=await import('/js/protocol/api-binary-shim.js');await initTransport();const r=await ApiBinary.action('authLoginRequest',cr);if(r&&r.jwt)await ApiBinary.setJwt(r.jwt);},CREDS);
console.log('login ok');
const s=await ev(p,(Api,a)=>Api.action('mlStudioFtExportRequest',{modelId:a.mid,outtype:'q8_0'}),{mid:MODEL_ID});
console.log('export start ->',JSON.stringify(s));
let last='';
for(let i=0;i<90;i++){await sleep(8000);
const st=await ev(p,(Api,id)=>Api.one('mlStudioFtExportStatusRequest',{modelId:id}),MODEL_ID).catch(e=>({error:String(e)}));
const js=JSON.stringify(st);
if(js!==last)console.log('poll '+i+': '+js.slice(0,300));last=js;
const es=st.exportStatus??st.export_status??st.status;
const gg=st.ggufPath??st.gguf_path;
if(es==='succeeded'||gg){console.log('EXPORT_SUCCEEDED gguf='+gg);break;}
if(es==='failed'||st.error){console.log('EXPORT_FAILED '+(st.exportError??st.export_error??st.error));break;}}
console.log('EXPORT_DONE');}catch(e){console.error('ERR:',e?.message||e);process.exitCode=1;}finally{await b.close();}})();
