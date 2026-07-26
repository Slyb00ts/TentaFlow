import { createRequire } from 'module';
const { chromium } = createRequire('/home/critix/repos/rust/TentaFlow/tests/e2e/package.json')('@playwright/test');
const S='/home/critix/repos/rust/TentaFlow/.pw-tmp'; const ORLEN='1b07e448-cd22-47eb-92a9-8974a153b940'; const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const fs=createRequire(import.meta.url)('fs');
const browser=await chromium.launch({args:['--ignore-certificate-errors']});
const page=await (await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1600,height:1000}})).newPage();
try{
  await page.goto('https://127.0.0.1:8090/',{waitUntil:'domcontentloaded'}); await sleep(1200);
  const u=page.locator('input[type="text"],input[name="username"]').first();
  if(await u.isVisible().catch(()=>false)){await u.fill('admin');await page.locator('input[type="password"]').first().fill('admin');await page.locator('input[type="password"]').first().press('Enter');await sleep(2500);}
  await page.evaluate(async()=>{const m=await import('/js/router.js');m.Router.navigate('access-keys');}); await sleep(2000);
  await page.evaluate(()=>{document.getElementById('ak-create')?.click();}); await sleep(1200);
  // krok 1: typ general + nazwa
  await page.evaluate(()=>{const c=[...document.querySelectorAll('.ak-type-card')].find(x=>/ogóln|general/i.test(x.textContent||''));c&&c.click();}); await sleep(300);
  await page.evaluate(()=>{const i=document.getElementById('ak-name'); if(i){i.value='remote-orlen'; i.dispatchEvent(new Event('input',{bubbles:true}));}}); await sleep(300);
  await page.evaluate(()=>{document.getElementById('ak-next')?.click();}); await sleep(1500);
  // krok 2: czy jest bucket ml_studio_export + Orlen row?
  const scope=await page.evaluate((orlen)=>{
    const rows=[...document.querySelectorAll('.ak-pick-row')].map(r=>r.dataset.key);
    const mlRows=rows.filter(k=>k&&k.startsWith('ml_studio_export:'));
    const orlenRow=rows.find(k=>k===('ml_studio_export:'+orlen));
    return {total:rows.length, mlStudioCount:mlRows.length, mlRows:mlRows.slice(0,5), orlenPresent:!!orlenRow};
  },ORLEN);
  console.log('SCOPE ROWS:',JSON.stringify(scope));
  if(!scope.orlenPresent){ console.log('BLAD: brak wiersza ml_studio_export dla Orlen'); await page.screenshot({path:S+'/a-scope.png',fullPage:true}); }
  else {
    // zaznacz Orlen + utwórz
    await page.evaluate((orlen)=>{const r=[...document.querySelectorAll('.ak-pick-row')].find(x=>x.dataset.key==='ml_studio_export:'+orlen); r&&r.click();},ORLEN); await sleep(500);
    await page.evaluate(()=>{document.getElementById('ak-create-btn')?.click();}); await sleep(2000);
    const token=await page.evaluate(()=>{const t=document.getElementById('ak-token'); return t?t.textContent.trim():null;});
    if(token){ fs.writeFileSync(S+'/orlen-key.txt', token); console.log('KLUCZ UTWORZONY, dlugosc:', token.length, 'prefiks:', token.slice(0,8)); }
    else { console.log('BRAK TOKENA'); await page.screenshot({path:S+'/a-token.png',fullPage:true}); }
  }
}catch(e){console.log('ERR',String(e).slice(0,220));}finally{await browser.close();}
