import { createRequire } from 'module';
const { chromium } = createRequire('/home/critix/repos/rust/TentaFlow/tests/e2e/package.json')('@playwright/test');
const S='/home/critix/repos/rust/TentaFlow/.pw-tmp'; const sleep=(ms)=>new Promise(r=>setTimeout(r,ms));
const browser=await chromium.launch({args:['--ignore-certificate-errors']});
const page=await (await browser.newContext({ignoreHTTPSErrors:true,viewport:{width:1600,height:1000}})).newPage();
try{
  await page.goto('https://127.0.0.1:8090/',{waitUntil:'domcontentloaded'}); await sleep(1200);
  const u=page.locator('input[type="text"],input[name="username"]').first();
  if(await u.isVisible().catch(()=>false)){await u.fill('admin');await page.locator('input[type="password"]').first().fill('admin');await page.locator('input[type="password"]').first().press('Enter');await sleep(2500);}
  await page.evaluate(async()=>{const m=await import('/js/router.js');m.Router.navigate('mesh');}); await sleep(3000);
  await page.screenshot({path:`${S}/a-mesh.png`,fullPage:true});
  // dump peers via protocol
  const peers=await page.evaluate(async()=>{
    try{const {ApiBinary}=await import('/js/api.js'); const r=await ApiBinary.one('meshPeersListRequest',{}); return r;}catch(e){return {err:String(e).slice(0,120)};}
  });
  console.log('MESH PEERS:', JSON.stringify(peers).slice(0,500));
}catch(e){console.log('ERR',String(e).slice(0,200));}finally{await browser.close();}
