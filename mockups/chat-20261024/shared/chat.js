// ===== File: chat.js — behaviour shared by the chat mockups: frame size, drawers, sparkline, streaming demo =====
(function () {
  'use strict';

  var root = document.documentElement;
  var reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  function store(key, value) {
    try { if (value === undefined) return localStorage.getItem(key); localStorage.setItem(key, value); } catch (e) { /* storage blocked */ }
    return null;
  }

  var query = new URLSearchParams(location.search);

  // ----- Conversation list: per-item options menu (rename, delete) -----
  var convMenu = null;
  function closeConvMenu() {
    if (!convMenu) return;
    convMenu.btn.setAttribute('aria-expanded', 'false');
    convMenu.el.remove();
    convMenu = null;
  }
  document.querySelectorAll('.conv-more').forEach(function (btn) {
    btn.addEventListener('click', function (e) {
      e.stopPropagation();
      var reopen = !(convMenu && convMenu.btn === btn);
      closeConvMenu();
      if (!reopen) return;
      var el = document.createElement('div');
      el.className = 'conv-menu';
      el.setAttribute('role', 'menu');
      el.innerHTML = '<button type="button" role="menuitem" data-act="rename"><svg class="icon"><use href="#i-edit"/></svg>Zmień nazwę</button>' +
                '<button type="button" role="menuitem" class="danger" data-act="delete"><svg class="icon"><use href="#i-trash"/></svg>Usuń rozmowę</button>';
      document.body.appendChild(el);
      var r = btn.getBoundingClientRect();
      el.style.top = Math.min(r.bottom + 4, window.innerHeight - el.offsetHeight - 8) + 'px';
      el.style.left = Math.max(8, Math.min(r.right - el.offsetWidth, window.innerWidth - el.offsetWidth - 8)) + 'px';
      btn.setAttribute('aria-expanded', 'true');
      convMenu = { el: el, btn: btn };
      el.querySelector('button').focus();
      el.addEventListener('keydown', function (ev) {
        if (ev.key !== 'ArrowDown' && ev.key !== 'ArrowUp') return;
        ev.preventDefault();
        var items = Array.prototype.slice.call(el.querySelectorAll('button'));
        var i = items.indexOf(document.activeElement);
        items[(i + (ev.key === 'ArrowDown' ? 1 : items.length - 1)) % items.length].focus();
      });
      el.addEventListener('click', function (ev) {
        var act = ev.target.closest('button');
        if (!act) return;
        var item = btn.closest('.conv-item');
        closeConvMenu();
        if (act.getAttribute('data-act') === 'delete') item.remove();
      });
    });
  });
  document.addEventListener('click', closeConvMenu);
  document.addEventListener('keydown', function (e) {
    if (e.key !== 'Escape' || !convMenu) return;
    var btn = convMenu.btn;
    closeConvMenu();
    btn.focus();
  });

  // ----- Frame size: desktop or 390 px phone -----
  var frame = document.querySelector('.screen-frame');
  function applyView(v) {
    if (!frame) return;
    frame.classList.toggle('phone', v === 'phone');
    document.querySelectorAll('[data-view-set]').forEach(function (b) { b.classList.toggle('active', b.getAttribute('data-view-set') === v); });
  }
  applyView(query.get('view') || frame && frame.getAttribute('data-default-view') || 'desktop');
  document.querySelectorAll('[data-view-set]').forEach(function (b) {
    b.addEventListener('click', function () { applyView(b.getAttribute('data-view-set')); });
  });

  // ----- Drawers (navigation and conversation list on narrow containers) -----
  var app = document.querySelector('.chat-app');
  var shell = document.querySelector('.chat-shell');
  document.querySelectorAll('.nav-toggle, .nav-scrim').forEach(function (el) {
    el.addEventListener('click', function () { app.classList.toggle('nav-open'); });
  });
  document.querySelectorAll('[data-conv-toggle], .conv-scrim').forEach(function (el) {
    el.addEventListener('click', function () { shell.classList.toggle('conv-open'); });
  });
  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape') { app.classList.remove('nav-open'); shell.classList.remove('conv-open'); }
  });

  // Keep the frame size when moving between the chat screens
  document.addEventListener('click', function (e) {
    var a = e.target.closest && e.target.closest('a[data-keep-view]');
    if (!a || !frame) return;
    var url = new URL(a.getAttribute('href'), location.href);
    url.searchParams.set('view', frame.classList.contains('phone') ? 'phone' : 'desktop');
    a.setAttribute('href', url.pathname.split('/').pop() + url.search);
  });

  // ----- Composer autogrow -----
  document.querySelectorAll('.composer textarea').forEach(function (ta) {
    var fit = function () { ta.style.height = 'auto'; ta.style.height = Math.min(ta.scrollHeight, 140) + 'px'; };
    ta.addEventListener('input', fit);
  });

  // ----- Session decode metric -----
  // The series is the decode speed of THIS conversation only, one sample per ~second of generation.
  var SPARK_W = 72, SPARK_H = 32;
  function renderSpark(svg, values, avg) {
    if (!svg || !values.length) return;
    var min = Math.min.apply(null, values.concat(avg)), max = Math.max.apply(null, values.concat(avg));
    var span = Math.max(max - min, 8);
    var y = function (v) { return (SPARK_H - 3 - ((v - min) / span) * (SPARK_H - 6)).toFixed(1); };
    var step = values.length > 1 ? SPARK_W / (values.length - 1) : 0;
    var pts = values.map(function (v, i) { return (i * step).toFixed(1) + ',' + y(v); }).join(' ');
    var last = values.length - 1;
    svg.innerHTML = '<line class="avg" x1="0" x2="' + SPARK_W + '" y1="' + y(avg) + '" y2="' + y(avg) + '"/>' +
      '<polyline points="' + pts + '"/>' +
      '<circle cx="' + (last * step).toFixed(1) + '" cy="' + y(values[last]) + '" r="2.4"/>';
  }
  function fmt(n, d) { return n.toFixed(d === undefined ? 1 : d).replace('.', ','); }

  var Session = {
    values: (window.__sessionSeed || []).slice(),
    cur: null,
    paint: function () {
      var v = this.values;
      if (!v.length) return;
      var avg = v.reduce(function (a, b) { return a + b; }, 0) / v.length;
      var cur = this.cur === null ? v[v.length - 1] : this.cur;
      document.querySelectorAll('[data-m="cur"]').forEach(function (e) { e.textContent = fmt(cur); });
      document.querySelectorAll('[data-m="avg"]').forEach(function (e) { e.textContent = fmt(avg); });
      document.querySelectorAll('[data-m="min"]').forEach(function (e) { e.textContent = fmt(Math.min.apply(null, v)); });
      document.querySelectorAll('[data-m="max"]').forEach(function (e) { e.textContent = fmt(Math.max.apply(null, v)); });
      document.querySelectorAll('[data-m="n"]').forEach(function (e) { e.textContent = v.length; });
      document.querySelectorAll('.spark').forEach(function (s) { renderSpark(s, v, avg); });
    },
    push: function (x) { this.values.push(x); if (this.values.length > 24) this.values.shift(); this.cur = x; this.paint(); }
  };
  window.ChatSession = Session;
  Session.paint();

  // ----- Streaming demo (ch02): tokens appear at the model's pace, the metric follows -----
  var stream = document.querySelector('[data-stream]');
  if (stream) {
    var target = stream.querySelector('.stream-target');
    var words = stream.getAttribute('data-stream').split('|');
    var sendBtn = document.querySelector('[data-send]');
    var meta = stream.querySelector('.msg-meta');
    var timer = null, sampler = null;

    var setBusy = function (busy) {
      if (sendBtn) {
        sendBtn.innerHTML = '<svg class="icon"><use href="#i-' + (busy ? 'stop' : 'send') + '"/></svg>';
        sendBtn.setAttribute('aria-label', busy ? 'Zatrzymaj' : 'Wyślij');
      }
      target.classList.toggle('caret', busy);
      if (meta) meta.style.visibility = busy ? 'hidden' : 'visible';
    };

    var run = function () {
      clearInterval(timer); clearInterval(sampler);
      target.innerHTML = '';
      var i = 0, shown = '', rate = 46, started = performance.now(), tokens = 0, window5 = [];
      setBusy(true);
      var scroller = document.querySelector('.chat-scroll');
      timer = setInterval(function () {
        // one or two words per 100 ms tick; Polish words are about 1.4 tokens each, so the pace is readable rather than literal
        var n = 1 + Math.round(Math.random());
        for (var k = 0; k < n && i < words.length; k++, i++) { shown += words[i] + ' '; tokens += 3; }
        target.innerHTML = shown.replace(/\\n/g, '<span class="para"></span>');
        if (scroller) scroller.scrollTop = scroller.scrollHeight;
        window5.push({ t: performance.now(), tk: tokens });
        if (i >= words.length) {
          clearInterval(timer); clearInterval(sampler);
          target.innerHTML = shown.replace(/\\n/g, '<span class="para"></span>');
          setBusy(false);
        }
      }, reduce ? 4 : 100);
      sampler = setInterval(function () {
        // decode speed over the last ~1 s window, with a little device jitter
        var now = performance.now();
        var recent = window5.filter(function (s) { return now - s.t < 1000; });
        var v = rate + (Math.random() - 0.5) * 9;
        if (recent.length > 1) {
          var span = (recent[recent.length - 1].t - recent[0].t) / 1000;
          v = Math.max(32, Math.min(56, (recent[recent.length - 1].tk - recent[0].tk) / span + (Math.random() - 0.5) * 4));
        }
        Session.push(v);
      }, reduce ? 50 : 900);
    };
    document.querySelectorAll('[data-replay]').forEach(function (b) { b.addEventListener('click', run); });
    if (sendBtn) sendBtn.addEventListener('click', function () {
      if (target.classList.contains('caret')) { clearInterval(timer); clearInterval(sampler); setBusy(false); } else { run(); }
    });
    run();
  }

})();
