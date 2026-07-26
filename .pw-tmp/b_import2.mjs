import { createRequire } from 'module';
const { chromium } = createRequire('/home/critix/repos/rust/TentaFlow/tests/e2e/package.json')('@playwright/test');
const S='/home/critix/repos/rust/TentaFlow/.pw-tmp'; const URL='https://127.0.0.1:8090/ml-studio/share/75303e4d-3583-423f-8aea-eff89a20eca3/manifest'; const KEY='sk-07788e8fcccced41f1f03c7052ba01be8681739971841e4a18ee1fd397c22223'; const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const browser=await chromium.launch({args:['--ignore-certificate-errors']});
const page=await (await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1600,height:1000}})).newPage();
page.on('pageerror',e=>console.log('[PAGEERR]',String(e).slice(0,140)));
const mt=()=>page.evaluate(()=>{const m=document.querySelector('tf-modal[open]');return m?m.innerText.replace(/\s+/g,' '):'';});
try{
  await page.goto('https://127.0.0.1:8091/',{waitUntil:'domcontentloaded'}); await sleep(1500);
  const u=page.locator('input[type="text"],input[name="username"]').first();
  if(await u.isVisible().catch(()=>false)){await u.fill('admin');await page.locator('input[type="password"]').first().fill('admin');await page.locator('input[type="password"]').first().press('Enter');await sleep(2500);}
  await page.evaluate(async()=>{const m=await import('/js/router.js');m.Router.navigate('ml-studio');}); await sleep(2500);
  const opened=await page.evaluate(()=>{const b=document.getElementById('ml-studio-import-url');if(b){b.click();return true;}return false;});
  console.log('remote import modal:',opened); await sleep(1200);
  await page.evaluate((d)=>{
    const setv=(id,v)=>{const el=document.getElementById(id);if(el){el.value=v;el.dispatchEvent(new Event('input',{bubbles:true}));}};
    setv('ml-studio-remote-url',d.url); setv('ml-studio-remote-key',d.key);
  },{url:URL,key:KEY}); await sleep(500);
  console.log('preview klik:',await page.evaluate(()=>{const b=document.getElementById('ml-studio-remote-preview');if(b){b.click();return true;}return false;}));
  // czekaj na preview
  let previewed=false;
  for(let i=0;i<50;i++){ await sleep(5000); const t=await mt();
    if(/dataset|klas|obraz|archiw|wersja/i.test(t)&&/importuj/i.test(t)){ console.log('PREVIEW OK ~'+(i*3)+'s: '+t.slice(0,220)); previewed=true; break; }
    if(/(błąd|nie udało|denied|403|401)/i.test(t)){ console.log('PREVIEW FAIL ~'+(i*3)+'s: '+t.slice(0,200)); break; }
  }
  if(previewed){
    console.log('import klik:',await page.evaluate(()=>{const b=document.getElementById('ml-studio-remote-import');if(b){b.click();return true;}return false;}));
    for(let i=0;i<160;i++){ await sleep(5000); const t=await mt();
      if(/(zaimportowano|utworzono|gotowe|zakończ|ukończ|sukces)/i.test(t)){ console.log('IMPORT DONE ~'+(i*5)+'s: '+t.slice(0,180)); break; }
      if(/(niepowodzen|nie udało|błąd)/i.test(t)&&!/bez błęd/i.test(t)){ console.log('IMPORT FAIL ~'+(i*5)+'s: '+t.slice(0,200)); break; }
      if(i%3===0) console.log('  '+(i*5)+'s: '+(t.match(/pobieran\w+|\d+ ?\/ ?\d+|\d+%|\d+(\.\d+)? ?[MG]B|faz\w+/gi)||[]).slice(0,3).join(' ')||t.slice(0,50));
    }
  }
  await page.screenshot({path:S+'/b-import-final.png',fullPage:true}).catch(()=>{});
}catch(e){console.log('ERR',String(e).slice(0,220));}finally{await browser.close();}
