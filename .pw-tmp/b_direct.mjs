import { createRequire } from 'module';
const { chromium } = createRequire('/home/critix/repos/rust/TentaFlow/tests/e2e/package.json')('@playwright/test');
const S='/home/critix/repos/rust/TentaFlow/.pw-tmp'; const URL='https://127.0.0.1:8090/ml-studio/share/1b07e448-cd22-47eb-92a9-8974a153b940/manifest'; const KEY='sk-e75e068899950ffc9c22c955af408486c00b226663d764d385994f7b9fa65788'; const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const browser=await chromium.launch({args:['--ignore-certificate-errors']});
const page=await (await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1600,height:1000}})).newPage();
page.on('pageerror',e=>console.log('[PAGEERR]',String(e).slice(0,140)));
const mt=()=>page.evaluate(()=>{const m=document.querySelector('tf-modal[open]');return m?m.innerText.replace(/\s+/g,' '):'';});
try{
  await page.goto('https://127.0.0.1:8091/',{waitUntil:'domcontentloaded'}); await sleep(1500);
  const u=page.locator('input[type="text"],input[name="username"]').first();
  if(await u.isVisible().catch(()=>false)){await u.fill('admin');await page.locator('input[type="password"]').first().fill('admin');await page.locator('input[type="password"]').first().press('Enter');await sleep(2500);}
  await page.evaluate(async()=>{const m=await import('/js/router.js');m.Router.navigate('ml-studio');}); await sleep(2500);
  await page.evaluate(()=>{document.getElementById('ml-studio-import-url')?.click();}); await sleep(1200);
  await page.evaluate((d)=>{const sv=(id,v)=>{const el=document.getElementById(id);if(el){el.value=v;el.dispatchEvent(new Event('input',{bubbles:true}));}};sv('ml-studio-remote-url',d.url);sv('ml-studio-remote-key',d.key);sv('ml-studio-remote-name','Cysterny-Import');},{url:URL,key:KEY}); await sleep(500);
  const t0=Date.now();
  await page.evaluate(()=>{document.getElementById('ml-studio-remote-import')?.click();});
  // KLUCZOWE: czas do progress view (bez 30s timeout)
  let started=false, startMs=0;
  for(let i=0;i<12;i++){ await sleep(2000); const t=await mt();
    if(/łączenie|pobieran|import|postęp|faz/i.test(t)&&!/URL manifestu/i.test(t)){ startMs=Date.now()-t0; console.log('IMPORT WYSTARTOWAL po '+startMs+'ms (bez timeout!): '+t.slice(0,120)); started=true; break; }
    if(/timed out|timeout|błąd|nie udało|denied/i.test(t)){ console.log('BLAD ~'+((Date.now()-t0))+'ms: '+t.slice(0,180)); break; }
  }
  if(started){
    // monitoruj postep 11GB (build A + download + import) - dluго
    for(let i=0;i<200;i++){ await sleep(6000); const t=await mt();
      if(/(zaimportowano|utworzono|gotowe|zakończ|ukończ|sukces)/i.test(t)){ console.log('IMPORT DONE ~'+(i*6)+'s'); break; }
      if(/(niepowodzen|nie udało|błąd|timed out)/i.test(t)&&!/bez błęd/i.test(t)){ console.log('IMPORT FAIL ~'+(i*6)+'s: '+t.slice(0,180)); break; }
      if(i%3===0) console.log('  '+(i*6)+'s: '+(t.match(/łączenie|pobieran\w+|import\w*|\d+ ?\/ ?\d+|\d+(\.\d+)? ?[MG]B/gi)||[]).slice(0,3).join(' ')||t.slice(0,50));
    }
  }
  await page.screenshot({path:S+'/b-direct-final.png',fullPage:true}).catch(()=>{});
}catch(e){console.log('ERR',String(e).slice(0,200));}finally{await browser.close();}
