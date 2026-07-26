import { createRequire } from 'module';
const { chromium } = createRequire('/home/critix/repos/rust/TentaFlow/tests/e2e/package.json')('@playwright/test');
const S='/home/critix/repos/rust/TentaFlow/.pw-tmp'; const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const browser=await chromium.launch({args:['--ignore-certificate-errors']});
const page=await (await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1600,height:1000}})).newPage();
page.on('console',m=>{if(m.type()==='error')console.log('[err]',m.text().slice(0,140));});
try{
  await page.goto('https://127.0.0.1:8090/',{waitUntil:'domcontentloaded'}); await sleep(1200);
  const u=page.locator('input[type="text"],input[name="username"]').first();
  if(await u.isVisible().catch(()=>false)){await u.fill('admin');await page.locator('input[type="password"]').first().fill('admin');await page.locator('input[type="password"]').first().press('Enter');await sleep(2500);}
  await page.evaluate(async()=>{const m=await import('/js/router.js');m.Router.navigate('access-keys');}); await sleep(2500);
  await page.screenshot({path:`${S}/a-keys-1.png`,fullPage:true});
  const info=await page.evaluate(()=>{
    const txt=el=>(el?.textContent||'').trim().replace(/\s+/g,' ').slice(0,40);
    const btns=[...document.querySelectorAll('tf-button,button')].map(txt).filter(Boolean).filter(t=>/klucz|nowy|utwórz|dodaj|generuj|create/i.test(t));
    const tabs=[...document.querySelectorAll('.tf-tab-btn,[data-tab]')].map(b=>b.dataset.tab||txt(b)).filter(Boolean);
    return {btns,tabs};
  });
  console.log('BTNS:',JSON.stringify(info.btns)); console.log('TABS:',JSON.stringify(info.tabs));
}catch(e){console.log('ERR',String(e).slice(0,200));}finally{await browser.close();}
