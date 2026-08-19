// =============================================================================
// Plik: shared/mock.js
// Opis: Wspólne helpery mockupów Analityki — adaptacyjne formatowanie liczb,
//       count-up KPI, stacked wykres słupkowy z tooltipem, sparkline, shell.
// Przykład: Mock.fmtCompact(121000000) -> "121 mln"
// =============================================================================

const Mock = (() => {
  const PL = new Intl.NumberFormat('pl-PL');

  // Adaptacyjne jednostki: <10 tys pełna liczba, dalej tys / mln / mld.
  function fmtCompact(n) {
    n = Number(n) || 0;
    const abs = Math.abs(n);
    if (abs < 10_000) return PL.format(n);
    if (abs < 1_000_000) return trim(n / 1_000) + ' tys';
    if (abs < 1_000_000_000) return trim(n / 1_000_000) + ' mln';
    return trim(n / 1_000_000_000) + ' mld';
  }
  function trim(v) {
    const s = v >= 100 ? Math.round(v).toString() : v.toFixed(1).replace('.', ',');
    return s.replace(',0', '');
  }
  const fmtExact = (n) => PL.format(Number(n) || 0);
  const fmtPln = (n) => new Intl.NumberFormat('pl-PL', { style: 'currency', currency: 'PLN' }).format(n);

  // Count-up KPI: rAF ~600 ms, tabular-nums zapobiega skakaniu szerokości.
  function countUp(el, target, fmt = fmtCompact) {
    const t0 = performance.now(), dur = 600;
    function tick(t) {
      const p = Math.min(1, (t - t0) / dur);
      const eased = 1 - Math.pow(1 - p, 3);
      el.textContent = fmt(Math.round(target * eased));
      if (p < 1) requestAnimationFrame(tick);
    }
    el.title = fmtExact(target);
    requestAnimationFrame(tick);
  }

  // Stacked słupki prompt/completion + tooltip + crosshair.
  // data: [{label, a, b}], colors: [prompt, completion]
  function stackedBars(host, data, opts = {}) {
    // viewBox = realna szerokość kontenera — tekst osi ma zawsze ~10px,
    // zamiast skalować się w dół razem z SVG na wąskich ekranach.
    const W = Math.max(340, host.clientWidth || 960);
    const H = opts.height || 240, padL = 46, padB = 22, padT = 10;
    const mobile = W < 560;
    if (mobile) data = data.slice(-10);
    const max = Math.max(...data.map(d => d.a + d.b)) * 1.08 || 1;
    const iw = (W - padL - 8) / data.length;
    const bw = Math.min(iw * 0.62, 34);
    const y = v => padT + (H - padT - padB) * (1 - v / max);
    let s = `<svg class="chart-svg" viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet">`;
    for (let g = 0; g <= 4; g++) {
      const v = max * g / 4, yy = y(v);
      s += `<line class="grid" x1="${padL}" y1="${yy}" x2="${W - 8}" y2="${yy}"/>`;
      s += `<text x="${padL - 6}" y="${yy + 3}" text-anchor="end">${fmtCompact(v)}</text>`;
    }
    const step = Math.ceil(data.length / (mobile ? 5 : 14));
    data.forEach((d, i) => {
      const x = padL + i * iw + (iw - bw) / 2;
      const hA = (H - padT - padB) * d.a / max;
      const hB = (H - padT - padB) * d.b / max;
      const yB = y(d.b), yA = yB - hA;
      const dl = (i * 0.014).toFixed(3);
      s += `<g class="bar-g" data-i="${i}">`;
      s += `<rect class="bar-anim" style="animation-delay:${dl}s" x="${x}" y="${yB}" width="${bw}" height="${hB}" rx="2" fill="${opts.colors?.[1] || 'var(--accent-1)'}"/>`;
      s += `<rect class="bar-anim" style="animation-delay:${dl}s" x="${x}" y="${yA}" width="${bw}" height="${hA}" rx="3" fill="${opts.colors?.[0] || 'var(--accent-2)'}"/>`;
      s += `<rect class="hit" x="${padL + i * iw}" y="${padT}" width="${iw}" height="${H - padT - padB}" fill="transparent"/>`;
      s += `</g>`;
      if (i % step === 0) s += `<text x="${x + bw / 2}" y="${H - 6}" text-anchor="middle">${d.label}</text>`;
    });
    s += `</svg><div class="chart-tip"></div>`;
    host.classList.add('chart-wrap');
    host.innerHTML = s;
    const tip = host.querySelector('.chart-tip');
    host.querySelectorAll('.bar-g').forEach(g => {
      g.addEventListener('mousemove', (e) => {
        const d = data[+g.dataset.i];
        tip.innerHTML = `<div class="t-date">${d.label}</div>
          <div class="t-row"><span>prompt</span><b>${fmtExact(d.a)}</b></div>
          <div class="t-row"><span>completion</span><b>${fmtExact(d.b)}</b></div>
          <div class="t-row"><span>razem</span><b>${fmtExact(d.a + d.b)}</b></div>`;
        const r = host.getBoundingClientRect();
        tip.style.left = Math.min(e.clientX - r.left + 14, r.width - 170) + 'px';
        tip.style.top = (e.clientY - r.top - 10) + 'px';
        tip.classList.add('on');
      });
      g.addEventListener('mouseleave', () => tip.classList.remove('on'));
    });
  }

  // Sparkline (mała linia trendu w KPI / kartach encji).
  function sparkline(values, w = 90, h = 26, color = 'var(--accent-2)') {
    const max = Math.max(...values), min = Math.min(...values);
    const pts = values.map((v, i) =>
      `${(i / (values.length - 1)) * w},${h - 2 - (h - 4) * ((v - min) / ((max - min) || 1))}`).join(' ');
    return `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">
      <polyline class="line-anim" style="--len:220" points="${pts}" fill="none" stroke="${color}" stroke-width="1.8" stroke-linecap="round"/></svg>`;
  }

  // Nawigacja shellowa wspólna dla wszystkich stron mockupu.
  function shell(active) {
    const items = [
      ['analytics', 'Analityka', true],
      ['audit', 'Dziennik audytu'], ['bench', 'Benchmark Studio'],
      ['rodo', 'Dokumenty RODO'], ['prof', 'Profilowanie'],
    ];
    return `<div class="topbar-mobile"><button class="burger">☰</button>
      <span class="name">TentaFlow</span><span class="where">Analityka</span></div>
    <aside class="sidebar">
      <div class="logo"><span style="font-size:20px">🐙</span><span class="name">TentaFlow</span></div>
      <div class="nav-section"><div class="heading">Zarządzanie</div>
        <div class="nav-item">Addons</div><div class="nav-item">Użytkownicy</div>
        <div class="nav-item">Dostęp i klucze API</div>
        ${items.map(([id, l, act]) => `<div class="nav-item${act ? ' active' : ''}">${l}</div>`).join('')}
      </div></aside>`;
  }

  function tabs(active) {
    const t = [['przeglad', 'Przegląd', 'm01-przeglad.html'],
      ['users', 'Użytkownicy i grupy', 'm02-drilldown.html'],
      ['models', 'Modele', 'm03-modele.html'],
      ['nodes', 'Nody i serwisy', 'm04-nody.html'],
      ['limits', 'Limity', 'm05-limity.html'],
      ['billing', 'Rozliczenia', 'm06-rozliczenia.html']];
    return `<div class="an-tabs">${t.map(([id, l, href]) =>
      `<button class="an-tab${id === active ? ' active' : ''}" onclick="location='${href}'">${l}</button>`).join('')}</div>`;
  }

  return { fmtCompact, fmtExact, fmtPln, countUp, stackedBars, sparkline, shell, tabs };
})();
