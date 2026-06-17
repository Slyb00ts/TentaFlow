// =============================================================================
// File: tests/e2e/ml-studio-share.spec.js
// Description: Real smoke test of the ML Studio "sharing slice" against a LIVE
//              TentaFlow instance (https://localhost:8095). Verifies the new
//              behaviour: (a) ML Studio nav item lives under the "Aplikacje"
//              (Moje aplikacje) section, NOT the admin/AI sections; (b) the
//              project list is split into "Moje projekty" / "Udostępnione mi"
//              with an owner badge; (c) the sharing screen lists members with the
//              admin as owner (members_list over the binary protocol) and exposes
//              an invite form; (d) the §11.2 Power User gate hides ML Studio from a
//              plain user (zwykly/admin). Standalone node script — does NOT spawn
//              its own binary. Screenshots land in /tmp/mlstudio-shots2/.
// =============================================================================

const fs = require('fs');
const { chromium } = require('playwright');

const BASE = 'https://localhost:8095';
const SHOT = '/tmp/mlstudio-shots3';
// Unique per run — the backend enforces UNIQUE(org_id, name), so a fixed name
// makes re-runs collide (second create fails on the taken name). The scenario
// asks for "Cysterny share"; we keep that prefix and append a short run tag.
const PROJECT_NAME = `Cysterny share ${Date.now().toString(36).slice(-5)}`;
const INVITE_ID = '00000000-0000-4000-8000-0000000000aa';

const results = [];
function step(name, pass, note) {
  results.push({ name, pass, note });
  console.log(`[${pass ? 'PASS' : 'FAIL'}] ${name} :: ${note}`);
}

async function shot(page, file) {
  await page.screenshot({ path: `${SHOT}/${file}`, fullPage: true }).catch(() => {});
}

// Wires console/network/WS error sinks onto a page and returns the buffers.
function attachDiagnostics(page, label) {
  const consoleErrors = [];
  const failedRequests = [];
  page.on('console', (msg) => { if (msg.type() === 'error') consoleErrors.push(`[${label}] ${msg.text()}`); });
  page.on('pageerror', (err) => consoleErrors.push(`[${label}] pageerror: ${err.message}`));
  page.on('requestfailed', (req) => {
    failedRequests.push(`[${label}] ${req.method()} ${req.url()} :: ${req.failure()?.errorText}`);
  });
  page.on('websocket', (ws) => {
    ws.on('socketerror', (e) => failedRequests.push(`[${label}] WS error ${ws.url()} :: ${e}`));
  });
  return { consoleErrors, failedRequests };
}

// Logs in via the SPA login form and waits for the sidebar to paint.
async function login(page, user, pass) {
  await page.goto(`${BASE}/`, { waitUntil: 'domcontentloaded', timeout: 30000 });
  const userInput = page.locator('#login-username input').first();
  await userInput.waitFor({ state: 'visible', timeout: 20000 });
  await userInput.fill(user);
  await page.locator('#login-password input').first().fill(pass);
  await page.locator('#login-submit').click();
  await page.waitForSelector('.sidebar .nav-item[data-view]', { timeout: 20000 });
  await page.waitForLoadState('networkidle', { timeout: 15000 }).catch(() => {});
}

// Resolves the section heading text that contains a given nav-item data-view.
async function sectionHeadingFor(page, view) {
  return page.evaluate((v) => {
    const item = document.querySelector(`.sidebar .nav-item[data-view="${v}"]`);
    if (!item) return null;
    const section = item.closest('.nav-section');
    return section?.querySelector('.heading')?.textContent?.trim() ?? '(brak nagłówka)';
  }, view);
}

(async () => {
  fs.mkdirSync(SHOT, { recursive: true });
  const browser = await chromium.launch({ headless: true });

  // ---------------------------------------------------------------------------
  // SCENARIO A — admin: nav placement, ownership split, sharing screen, invite.
  // ---------------------------------------------------------------------------
  const ctxA = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const page = await ctxA.newPage();
  const diagA = attachDiagnostics(page, 'admin');

  let createdProjectId = null;

  try {
    // ---- Step 1: login admin + verify ML Studio is under "Aplikacje" ----
    try {
      await login(page, 'admin', 'admin');
      await shot(page, '01-admin-dashboard.png');

      const heading = await sectionHeadingFor(page, 'ml-studio');
      await shot(page, '02-admin-sidebar.png');

      // The apps section heading is i18n "nav.section_apps" => "Moje aplikacje".
      // We assert it is NOT one of the admin/AI sections.
      const wrongSections = ['Sztuczna inteligencja', 'AI', 'Zarządzanie', 'Core', 'Ogólne', 'Integracje', 'Przepływy'];
      const inApps = heading != null && /aplikacj/i.test(heading);
      const inWrong = wrongSections.some((w) => heading && heading.includes(w));
      step('A1. ML Studio w sekcji „Aplikacje" (nie admin/AI)', inApps && !inWrong,
        `Nagłówek sekcji zawierającej „ML Studio": "${heading}"`);
    } catch (e) {
      await shot(page, '02-admin-sidebar-FAIL.png');
      step('A1. ML Studio w sekcji „Aplikacje"', false, `Błąd: ${e.message}`);
      throw e;
    }

    // ---- Step 2: open ML Studio + create project, verify owner split ----
    try {
      await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-new', { timeout: 15000 });
      await page.waitForFunction(() => {
        const list = document.querySelector('#ml-studio-list');
        return list && (list.querySelector('.ml-studio-card') || list.querySelector('tf-empty-state') || list.querySelector('.ml-studio-section-head'));
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(500);
      await shot(page, '03-ml-studio-list.png');

      // Create the project via the wizard modal.
      await page.locator('#ml-studio-new').click();
      const nameInput = page.locator('#ml-studio-name input').first();
      await nameInput.waitFor({ state: 'visible', timeout: 10000 });
      await nameInput.fill(PROJECT_NAME);
      await page.locator('#ml-studio-desc textarea').first().fill('smoke test udostępniania');
      const recRadio = page.locator('#ml-studio-types tf-radio[value="recognition"]');
      if (await recRadio.count()) await recRadio.first().click();
      await shot(page, '04-create-form.png');

      const submit = page.locator('#ml-studio-create-modal tf-button', { hasText: 'Utwórz projekt' }).first();
      await submit.click();
      await page.waitForSelector('#ml-studio-create-modal', { state: 'detached', timeout: 15000 }).catch(() => {});
      // On success the module navigates to the project detail.
      await page.waitForSelector('#ml-studio-detail tf-detail-header, .ml-studio-card', { timeout: 15000 });
      await page.waitForTimeout(800);

      // Capture the projectId from the router URL (hash/query) if we landed on detail.
      createdProjectId = await page.evaluate(() => {
        const h = location.hash || location.search || '';
        const m = h.match(/projectId[=:/]+([0-9a-fA-F-]{36})/);
        return m ? m[1] : null;
      });

      // Go back to the list to verify the ownership split + owner badge.
      const backBtn = page.locator('#ml-studio-back');
      if (await backBtn.count()) await backBtn.click();
      else await page.locator('.sidebar .nav-item[data-view="ml-studio"]').click();
      await page.waitForSelector('#ml-studio-list', { timeout: 15000 });
      await page.waitForFunction(() => {
        const list = document.querySelector('#ml-studio-list');
        return list && list.querySelector('.ml-studio-card');
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(500);

      // Find the card by name, confirm it is under "Moje projekty" with owner strip.
      const split = await page.evaluate((pname) => {
        const headings = [...document.querySelectorAll('#ml-studio-list .ml-studio-section-head h3')].map((h) => h.textContent.trim());
        const cards = [...document.querySelectorAll('#ml-studio-list .ml-studio-card')];
        const card = cards.find((c) => c.querySelector('.ml-studio-card-name')?.textContent.trim() === pname);
        const ownerText = card?.querySelector('.ml-studio-card-owner')?.textContent.trim() ?? null;
        const hasShareBtn = !!card?.querySelector('[data-share-id]');
        // Which section head precedes this card in DOM order?
        let sectionTitle = null;
        if (card) {
          let n = card.closest('.ml-studio-grid')?.previousElementSibling;
          if (n && n.classList.contains('ml-studio-section-head')) sectionTitle = n.querySelector('h3')?.textContent.trim();
        }
        return { headings, found: !!card, ownerText, hasShareBtn, sectionTitle };
      }, PROJECT_NAME);

      await shot(page, '05-list-owner-split.png');
      const okOwner = split.found && /Właściciel:\s*Ty/i.test(split.ownerText || '');
      step('A2. Projekt w „Moje projekty" + badge „Właściciel: Ty"', okOwner,
        `Sekcje listy: [${split.headings.join(' | ')}]; karta znaleziona: ${split.found}; owner-strip: "${split.ownerText}"; sekcja karty: "${split.sectionTitle}"; przycisk Udostępnij: ${split.hasShareBtn}`);
    } catch (e) {
      await shot(page, '05-list-FAIL.png');
      step('A2. Projekt + split własności', false, `Błąd: ${e.message}`);
    }

    // ---- Step 3: open sharing screen, verify owner is in members list ----
    try {
      // Two routes into #ml-studio-share: the card share icon (owner-only, on the
      // card) and the detail "Zarządzaj dostępem" button (owner-only, in detail).
      // We open the card detail first and capture whether the owner-only action
      // is present (a proxy for the detail response's isOwner). If absent we fall
      // back to the card share icon so we still exercise members_list.
      const card = page.locator('#ml-studio-list .ml-studio-card', { hasText: PROJECT_NAME }).first();
      await card.waitFor({ state: 'visible', timeout: 15000 });
      const cardShareIcon = card.locator('[data-share-id]').first();
      const hasCardShare = await cardShareIcon.count();
      await card.click();
      await page.waitForSelector('#ml-studio-detail tf-detail-header', { timeout: 15000 });
      await page.waitForTimeout(400);
      const detailManage = await page.locator('#ml-studio-manage-access').count();
      step('A3-detail. „Zarządzaj dostępem" w szczególe (detail isOwner)', detailManage > 0,
        `Karta miała ikonę Udostępnij: ${hasCardShare > 0}; przycisk „Zarządzaj dostępem" w szczególe: ${detailManage > 0}`);

      if (detailManage > 0) {
        await page.locator('#ml-studio-manage-access').click();
      } else {
        // Fall back through the card share icon (re-open list).
        await page.locator('#ml-studio-back').click().catch(() => {});
        await page.waitForSelector('#ml-studio-list .ml-studio-card', { timeout: 15000 });
        await page.locator('#ml-studio-list .ml-studio-card', { hasText: PROJECT_NAME })
          .first().locator('[data-share-id]').first().click();
      }

      await page.waitForSelector('#ml-studio-share #ml-studio-members', { timeout: 15000 });
      // Wait for the table rows to materialise inside the open shadow root.
      await page.waitForFunction(() => {
        const t = document.querySelector('#ml-studio-members');
        return t && t.shadowRoot && t.shadowRoot.querySelectorAll('tbody tr').length > 0;
      }, { timeout: 15000 }).catch(() => {});
      await page.waitForTimeout(400);
      await shot(page, '06-share-members.png');

      const shareState = await page.evaluate(() => {
        const t = document.querySelector('#ml-studio-members');
        const rows = t?.shadowRoot ? [...t.shadowRoot.querySelectorAll('tbody tr')] : [];
        const rowTexts = rows.map((r) => r.textContent.replace(/\s+/g, ' ').trim());
        // Owner row: any member cell rendering a "Właściciel" chip.
        const ownerRow = rows.find((r) => /Właściciel/i.test(r.textContent));
        const hasSelf = rows.some((r) => /\(Ty\)/.test(r.textContent));
        const inviteUser = !!document.querySelector('#ml-studio-invite-user');
        const inviteRole = !!document.querySelector('#ml-studio-invite-role');
        const inviteSend = !!document.querySelector('#ml-studio-invite-send');
        // Diagnostics: is the owner-only invite section in the DOM at all, and what
        // badge does the share header carry (drives the isOwner branch)?
        const headerBadges = [...document.querySelectorAll('#ml-studio-share tf-detail-header [slot="badges"] tf-badge')]
          .map((b) => b.getAttribute('value'));
        const shareHeads = [...document.querySelectorAll('#ml-studio-share .ml-studio-share-head')].map((h) => h.textContent.trim());
        const ownerOnlyHint = !!document.querySelector('#ml-studio-share .ml-studio-share-hint');
        return {
          rowCount: rows.length,
          rowTexts,
          hasOwnerRow: !!ownerRow,
          ownerRowText: ownerRow ? ownerRow.textContent.replace(/\s+/g, ' ').trim() : null,
          hasSelf,
          inviteForm: inviteUser && inviteRole && inviteSend,
          headerBadges,
          shareHeads,
          ownerOnlyHint,
        };
      });

      const okShare = shareState.rowCount > 0 && shareState.hasOwnerRow;
      step('A3. Ekran udostępniania — owner na liście członków', okShare,
        `Wierszy: ${shareState.rowCount}; owner-row: ${shareState.hasOwnerRow} ("${shareState.ownerRowText}"); (Ty): ${shareState.hasSelf}; wiersze: [${shareState.rowTexts.join(' || ')}]`);
      const inviteDiag = shareState.inviteForm
        ? 'formularz obecny'
        : 'BRAK formularza — ekran traktuje admina jak NIE-właściciela (renderShare isOwner=false), bo odpowiedź ProjectDetail nie niesie pola is_owner/role (backend: to_detail() w dispatch/ml_studio.rs nie ustawia is_owner, w przeciwieństwie do to_summary()). Dowód: pokazano hint „tylko dla właściciela"';
      step('A3b. Formularz „Zaproś" (id + rola + przycisk)', shareState.inviteForm,
        `Pola zaproszenia obecne: ${shareState.inviteForm} — ${inviteDiag}; badge nagłówka: [${(shareState.headerBadges || []).join(', ')}]; sekcje ekranu: [${(shareState.shareHeads || []).join(' | ')}]; hint-tylko-właściciel: ${shareState.ownerOnlyHint}`);
    } catch (e) {
      await shot(page, '06-share-FAIL.png');
      step('A3. Ekran udostępniania', false, `Błąd: ${e.message}`);
    }

    // ---- Step 4: send invite for "zwykly" (editor) and verify member appears ----
    try {
      const inviteUser = page.locator('#ml-studio-invite-user input').first();
      if (await inviteUser.count()) {
        await inviteUser.fill(INVITE_ID);
        // tf-select for role; default is "editor" which is what we want.
        await shot(page, '07-invite-filled.png');
        await page.locator('#ml-studio-invite-send').click();

        // The module reloads the share screen on success; wait for the new id row.
        // We match the FULL invited id (its 8-char prefix "00000000" collides with the
        // owner UUID, so a prefix match would false-positive on the owner row).
        await page.waitForFunction((id) => {
          const t = document.querySelector('#ml-studio-members');
          if (!t || !t.shadowRoot) return false;
          return [...t.shadowRoot.querySelectorAll('tbody tr')].some((r) => r.textContent.includes(id));
        }, INVITE_ID, { timeout: 12000 }).catch(() => {});
        await page.waitForTimeout(500);
        await shot(page, '08-invite-result.png');

        const inviteState = await page.evaluate((id) => {
          const t = document.querySelector('#ml-studio-members');
          const rows = t?.shadowRoot ? [...t.shadowRoot.querySelectorAll('tbody tr')] : [];
          // The invited row is the one carrying the full invited id in its member cell.
          const row = rows.find((r) => r.textContent.includes(id));
          const txt = row ? row.textContent.replace(/\s+/g, ' ').trim() : '';
          const toastEl = document.querySelector('tf-toast, .tf-toast');
          return {
            rowFound: !!row,
            rowText: row ? txt : null,
            isEditor: /edytor/i.test(txt),
            isPending: /oczekuj/i.test(txt) || /aktywn/i.test(txt),
            rowCount: rows.length,
            toast: toastEl ? toastEl.textContent.replace(/\s+/g, ' ').trim() : null,
          };
        }, INVITE_ID);

        const okInvite = inviteState.rowFound && inviteState.isEditor;
        step('A4. Zaproszenie „zwykly" (Edytor) na liście członków', okInvite,
          okInvite
            ? `Wiersz zaproszonego: "${inviteState.rowText}" (rola Edytor: ${inviteState.isEditor}, status oczekuje/aktywny: ${inviteState.isPending}; łącznie ${inviteState.rowCount} wierszy)`
            : `Zaproszony NIE pojawił się poprawnie. rowFound=${inviteState.rowFound}, isEditor=${inviteState.isEditor}; wierszy: ${inviteState.rowCount}; toast: "${inviteState.toast}". Sprawdź backend (project_members).`);
      } else {
        step('A4. Zaproszenie „zwykly"', false,
          'ZABLOKOWANE: formularz zaproszenia nie istnieje, bo ekran udostępniania nie rozpoznaje admina jako właściciela (ProjectDetail bez is_owner). Zaproszenia nie da się wysłać przez UI mimo że members_list pokazuje admina jako ownera. Naprawa po stronie backendu: to_detail() musi nieść is_owner/role.');
      }
    } catch (e) {
      await shot(page, '08-invite-FAIL.png');
      step('A4. Zaproszenie „zwykly"', false, `Błąd: ${e.message}`);
    }
  } catch (fatal) {
    console.log(`\nFATAL (admin): ${fatal.message}`);
  }

  // ---- Step 5: logout admin (close context to drop session) ----
  try {
    const logout = page.locator('#nav-logout');
    if (await logout.count()) {
      await logout.click().catch(() => {});
      await page.waitForTimeout(800);
    }
    step('A5. Wylogowanie admina', true, await logout.count() ? 'Kliknięto #nav-logout' : 'Brak przycisku — kontekst zostanie zamknięty');
  } catch (e) {
    step('A5. Wylogowanie admina', false, `Błąd: ${e.message}`);
  }
  await ctxA.close();

  // ---------------------------------------------------------------------------
  // SCENARIO B — plain user (zwykly/admin): §11.2 gate hides ML Studio.
  // ---------------------------------------------------------------------------
  const ctxB = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });
  const pageB = await ctxB.newPage();
  const diagB = attachDiagnostics(pageB, 'zwykly');

  try {
    await login(pageB, 'zwykly', 'admin');
    await shot(pageB, '09-zwykly-sidebar.png');

    const gate = await pageB.evaluate(() => {
      const items = [...document.querySelectorAll('.sidebar .nav-item[data-view]')].map((el) => el.dataset.view);
      const headings = [...document.querySelectorAll('.sidebar .nav-section .heading')].map((h) => h.textContent.trim());
      const roleText = document.querySelector('.user-chip .role')?.textContent.trim() ?? null;
      return { items, headings, roleText, hasMlStudio: items.includes('ml-studio') };
    });

    step('B6. §11.2 — zwykły user NIE widzi „ML Studio"', !gate.hasMlStudio,
      `Rola w UI: "${gate.roleText}"; pozycje nawigacji: [${gate.items.join(', ')}]; sekcje: [${gate.headings.join(' | ')}]`);
  } catch (e) {
    await shot(pageB, '09-zwykly-FAIL.png');
    step('B6. §11.2 gate Power User', false, `Błąd: ${e.message}`);
  }
  await ctxB.close();

  // ---------------------------------------------------------------------------
  // Report
  // ---------------------------------------------------------------------------
  const consoleErrors = [...diagA.consoleErrors, ...diagB.consoleErrors];
  const failedRequests = [...diagA.failedRequests, ...diagB.failedRequests];

  console.log('\n================ KONSOLA / SIEĆ ================');
  console.log(`Błędy konsoli JS: ${consoleErrors.length}`);
  consoleErrors.slice(0, 40).forEach((e) => console.log('  JS> ' + e));
  console.log(`Nieudane żądania / WS: ${failedRequests.length}`);
  failedRequests.slice(0, 40).forEach((e) => console.log('  NET> ' + e));

  console.log('\n================ PODSUMOWANIE =================');
  results.forEach((r) => console.log(`  [${r.pass ? 'PASS' : 'FAIL'}] ${r.name} — ${r.note}`));
  const allPass = results.length > 0 && results.every((r) => r.pass);
  console.log(`\nZrzuty: ${SHOT}/`);
  console.log(`WYNIK OGÓLNY: ${allPass ? 'PASS' : 'FAIL'}`);

  await browser.close();
  process.exit(allPass ? 0 : 1);
})();
