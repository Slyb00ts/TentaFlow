import { createRequire } from 'module';
const { chromium } = createRequire('/home/critix/repos/rust/TentaFlow/tests/e2e/package.json')('@playwright/test');
const S='/home/critix/repos/rust/TentaFlow/.pw-tmp'; const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const browser=await chromium.launch({args:['--ignore-certificate-errors']});
const page=await (await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1600,height:1000}})).newPage();
try{
  await page.goto('https://127.0.0.1:8090/',{waitUntil:'domcontentloaded'}); await sleep(1200);
  const u=page.locator('input[type="text"],input[name="username"]').first();
  if(await u.isVisible().catch(()=>false)){await u.fill('admin');await page.locator('input[type="password"]').first().fill('admin');await page.locator('input[type="password"]').first().press('Enter');await sleep(2500);}
  await page.evaluate(async()=>{const m=await import('/js/router.js');m.Router.navigate('access-keys');}); await sleep(2000);
  await page.evaluate(()=>{const b=[...document.querySelectorAll('tf-button,button')].find(x=>/nowy klucz/i.test(x.textContent||''));b&&b.click();}); await sleep(1500);
  await page.screenshot({path:`${S}/a-newkey.png`,fullPage:true});
  const info=await page.evaluate(()=>{
    const txt=el=>(el?.textContent||'').trim().replace(/\s+/g,' ').slice(0,50);
    const modal=document.querySelector('tf-modal[open],tf-window[open]')||document;
    const labels=[...modal.querySelectorAll('label,.subtab,[data-rv],h3,h4,legend')].map(txt).filter(Boolean).slice(0,30);
    const hasMlStudio=/projekty ml studio|ml_studio/i.test(modal.innerText||'');
    const inputs=[...modal.querySelectorAll('input,tf-input,tf-select')].map(i=>i.id||i.getAttribute('placeholder')||i.tagName).slice(0,10);
    return {labels,hasMlStudio,inputs};
  });
  console.log('ML STUDIO BUCKET WIDOCZNY:',info.hasMlStudio);
  console.log('LABELS/SUBTABS:',JSON.stringify(info.labels));
  console.log('INPUTS:',JSON.stringify(info.inputs));
}catch(e){console.log('ERR',String(e).slice(0,200));}finally{await browser.close();}
