// ===== File: praca.js — small shared interactions for the projekty-praca mockups =====
// Everything is optional sugar: every screen must read correctly with JS disabled.
(function () {
  var reduced = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // SLA rings: <span class="sla sla-warn" data-left="0.35">…<svg data-ring></svg>…</span>
  document.querySelectorAll('.sla[data-left]').forEach(function (el) {
    var left = Math.max(0, Math.min(1, parseFloat(el.getAttribute('data-left'))));
    var r = 7, c = 2 * Math.PI * r;
    var svg = '<svg viewBox="0 0 18 18" aria-hidden="true"><circle class="sla-track" cx="9" cy="9" r="' + r + '" fill="none" stroke-width="2.4"/>' +
      '<circle class="sla-fill" cx="9" cy="9" r="' + r + '" fill="none" stroke-width="2.4" stroke-dasharray="' + c.toFixed(2) + '" stroke-dashoffset="' + c.toFixed(2) + '"/></svg>';
    el.insertAdjacentHTML('afterbegin', svg);
    var fill = el.querySelector('.sla-fill');
    requestAnimationFrame(function () { fill.style.strokeDashoffset = (c * (1 - left)).toFixed(2); });
  });

  // Count-up numbers: <span data-count="128">0</span>
  document.querySelectorAll('[data-count]').forEach(function (el) {
    var target = parseFloat(el.getAttribute('data-count'));
    var decimals = (el.getAttribute('data-count').split('.')[1] || '').length;
    if (reduced) { el.textContent = target.toFixed(decimals).replace('.', ','); return; }
    var start = performance.now(), dur = 700;
    function tick(now) {
      var t = Math.min(1, (now - start) / dur), e = 1 - Math.pow(1 - t, 3);
      el.textContent = (target * e).toFixed(decimals).replace('.', ',');
      if (t < 1) requestAnimationFrame(tick);
    }
    requestAnimationFrame(tick);
  });


  // Subproject switcher in the project header: <span class="sub-switch-wrap"><button class="sub-switch" data-node="na">…</button></span>
  // The tree is mock data; nodes without a screen in this set link to "#".
  var TREE = [
    { id: 'na',  name: 'NextApp', key: 'NA', lvl: 0, open: 48, late: 1, href: 'a02-tablica.html', note: 'produkt' },
    { id: 'ec',  name: 'Energetyka Centrum', key: 'EC', lvl: 1, group: true, open: 38, late: 1, href: '#', note: 'klient' },
    { id: 'ecw', name: 'Wdrożenie DCIM', key: 'ECW', lvl: 2, open: 31, late: 0, href: 'c01-roadmapa.html' },
    { id: 'ecu', name: 'Utrzymanie', key: 'ECU', lvl: 2, open: 7, late: 1, href: 'a07-tablica-utrzymanie.html' },
    { id: 'plw', name: 'Port Lotniczy Wschód', key: 'PLW', lvl: 1, group: true, open: 4, late: 0, href: '#', note: 'klient' },
    { id: 'plu', name: 'Utrzymanie', key: 'PLU', lvl: 2, open: 4, late: 0, href: '#' },
    { id: 'br',  name: 'Bank Regionalny', key: 'BR', lvl: 1, group: true, open: 22, late: 0, href: '#', note: 'klient · prywatny', closed: true },
    { id: 'brw', name: 'Wdrożenie', key: 'BRW', lvl: 2, open: 19, late: 0, href: '#' },
    { id: 'bru', name: 'Utrzymanie', key: 'BRU', lvl: 2, open: 3, late: 0, href: '#' }
  ];
  function subRow(n, current) {
    var ind = '<span class="sp-ind" style="width:' + (n.lvl * 16) + 'px"></span>';
    var ico = n.lvl === 0 ? 'i-folder' : (n.group ? 'i-sitemap' : 'i-layers');
    var late = n.late ? '<span class="sp-late">' + n.late + ' po SLA</span>' : '';
    var lock = n.closed ? '<span class="sp-closed" title="Podprojekt prywatny — widzą go tylko jawni członkowie"><svg class="icon"><use href="#i-lock"/></svg></span>' : '';
    var t = n.href === '#' ? ' title="Ekran spoza tego zestawu mockupów"' : '';
    return '<a class="sp-row' + (n.group ? ' group' : '') + (n.id === current ? ' current' : '') + '" href="' + n.href + '"' + t + '>' + ind +
      '<svg class="icon"><use href="#' + ico + '"/></svg><span class="sp-name">' + n.name + (n.note ? '<small>' + n.note + '</small>' : '') + '</span>' + lock +
      '<span class="sp-key">' + n.key + '</span>' + late + '<span class="sp-n">' + n.open + '</span></a>';
  }
  document.querySelectorAll('.sub-switch').forEach(function (btn) {
    var current = btn.getAttribute('data-node') || 'na';
    var pop = document.createElement('div');
    pop.className = 'sub-pop hidden'; pop.setAttribute('role', 'dialog'); pop.setAttribute('aria-label', 'Wybierz podprojekt');
    pop.innerHTML = '<div class="sp-search"><svg class="icon"><use href="#i-search"/></svg><input placeholder="Szukaj projektu lub klucza…" aria-label="Szukaj projektu"></div>' +
      '<div class="sp-list">' + TREE.map(function (n) { return subRow(n, current); }).join('') + '</div>' +
      '<div class="sp-foot"><a class="btn btn-sm" href="d10-nowy-podprojekt.html"><svg class="icon"><use href="#i-plus"/></svg>Podprojekt</a><span class="hint">liczba = otwarte z poddrzewem · <a href="d09-podprojekty.html">zakończone (1)</a></span></div>';
    btn.parentNode.appendChild(pop);
    btn.setAttribute('aria-expanded', 'false');
    if (btn.hasAttribute('data-open')) { pop.classList.remove('hidden'); btn.setAttribute('aria-expanded', 'true'); }
    btn.addEventListener('click', function (e) {
      e.stopPropagation();
      var open = pop.classList.toggle('hidden') === false;
      btn.setAttribute('aria-expanded', open ? 'true' : 'false');
      if (open) pop.querySelector('input').focus();
    });
    pop.addEventListener('click', function (e) { e.stopPropagation(); });
    pop.querySelector('input').addEventListener('input', function (e) {
      var q = e.target.value.trim().toLowerCase();
      pop.querySelectorAll('.sp-row').forEach(function (r) { r.classList.toggle('scope-hidden', !!q && r.textContent.toLowerCase().indexOf(q) < 0); });
    });
    document.addEventListener('click', function () { pop.classList.add('hidden'); btn.setAttribute('aria-expanded', 'false'); });
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') { pop.classList.add('hidden'); btn.setAttribute('aria-expanded', 'false'); btn.focus(); } });
  });

  // Scope toggle (Ten projekt / Z podprojektami): visual toggle; rows/cards marked .from-sub hide in "own" scope
  document.querySelectorAll('.scope-toggle').forEach(function (group) {
    group.addEventListener('click', function (e) {
      var b = e.target.closest('button'); if (!b) return;
      group.querySelectorAll('button').forEach(function (x) { x.classList.toggle('active', x === b); });
      var own = b.getAttribute('data-scope') === 'own';
      document.querySelectorAll('.from-sub').forEach(function (el) { el.classList.toggle('scope-hidden', own); });
    });
  });

  // Pill tabs / segmented: visual toggle only
  document.querySelectorAll('.pill-tabs').forEach(function (group) {
    group.addEventListener('click', function (e) {
      var b = e.target.closest('button'); if (!b) return;
      group.querySelectorAll('button').forEach(function (x) { x.classList.toggle('active', x === b); });
    });
  });

  // Copy buttons with confirmation overlay
  document.querySelectorAll('.copy-btn').forEach(function (btn) {
    if (!btn.querySelector('.done')) btn.insertAdjacentHTML('beforeend', '<span class="done">Skopiowano</span>');
    btn.addEventListener('click', function () {
      btn.classList.add('copied');
      setTimeout(function () { btn.classList.remove('copied'); }, 1400);
    });
  });

  // Kanban demo: drag cards between columns (.board-col[data-accept] / .tcard[draggable])
  var dragged = null;
  document.querySelectorAll('.tcard[draggable="true"]').forEach(function (card) {
    card.addEventListener('dragstart', function (e) {
      dragged = card; card.classList.add('dragging');
      e.dataTransfer.effectAllowed = 'move';
      document.querySelectorAll('.board-col').forEach(function (c) { if (c.dataset.locked === 'true') c.classList.add('locked'); });
    });
    card.addEventListener('dragend', function () {
      card.classList.remove('dragging'); dragged = null;
      document.querySelectorAll('.board-col').forEach(function (c) { c.classList.remove('drop-target', 'locked'); });
    });
  });
  document.querySelectorAll('.board-col').forEach(function (col) {
    col.addEventListener('dragover', function (e) {
      if (!dragged || col.dataset.locked === 'true') return;
      e.preventDefault(); col.classList.add('drop-target');
    });
    col.addEventListener('dragleave', function () { col.classList.remove('drop-target'); });
    col.addEventListener('drop', function (e) {
      if (!dragged || col.dataset.locked === 'true') return;
      e.preventDefault();
      col.appendChild(dragged);
      dragged.classList.remove('just-moved'); void dragged.offsetWidth; dragged.classList.add('just-moved');
      col.classList.remove('drop-target');
      var stack = document.querySelector('.toast-stack');
      if (stack) {
        var name = (col.querySelector('.board-col-head .name') || {}).textContent || 'kolumny';
        stack.insertAdjacentHTML('beforeend', '<div class="toast"><svg class="icon"><use href="#i-check-circle"/></svg>Przeniesiono do „' + name.trim() + '”<span class="undo">Cofnij</span></div>');
        var t = stack.lastElementChild; setTimeout(function () { t.remove(); }, 3200);
      }
    });
  });
})();
